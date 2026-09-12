//! Attestation scanning.
//!
//! - [`decode`]: pure, fork-specific attestation parsing. No I/O.
//! - [`dense`]: fetches every block in the epoch + computes vote correctness
//!   against the canonical chain. Amortises well for 30+ tracked validators.
//! - [`sparse`]: rewards-first per-duty flow for 1–2 tracked validators,
//!   scanning forward from each duty to find its inclusion slot.
//!
//! This file owns the shared internal types and the live-path entry point
//! [`scan_live_attestations_in_slot`].

use std::collections::{HashMap, HashSet};

use crate::beacon_client::BeaconClient;
use crate::beacon_client::types::{BlockRoot, Checkpoint, SignedBeaconBlock};
use crate::chain::{epoch_start_slot, slot_to_epoch};
use crate::db;
use crate::db::Pool as PgPool;
use crate::error::{Error, Result};

mod decode;
mod dense;
mod sparse;

#[cfg(test)]
mod integration_tests;

pub use dense::process_epoch_attestation_duties;
pub use sparse::process_epoch_attestation_duties_sparse;

/// Missing duties or rewards must remain repairable, not become a completed scan.
fn validate_epoch_response(
    tracked: &HashSet<u64>,
    duties: &[crate::beacon_client::types::AttesterDuty],
    rewards: &HashMap<u64, crate::beacon_client::types::ValidatorAttestationReward>,
) -> Result<()> {
    let assigned: HashSet<u64> = duties.iter().map(|d| d.validator_index).collect();
    for validator in tracked {
        if !assigned.contains(validator) || !rewards.contains_key(validator) {
            return Err(Error::InconsistentBeaconData(format!(
                "missing attestation duty or reward for validator {validator}"
            )));
        }
    }
    Ok(())
}

/// The canonical chain context needed to verify attestation vote correctness.
pub(super) struct VoteContext {
    /// Canonical block root at each slot in the epoch.
    /// For missed slots, this is the most recent block root before that slot.
    pub(super) block_roots: HashMap<u64, BlockRoot>,
    pub(super) target_root: BlockRoot,
    pub(super) target_epoch: u64,
    /// None means the source was validated by inclusion in an accepted block.
    /// Live collection uses this consensus invariant without fetching old state.
    pub(super) source: Option<Checkpoint>,
}

/// Record of when/how a validator's attestation was included, with vote correctness.
pub(super) struct AttestationInclusion {
    pub(super) inclusion_slot: u64,
    pub(super) inclusion_delay: u64,
    pub(super) source_correct: bool,
    pub(super) target_correct: bool,
    pub(super) head_correct: bool,
}

/// Head / target / source correctness for a single attestation vote.
#[derive(Clone, Copy, Default)]
pub(super) struct VoteMarks {
    pub(super) head_correct: bool,
    pub(super) target_correct: bool,
    pub(super) source_correct: bool,
}

/// A committee whose validators' aggregation bits occupy
/// `agg_bits[bit_offset..bit_offset + validators.len()]`. This is the uniform view
/// that hides pre-Electra (single committee at offset 0) vs Electra (N committees at
/// accumulating offsets) from the shared inclusion loop.
pub(super) struct AssignedCommittee<'a> {
    pub(super) index: u64,
    pub(super) validators: &'a [u64],
    pub(super) bit_offset: usize,
}

/// Slots between an attestation's `data.slot` and the slot of the including block.
/// Rejects the spec-violating case where inclusion slot precedes attestation slot.
pub(super) fn inclusion_delay(inclusion_slot: u64, att_slot: u64) -> Result<u64> {
    inclusion_slot.checked_sub(att_slot).ok_or_else(|| {
        Error::InconsistentBeaconData(format!(
            "attestation slot {att_slot} is after inclusion slot {inclusion_slot}"
        ))
    })
}

/// Inclusion delay with missed proposer slots between `att_slot` and `inclusion_slot`
/// subtracted out. Isolates validator-side lateness from chain-level gaps: a delay of
/// 2 where the intervening slot had no block becomes an effective delay of 1. By
/// construction the result is >= 1 whenever `raw_delay` is.
pub(super) fn effective_inclusion_delay(
    att_slot: u64,
    inclusion_slot: u64,
    raw_delay: u64,
    missed_slots: &HashSet<u64>,
) -> u64 {
    let missed = ((att_slot + 1)..inclusion_slot)
        .filter(|s| missed_slots.contains(s))
        .count() as u64;
    raw_delay.saturating_sub(missed)
}

