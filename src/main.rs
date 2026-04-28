mod backfill;
mod beacon_client;
mod chain;
mod config;
mod db;
mod error;
mod exits;
mod live;
mod live_updates;
mod metrics;
mod scanner;
mod web;

use crate::live_updates::LiveUpdateEvent;
use config::Config;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::load().await?;
    tracing::debug!(
        beacon_url = %config.beacon_url,
        backfill_beacon_url = ?config.backfill_beacon_url,
        auth_enabled = !config.api_key.is_empty(),
        backfill_only = config.backfill_only,
        max_backfill_depth = ?config.max_backfill_depth,
        non_contiguous_backfill = config.non_contiguous_backfill,
        scan_mode = ?config.scan_mode,
        tags_count = config.validator_meta.len(),
        "Configuration loaded"
    );

    let validator_indices = config.validator_indices.clone();
    if validator_indices.is_empty() {
        anyhow::bail!("No validator indices specified. Use --validators or --config-file");
    }

    let effective_scan_mode = config.scan_mode.resolve(validator_indices.len());
    tracing::info!(
        configured = ?config.scan_mode,
        effective = ?effective_scan_mode,
        validator_count = validator_indices.len(),
        "Resolved attestation scan mode"
    );

    let pool = db::connect(&config.database_url).await?;
    let live_client = Arc::new(beacon_client::BeaconClient::new(&config.beacon_url));

    // Load chain config from the live node before anything else touches the
    // `chain::` accessors (scanner pipeline, backfill probe, web-SSE handler).
    // Runs on the live client because it's available in every mode and the
    // spec endpoint doesn't need archival state.
    let chain_spec = live_client
        .get_chain_spec()
        .await
        .map_err(|e| anyhow::anyhow!("failed to fetch chain spec from beacon node: {e}"))?;
    tracing::info!(
        slots_per_epoch = chain_spec.slots_per_epoch,
        seconds_per_slot = chain_spec.seconds_per_slot,
        sync_committee_size = chain_spec.sync_committee_size,
        max_committees_per_slot = chain_spec.max_committees_per_slot,
        altair_fork_epoch = chain_spec.altair_fork_epoch,
        "Loaded chain spec from beacon node"
    );
    chain::init(chain_spec);

    // Separate client for backfill when a dedicated URL is configured (typical:
    // archive node for backfill, non-archive for live). Otherwise share.
    let backfill_client = match &config.backfill_beacon_url {
        Some(url) => {
            tracing::info!(url, "Using separate beacon client for backfill");
            Arc::new(beacon_client::BeaconClient::new(url))
        }
        None => live_client.clone(),
    };
    let instance_id = Uuid::new_v4();
    let tracked_validators = Arc::new(validator_indices.clone());
    let (live_updates_tx, _) = broadcast::channel::<LiveUpdateEvent>(512);

    let metrics_addr = std::net::SocketAddr::from(([0, 0, 0, 0], config.metrics_port));
    tokio::spawn(async move {
        if let Err(e) = metrics::run_server(metrics_addr).await {
            tracing::error!(error = %e, "Metrics server failed");
        }
    });

    if !config.backfill_only {
        let web_addr = std::net::SocketAddr::from(([0, 0, 0, 0], config.web_port));
        let web_pool = pool.clone();
        let web_client = live_client.clone();
        let web_tracked_validators = tracked_validators.clone();
        let web_live_updates_tx = live_updates_tx.clone();
        let web_config = config.clone();
        tokio::spawn(async move {
            if let Err(e) = web::run_server(
                web_pool,
                web_client,
                web_tracked_validators,
                web_live_updates_tx,
                web_addr,
                web_config,
            )
            .await
            {
                tracing::error!(error = %e, "Web server failed");
            }
        });
    } else {
        tracing::info!("Backfill-only mode enabled; web server will not start");
    }

    tracing::info!(
        count = validator_indices.len(),
        instance = %instance_id,
        validators = ?validator_indices,
        "Starting indexer"
    );

    db::scanner::instance::register_instance(&pool, instance_id).await?;

    let beacon_validators = live_client
        .get_validators("head", &validator_indices)
        .await?;
    tracing::debug!(fetched = beacon_validators.len(), "Got validator metadata");

    for v in &beacon_validators {
        let pubkey_bytes =
            hex::decode(v.validator.pubkey.trim_start_matches("0x")).unwrap_or_default();
        let exit_epoch = if v.validator.exit_epoch == u64::MAX {
            None
        } else {
            Some(v.validator.exit_epoch as i64)
        };

        db::scanner::validators::upsert_validator(
            &pool,
            v.index as i64,
            &pubkey_bytes,
            v.validator.activation_epoch as i64,
            exit_epoch,
        )
        .await?;
    }

    let db_validators = db::scanner::validators::get_all_validators(&pool).await?;
    let tracked_set: HashSet<u64> = validator_indices.iter().copied().collect();
    let mut validator_scan_state: HashMap<u64, (u64, Option<u64>, Option<u64>)> = HashMap::new();
    let mut validator_exits: HashMap<u64, u64> = HashMap::new();
    for v in &db_validators {
        if tracked_set.contains(&(v.validator_index as u64)) {
            let idx = v.validator_index as u64;
            let exit = v.exit_epoch.map(|e| e as u64);
            validator_scan_state.insert(
                idx,
                (
                    v.activation_epoch as u64,
                    v.last_scanned_epoch.map(|e| e as u64),
                    exit,
                ),
            );
            if let Some(e) = exit {
                validator_exits.insert(idx, e);
            }
        }
    }
    let validator_exits = Arc::new(validator_exits);
    if !validator_exits.is_empty() {
        tracing::info!(
            count = validator_exits.len(),
            "Tracked validators with exit_epoch set — they will be skipped past exit"
        );
    }

    let needs_backfill = validator_scan_state
        .values()
        .filter(|(_, last, _)| last.is_none())
        .count();
    if needs_backfill > 0 {
        tracing::info!(count = needs_backfill, "Validators need backfill");
    }

    let f0 = live_client
        .get_finality_checkpoints("head")
        .await?
        .finalized
        .epoch;
    tracing::info!(
        f0,
        backfill_only = config.backfill_only,
        "Spawning backfill task (initial target = current finality)"
    );

    // Inform the user about resume-after-gap situations: if the DB has a
    // watermark older than current finality, we're picking up from where a
    // prior run left off and need to catch up on the gap.
    let min_last_scanned = validator_scan_state
        .values()
        .filter_map(|(_, ls, _)| *ls)
        .min();
    if let Some(ls) = min_last_scanned
        && f0 > ls + 1
    {
        tracing::info!(
            last_scanned_epoch = ls,
            current_finalized = f0,
            gap_epochs = f0 - ls,
            "Resuming after gap; backfill will catch up"
        );
    }

    // Probe the backfill client's archival capability at the earliest epoch
    // backfill would try to scan. We bail early on a mis-configured dedicated
    // backfill URL and warn explicitly on a shared non-archive node so the
    // operator knows which flags to set.
    if let Some(earliest) = backfill::earliest_epoch_to_scan(&config, &validator_scan_state)
        && earliest <= f0
    {
        match backfill::probe_archival_capability(&backfill_client, earliest).await {
            Ok(true) => {
                tracing::debug!(
                    earliest,
                    "Backfill client can serve earliest required epoch"
                );
            }
            Ok(false) => {
                if config.backfill_beacon_url.is_some() {
                    // Dedicated URL → the operator explicitly opted into a
                    // separate backfill node expecting it to be archival.
                    // A 404 here means the config is wrong; fail fast.
                    anyhow::bail!(
                        "configured `backfill_beacon_url` cannot serve epoch {earliest} \
                         — it must be an archive node. Point it at a node with historical \
                         state retention or remove the setting to share the live client."
                    );
                }
                let has_existing_data =
                    validator_scan_state.values().any(|(_, ls, _)| ls.is_some());
                if has_existing_data {
                    // Resume-after-gap: DB has rows from a prior run but the
                    // current beacon client has pruned past them.
                    tracing::warn!(
                        earliest,
                        f0,
                        "Data gap cannot be refilled by the current beacon client \
                         (state at epoch {earliest} has been pruned). To refill, point \
                         `backfill_beacon_url` at an archive node and run with \
                         `--non-contiguous-backfill` (also settable via config file or \
                         `NON_CONTIGUOUS_BACKFILL=1`)."
                    );
                } else {
                    // Fresh start: validators have no DB history and the node
                    // can't serve the full range.
                    tracing::warn!(
                        earliest,
                        "Backfill will be truncated: the beacon client cannot serve epoch \
                         {earliest} (state pruned). Set `backfill_beacon_url` to an archive \
                         node for full history."
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    earliest,
                    "Failed to probe backfill client capability; proceeding anyway"
                );
            }
        }
    }

    let bf_client = backfill_client.clone();
    let bf_pool = pool.clone();
    let bf_config = config.clone();
    let bf_tx = live_updates_tx.clone();
    let extend = config.backfill_only;
    let backfill_handle = tokio::spawn(async move {
        backfill::run_backfill(
            &bf_client,
            &bf_pool,
            &bf_config,
            validator_scan_state,
            f0,
            instance_id,
            bf_tx,
            extend,
        )
        .await
    });

    if config.backfill_only {
        // Await the background task; its extend loop exits once caught up.
        match backfill_handle.await {
            Ok(Ok(())) => {
                tracing::info!("Backfill-only mode complete; exiting");
                Ok(())
            }
            Ok(Err(e)) => Err(e.into()),
            Err(e) => Err(anyhow::anyhow!("backfill task panicked: {e}")),
        }
    } else {
        // Concurrent mode: live in the foreground, backfill in the background.
        // SSE events are never blocked by the slower historical scan.
        let live_result = live::run_live_tracking(
            live_client.as_ref(),
            &pool,
            instance_id,
            &tracked_set,
            &validator_exits,
            effective_scan_mode,
            live_updates_tx,
            f0,
        )
        .await;

        // Live returning is fatal — stop the background task so we don't leak
        // it on process exit.
        backfill_handle.abort();
        live_result?;
        Ok(())
    }
}
