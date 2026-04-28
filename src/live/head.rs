use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::db::Pool as PgPool;

use crate::beacon_client::BeaconClient;
use crate::beacon_client::types::HeadEvent;
use crate::beacon_client::types::{BlockRoot, SignedBeaconBlock};
use crate::chain::slot_to_epoch;
use crate::error::Result;
use crate::exits;
use crate::scanner;

const BLOCK_FETCH_RETRIES: u32 = 3;
const BLOCK_FETCH_RETRY_BASE_DELAY_MS: u64 = 200;
const INITIAL_SLEEP_MS: u64 = 100;

pub(super) async fn process_head_scan(
    client: &BeaconClient,
    pool: &PgPool,
    scan_validators: &HashSet<u64>,
    validator_exits: &HashMap<u64, u64>,
    head: &HeadEvent,
    last_scanned_slot: &mut Option<u64>,
) -> Result<()> {
    tokio::time::sleep(Duration::from_millis(INITIAL_SLEEP_MS)).await; // give node a chance to process a block
    let scan_started_at = std::time::Instant::now();
    let head_epoch = slot_to_epoch(head.slot);
    tracing::debug!(
        slot = head.slot,
        head_epoch,
        block = %head.block,
        epoch_transition = head.epoch_transition,
        last_scanned_slot = ?last_scanned_slot,
        "Head event"
    );

    let scan_start_slot = last_scanned_slot.map_or(head.slot, |ls| ls + 1);
    if scan_start_slot > head.slot {
        tracing::trace!(slot = head.slot, head_epoch, "No new live slot to process");
        return Ok(());
    }

    let blocks_by_slot = resolve_chain(client, head, scan_start_slot).await?;

    // Fetch per-epoch duties up front so each slot becomes a pure map lookup.
    // Already filtered to validators active at each epoch — exited validators
    // produce no proposer/sync rows.
    let duties = fetch_epoch_duties(
        client,
        scan_validators,
        validator_exits,
        scan_start_slot,
        head.slot,
    )
    .await?;

    for slot in scan_start_slot..=head.slot {
        if slot == 0 {
            *last_scanned_slot = Some(slot);
            continue;
        }

        let target_slot = slot - 1;
        let epoch = slot_to_epoch(target_slot);
        tracing::info!(slot, target_slot, head_epoch, epoch, "Processing live slot");

        let block = blocks_by_slot.get(&slot);
        if let Some(block) = block {
            let active = exits::active_at(scan_validators, validator_exits, epoch);
            if !active.is_empty()
                && let Err(e) = scanner::scan_live_attestations_in_slot(
                    client,
                    pool,
                    slot,
                    &active,
                    Some(block),
                )
                .await
            {
                tracing::error!(slot, epoch, error = %e, "Live attestation scan failed; aborting");
                return Err(e);
            }
        } else {
            tracing::trace!(slot, "No canonical block at slot");
        }

        if let Some(&proposer_index) = duties.proposer_by_slot.get(&slot)
            && let Err(e) =
                scanner::upsert_live_proposal_in_slot(pool, slot, proposer_index, block).await
        {
            tracing::error!(slot, error = %e, "Live proposal scan failed; aborting");
            return Err(e);
        }

        let slot_epoch = slot_to_epoch(slot);
        if let Some(positions) = duties.sync_positions_by_epoch.get(&slot_epoch)
            && let Err(e) = scanner::upsert_live_sync_in_slot(pool, slot, block, positions).await
        {
            tracing::error!(slot, error = %e, "Live sync scan failed; aborting");
            return Err(e);
        }

        *last_scanned_slot = Some(slot);
        crate::metrics::LIVE_LAST_SLOT
            .with_label_values(&["processed"])
            .set(slot as i64);
    }

    crate::metrics::LIVE_HEAD_SCAN_DURATION
        .with_label_values(&["total"])
        .observe(scan_started_at.elapsed().as_secs_f64());
    Ok(())
}

struct EpochDuties {
    /// slot -> validator_index for tracked validators scheduled to propose.
    proposer_by_slot: HashMap<u64, u64>,
    /// epoch -> (validator_index -> sync-committee positions).
    sync_positions_by_epoch: HashMap<u64, HashMap<u64, Vec<u64>>>,
}