/// Process attestation inclusions observed in a single live inclusion slot.
///
/// This is intentionally narrower than `scan_epoch`: it does NOT fetch future slots
/// for late-inclusion discovery. It updates rows only for validators whose attestation
/// inclusion is observed in the given slot's block.
///
/// Post-Deneb (EIP-7045) an attestation can be included as long as its target epoch
/// is the previous or current epoch of the including state. For an inclusion slot in
/// epoch E that means valid `data.slot` values range from the start of epoch E-1 up to
/// `inclusion_slot - 1` (a window of up to 2*chain::slots_per_epoch() - 1 slots).
/// `roots` must come from a connected parent walk through the including block;
/// `finalized` permits detail repair only after verifying finalized ancestry.
pub async fn scan_live_attestations_in_slot(
    client: &BeaconClient,
    pool: &PgPool,
    scan_validators: &HashSet<u64>,
    block: &SignedBeaconBlock,
    roots: &HashMap<u64, BlockRoot>,
    finalized: bool,
) -> Result<()> {
    let inclusion_slot = block.slot();
    if scan_validators.is_empty() {
        return Ok(());
    }

    if inclusion_slot == 0 {
        tracing::trace!(
            inclusion_slot,
            "Skipping genesis slot for live attestation scan"
        );
        return Ok(());
    }

    let inclusion_epoch = slot_to_epoch(inclusion_slot);
    let min_att_slot = epoch_start_slot(inclusion_epoch.saturating_sub(1));
    let max_att_slot = inclusion_slot - 1;

    let mut candidate_epochs: Vec<u64> = block
        .attestation_slots()
        .filter(|&slot| slot >= min_att_slot && slot <= max_att_slot)
        .map(slot_to_epoch)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    candidate_epochs.sort_unstable();

    if candidate_epochs.is_empty() {
        tracing::trace!(
            inclusion_slot,
            min_att_slot,
            max_att_slot,
            "No attestations in live lookback window"
        );
        return Ok(());
    }

    // Old committee history must not prevent collecting this block's newer
    // attestations. Keep the slot incomplete until every candidate epoch succeeds.
    let mut first_error: Option<Error> = None;
    for epoch in candidate_epochs {
        if let Err(error) = scan_live_attestation_epoch(
            client,
            pool,
            scan_validators,
            block,
            roots,
            finalized,
            epoch,
        )
        .await
        {
            // An unavailable old input must not hide a later database/data error.
            if first_error.is_none()
                || (first_error
                    .as_ref()
                    .is_some_and(Error::is_unavailable_input)
                    && !error.is_unavailable_input())
            {
                first_error = Some(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

async fn scan_live_attestation_epoch(
    client: &BeaconClient,
    pool: &PgPool,
    scan_validators: &HashSet<u64>,
    block: &SignedBeaconBlock,
    roots: &HashMap<u64, BlockRoot>,
    finalized: bool,
    epoch: u64,
) -> Result<()> {
    let inclusion_slot = block.slot();
    let min_att_slot = epoch_start_slot(slot_to_epoch(inclusion_slot).saturating_sub(1));
    let max_att_slot = inclusion_slot - 1;
    let scan_validator_indices: Vec<u64> = scan_validators.iter().copied().collect();
    let committees = client.get_committees(epoch).await?;
    let committee_map = decode::build_committee_map(&committees);
    let vote_context = live_vote_context(epoch, inclusion_slot, roots)?;

    let mut inclusions = HashMap::new();
    decode::extract_attestation_inclusions(
        block,
        inclusion_slot,
        epoch,
        &committee_map,
        scan_validators,
        Some(&vote_context),
        &mut inclusions,
    )?;

    if inclusions.is_empty() {
        tracing::trace!(
            epoch,
            inclusion_slot,
            "No tracked attestation inclusions in live slot"
        );
        return Ok(());
    }

    let duties = client
        .get_attester_duties(epoch, &scan_validator_indices)
        .await?;
    let duties_map: HashMap<u64, _> = duties.into_iter().map(|d| (d.validator_index, d)).collect();

    let mut updated = 0u32;
    for (&validator_index, inc) in &inclusions {
        let duty = duties_map.get(&validator_index).ok_or_else(|| {
            Error::InconsistentBeaconData(format!(
                "missing duty for observed validator {validator_index} in epoch {epoch}"
            ))
        })?;

        if duty.slot < min_att_slot || duty.slot > max_att_slot {
            return Err(Error::InconsistentBeaconData(format!(
                "duty outside inclusion window for validator {validator_index}"
            )));
        }

        let effective_delay = (duty.slot + 1..=inc.inclusion_slot)
            .filter(|slot| roots.contains_key(slot))
            .count() as i32;
        db::scanner::attestations::upsert_attestation_duty(
            pool,
            validator_index as i64,
            epoch as i64,
            duty.slot as i64,
            duty.committee_index as i32,
            duty.validator_committee_index as i32,
            true,
            Some(inc.inclusion_slot as i64),
            Some(inc.inclusion_delay as i32),
            Some(effective_delay),
            Some(inc.source_correct),
            Some(inc.target_correct),
            Some(inc.head_correct),
            None,
            None,
            None,
            None,
            false,
        )
        .await?;

        if finalized {
            db::scanner::attestations::repair_finalized_inclusion(
                pool,
                validator_index as i64,
                epoch as i64,
                &db::scanner::attestations::InclusionDetails {
                    slot: inc.inclusion_slot as i64,
                    delay: inc.inclusion_delay as i32,
                    effective_delay,
                    source_correct: inc.source_correct,
                    target_correct: inc.target_correct,
                    head_correct: inc.head_correct,
                },
            )
            .await?;
        }

        updated += 1;
    }

    tracing::debug!(
        epoch,
        inclusion_slot,
        updated,
        "Live slot attestation updates written"
    );
    Ok(())
}

/// Carry roots forward across proven skipped slots on the resolved parent chain.
/// This supplies vote comparisons and adjusted delay without state/reward APIs.
fn live_vote_context(
    epoch: u64,
    inclusion_slot: u64,
    roots: &HashMap<u64, BlockRoot>,
) -> Result<VoteContext> {
    let start = epoch_start_slot(epoch);
    let mut previous = roots
        .iter()
        .filter(|(slot, _)| **slot <= start)
        .max_by_key(|(slot, _)| **slot)
        .map(|(_, root)| root.clone())
        .ok_or_else(|| {
            Error::InconsistentBeaconData(format!(
                "Resolved ancestry does not cover attestation epoch {epoch}"
            ))
        })?;
    let target_root = previous.clone();
    let mut block_roots = HashMap::new();
    for slot in start..inclusion_slot.min(epoch_start_slot(epoch + 1)) {
        if let Some(root) = roots.get(&slot) {
            previous = root.clone();
        }
        block_roots.insert(slot, previous.clone());
    }
    Ok(VoteContext {
        block_roots,
        target_root,
        target_epoch: epoch,
        source: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires a disposable RECOVERY_TEST_DATABASE_URL"]
    async fn pruned_previous_epoch_does_not_hide_current_inclusions() {
        use axum::{Json, Router, extract::Request, http::StatusCode, response::IntoResponse};
        use serde_json::json;
        let pool = db::isolated_test_pool().await;
        db::scanner::validators::upsert_validator(&pool, 1, &[1], 0, None)
            .await
            .unwrap();
        let root = BlockRoot::parse(&format!("0x{}", "11".repeat(32))).unwrap();
        let mock_root = root.clone();
        let app = Router::new().fallback(move |request: Request| {
            let root = mock_root.clone();
            async move {
                let path = request.uri().path();
                if path.starts_with("/eth/v1/beacon/headers/") {
                    let id = path.rsplit('/').next().unwrap();
                    let slot = if id == "head" { "65" } else { id };
                    return Json(json!({"execution_optimistic": false, "data": {
                        "root": root, "canonical": true, "header": {"message": {
                            "slot": slot, "proposer_index":"1", "parent_root":root,
                            "state_root":root, "body_root":root
                        }}
                    }}))
                    .into_response();
                }
                if path == "/eth/v1/beacon/states/64/committees" {
                    return Json(json!({"execution_optimistic":false,"data":[
                        {"index":"0","slot":"64","validators":["1"]}
                    ]}))
                    .into_response();
                }
                if path == "/eth/v1/validator/duties/attester/2" {
                    return Json(json!({"execution_optimistic":false,"data":[{
                        "pubkey":"0x01","validator_index":"1","committee_index":"0",
                        "committee_length":"1","committees_at_slot":"1",
                        "validator_committee_index":"0","slot":"64"
                    }]}))
                    .into_response();
                }
                StatusCode::NOT_FOUND.into_response()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = BeaconClient::new(&format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut block: serde_json::Value =
            serde_json::from_str(include_str!("../../../testdata/blocks/phase0.json")).unwrap();
        block["data"]["message"]["slot"] = json!("65");
        block["data"]["message"]["body"]["attestations"] = json!([63, 64].map(|slot| json!({
            "aggregation_bits":"0x03","signature":"0x00","data":{
                "slot":slot.to_string(),"index":"0","beacon_block_root":root,
                "source":{"epoch":"0","root":root},
                "target":{"epoch":(slot/32).to_string(),"root":root}
            }
        })));
        let block: crate::beacon_client::types::RawBlockResponse =
            serde_json::from_value(block).unwrap();
        let block = block.into_parts().0;
        let roots = HashMap::from([(32, root.clone()), (64, root.clone()), (65, root)]);
        let result = scan_live_attestations_in_slot(
            &client,
            &pool,
            &HashSet::from([1]),
            &block,
            &roots,
            false,
        )
        .await;
        assert!(matches!(result, Err(Error::BeaconApi { status: 404, .. })));
        let stored = db::api::live::fetch_attestation_status(&pool, &[1], 1, 2)
            .await
            .unwrap();
        assert_eq!(stored.get(&(64, 1)), Some(&(true, false, Some(65))));
        assert!(!stored.contains_key(&(63, 1)));
        let completed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM completed_scans")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(completed, 0);
        server.abort();
        pool.close().await;
    }

    #[test]
    fn live_votes_carry_skipped_roots_and_do_not_depend_on_rewards() {
        let root = BlockRoot::parse(&format!("0x{}", "11".repeat(32))).unwrap();
        let next = BlockRoot::parse(&format!("0x{}", "22".repeat(32))).unwrap();
        // Epoch 3 begins at 96, which is skipped, as are duty 97 and slot 98.
        let roots = HashMap::from([(95, root.clone()), (99, next)]);
        let context = live_vote_context(3, 99, &roots).unwrap();
        let vote = crate::beacon_client::types::AttestationData {
            slot: 97,
            index: 0,
            beacon_block_root: root.clone(),
            source: Checkpoint {
                epoch: 1,
                root: root.clone(),
            },
            target: Checkpoint { epoch: 3, root },
        };
        let (_, marks) = decode::compute_vote_correctness(&vote, &context).unwrap();
        assert!(marks.head_correct && marks.target_correct && marks.source_correct);
        assert_eq!(inclusion_delay(99, 97).unwrap(), 2);
        assert_eq!((98..=99).filter(|slot| roots.contains_key(slot)).count(), 1);
        // A two-slot delay earns no timely-head reward, despite a correct head vote.
    }

    #[test]
    fn live_votes_require_boundary_ancestry() {
        let root = BlockRoot::parse(&format!("0x{}", "11".repeat(32))).unwrap();
        assert!(live_vote_context(3, 99, &HashMap::from([(98, root)])).is_err());
    }

    #[test]
    fn inclusion_delay_rejects_negative() {
        assert!(matches!(
            inclusion_delay(100, 101),
            Err(Error::InconsistentBeaconData(_))
        ));
    }

    #[test]
    fn inclusion_delay_zero_is_permissive() {
        // Same-slot inclusion is a spec violation (MIN_ATTESTATION_INCLUSION_DELAY=1)
        // but unreachable from real chain data, so the helper doesn't reject it.
        assert_eq!(inclusion_delay(100, 100).unwrap(), 0);
    }

    #[test]
    fn effective_inclusion_delay_subtracts_interior_misses() {
        let missed: HashSet<u64> = [101, 102].into_iter().collect();
        assert_eq!(effective_inclusion_delay(100, 103, 3, &missed), 1);
    }

    #[test]
    fn effective_inclusion_delay_optimal_is_unchanged() {
        let missed: HashSet<u64> = HashSet::new();
        assert_eq!(effective_inclusion_delay(100, 101, 1, &missed), 1);
    }

    #[test]
    fn effective_inclusion_delay_ignores_misses_outside_window() {
        let missed: HashSet<u64> = [99, 103, 104].into_iter().collect();
        assert_eq!(effective_inclusion_delay(100, 103, 3, &missed), 3);
    }
}
