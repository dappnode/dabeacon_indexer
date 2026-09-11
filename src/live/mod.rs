use std::collections::HashSet;
use std::time::Duration;

use futures::StreamExt;
use reqwest_eventsource::{Event, EventSource};
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

use crate::beacon_client::BeaconClient;
use crate::beacon_client::types::{BlockRoot, FinalizedCheckpointEvent, HeadEvent};
use crate::chain::{epoch_start_slot, slot_to_epoch};
use crate::config::LiveConfig;
use crate::db::{Pool as PgPool, scanner as db_scanner};
use crate::error::{Error, Result};
use crate::live_updates::LiveUpdateEvent;

mod finalization;
mod head;
mod reorg;

/// SSE is a bounded wake-up signal, never the canonical database writer.
/// Reconnect, malformed events, and missed events all recover through polling.
async fn receive_events(url: String, wake: watch::Sender<u64>) {
    let mut generation = 0u64;
    loop {
        let mut stream = EventSource::get(&url);
        while let Some(event) = stream.next().await {
            match event {
                Ok(Event::Open) | Ok(Event::Message(_)) => {
                    crate::metrics::LIVE_SSE_EVENTS
                        .with_label_values(&["notification", "ok"])
                        .inc();
                    generation = generation.wrapping_add(1);
                    if wake.send(generation).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "Beacon SSE disconnected; polling remains active");
                    break;
                }
            }
        }
        stream.close();
        if wake.is_closed() {
            return;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

pub async fn run_live_tracking(
    client: &BeaconClient,
    pool: &PgPool,
    instance_id: Uuid,
    tracked: &HashSet<u64>,
    live_updates_tx: broadcast::Sender<LiveUpdateEvent>,
    config: LiveConfig,
) -> Result<()> {
    let url = client.url("/eth/v1/events?topics=head,finalized_checkpoint,chain_reorg");
    let (wake_tx, mut wake_rx) = watch::channel(0);
    // Both futures are scoped to this invocation; cancellation drops the SSE reader too.
    let worker = async {
        let indices: Vec<i64> = tracked.iter().map(|v| *v as i64).collect();
        let mut cursor = db_scanner::live::processed_tip(pool, &indices).await?;
        let mut timer = tokio::time::interval(Duration::from_secs(config.poll_interval_seconds));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = timer.tick() => {},
                changed = wake_rx.changed() => { if changed.is_err() { return Ok(()); } }
            }
            if let Err(error) = reconcile(client, pool, tracked, &config, &mut cursor).await {
                tracing::warn!(%error, "Live reconciliation incomplete; retrying on next event or poll");
            }
            let _ = live_updates_tx.send(LiveUpdateEvent::LiveHeadProcessed);
            if let Err(error) = db_scanner::instance::update_heartbeat(pool, instance_id).await {
                tracing::warn!(%error, "Live heartbeat failed");
            }
        }
    };
    tokio::select! {
        result = worker => result,
        () = receive_events(url, wake_tx) => Ok(()),
    }
}

fn collection_start(head_slot: u64, cursor: Option<u64>, window_epochs: u64) -> u64 {
    let floor = epoch_start_slot(slot_to_epoch(head_slot).saturating_sub(window_epochs));
    cursor.map_or(epoch_start_slot(slot_to_epoch(head_slot)), |slot| {
        slot.saturating_add(1).max(floor)
    })
}

