use anyhow::Context;
use axum::{
    extract::Query,
    extract::State,
    http::StatusCode,
    response::{Sse, sse::Event},
};
use futures::stream::{self, Stream};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;
use tokio::sync::broadcast;

use super::AppState;
use crate::beacon_client::types::{AttesterDuty, ProposerDuty, SyncDuty};
use crate::chain::{self, epoch_start_slot, slot_to_epoch};
use crate::db::api::live as db_live;

#[derive(Deserialize)]
pub struct LiveSseQuery {
    pub api_key: Option<String>,
}

#[derive(Serialize)]
pub struct AttestationOutcome {
    pub validator_index: u64,
    pub included: Option<bool>,
}

#[derive(Serialize)]
pub struct LiveSlot {
    pub slot: u64,
    pub block: bool,
    pub skipped: bool,
    /// Tracked validator scheduled to propose this slot (if any).
    pub proposer: Option<u64>,
    /// Outcome when `proposer` is set.
    pub proposed: Option<bool>,
    /// Tracked validators attesting at this slot, with their inclusion status.
    pub attestations: Vec<AttestationOutcome>,
    /// Sync participation positionally aligned with `LiveUpdate.sync_committee`.
    /// Some(true) = participated, Some(false) = missed, None = unknown or the
    /// validator isn't in the committee covering this slot's epoch.
    pub sync: Vec<Option<bool>>,
}

#[derive(Serialize)]
pub struct LiveUpdate {
    pub epoch: u64,
    pub previous_epoch: Option<u64>,
    pub head_slot: u64,
    pub start_slot: u64,
    pub end_slot: u64,
    /// Tracked validators in any sync committee covering this window.
    /// Sync committees are stable per 256-epoch period, so this is effectively
    /// constant across the whole window and deduped out of per-slot rows.
    pub sync_committee: Vec<u64>,
    pub slots: Vec<LiveSlot>,
}

