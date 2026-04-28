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
use crate::beacon_client::types::{BlockRoot, SignedBeaconBlock};
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

/// The canonical chain context needed to verify attestation vote correctness.
pub(super) struct VoteContext {
    /// Canonical block root at each slot in the epoch.
    /// For missed slots, this is the most recent block root before that slot.
    pub(super) block_roots: HashMap<u64, BlockRoot>,
    pub(super) target_root: BlockRoot,
    pub(super) target_epoch: u64,
    pub(super) source_epoch: u64,
    pub(super) source_root: BlockRoot,
}

/// Record of when/how a validator's attestation was included, with vote correctness.
pub(super) struct AttestationInclusion {
    pub(super) inclusion_slot: u64,
    pub(super) inclusion_delay: u64,
    pub(super) source_correct: bool,
    pub(super) target_correct: bool,
    pub(super) head_correct: bool,
}

/// Head / target / source correctness for a single attestation vote. Default is all
/// false, which is what live scans (no `VoteContext`) record.
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
pub async fn scan_live_attestations_in_slot(
    client: &BeaconClient,
    pool: &PgPool,
    inclusion_slot: u64,
    scan_validators: &HashSet<u64>,
    block_override: Option<&SignedBeaconBlock>,
) -> Result<()> {
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

    let block = if let Some(block) = block_override {
        Some(block.clone())
    } else {
        client.get_block(inclusion_slot).await?.0
    };

    let Some(block) = block else {
        tracing::trace!(
            inclusion_slot,
            "No block at inclusion slot; nothing to process"
        );
        return Ok(());
    };

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

    let scan_validator_indices: Vec<u64> = scan_validators.iter().copied().collect();
    let mut total_updated = 0u32;

    for epoch in candidate_epochs {
        let committees = client.get_committees(epoch).await?;
        let committee_map = decode::build_committee_map(&committees);

        let mut inclusions = HashMap::new();
        decode::extract_attestation_inclusions(
            &block,
            inclusion_slot,
            epoch,
            &committee_map,
            scan_validators,
            None,
            &mut inclusions,
        )?;

        if inclusions.is_empty() {
            tracing::trace!(
                epoch,
                inclusion_slot,
                "No tracked attestation inclusions in live slot"
            );
            continue;
        }

        let duties = client
            .get_attester_duties(epoch, &scan_validator_indices)
            .await?;
        let duties_map: HashMap<u64, _> =
            duties.into_iter().map(|d| (d.validator_index, d)).collect();

        let mut updated = 0u32;
        for (&validator_index, inc) in &inclusions {
            let Some(duty) = duties_map.get(&validator_index) else {
                tracing::trace!(
                    validator_index,
                    epoch,
                    "No duty found for included validator"
                );
                continue;
            };

            if duty.slot < min_att_slot || duty.slot > max_att_slot {
                continue;
            }

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
                // Live path doesn't know which intervening slots were missed. Write the
                // raw delay as an optimistic placeholder so UIs filtering on effective
                // delay still see the row; the finalized scan_epoch pass overwrites with
                // the chain-level-adjusted value.
                Some(inc.inclusion_delay as i32),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                false,
            )
            .await?;

            updated += 1;
        }

        total_updated += updated;

        tracing::debug!(
            epoch,
            inclusion_slot,
            updated,
            "Live slot attestation updates written"
        );
    }

    tracing::debug!(
        inclusion_slot,
        min_att_slot,
        max_att_slot,
        total_updated,
        "Live slot attestation processing complete"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
