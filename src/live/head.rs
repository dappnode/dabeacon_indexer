use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::db::Pool as PgPool;

use crate::beacon_client::BeaconClient;
use crate::beacon_client::types::HeadEvent;
use crate::beacon_client::types::{BlockRoot, SignedBeaconBlock};
use crate::chain::slot_to_epoch;
use crate::db::scanner as db_scanner;
use crate::error::Result;
use crate::scanner;

const BLOCK_FETCH_RETRIES: u32 = 3;
const BLOCK_FETCH_RETRY_BASE_DELAY_MS: u64 = 200;

pub(super) async fn process_head_scan(
    client: &BeaconClient,
    pool: &PgPool,
    scan_validators: &HashSet<u64>,
    last_scanned_slot: &mut Option<u64>,
    resolved: &ResolvedChain,
    collection_range: std::ops::RangeInclusive<u64>,
) -> Result<()> {
    let scan_started_at = std::time::Instant::now();
    let target_slot = *collection_range.end();
    let start = last_scanned_slot.map_or(0, |slot| slot.saturating_add(1));
    let indices: Vec<i64> = scan_validators.iter().map(|v| *v as i64).collect();
    let mut slots: Vec<u64> = if start <= target_slot {
        (start..=target_slot).collect()
    } else {
        Vec::new()
    };
    // The older root context is for vote comparisons, not a request to start
    // collecting backwards beyond the configured recovery window.
    let resolved_start = resolved
        .blocks
        .keys()
        .min()
        .copied()
        .unwrap_or(start)
        .max(*collection_range.start());
    slots.extend(
        db_scanner::live::missing_coverage_slots(pool, &indices, resolved_start, target_slot, 12)
            .await?,
    );
    slots.sort_unstable();
    slots.dedup();
    // A stale gap must not consume the state-retention window for new blocks.
    slots.reverse();
    slots.truncate(32);
    let mut duties_by_epoch = HashMap::new();
    let mut active_by_epoch = HashMap::new();
    let mut first_error: Option<crate::error::Error> = None;
    for &slot in &slots {
        let epoch = slot_to_epoch(slot);
        if let std::collections::hash_map::Entry::Vacant(entry) = duties_by_epoch.entry(epoch) {
            let duties = match fetch_epoch_duties(client, pool, &indices, slot, slot).await {
                Ok(duties) => Some(duties),
                Err(error) => {
                    tracing::debug!(epoch, %error, "Epoch assignments unavailable; preserving coverage gap");
                    if first_error.is_none()
                        || (first_error
                            .as_ref()
                            .is_some_and(crate::error::Error::is_unavailable_input)
                            && !error.is_unavailable_input())
                    {
                        first_error = Some(error);
                    }
                    None
                }
            };
            entry.insert(duties);
            let mut active =
                db_scanner::validators::active_validators_at(pool, &indices, epoch as i64).await?;
            if epoch > 0 {
                active.extend(
                    db_scanner::validators::active_validators_at(
                        pool,
                        &indices,
                        (epoch - 1) as i64,
                    )
                    .await?,
                );
            }
            active_by_epoch.insert(epoch, active);
        }
    }
    for slot in slots {
        let epoch = slot_to_epoch(slot);
        let Some(duties) = &duties_by_epoch[&epoch] else {
            continue;
        };
        let result = process_slot(
            client,
            pool,
            slot,
            &indices,
            &active_by_epoch[&epoch],
            duties,
            resolved,
        )
        .await;
        match result {
            Ok(()) => {
                *last_scanned_slot = Some(last_scanned_slot.map_or(slot, |last| last.max(slot)));
                crate::metrics::LIVE_LAST_SLOT
                    .with_label_values(&["processed"])
                    .set(slot as i64);
            }
            Err(error) => {
                tracing::debug!(slot, %error, "Live slot incomplete; retained for retry");
                if first_error.is_none()
                    || (first_error
                        .as_ref()
                        .is_some_and(crate::error::Error::is_unavailable_input)
                        && !error.is_unavailable_input())
                {
                    first_error = Some(error);
                }
            }
        }
    }
    crate::metrics::LIVE_HEAD_SCAN_DURATION
        .with_label_values(&["total"])
        .observe(scan_started_at.elapsed().as_secs_f64());
    first_error.map_or(Ok(()), Err)
}