async fn reconcile(
    client: &BeaconClient,
    pool: &PgPool,
    tracked: &HashSet<u64>,
    config: &LiveConfig,
    cursor: &mut Option<u64>,
) -> Result<()> {
    let header = client.get_head_header().await?;
    let head_state_root = header.header.message.state_root.clone();
    let head = HeadEvent {
        slot: header.header.message.slot,
        block: header.root,
        epoch_transition: false,
    };
    let target = head.slot.saturating_sub(config.lag_slots);
    let epoch = slot_to_epoch(head.slot);
    let indices: Vec<i64> = tracked.iter().map(|v| *v as i64).collect();
    db_scanner::live::register_tracking_start(pool, &indices, epoch).await?;
    let floor = epoch_start_slot(epoch.saturating_sub(config.retry_window_epochs));
    if cursor.is_some_and(|last| last < floor) {
        // An outage beyond the retry window must not pin collection to pruned
        // state. Check the old branch independently, then start a recent range.
        let old = db_scanner::live::recent_blocks(pool, 0).await?;
        if let Some((_, tip, _)) = old.last()
            && client.get_header(tip.as_str()).await.is_err()
            && let Some((first, _, _)) = old.first()
        {
            reorg::rollback(client, pool, *first, cursor).await?;
        }
        *cursor = None;
    }
    let stored = db_scanner::live::recent_blocks(pool, floor).await?;
    let start = collection_start(head.slot, *cursor, config.retry_window_epochs);
    let walk_start = stored
        .first()
        .map_or(start, |(slot, _, _)| start.min(*slot));
    let resolved = head::resolve_chain(client, &head, walk_start.min(target)).await?;

    if let Some((changed, _, _)) = stored
        .iter()
        .find(|(slot, root, _)| *slot <= head.slot && resolved.roots.get(slot) != Some(root))
    {
        reorg::rollback(client, pool, *changed, cursor).await?;
        return Ok(()); // Resolve again with the expanded replay range before writing.
    }
    if stored.iter().any(|(slot, _, _)| *slot > head.slot) {
        reorg::rollback(client, pool, head.slot.saturating_add(1), cursor).await?;
        return Ok(());
    }

    let start = collection_start(head.slot, *cursor, config.retry_window_epochs);
    if cursor.is_none_or(|last| last.saturating_add(1) < start) {
        tracing::warn!(start, previous = ?cursor, "Starting recent collection; earlier slots remain incomplete for backfill");
        *cursor = start.checked_sub(1);
    }
    // Fetch current/next assignments before their state can disappear. A future
    // epoch prefetch failure is never allowed to block current collection.
    let validators: Vec<u64> = tracked.iter().copied().collect();
    for duty_epoch in epoch..=epoch.saturating_add(1) {
        let result = async {
            client.get_attester_duties(duty_epoch, &validators).await?;
            client.get_committees(duty_epoch).await?;
            client.get_proposer_duties(duty_epoch).await?;
            client.get_sync_duties(duty_epoch, &validators).await?;
            Ok::<(), Error>(())
        }
        .await;
        if let Err(error) = result {
            tracing::debug!(duty_epoch, %error, "Duty prefetch pending");
        }
    }
    let scan_result =
        head::process_head_scan(client, pool, tracked, &head, cursor, &resolved, target).await;
    // A branch switch during collection invalidates everything written in this
    // attempt before rewards or finalization can observe it as complete.
    if !canonical_root(client, head.slot, &head.block).await? {
        reorg::rollback(client, pool, walk_start, cursor).await?;
        return Ok(());
    }
    if let Err(error) = scan_result {
        tracing::warn!(%error, "Recent slot collection pending");
    }

    let min_epoch = epoch.saturating_sub(config.retry_window_epochs);
    if let Some(latest_reward_epoch) = latest_reward_epoch(head.slot, config.lag_slots) {
        for reward_epoch in min_epoch..=latest_reward_epoch {
            let active =
                db_scanner::validators::active_validators_at(pool, &indices, reward_epoch as i64)
                    .await?;
            let active: Vec<i64> = active.into_iter().map(|v| v as i64).collect();
            db_scanner::live::enqueue_jobs(
                pool,
                reward_epoch,
                None,
                "attestation_rewards",
                &active,
                None,
            )
            .await?;
        }
    }
    db_scanner::live::expire_jobs(pool, min_epoch).await?;
    for job in db_scanner::live::pending_jobs(pool, min_epoch, 12).await? {
        let timer = crate::metrics::LIVE_EPOCH_REWARDS_DURATION
            .with_label_values(&[&job.component])
            .start_timer();
        if let Err(error) = collect_job(client, pool, &job, &resolved).await {
            db_scanner::live::fail_job(pool, &job, &error.to_string()).await?;
            tracing::debug!(epoch = job.epoch, component = %job.component, %error, "Live reward component pending");
        }
        timer.observe_duration();
    }
    let finality = client
        .get_finality_checkpoints_response(head_state_root.as_str())
        .await?;
    if finality.execution_optimistic == Some(true) {
        return Err(Error::InconsistentBeaconData(
            "Optimistic finality response".into(),
        ));
    }
    let finalized = FinalizedCheckpointEvent {
        block: finality.data.finalized.root,
        epoch: finality.data.finalized.epoch,
    };
    finalization::finalize_collected_evidence(client, pool, tracked, &finalized).await?;
    let retain_epoch = min_epoch.min(finalized.epoch.saturating_sub(2));
    db_scanner::live::prune_completed_evidence(pool, retain_epoch).await?;
    client.prune_inputs_before(retain_epoch).await?;
    db_scanner::live::report_progress(pool).await?;
    Ok(())
}

/// Attestation rewards for E need the end-of-E+1 state. Apply the same
/// imported-slot settling delay after that boundary instead of requesting at
/// the first slot of E+2.
fn latest_reward_epoch(head_slot: u64, settling_slots: u64) -> Option<u64> {
    head_slot
        .checked_sub(settling_slots)
        .map(slot_to_epoch)
        .and_then(|epoch| epoch.checked_sub(2))
}