async fn fetch_epoch_duties(
    client: &BeaconClient,
    scan_validators: &HashSet<u64>,
    validator_exits: &HashMap<u64, u64>,
    scan_start_slot: u64,
    head_slot: u64,
) -> Result<EpochDuties> {
    let mut epochs: Vec<u64> = (scan_start_slot..=head_slot)
        .filter(|&s| s > 0)
        .map(slot_to_epoch)
        .collect();
    epochs.sort_unstable();
    epochs.dedup();

    let mut proposer_by_slot: HashMap<u64, u64> = HashMap::new();
    let mut sync_positions_by_epoch: HashMap<u64, HashMap<u64, Vec<u64>>> = HashMap::new();

    for epoch in epochs {
        let active = exits::active_at(scan_validators, validator_exits, epoch);
        if active.is_empty() {
            continue;
        }
        let active_indices: Vec<u64> = active.iter().copied().collect();

        for d in client.get_proposer_duties(epoch).await? {
            if active.contains(&d.validator_index) {
                proposer_by_slot.insert(d.slot, d.validator_index);
            }
        }

        let sync = client.get_sync_duties(epoch, &active_indices).await?;
        let positions: HashMap<u64, Vec<u64>> = sync
            .into_iter()
            .map(|d| {
                (
                    d.validator_index,
                    d.validator_sync_committee_indices
                        .iter()
                        .map(|i| i.0)
                        .collect(),
                )
            })
            .collect();
        sync_positions_by_epoch.insert(epoch, positions);
    }

    Ok(EpochDuties {
        proposer_by_slot,
        sync_positions_by_epoch,
    })
}

/// Build `slot -> block` for `[scan_start_slot, head.slot]` on the canonical
/// chain rooted at `head.block`. Prefers root-based walks (stable under
/// fork-choice churn); falls back to slot lookups on failure.
async fn resolve_chain(
    client: &BeaconClient,
    head: &HeadEvent,
    scan_start_slot: u64,
) -> Result<HashMap<u64, SignedBeaconBlock>> {
    let mut blocks_by_slot: HashMap<u64, SignedBeaconBlock> = HashMap::new();

    let mut cursor = fetch_by_root(client, &head.block).await?;
    if cursor.is_none() {
        tracing::warn!(
            head_slot = head.slot,
            head_block = %head.block,
            "Head block unavailable by root after retries; falling back to slot lookup"
        );
        cursor = fetch_by_slot(client, head.slot).await?;
    }

    while let Some(block) = cursor {
        let slot = block.slot();
        if slot < scan_start_slot {
            break;
        }
        let parent_root = block.parent_root().clone();
        blocks_by_slot.insert(slot, block);
        if slot == 0 || slot == scan_start_slot {
            break;
        }

        match fetch_by_root(client, &parent_root).await? {
            Some(parent) => cursor = Some(parent),
            None => {
                tracing::debug!(
                    child_slot = slot,
                    parent_root = %parent_root,
                    "Parent block unavailable by root; walking backwards by slot"
                );
                cursor = None;
                for fb_slot in (scan_start_slot..slot).rev() {
                    if let Some(b) = fetch_by_slot(client, fb_slot).await? {
                        cursor = Some(b);
                        break;
                    }
                }
            }
        }
    }

    // Final safety net: direct slot lookup for any slot still missing.
    for slot in scan_start_slot..=head.slot {
        if slot == 0 || blocks_by_slot.contains_key(&slot) {
            continue;
        }
        if let Some(b) = fetch_by_slot(client, slot).await? {
            blocks_by_slot.insert(slot, b);
        }
    }

    Ok(blocks_by_slot)
}

/// A 404 by root is never a "missed slot" — the root names a specific block.
/// Retry to absorb transient "node hasn't processed yet" cases.
async fn fetch_by_root(
    client: &BeaconClient,
    root: &BlockRoot,
) -> Result<Option<SignedBeaconBlock>> {
    for attempt in 1..=BLOCK_FETCH_RETRIES {
        if let Some(b) = client.get_block(root).await?.0 {
            return Ok(Some(b));
        }
        if attempt < BLOCK_FETCH_RETRIES {
            let delay = BLOCK_FETCH_RETRY_BASE_DELAY_MS * attempt as u64;
            tracing::debug!(
                root = %root,
                attempt,
                delay_ms = delay,
                "Block not found by root; retrying"
            );
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    }
    Ok(None)
}

/// 404 by slot is legitimate (missed slot); retry just once to absorb brief
/// fork-choice races near the head.
async fn fetch_by_slot(client: &BeaconClient, slot: u64) -> Result<Option<SignedBeaconBlock>> {
    if let Some(b) = client.get_block(slot).await?.0 {
        return Ok(Some(b));
    }
    tokio::time::sleep(Duration::from_millis(BLOCK_FETCH_RETRY_BASE_DELAY_MS)).await;
    Ok(client.get_block(slot).await?.0)
}