/// SSE endpoint that streams live slot and duty updates.
pub async fn live_sse(
    Query(query): Query<LiveSseQuery>,
    State(state): State<AppState>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, StatusCode> {
    if !state.config.api_key.is_empty() {
        let provided = query.api_key.as_deref().unwrap_or_default();
        if provided != state.config.api_key {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    let app_state = state.clone();
    let receiver = state.live_updates_tx.subscribe();

    // Refresh on every broadcast event, on lag, and at least every 6s. Close
    // the stream only when the broadcaster drops.
    let stream = stream::unfold((true, receiver), move |(first, mut rx)| {
        let app_state = app_state.clone();
        async move {
            if !first
                && let Ok(Err(broadcast::error::RecvError::Closed)) =
                    tokio::time::timeout(Duration::from_secs(6), rx.recv()).await
            {
                return None;
            }
            let data = build_live_update(&app_state)
                .await
                .unwrap_or_else(|_| empty_live_update());
            let event = Event::default().json_data(data).unwrap();
            Some((Ok(event), (false, rx)))
        }
    });
    Ok(Sse::new(stream))
}

fn empty_live_update() -> LiveUpdate {
    LiveUpdate {
        epoch: 0,
        previous_epoch: None,
        head_slot: 0,
        start_slot: 0,
        end_slot: chain::slots_per_epoch(),
        sync_committee: Vec::new(),
        slots: (0..chain::slots_per_epoch())
            .map(|slot| LiveSlot {
                slot,
                block: false,
                skipped: false,
                proposer: None,
                proposed: None,
                attestations: Vec::new(),
                sync: Vec::new(),
            })
            .collect(),
    }
}

async fn build_live_update(state: &AppState) -> anyhow::Result<LiveUpdate> {
    let pool = &state.pool;
    let beacon_client = state.beacon_client.as_ref();
    let tracked: &Vec<u64> = state.tracked_validators.as_ref();
    let tracked_set: HashSet<u64> = tracked.iter().copied().collect();
    let tracked_i64: Vec<i64> = tracked.iter().map(|&v| v as i64).collect();

    let head_slot = beacon_client.get_head_slot().await?;
    let current_epoch = slot_to_epoch(head_slot);
    let previous_epoch = (current_epoch > 0).then_some(current_epoch - 1);
    let start_epoch = previous_epoch.unwrap_or(current_epoch);
    let start_slot = epoch_start_slot(start_epoch);
    let end_slot = epoch_start_slot(current_epoch) + chain::slots_per_epoch();

    let finalized_epoch = beacon_client
        .get_finality_checkpoints("head")
        .await
        .context("failed to fetch finality checkpoints")?
        .finalized
        .epoch;

    // Backfill lag (1 epoch grace): affects whether "no row yet" should be
    // treated as "missed" or "unknown".
    let is_backfilling = db_live::min_tracked_scanned_epoch(pool, &tracked_i64)
        .await?
        .map(|m| m.saturating_add(1) < finalized_epoch)
        .unwrap_or(true);

    // Schedule (who was assigned what). DB only records outcomes — upcoming
    // slots have none yet, so we still need the beacon node for this.
    let schedule = fetch_schedule(
        beacon_client,
        tracked,
        &tracked_set,
        previous_epoch,
        current_epoch,
    )
    .await?;

    // Outcomes from DB (populated by live tracking + finalization rescans).
    let attestation_status =
        db_live::fetch_attestation_status(pool, &tracked_i64, start_epoch, current_epoch).await?;
    let sync_status = db_live::fetch_sync_status(pool, &tracked_i64, start_slot, end_slot).await?;
    let proposal_status = db_live::fetch_proposal_status(pool, start_slot, end_slot).await?;

    // Derive per-slot block/skipped from proposal and sync rows. Either a
    // tracked proposal row or any sync row at the slot settles it (sync rows
    // for the same slot all share the same missed_block flag).
    let (has_block, is_missed) = slot_block_signals(&proposal_status, &sync_status);

    // Sync committee membership hoisted to top level. Sync committees are
    // stable within a 256-epoch period, so effectively this list is constant
    // across the 2-epoch window and doesn't need to repeat per slot. On a
    // period boundary the union of both periods is sent.
    let sync_committee: Vec<u64> = schedule
        .sync_by_epoch
        .values()
        .flat_map(|duties| duties.iter().map(|d| d.validator_index))
        .collect::<BTreeSet<u64>>()
        .into_iter()
        .collect();

    // Which epochs each member's committee covers — used to emit None for
    // slots outside their committee's period (only matters on rollover).
    let member_epochs: HashMap<u64, HashSet<u64>> = sync_committee
        .iter()
        .map(|&vi| {
            let epochs = schedule
                .sync_by_epoch
                .iter()
                .filter(|(_, duties)| duties.iter().any(|d| d.validator_index == vi))
                .map(|(&e, _)| e)
                .collect();
            (vi, epochs)
        })
        .collect();

    let mut slots = Vec::with_capacity((end_slot - start_slot) as usize);
    for slot in start_slot..end_slot {
        let slot_epoch = slot_to_epoch(slot);

        let attestations = schedule
            .attesters
            .iter()
            .filter(|d| d.slot == slot)
            .map(|d| AttestationOutcome {
                validator_index: d.validator_index,
                included: resolve_included(
                    attestation_status.get(&(slot, d.validator_index)).copied(),
                    slot,
                    head_slot,
                    is_backfilling,
                ),
            })
            .collect();

        let sync = sync_committee
            .iter()
            .map(|&vi| {
                // Outside this member's active period → unknown.
                if !member_epochs[&vi].contains(&slot_epoch) {
                    return None;
                }
                sync_status
                    .get(&(slot, vi))
                    .map(|&(p, _)| p)
                    .or_else(|| fallback_missed_if_past(slot, head_slot, is_backfilling))
            })
            .collect();

        let (proposer, proposed) = schedule
            .proposers
            .iter()
            .find(|d| d.slot == slot)
            .map(|d| {
                (
                    Some(d.validator_index),
                    proposal_status
                        .get(&slot)
                        .map(|&(_, p)| p)
                        .or_else(|| fallback_missed_if_past(slot, head_slot, is_backfilling)),
                )
            })
            .unwrap_or((None, None));

        slots.push(LiveSlot {
            slot,
            block: has_block.contains(&slot),
            skipped: slot <= head_slot && is_missed.contains(&slot),
            proposer,
            proposed,
            attestations,
            sync,
        });
    }

    Ok(LiveUpdate {
        epoch: current_epoch,
        previous_epoch,
        head_slot,
        start_slot,
        end_slot,
        sync_committee,
        slots,
    })
}

/// Per-epoch duty schedule pulled from the beacon node.
struct Schedule {
    attesters: Vec<AttesterDuty>,
    sync_by_epoch: HashMap<u64, Vec<SyncDuty>>,
    proposers: Vec<ProposerDuty>,
}

async fn fetch_schedule(
    beacon_client: &crate::beacon_client::BeaconClient,
    tracked: &[u64],
    tracked_set: &HashSet<u64>,
    previous_epoch: Option<u64>,
    current_epoch: u64,
) -> anyhow::Result<Schedule> {
    let epochs: Vec<u64> = previous_epoch
        .into_iter()
        .chain(std::iter::once(current_epoch))
        .collect();

    let mut attesters = Vec::new();
    let mut sync_by_epoch: HashMap<u64, Vec<SyncDuty>> = HashMap::new();
    let mut proposers = Vec::new();

    for epoch in &epochs {
        attesters.extend(
            beacon_client
                .get_attester_duties(*epoch, tracked)
                .await
                .with_context(|| format!("fetch attester duties epoch {epoch}"))?,
        );
        if *epoch >= chain::altair_epoch() {
            let sd = beacon_client
                .get_sync_duties(*epoch, tracked)
                .await
                .with_context(|| format!("fetch sync duties epoch {epoch}"))?;
            sync_by_epoch.insert(*epoch, sd);
        }
        proposers.extend(
            beacon_client
                .get_proposer_duties(*epoch)
                .await
                .with_context(|| format!("fetch proposer duties epoch {epoch}"))?
                .into_iter()
                .filter(|d| tracked_set.contains(&d.validator_index)),
        );
    }

    Ok(Schedule {
        attesters,
        sync_by_epoch,
        proposers,
    })
}

/// (has_block, missed) derived from DB signals.
fn slot_block_signals(
    proposals: &HashMap<u64, (u64, bool)>,
    sync: &HashMap<(u64, u64), (bool, bool)>,
) -> (HashSet<u64>, HashSet<u64>) {
    let mut has_block = HashSet::new();
    let mut missed = HashSet::new();

    for (&slot, &(_, proposed)) in proposals {
        if proposed {
            has_block.insert(slot);
        } else {
            missed.insert(slot);
        }
    }
    for (&(slot, _), &(_, missed_block)) in sync {
        if missed_block {
            missed.insert(slot);
        } else {
            has_block.insert(slot);
        }
    }

    (has_block, missed)
}

/// Finalized rows are authoritative; otherwise we only call "missed" once the
/// inclusion window (chain::slots_per_epoch()) has passed AND backfill is caught up.
fn resolve_included(
    stored: Option<(bool, bool)>,
    assigned_slot: u64,
    head_slot: u64,
    is_backfilling: bool,
) -> Option<bool> {
    let window_closed =
        !is_backfilling && head_slot >= assigned_slot.saturating_add(chain::slots_per_epoch());
    match stored {
        Some((true, _)) => Some(true),
        Some((false, finalized)) if finalized || window_closed => Some(false),
        Some((false, _)) => None,
        None if window_closed => Some(false),
        None => None,
    }
}

/// "Slot is already past head and backfill is caught up — call missing rows missed".
fn fallback_missed_if_past(slot: u64, head_slot: u64, is_backfilling: bool) -> Option<bool> {
    (!is_backfilling && slot < head_slot).then_some(false)
}