async fn canonical_root(client: &BeaconClient, slot: u64, root: &BlockRoot) -> Result<bool> {
    let response = client.get_header(&slot.to_string()).await?;
    Ok(response.execution_optimistic != Some(true)
        && response.data.canonical
        && response.data.root == *root)
}

async fn collect_job(
    client: &BeaconClient,
    pool: &PgPool,
    job: &db_scanner::live::LiveJob,
    resolved: &head::ResolvedChain,
) -> Result<()> {
    let validators: HashSet<u64> = job.validators.iter().map(|v| *v as u64).collect();
    if job.component == "attestation_rewards" {
        let boundary = epoch_start_slot(job.epoch + 2) - 1;
        let (&slot, root) = resolved
            .roots
            .iter()
            .filter(|(slot, _)| **slot <= boundary)
            .max_by_key(|(slot, _)| **slot)
            .ok_or_else(|| {
                Error::InconsistentBeaconData("Reward boundary ancestry not resolved".into())
            })?;
        if !canonical_root(client, slot, root).await? {
            return Err(Error::InconsistentBeaconData(
                "Reward boundary changed".into(),
            ));
        }
        let before = client.get_state_root(&boundary.to_string()).await?;
        let response =
            crate::scanner::fetch_live_attestation_rewards(client, job.epoch, &validators).await?;
        let after = client.get_state_root(&boundary.to_string()).await?;
        if before.data.root != after.data.root
            || before.execution_optimistic == Some(true)
            || after.execution_optimistic == Some(true)
            || response.execution_optimistic == Some(true)
            || !canonical_root(client, slot, root).await?
        {
            return Err(Error::InconsistentBeaconData(
                "Attestation reward state changed or optimistic".into(),
            ));
        }
        let payload = serde_json::to_value(&response.data)?;
        db_scanner::live::complete_job(pool, job, &payload, root).await?;
        crate::scanner::persist_live_attestation_rewards(pool, job.epoch, &response.data).await?;
        return Ok(());
    }
    let root = job
        .root
        .as_ref()
        .ok_or_else(|| Error::InconsistentBeaconData("Block reward job has no root".into()))?;
    let slot = job
        .slot
        .ok_or_else(|| Error::InconsistentBeaconData("Block reward job has no slot".into()))?;
    if !canonical_root(client, slot, root).await? {
        return Err(Error::InconsistentBeaconData(
            "Block reward dependency orphaned".into(),
        ));
    }
    let block = client
        .get_block(root)
        .await?
        .0
        .ok_or_else(|| Error::InconsistentBeaconData("Reward block unavailable".into()))?;
    match job.component.as_str() {
        "proposal_rewards" => {
            let response =
                crate::scanner::fetch_live_proposal_rewards(client, root, &block).await?;
            if response.execution_optimistic == Some(true)
                || !canonical_root(client, slot, root).await?
            {
                return Err(Error::InconsistentBeaconData(
                    "Proposal reward dependency changed or optimistic".into(),
                ));
            }
            db_scanner::live::complete_job(pool, job, &serde_json::to_value(&response.data)?, root)
                .await?;
            crate::scanner::persist_live_proposal_rewards(pool, slot, &response.data).await?;
        }
        "sync_rewards" => {
            let indices: Vec<u64> = validators.into_iter().collect();
            let response = crate::scanner::fetch_live_sync_rewards(client, root, &indices).await?;
            if response.execution_optimistic == Some(true)
                || !canonical_root(client, slot, root).await?
            {
                return Err(Error::InconsistentBeaconData(
                    "Sync reward dependency changed or optimistic".into(),
                ));
            }
            db_scanner::live::complete_job(pool, job, &serde_json::to_value(&response.data)?, root)
                .await?;
            crate::scanner::persist_live_sync_rewards(pool, slot, &response.data).await?;
        }
        other => {
            return Err(Error::InconsistentBeaconData(format!(
                "Unknown live component {other}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lag_preserves_next_unprocessed_slot_and_bounds_old_outages() {
        assert_eq!(super::collection_start(320, Some(315), 4), 316);
        assert_eq!(super::collection_start(320, Some(10), 4), 192);
        assert_eq!(super::collection_start(320, None, 4), 320);
    }

    #[test]
    fn epoch_rewards_wait_for_imported_slots_after_e_plus_one() {
        let start_e2 = epoch_start_slot(12);
        assert_eq!(latest_reward_epoch(start_e2, 2), Some(9));
        assert_eq!(latest_reward_epoch(start_e2 + 1, 2), Some(9));
        assert_eq!(latest_reward_epoch(start_e2 + 2, 2), Some(10));
    }

    #[tokio::test]
    #[ignore = "requires a disposable RECOVERY_TEST_DATABASE_URL"]
    async fn polling_reconciles_a_reorg_without_an_sse_reorg_event() {
        use axum::{Json, Router, extract::Request, http::StatusCode, response::IntoResponse};
        use serde_json::json;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let pool = crate::db::isolated_test_pool().await;
        // Inactive here so the fixture can isolate ancestry from duty decoding.
        db_scanner::validators::upsert_validator(&pool, 1, &[1], 1000, None)
            .await
            .unwrap();
        let changed = Arc::new(AtomicBool::new(false));
        let control = changed.clone();
        fn root(slot: u64, changed: bool) -> String {
            format!(
                "0x{:064x}",
                slot + if changed && slot >= 98 { 1000 } else { 0 }
            )
        }
        let app = Router::new().fallback(move |request:Request| {
            let changed = changed.load(Ordering::SeqCst);
            async move {
                let path = request.uri().path();
                if let Some(id)=path.strip_prefix("/eth/v1/beacon/headers/") {
                    let code=if id=="head" {100} else if let Some(hex)=id.strip_prefix("0x") {
                        u64::from_str_radix(hex,16).unwrap()
                    } else {id.parse::<u64>().unwrap()};
                    let slot=code%1000;
                    let selected=root(slot,changed);
                    return Json(json!({"execution_optimistic":false,"data":{
                        "root":selected,"canonical": !id.starts_with("0x") || id==selected,
                        "header":{"message":{"slot":slot.to_string(),"proposer_index":"0",
                            "parent_root":root(slot.saturating_sub(1),changed),
                            "state_root":format!("0x{:064x}",code+2000),"body_root":root(0,false)}}
                    }})).into_response();
                }
                if let Some(id)=path.strip_prefix("/eth/v2/beacon/blocks/") {
                    let code=u64::from_str_radix(id.trim_start_matches("0x"),16).unwrap();
                    let slot=code%1000;
                    let mut value:serde_json::Value=serde_json::from_str(include_str!("../../testdata/blocks/phase0.json")).unwrap();
                    value["data"]["message"]["slot"]=json!(slot.to_string());
                    value["data"]["message"]["parent_root"]=json!(root(slot.saturating_sub(1),code>=1000));
                    value["data"]["message"]["body"]["attestations"]=json!([]);
                    value["finalized"]=json!(false);
                    return Json(value).into_response();
                }
                if path.ends_with("/finality_checkpoints") {
                    return Json(json!({"execution_optimistic":false,"data":{
                        "previous_justified":{"epoch":"0","root":root(0,false)},
                        "current_justified":{"epoch":"0","root":root(0,false)},
                        "finalized":{"epoch":"0","root":root(0,false)}
                    }})).into_response();
                }
                if let Some(epoch)=path.strip_prefix("/eth/v1/validator/duties/proposer/") {
                    let start=epoch.parse::<u64>().unwrap()*32;
                    let duties:Vec<_>=(start..start+32).map(|slot|json!({"slot":slot.to_string(),"validator_index":"0","pubkey":"0x01"})).collect();
                    return Json(json!({"data":duties})).into_response();
                }
                if path.contains("/duties/") || path.ends_with("/committees") {
                    return Json(json!({"data":[]})).into_response();
                }
                StatusCode::NOT_FOUND.into_response()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = BeaconClient::new(&format!("http://{}", listener.local_addr().unwrap()))
            .with_pool(pool.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut cursor = None;
        let tracked = HashSet::from([1]);
        reconcile(
            &client,
            &pool,
            &tracked,
            &LiveConfig::default(),
            &mut cursor,
        )
        .await
        .unwrap();
        assert_eq!(cursor, Some(98));
        control.store(true, Ordering::SeqCst);
        reconcile(
            &client,
            &pool,
            &tracked,
            &LiveConfig::default(),
            &mut cursor,
        )
        .await
        .unwrap();
        assert_eq!(cursor, Some(63));
        reconcile(
            &client,
            &pool,
            &tracked,
            &LiveConfig::default(),
            &mut cursor,
        )
        .await
        .unwrap();
        let roots = db_scanner::live::recent_blocks(&pool, 96).await.unwrap();
        assert!(
            roots
                .iter()
                .any(|(slot, value, _)| *slot == 98 && value.as_str() == root(98, true))
        );
        assert!(
            !roots
                .iter()
                .any(|(_, value, _)| value.as_str() == root(98, false))
        );
        assert_eq!(cursor, Some(98));
        server.abort();
        pool.close().await;
    }
}
