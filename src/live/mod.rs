use std::collections::{HashMap, HashSet};

use crate::db::Pool as PgPool;
use futures::StreamExt;
use reqwest_eventsource::{Event, EventSource};
use uuid::Uuid;

use crate::beacon_client::BeaconClient;
use crate::beacon_client::types::{ChainReorgEvent, FinalizedCheckpointEvent, HeadEvent};
use crate::chain::{self, epoch_start_slot};
use crate::config::EffectiveScanMode;
use crate::db::scanner;
use crate::error::{Error, Result};
use crate::live_updates::LiveUpdateEvent;
use tokio::sync::broadcast;

mod finalization;
mod head;
mod reorg;

use finalization::process_finalized_rescan;
use head::process_head_scan;
use reorg::process_chain_reorg;

#[allow(clippy::too_many_arguments)]
pub async fn run_live_tracking(
    client: &BeaconClient,
    pool: &PgPool,
    instance_id: Uuid,
    tracked: &HashSet<u64>,
    validator_exits: &HashMap<u64, u64>,
    scan_mode: EffectiveScanMode,
    live_updates_tx: broadcast::Sender<LiveUpdateEvent>,
    last_backfilled_epoch: u64,
) -> Result<()> {
    let url = client.url("/eth/v1/events?topics=head,finalized_checkpoint,chain_reorg");
    let scan_validators = tracked.clone();

    let start_slot = epoch_start_slot(last_backfilled_epoch) + chain::slots_per_epoch() - 1;
    let mut last_scanned_slot: Option<u64> = Some(start_slot);
    let mut last_finalized_rescanned_epoch = last_backfilled_epoch.saturating_sub(1);
    tracing::info!(
        last_backfilled_epoch,
        start_slot,
        rescan_watermark = last_finalized_rescanned_epoch,
        "Initialized live tracking from last backfilled epoch"
    );

    loop {
        tracing::info!(url = %url, "Connecting to SSE event stream");
        let mut es = EventSource::get(&url);

        while let Some(event_result) = es.next().await {
            match event_result {
                Ok(Event::Message(msg)) => {
                    tracing::trace!(
                        event = %msg.event,
                        data_len = msg.data.len(),
                        "SSE message received"
                    );

                    match msg.event.as_str() {
                        "head" => match serde_json::from_str::<HeadEvent>(&msg.data) {
                            Ok(head) => {
                                crate::metrics::LIVE_SSE_EVENTS
                                    .with_label_values(&["head", "ok"])
                                    .inc();
                                crate::metrics::LIVE_LAST_SLOT
                                    .with_label_values(&["head_event"])
                                    .set(head.slot as i64);
                                process_head_scan(
                                    client,
                                    pool,
                                    &scan_validators,
                                    validator_exits,
                                    &head,
                                    &mut last_scanned_slot,
                                )
                                .await
                                .map_err(|e| {
                                    tracing::error!(slot = head.slot, error = %e, "Head event processing failed; aborting");
                                    e
                                })?;

                                if let Err(e) =
                                    live_updates_tx.send(LiveUpdateEvent::LiveHeadProcessed)
                                {
                                    tracing::debug!(error = %e, "No active live SSE subscribers for head update event");
                                }

                                if let Err(e) =
                                    scanner::instance::update_heartbeat(pool, instance_id).await
                                {
                                    tracing::error!(error = %e, "Failed to update heartbeat during live tracking");
                                }
                            }
                            Err(e) => {
                                crate::metrics::LIVE_SSE_EVENTS
                                    .with_label_values(&["head", "parse_error"])
                                    .inc();
                                tracing::error!(
                                    error = %e,
                                    raw_data = %msg.data,
                                    "Failed to parse head event; aborting"
                                );
                                return Err(Error::Json(e));
                            }
                        },
                        "finalized_checkpoint" => {
                            match serde_json::from_str::<FinalizedCheckpointEvent>(&msg.data) {
                                Ok(finalized) => {
                                    crate::metrics::LIVE_SSE_EVENTS
                                        .with_label_values(&["finalized_checkpoint", "ok"])
                                        .inc();
                                    process_finalized_rescan(
                                        client,
                                        pool,
                                        &scan_validators,
                                        validator_exits,
                                        scan_mode,
                                        &finalized,
                                        &mut last_finalized_rescanned_epoch,
                                    )
                                    .await
                                    .map_err(|e| {
                                        tracing::error!(epoch = finalized.epoch, error = %e, "Finalized checkpoint processing failed; aborting");
                                        e
                                    })?;
                                }
                                Err(e) => {
                                    crate::metrics::LIVE_SSE_EVENTS
                                        .with_label_values(&["finalized_checkpoint", "parse_error"])
                                        .inc();
                                    tracing::error!(
                                        error = %e,
                                        raw_data = %msg.data,
                                        "Failed to parse finalized_checkpoint event; aborting"
                                    );
                                    return Err(Error::Json(e));
                                }
                            }
                        }
                        "chain_reorg" => match serde_json::from_str::<ChainReorgEvent>(&msg.data) {
                            Ok(reorg) => {
                                crate::metrics::LIVE_SSE_EVENTS
                                    .with_label_values(&["chain_reorg", "ok"])
                                    .inc();
                                crate::metrics::LIVE_REORGS.inc();
                                process_chain_reorg(
                                    client,
                                    pool,
                                    &reorg,
                                    &mut last_scanned_slot,
                                )
                                .await
                                .map_err(|e| {
                                    tracing::error!(slot = reorg.slot, depth = reorg.depth, error = %e, "Chain reorg processing failed; aborting");
                                    e
                                })?;
                            }
                            Err(e) => {
                                crate::metrics::LIVE_SSE_EVENTS
                                    .with_label_values(&["chain_reorg", "parse_error"])
                                    .inc();
                                tracing::error!(
                                    error = %e,
                                    raw_data = %msg.data,
                                    "Failed to parse chain_reorg event; aborting"
                                );
                                return Err(Error::Json(e));
                            }
                        },
                        other => {
                            tracing::trace!(event = other, "Unknown SSE event type, ignoring");
                        }
                    }
                }
                Ok(Event::Open) => {
                    tracing::info!("SSE connection established");
                }
                Err(e) => {
                    tracing::error!(error = %e, "SSE error, will reconnect");
                    break;
                }
            }
        }

        tracing::warn!("SSE stream disconnected, reconnecting in 5 seconds...");
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }
}
