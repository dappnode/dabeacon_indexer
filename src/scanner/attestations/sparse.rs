//! Sparse-mode attestation pipeline.
//!
//! Designed for 1–5 tracked validators where the dense flow's 64-block fetch
//! amortises poorly — most decoded attestations are for untracked validators.
//!
//! Every duty is scanned through its full inclusion window, including when all
//! rewards are zero (for example during an inactivity leak). `included` records
//! observed inclusion independently of rewards. The shared `*_correct` contract
//! is vote correctness: a positive reward proves a correct vote, while zero or
//! negative rewards leave correctness unknown without canonical vote context.
//! Dense archive scans may refine unknown flags using that context.

use std::collections::{HashMap, HashSet};

use super::decode::{build_committee_map, extract_attestation_inclusions};
use crate::beacon_client::BeaconClient;
use crate::beacon_client::types::{AttesterDuty, ValidatorAttestationReward};
use crate::chain;
use crate::db;
use crate::db::Pool as PgPool;
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SparseRow {
    pub included: bool,
    pub inclusion_slot: Option<i64>,
    pub inclusion_delay: Option<i32>,
    pub effective_inclusion_delay: Option<i32>,
    pub source_correct: Option<bool>,
    pub target_correct: Option<bool>,
    pub head_correct: Option<bool>,
    pub source_reward: Option<i64>,
    pub target_reward: Option<i64>,
    pub head_reward: Option<i64>,
    pub inactivity_penalty: Option<i64>,
}

/// Derive the per-duty row from `(duty, reward, found_inclusion)`. Falls back
/// to `reward.inclusion_delay` (pre-Altair responses carry it directly) when
/// the scan-forward didn't locate the block, leaving `inclusion_slot` NULL.
pub(super) fn derive_sparse_row(
    duty: &AttesterDuty,
    reward: Option<&ValidatorAttestationReward>,
    found_inclusion: Option<(u64, u32)>,
) -> SparseRow {
    let included = found_inclusion.is_some();

    let (inclusion_slot, inclusion_delay, effective_inclusion_delay) =
        match (found_inclusion, reward.and_then(|r| r.inclusion_delay)) {
            (Some((slot, missed_before)), _) => {
                let delay = (slot as i64 - duty.slot as i64) as i32;
                let effective = delay.saturating_sub(missed_before as i32);
                (Some(slot as i64), Some(delay), Some(effective))
            }
            // A response delay without the including block does not provide the
            // skipped-slot coverage needed for an effective delay.
            (None, Some(delay)) => (None, Some(delay as i32), None),
            (None, None) => (None, None, None),
        };

    SparseRow {
        included,
        inclusion_slot,
        inclusion_delay,
        effective_inclusion_delay,
        source_correct: reward.and_then(|r| (r.source > 0).then_some(true)),
        target_correct: reward.and_then(|r| (r.target > 0).then_some(true)),
        head_correct: reward.and_then(|r| (r.head > 0).then_some(true)),
        source_reward: reward.map(|r| r.source),
        target_reward: reward.map(|r| r.target),
        head_reward: reward.map(|r| r.head),
        inactivity_penalty: reward.and_then(|r| r.inactivity),
    }
}

