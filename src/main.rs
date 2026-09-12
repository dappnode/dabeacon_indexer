mod backfill;
mod beacon_client;
mod chain;
mod config;
mod db;
mod error;
mod live;
mod live_updates;
mod metrics;
mod scanner;
mod validators_update;
mod web;

use crate::live_updates::LiveUpdateEvent;
use config::{Config, RunMode};
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
        run_mode = ?config.mode,
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
    let live_client =
        Arc::new(beacon_client::BeaconClient::new(&config.beacon_url).with_pool(pool.clone()));

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
    anyhow::ensure!(
        config.live.lag_slots < chain::epoch_start_slot(config.live.retry_window_epochs),
        "live lag must fit inside retry window"
    );

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

    if config.mode.runs_live() {
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
        tracing::info!("Backfill-only mode; web server will not start");
    }

    tracing::info!(
        count = validator_indices.len(),
        instance = %instance_id,
        validators = ?validator_indices,
        "Starting indexer"
    );

    db::scanner::instance::register_instance(&pool, instance_id).await?;
    validators_update::update(&live_client, &pool, &validator_indices).await?;

    tracing::debug!(count = validator_indices.len(), "Seeded validator metadata");

    if config.mode.runs_live() {
        let client = live_client.clone();
        let pool = pool.clone();
        let tracked = tracked_validators.clone();
        tokio::spawn(async move {
            validators_update::run_update_loop(client, pool, tracked).await;
        });
    }

    let db_validators = db::scanner::validators::get_all_validators(&pool).await?;
    let tracked_set: HashSet<u64> = validator_indices.iter().copied().collect();
    let mut validator_scan_state: HashMap<u64, (u64, Option<u64>)> = HashMap::new();
    for v in &db_validators {
        if tracked_set.contains(&(v.validator_index as u64)) {
            validator_scan_state.insert(
                v.validator_index as u64,
                (
                    v.activation_epoch as u64,
                    v.last_scanned_epoch.map(|e| e as u64),
                ),
            );
        }
    }

    let needs_backfill = validator_scan_state
        .values()
        .filter(|(_, last)| last.is_none())
        .count();
    if needs_backfill > 0 {
        tracing::info!(count = needs_backfill, "Validators need backfill");
    }

    let checkpoint_epoch = live_client
        .get_finality_checkpoints("head")
        .await?
        .finalized
        .epoch;
    let finalized_target = chain::finalized_scan_target(checkpoint_epoch);
    let f0 = finalized_target.unwrap_or(0);
    tracing::info!(checkpoint_epoch, finalized_target = ?finalized_target, run_mode = ?config.mode, "Current finality target");

    let backfill_should_run = config.mode.runs_backfill() && finalized_target.is_some();

    // ─── Gap detection ─────────────────────────────────────────────────────
    // Check whether the DB has attestation_duties rows for each validator's
    // full range [effective_start, last_scanned]. A mismatch means a previous
    // live-only run advanced one or more watermarks past epochs it never scanned.
    if config.mode.runs_backfill() && !config.non_contiguous_backfill {
        let mut coverage_ranges: Vec<(i64, i64, i64)> = Vec::new();
        let mut expected = 0_u64;

        for (&validator_index, &(activation_epoch, last_scanned_epoch)) in &validator_scan_state {
            let Some(last_scanned_epoch) = last_scanned_epoch else {
                continue;
            };
            let start_epoch = config
                .max_backfill_depth
                .map_or(activation_epoch, |depth| activation_epoch.max(depth));
            if last_scanned_epoch < start_epoch {
                continue;
            }

            coverage_ranges.push((
                validator_index as i64,
                start_epoch as i64,
                last_scanned_epoch as i64,
            ));
            expected += last_scanned_epoch - start_epoch + 1;
        }

        if !coverage_ranges.is_empty() {
            let from_epoch = coverage_ranges
                .iter()
                .map(|(_, from, _)| *from)
                .min()
                .unwrap() as u64;
            let to_epoch = coverage_ranges.iter().map(|(_, _, to)| *to).max().unwrap() as u64;
            let found =
                db::scanner::attestations::count_covered_validator_epochs(&pool, &coverage_ranges)
                    .await
                    .unwrap_or(0) as u64;
            let missing = expected.saturating_sub(found);
            if missing > 0 {
                tracing::warn!(
                    from_epoch,
                    to_epoch,
                    validators = coverage_ranges.len(),
                    expected_validator_epochs = expected,
                    found_validator_epochs = found,
                    missing_validator_epochs = missing,
                    "Incomplete historical scans detected. Run with `--non-contiguous-backfill` \
                     and an archive node to repair missing or unverified data."
                );
            }
        }
    }

    if config.mode.runs_backfill() {
        let min_last_scanned = validator_scan_state
            .values()
            .filter_map(|(_, ls)| *ls)
            .min();
        if let Some(ls) = min_last_scanned
            && f0 > ls + 1
        {
            tracing::info!(
                last_scanned_epoch = ls,
                current_finalized = f0,
                gap_epochs = f0 - ls,
                "Resuming after gap; backfill will attempt to catch up"
            );
        }

        if matches!(config.mode, RunMode::Backfill)
            && finalized_target.is_some()
            && let Some(earliest) = backfill::earliest_epoch_to_scan(&config, &validator_scan_state)
            && earliest <= f0
        {
            match backfill::probe_archival_capability(&backfill_client, earliest).await {
                Ok(true) => {}
                Ok(false) => anyhow::bail!(
                    "backfill client cannot serve epoch {earliest} (state pruned). \
                     Use an archive node, or --mode both to keep live tracking running."
                ),
                Err(e) => anyhow::bail!("failed to probe backfill client at epoch {earliest}: {e}"),
            }
        }
    }

    // ─── Spawn workloads per mode ──────────────────────────────────────────

    let live_updates_tx_for_bf = live_updates_tx.clone();
    let live_fut = async {
        if config.mode.runs_live() {
            live::run_live_tracking(
                live_client.as_ref(),
                &pool,
                instance_id,
                &tracked_set,
                live_updates_tx,
                config.live.clone(),
            )
            .await
            .map_err(anyhow::Error::from)
        } else {
            futures::future::pending::<anyhow::Result<()>>().await
        }
    };

    let backfill_fut = async {
        if matches!(config.mode, RunMode::Both) {
            let mut historical_done = !backfill_should_run;
            let mut historical_retry = backfill::HistoricalRetry::default();
            let tracked: Vec<i64> = tracked_set.iter().map(|v| *v as i64).collect();
            loop {
                // Repair recent persisted gaps even if much older history is unavailable.
                match live_client.get_finality_checkpoints("head").await {
                    Ok(checkpoint) => {
                        if let Some(target) =
                            chain::finalized_scan_target(checkpoint.finalized.epoch)
                            && let Err(error) = backfill::repair_live_gaps(
                                &backfill_client,
                                &pool,
                                &tracked,
                                target,
                                effective_scan_mode,
                            )
                            .await
                        {
                            tracing::warn!(%error,"Live gap repair pending");
                        }
                    }
                    Err(error) => tracing::debug!(%error,"Cannot refresh archive repair target"),
                }
                if !historical_done && historical_retry.ready(std::time::Instant::now()) {
                    match backfill::run_backfill(
                        &backfill_client,
                        &pool,
                        &config,
                        validator_scan_state.clone(),
                        f0,
                        instance_id,
                        live_updates_tx_for_bf.clone(),
                        false,
                    )
                    .await
                    {
                        Ok(()) => historical_done = true,
                        Err(error) => {
                            historical_retry.failed(&error, std::time::Instant::now());
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        } else if backfill_should_run {
            backfill::run_backfill(
                &backfill_client,
                &pool,
                &config,
                validator_scan_state.clone(),
                f0,
                instance_id,
                live_updates_tx_for_bf.clone(),
                true,
            )
            .await
            .map_err(anyhow::Error::from)
        } else if matches!(config.mode, RunMode::Backfill) {
            Ok(())
        } else {
            futures::future::pending::<anyhow::Result<()>>().await
        }
    };

    tokio::pin!(live_fut);
    tokio::pin!(backfill_fut);

    tokio::select! {
        live_res = &mut live_fut => {
            match live_res {
                Ok(()) => {
                    tracing::info!("Live worker exited cleanly");
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        bf_res = &mut backfill_fut => {
            match bf_res {
                Ok(()) if matches!(config.mode, RunMode::Both) => {
                    tracing::info!("Backfill caught up; live continuing");
                    live_fut.await
                }
                Ok(()) => {
                    tracing::info!("Backfill complete; exiting");
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
    }
}