async fn process_slot(
    client: &BeaconClient,
    pool: &PgPool,
    slot: u64,
    tracked_indices: &[i64],
    active: &HashSet<u64>,
    duties: &EpochDuties,
    resolved: &ResolvedChain,
) -> Result<()> {
    let block = resolved.blocks.get(&slot);
    let epoch = slot_to_epoch(slot);
    if let Some(block) = block {
        // Save ancestry before any outcome; even a partially failed scan must
        // remain identifiable and removable if its branch is later orphaned.
        let root = &resolved.roots[&slot];
        db_scanner::live::record_block(pool, slot, root, block.parent_root()).await?;
    }
    if let Some(&proposer) = duties.proposer_by_slot.get(&slot) {
        scanner::upsert_live_proposal_in_slot(pool, slot, proposer, block).await?;
        if block.is_some() {
            db_scanner::live::enqueue_jobs(
                pool,
                epoch,
                Some(slot),
                "proposal_rewards",
                &[proposer as i64],
                Some(&resolved.roots[&slot]),
            )
            .await?;
        }
    }
    if let Some(positions) = duties.sync_positions_by_epoch.get(&epoch) {
        scanner::upsert_live_sync_in_slot(pool, slot, block, positions).await?;
        if block.is_some_and(|block| block.sync_aggregate().is_some()) {
            let members: Vec<i64> = positions.keys().map(|v| *v as i64).collect();
            db_scanner::live::enqueue_jobs(
                pool,
                epoch,
                Some(slot),
                "sync_rewards",
                &members,
                Some(&resolved.roots[&slot]),
            )
            .await?;
        }
    }
    if let Some(block) = block
        && !active.is_empty()
    {
        scanner::scan_live_attestations_in_slot(
            client,
            pool,
            active,
            block,
            &resolved.roots,
            false,
        )
        .await?;
    }
    if !duties.complete {
        return Err(crate::error::Error::BeaconDataUnavailable(
            "Assignments incomplete; retaining slot coverage gap".into(),
        ));
    }
    db_scanner::live::record_coverage(pool, tracked_indices, slot).await?;
    Ok(())
}

struct EpochDuties {
    complete: bool,
    /// slot -> validator_index for tracked validators scheduled to propose.
    proposer_by_slot: HashMap<u64, u64>,
    /// epoch -> (validator_index -> sync-committee positions).
    sync_positions_by_epoch: HashMap<u64, HashMap<u64, Vec<u64>>>,
}

async fn fetch_epoch_duties(
    client: &BeaconClient,
    pool: &PgPool,
    tracked_indices: &[i64],
    scan_start_slot: u64,
    head_slot: u64,
) -> Result<EpochDuties> {
    let mut epochs: Vec<u64> = (scan_start_slot..=head_slot)
        .filter(|&s| s > 0)
        .map(slot_to_epoch)
        .collect();
    epochs.sort_unstable();
    epochs.dedup();

    let mut complete = true;
    let mut proposer_by_slot: HashMap<u64, u64> = HashMap::new();
    let mut sync_positions_by_epoch: HashMap<u64, HashMap<u64, Vec<u64>>> = HashMap::new();

    for epoch in epochs {
        let active =
            db_scanner::validators::active_validators_at(pool, tracked_indices, epoch as i64)
                .await?;
        if active.is_empty() {
            continue;
        }
        if let Err(error) =
            scanner::seed_live_attestation_duties(client, pool, epoch, &active).await
        {
            if !error.is_unavailable_input() {
                return Err(error);
            }
            tracing::debug!(epoch, %error, "Attestation assignments pending");
            complete = false;
        }
        let active_indices: Vec<u64> = active.iter().copied().collect();
        match client.get_proposer_duties(epoch).await {
            Ok(duties) => {
                for duty in duties {
                    if active.contains(&duty.validator_index) {
                        proposer_by_slot.insert(duty.slot, duty.validator_index);
                    }
                }
            }
            Err(error) => {
                if !error.is_unavailable_input() {
                    return Err(error);
                }
                complete = false;
                tracing::debug!(epoch, %error, "Proposer assignments pending");
            }
        }
        if epoch >= crate::chain::altair_epoch() {
            match client.get_sync_duties(epoch, &active_indices).await {
                Ok(duties) => {
                    let positions = duties
                        .into_iter()
                        .map(|duty| {
                            (
                                duty.validator_index,
                                duty.validator_sync_committee_indices
                                    .iter()
                                    .map(|index| index.0)
                                    .collect(),
                            )
                        })
                        .collect();
                    sync_positions_by_epoch.insert(epoch, positions);
                }
                Err(error) => {
                    if !error.is_unavailable_input() {
                        return Err(error);
                    }
                    complete = false;
                    tracing::debug!(epoch, %error, "Sync assignments pending");
                }
            }
        }
    }

    Ok(EpochDuties {
        complete,
        proposer_by_slot,
        sync_positions_by_epoch,
    })
}