pub async fn process_epoch_attestation_duties_sparse(
    client: &BeaconClient,
    pool: &PgPool,
    epoch: u64,
    scan_validators: &HashSet<u64>,
    finalized: bool,
) -> Result<()> {
    if scan_validators.is_empty() {
        return Ok(());
    }

    use std::time::Instant;
    let total_t = Instant::now();
    let scan_validator_indices: Vec<u64> = scan_validators.iter().copied().collect();

    // Phase 1+2: duties + rewards run concurrently.
    let duties_rewards_t = Instant::now();
    let (duties, att_rewards) = tokio::try_join!(
        client.get_attester_duties(epoch, &scan_validator_indices),
        client.get_attestation_rewards(epoch, &scan_validator_indices),
    )?;
    let duties_rewards_ms = duties_rewards_t.elapsed().as_millis() as u64;
    let rewards_map: HashMap<u64, ValidatorAttestationReward> = att_rewards
        .total_rewards
        .into_iter()
        .map(|r| (r.validator_index, r))
        .collect();
    super::validate_epoch_response(scan_validators, &duties, &rewards_map)?;

    // Phase 3: committees.
    let phase_t = Instant::now();
    let committees = client.get_committees(epoch).await?;
    let committee_map = build_committee_map(&committees);
    let committees_ms = phase_t.elapsed().as_millis() as u64;

    let mut included_count = 0u32;
    let mut missed_count = 0u32;
    let mut block_fetches = 0u32;
    // Phase 4: per-duty work — split into scan-forward (block fetches +
    // attestation decode) and DB upsert.
    let mut scan_ms = 0u64;
    let mut writes_ms = 0u64;

    for duty in &duties {
        if !scan_validators.contains(&duty.validator_index) {
            continue;
        }

        let reward = rewards_map.get(&duty.validator_index);

        // A zero reward is not evidence of a miss. Only a successful complete
        // scan can establish absence; any failed block request aborts this duty.
        let scan_t = Instant::now();
        let found_inclusion_slot =
            scan_forward_for_inclusion(client, duty, epoch, &committee_map, &mut block_fetches)
                .await?;
        if found_inclusion_slot.is_none()
            && reward.is_some_and(|r| r.source > 0 || r.target > 0 || r.head > 0)
        {
            return Err(crate::error::Error::InconsistentBeaconData(format!(
                "positive rewards but no inclusion for validator {} in epoch {epoch}",
                duty.validator_index
            )));
        }
        scan_ms += scan_t.elapsed().as_millis() as u64;

        let row = derive_sparse_row(duty, reward, found_inclusion_slot);
        if row.included {
            included_count += 1;
        } else {
            missed_count += 1;
        }

        tracing::trace!(
            epoch,
            validator = duty.validator_index,
            assigned_slot = duty.slot,
            included = row.included,
            inclusion_slot = row.inclusion_slot,
            inclusion_delay = row.inclusion_delay,
            source_correct = row.source_correct,
            target_correct = row.target_correct,
            head_correct = row.head_correct,
            "Sparse attestation row"
        );

        let write_t = Instant::now();
        db::scanner::attestations::upsert_attestation_duty(
            pool,
            duty.validator_index as i64,
            epoch as i64,
            duty.slot as i64,
            duty.committee_index as i32,
            duty.validator_committee_index as i32,
            row.included,
            row.inclusion_slot,
            row.inclusion_delay,
            row.effective_inclusion_delay,
            row.source_correct,
            row.target_correct,
            row.head_correct,
            row.source_reward,
            row.target_reward,
            row.head_reward,
            row.inactivity_penalty,
            finalized,
        )
        .await?;
        writes_ms += write_t.elapsed().as_millis() as u64;
    }

    crate::metrics::SCANNER_ATT_DUTIES
        .with_label_values(&["sparse", "included"])
        .inc_by(included_count as u64);
    crate::metrics::SCANNER_ATT_DUTIES
        .with_label_values(&["sparse", "missed"])
        .inc_by(missed_count as u64);
    tracing::debug!(
        epoch,
        validators = scan_validators.len(),
        included = included_count,
        missed = missed_count,
        block_fetches,
        duties_rewards_ms,
        committees_ms,
        scan_ms,
        writes_ms,
        total_ms = total_t.elapsed().as_millis() as u64,
        "Sparse phase breakdown"
    );
    Ok(())
}

