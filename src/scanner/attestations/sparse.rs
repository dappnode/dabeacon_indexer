//! Sparse-mode attestation pipeline.
//!
//! Designed for 1–5 tracked validators where the dense flow's 64-block fetch
//! amortises poorly — most decoded attestations are for untracked validators.
//!
//! Correctness is derived directly from `/eth/v1/beacon/rewards/attestations`:
//! Altair+ rewards are paid for timely-and-correct components, so a positive
//! reward is the authoritative signal that the corresponding vote (source /
//! target / head) was correct. Inclusion is detected by scanning forward from
//! each duty's slot and short-circuiting on the first block that contains the
//! validator's attestation. Duties whose rewards show no inclusion skip the
//! block scan entirely.
//!
//! # Semantic difference from dense
//!
//! Dense mode's `*_correct` columns reflect "the vote was right". Sparse mode's
//! columns reflect "the validator earned the reward for that component", which
//! requires both a correct vote AND timely inclusion (next slot for head,
//! within ~5 for source, within 32 for target). A correct head vote included
//! one slot late therefore reads `head_correct = false` in sparse but `true`
//! in dense. Operators typically care about the reward-qualifying definition.

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
    let Some(reward) = reward else {
        return SparseRow {
            included: false,
            inclusion_slot: None,
            inclusion_delay: None,
            effective_inclusion_delay: None,
            source_correct: None,
            target_correct: None,
            head_correct: None,
            source_reward: None,
            target_reward: None,
            head_reward: None,
            inactivity_penalty: None,
        };
    };

    // A positive reward for any component implies both inclusion AND a correct
    // vote for that component — see module docs for the semantic caveat.
    let included = reward.source > 0 || reward.target > 0 || reward.head > 0;

    let (inclusion_slot, inclusion_delay, effective_inclusion_delay) =
        match (found_inclusion, reward.inclusion_delay) {
            (Some((slot, missed_before)), _) => {
                let delay = (slot as i64 - duty.slot as i64) as i32;
                let effective = delay.saturating_sub(missed_before as i32);
                (Some(slot as i64), Some(delay), Some(effective))
            }
            // Scan didn't locate the block (or wasn't run). Without the intervening
            // missed-slot count we fall back to raw == effective: the row remains
            // visible to effective-delay filters, and a later re-scan can refine it.
            (None, Some(delay)) => (None, Some(delay as i32), Some(delay as i32)),
            (None, None) => (None, None, None),
        };

    SparseRow {
        included,
        inclusion_slot,
        inclusion_delay,
        effective_inclusion_delay,
        source_correct: Some(reward.source > 0),
        target_correct: Some(reward.target > 0),
        head_correct: Some(reward.head > 0),
        source_reward: Some(reward.source),
        target_reward: Some(reward.target),
        head_reward: Some(reward.head),
        inactivity_penalty: reward.inactivity,
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

        let rewards_say_included = reward
            .map(|r| r.source > 0 || r.target > 0 || r.head > 0)
            .unwrap_or(false);

        // Skip the forward scan when rewards show no inclusion — no block fetch
        // can find what isn't there.
        let scan_t = Instant::now();
        let found_inclusion_slot = if rewards_say_included {
            let scan =
                scan_forward_for_inclusion(client, duty, epoch, &committee_map, &mut block_fetches)
                    .await?;
            if scan.is_none() {
                tracing::warn!(
                    epoch,
                    validator = duty.validator_index,
                    duty_slot = duty.slot,
                    "Rewards show inclusion but scan-forward didn't find the including block — \
                     inclusion_slot will be NULL"
                );
            }
            scan
        } else {
            None
        };
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
    let last_slot = duty.slot + chain::slots_per_epoch();
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
    fn sparse_row_all_zero_rewards_is_missed() {
        let r = reward(0, 0, 0, None, Some(-100));
        let row = derive_sparse_row(&duty(100), Some(&r), None);
        assert!(!row.included);
        assert_eq!(row.source_correct, Some(false));
        assert_eq!(row.target_correct, Some(false));
        assert_eq!(row.head_correct, Some(false));
        assert_eq!(row.inactivity_penalty, Some(-100));
        assert_eq!(row.inclusion_slot, None);
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
    fn sparse_row_head_late_is_included_head_incorrect() {
        let r = reward(10, 20, 0, None, None);
        let row = derive_sparse_row(&duty(100), Some(&r), Some((103, 0)));
        assert!(row.included);
        assert_eq!(row.source_correct, Some(true));
        assert_eq!(row.target_correct, Some(true));
        assert_eq!(row.head_correct, Some(false));
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
        // Pre-Altair path: reward carries the delay directly. Without a miss
        // count we optimistically set effective = raw so the row stays visible
        // to effective-delay filters.
        let r = reward(10, 0, 0, Some(2), None);
        let row = derive_sparse_row(&duty(100), Some(&r), None);
        assert!(row.included);
        assert_eq!(row.inclusion_slot, None);
        assert_eq!(row.inclusion_delay, Some(2));
        assert_eq!(row.effective_inclusion_delay, Some(2));
    }

    #[test]
    fn sparse_row_no_reward_entry_is_defensively_missed() {
        let row = derive_sparse_row(&duty(100), None, None);
        assert!(!row.included);
        assert_eq!(row.source_correct, None);
        assert_eq!(row.inclusion_slot, None);
    }
}