/// Every missing slot in this range is proven by an actual parent link.
/// Missing a named root is an unresolved chain, never proof of a skipped slot.
pub(super) struct ResolvedChain {
    pub blocks: HashMap<u64, SignedBeaconBlock>,
    pub roots: HashMap<u64, BlockRoot>,
}

pub(super) async fn resolve_chain(
    client: &BeaconClient,
    head: &HeadEvent,
    scan_start_slot: u64,
) -> Result<ResolvedChain> {
    let mut chain = ResolvedChain {
        blocks: HashMap::new(),
        roots: HashMap::new(),
    };
    let mut root = head.block.clone();
    let mut child_slot = None;
    loop {
        let block = fetch_by_root(client, &root).await?;
        let slot = block.slot();
        if child_slot.is_none() && slot != head.slot {
            return Err(crate::error::Error::InconsistentBeaconData(
                "Head root and slot disagree".into(),
            ));
        }
        if child_slot.is_some_and(|child| slot >= child) {
            return Err(crate::error::Error::InconsistentBeaconData(
                "Parent slot does not precede child".into(),
            ));
        }
        let parent = block.parent_root().clone();
        chain.roots.insert(slot, root);
        chain.blocks.insert(slot, block);
        if slot <= scan_start_slot {
            break;
        }
        child_slot = Some(slot);
        root = parent;
    }
    Ok(chain)
}

async fn fetch_by_root(client: &BeaconClient, root: &BlockRoot) -> Result<SignedBeaconBlock> {
    for attempt in 1..=BLOCK_FETCH_RETRIES {
        if let Some(block) = client.get_block(root).await?.0 {
            return Ok(block);
        }
        if attempt < BLOCK_FETCH_RETRIES {
            tokio::time::sleep(Duration::from_millis(
                BLOCK_FETCH_RETRY_BASE_DELAY_MS * u64::from(attempt),
            ))
            .await;
        }
    }
    Err(crate::error::Error::InconsistentBeaconData(format!(
        "Named block {root} unavailable; ancestry remains unresolved"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::Request, http::StatusCode, response::IntoResponse};
    use serde_json::{Value, json};

    #[tokio::test]
    async fn parent_root_gap_never_imports_competing_slot_block() {
        let h = format!("0x{}", "11".repeat(32));
        let p = format!("0x{}", "22".repeat(32));
        let x = format!("0x{}", "33".repeat(32));
        let head: HeadEvent =
            serde_json::from_value(json!({"slot":"102", "block": h, "epoch_transition":false}))
                .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = BeaconClient::new(&format!("http://{}", listener.local_addr().unwrap()));
        let router = Router::new().fallback(move |req: Request| {
            let (h, p, x) = (h.clone(), p.clone(), x.clone());
            async move {
                let path = req.uri().path();
                if path == "/eth/v1/beacon/blocks/101/root" {
                    return Json(json!({"data":{"root":x},"finalized":false})).into_response();
                }
                let slot = if path == format!("/eth/v2/beacon/blocks/{h}") {
                    102
                } else if path == format!("/eth/v2/beacon/blocks/{p}") {
                    100
                } else if path == "/eth/v2/beacon/blocks/101" {
                    101
                } else {
                    return StatusCode::NOT_FOUND.into_response();
                };
                let mut block: Value =
                    serde_json::from_str(include_str!("../../testdata/blocks/phase0.json"))
                        .unwrap();
                block["data"]["message"]["slot"] = json!(slot.to_string());
                block["data"]["message"]["parent_root"] = json!(p);
                block["finalized"] = json!(false);
                Json(block).into_response()
            }
        });
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let blocks = resolve_chain(&client, &head, 100).await.unwrap();
        assert_eq!(blocks.blocks[&102].parent_root(), &head_parent());
        // H directly descends from slot 100: slot 101 is absent on H's branch.
        // No direct slot lookup may insert a competing block there.
        assert!(!blocks.blocks.contains_key(&101));
        assert_eq!(blocks.blocks.len(), 2);
        server.abort();
    }
    fn head_parent() -> BlockRoot {
        serde_json::from_value(json!(format!("0x{}", "22".repeat(32)))).unwrap()
    }
}