/// Walk slots after `duty.slot` up to one epoch later, returning
/// `(inclusion_slot, missed_slots_in_between)` on the first match. The second
/// value — slots in `(duty.slot, inclusion_slot)` with no block — feeds
/// `effective_inclusion_delay`. `block_fetches` is incremented per `get_block`
/// so the caller can report network cost.
async fn scan_forward_for_inclusion(
    client: &BeaconClient,
    duty: &AttesterDuty,
    target_epoch: u64,
    committee_map: &HashMap<(u64, u64), Vec<u64>>,
    block_fetches: &mut u32,
) -> Result<Option<(u64, u32)>> {
    // EIP-7045: inclusion must happen by target_epoch+1.
    let last_slot = chain::epoch_start_slot(target_epoch + 2) - 1;
    let probe_set: HashSet<u64> = std::iter::once(duty.validator_index).collect();
    let mut missed_before = 0u32;

    for slot in (duty.slot + 1)..=last_slot {
        let (block_opt, _) = client.get_block(slot).await?;
        *block_fetches += 1;
        let Some(block) = block_opt else {
            missed_before += 1;
            tracing::trace!(
                validator = duty.validator_index,
                duty_slot = duty.slot,
                probe_slot = slot,
                "Probe slot has no block"
            );
            continue;
        };

        let mut inclusions = HashMap::new();
        extract_attestation_inclusions(
            &block,
            slot,
            target_epoch,
            committee_map,
            &probe_set,
            /* ctx */ None,
            &mut inclusions,
        )?;

        if inclusions.contains_key(&duty.validator_index) {
            return Ok(Some((slot, missed_before)));
        }
        tracing::trace!(
            validator = duty.validator_index,
            duty_slot = duty.slot,
            probe_slot = slot,
            "Probe slot didn't include this validator"
        );
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beacon_client::types::ValidatorAttestationReward;

    fn reward(
        source: i64,
        target: i64,
        head: i64,
        delay: Option<i64>,
        inactivity: Option<i64>,
    ) -> ValidatorAttestationReward {
        ValidatorAttestationReward {
            validator_index: 42,
            source,
            target,
            head,
            inclusion_delay: delay,
            inactivity,
        }
    }

    fn duty(slot: u64) -> AttesterDuty {
        AttesterDuty {
            pubkey: "0x00".into(),
            validator_index: 42,
            committee_index: 0,
            validator_committee_index: 0,
            committee_length: 1,
            committees_at_slot: 1,
            slot,
        }
    }

    #[test]
    fn sparse_row_zero_rewards_without_observed_inclusion() {
        let r = reward(0, 0, 0, None, Some(-100));
        let row = derive_sparse_row(&duty(100), Some(&r), None);
        assert!(!row.included);
        assert_eq!(row.source_correct, None);
        assert_eq!(row.target_correct, None);
        assert_eq!(row.head_correct, None);
        assert_eq!(row.inactivity_penalty, Some(-100));
        assert_eq!(row.inclusion_slot, None);
    }

    #[test]
    fn observed_inclusion_survives_zero_or_missing_rewards() {
        let zero = reward(0, 0, 0, None, Some(-100));
        for rewards in [Some(&zero), None] {
            let row = derive_sparse_row(&duty(100), rewards, Some((103, 1)));
            assert!(row.included);
            assert_eq!(row.inclusion_slot, Some(103));
            assert_eq!(row.effective_inclusion_delay, Some(2));
            assert_eq!(row.source_correct, None);
            assert_eq!(row.head_correct, None);
        }
    }

    #[test]
    fn sparse_row_all_positive_rewards_is_included_all_correct() {
        let r = reward(10, 20, 5, None, None);
        let row = derive_sparse_row(&duty(100), Some(&r), Some((101, 0)));
        assert!(row.included);
        assert_eq!(row.source_correct, Some(true));
        assert_eq!(row.target_correct, Some(true));
        assert_eq!(row.head_correct, Some(true));
        assert_eq!(row.inclusion_slot, Some(101));
        assert_eq!(row.inclusion_delay, Some(1));
        assert_eq!(row.effective_inclusion_delay, Some(1));
    }

    #[test]
    fn sparse_row_head_late_leaves_vote_correctness_unknown() {
        let r = reward(10, 20, 0, None, None);
        let row = derive_sparse_row(&duty(100), Some(&r), Some((103, 0)));
        assert!(row.included);
        assert_eq!(row.source_correct, Some(true));
        assert_eq!(row.target_correct, Some(true));
        assert_eq!(row.head_correct, None);
        assert_eq!(row.inclusion_slot, Some(103));
        assert_eq!(row.inclusion_delay, Some(3));
        assert_eq!(row.effective_inclusion_delay, Some(3));
    }

    #[test]
    fn sparse_row_effective_delay_subtracts_missed_slots() {
        let r = reward(10, 20, 5, None, None);
        let row = derive_sparse_row(&duty(100), Some(&r), Some((103, 2)));
        assert_eq!(row.inclusion_delay, Some(3));
        assert_eq!(row.effective_inclusion_delay, Some(1));
    }

    #[test]
    fn sparse_row_uses_reward_delay_when_scan_missed() {
        // A legacy reward can carry a raw delay, but without the including
        // block it cannot prove inclusion or the skipped-slot adjustment.
        let r = reward(10, 0, 0, Some(2), None);
        let row = derive_sparse_row(&duty(100), Some(&r), None);
        assert!(!row.included);
        assert_eq!(row.inclusion_slot, None);
        assert_eq!(row.inclusion_delay, Some(2));
        assert_eq!(row.effective_inclusion_delay, None);
    }

    #[test]
    fn sparse_row_no_reward_entry_leaves_reward_and_correctness_unknown() {
        let row = derive_sparse_row(&duty(100), None, None);
        assert!(!row.included);
        assert_eq!(row.source_correct, None);
        assert_eq!(row.inclusion_slot, None);
    }
}
