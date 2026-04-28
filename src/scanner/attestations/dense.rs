//! Dense-mode attestation pipeline.
//!
//! Fetches every block in the epoch (+ a one-epoch late window for inclusion
//! discovery), builds a block-root map, derives vote correctness from the
//! attestation data vs the canonical chain, and writes a row per tracked duty.
//!
//! Amortises network cost across many validators: at 30+ tracked validators
//! nearly every slot has a duty so the 64-block fetch pays for itself. For
//! sparse sets see `super::sparse`.

use std::collections::{HashMap, HashSet};

use futures::future::join_all;

use super::decode::{build_committee_map, collect_inclusions_from_blocks};
use super::{AttestationInclusion, VoteContext, effective_inclusion_delay};
use crate::beacon_client::BeaconClient;
use crate::beacon_client::types::{
    AttesterDuty, BlockRoot, SignedBeaconBlock, ValidatorAttestationReward,
};
use crate::chain;
use crate::db;
use crate::db::Pool as PgPool;
use crate::error::{Error, Result};

/// Process attestation duties for an epoch and write outcomes/rewards.
pub async fn process_epoch_attestation_duties(
    client: &BeaconClient,
    pool: &PgPool,
    epoch: u64,
    scan_validators: &HashSet<u64>,
    finalized: bool,
) -> Result<()> {
    use std::time::Instant;
    let total_t = Instant::now();

    let start_slot = chain::epoch_start_slot(epoch);
    let slots_per_epoch = chain::slots_per_epoch();

    let phase_t = Instant::now();
    let all_slots: Vec<u64> = (start_slot..start_slot + slots_per_epoch * 2).collect();
    let all_blocks = fetch_blocks_for_slots(client, &all_slots).await?;
    let blocks_ms = phase_t.elapsed().as_millis() as u64;

    let missed_slots: HashSet<u64> = all_blocks
        .iter()
        .filter(|(_, b)| b.is_none())
        .map(|(s, _)| *s)
        .collect();
    let epoch_end = slots_per_epoch as usize;
    let epoch_blocks = &all_blocks[..epoch_end];
    let (block_count, missed_count) =
        epoch_blocks
            .iter()
            .fold((0u32, 0u32), |(b, m), (_, block)| {
                if block.is_some() {
                    (b + 1, m)
                } else {
                    (b, m + 1)
                }
            });

    let phase_t = Instant::now();
    let committees = client.get_committees(epoch).await?;
    let committee_map = build_committee_map(&committees);
    let committees_ms = phase_t.elapsed().as_millis() as u64;

    let phase_t = Instant::now();
    let vote_ctx = build_vote_context(client, epoch, start_slot, epoch_blocks).await?;
    let vote_ctx_ms = phase_t.elapsed().as_millis() as u64;

    let scan_validators_for_decode = scan_validators.clone();
    let dispatch_t = Instant::now();
    let inclusions_handle = tokio::task::spawn_blocking(move || {
        let cpu_t = Instant::now();
        let result = collect_inclusions_from_blocks(
            &all_blocks,
            &committee_map,
            &scan_validators_for_decode,
            &vote_ctx,
        );
        (result, cpu_t.elapsed().as_millis() as u64)
    });

    let scan_validator_indices: Vec<u64> = scan_validators.iter().copied().collect();
    let duties_rewards_t = Instant::now();
    let (attester_duties, att_rewards) = tokio::try_join!(
        client.get_attester_duties(epoch, &scan_validator_indices),
        client.get_attestation_rewards(epoch, &scan_validator_indices),
    )?;
    let duties_rewards_ms = duties_rewards_t.elapsed().as_millis() as u64;
    let rewards_map: HashMap<u64, ValidatorAttestationReward> = att_rewards
        .total_rewards
        .into_iter()
        .map(|r| (r.validator_index, r))
        .collect();

    let wait_t = Instant::now();
    let (inclusions_res, inclusions_cpu_ms) = inclusions_handle
        .await
        .expect("attestation inclusion decode task panicked");
    let inclusions_wait_ms = wait_t.elapsed().as_millis() as u64;
    let attestation_inclusions = inclusions_res?;
    let concurrent_ms = dispatch_t.elapsed().as_millis() as u64;

    let phase_t = Instant::now();
    let (included_count, missed_count_writes) = write_duty_rows(
        pool,
        epoch,
        finalized,
        scan_validators,
        &attester_duties,
        &attestation_inclusions,
        &rewards_map,
        &missed_slots,
    )
    .await?;
    let writes_ms = phase_t.elapsed().as_millis() as u64;

    tracing::debug!(
        epoch,
        validators = scan_validators.len(),
        included = included_count,
        missed = missed_count_writes,
        blocks_present = block_count,
        blocks_missed = missed_count,
        inclusions_found = attestation_inclusions.len(),
        blocks_ms,
        committees_ms,
        vote_ctx_ms,
        inclusions_cpu_ms,
        inclusions_wait_ms,
        duties_rewards_ms,
        concurrent_ms,
        writes_ms,
        total_ms = total_t.elapsed().as_millis() as u64,
        "Dense phase breakdown"
    );

    Ok(())
}

async fn fetch_blocks_for_slots(
    client: &BeaconClient,
    slots: &[u64],
) -> Result<Vec<(u64, Option<SignedBeaconBlock>)>> {
    let fetched = join_all(
        slots
            .iter()
            .copied()
            .map(|slot| async move { (slot, client.get_block(slot).await.map(|(b, _)| b)) }),
    )
    .await;

    let mut blocks = Vec::with_capacity(slots.len());
    for (slot, block_res) in fetched {
        blocks.push((slot, block_res?));
    }
    Ok(blocks)
}

/// Build the canonical block root map for an epoch. Every slot in
/// `[start_slot, start_slot + slots_per_epoch)` must end up in the map — either by
/// its own block or by carrying forward the most recent prior root. A missing
/// entry makes head-vote correctness undecidable at that slot.
async fn build_block_root_map(
    client: &BeaconClient,
    start_slot: u64,
    blocks: &[(u64, Option<SignedBeaconBlock>)],
) -> Result<HashMap<u64, BlockRoot>> {
    let mut roots: HashMap<u64, BlockRoot> = HashMap::new();

    // Most hits come from the slot_root cache warmed by the earlier `get_block`
    // calls; the rest execute concurrently.
    let root_fetches = blocks
        .iter()
        .filter(|(_, b)| b.is_some())
        .map(|&(slot, _)| async move { (slot, client.get_block_root(slot).await) });
    for (slot, res) in join_all(root_fetches).await {
        match res? {
            (Some(root), _) => {
                tracing::trace!(slot, root = %root, "Fetched block root");
                roots.insert(slot, root);
            }
            (None, _) => {
                return Err(Error::InconsistentBeaconData(format!(
                    "block present at slot {slot} but /blocks/{slot}/root returned 404",
                )));
            }
        }
    }

    let mut last_known_root: Option<BlockRoot> = None;

    if start_slot > 0 && !roots.contains_key(&start_slot) {
        for prior_slot in (start_slot.saturating_sub(chain::slots_per_epoch())..start_slot).rev() {
            match client.get_block_root(prior_slot).await? {
                (Some(root), _) => {
                    tracing::trace!(
                        prior_slot,
                        root = %root,
                        "Found carry-forward root from prior epoch"
                    );
                    last_known_root = Some(root);
                    break;
                }
                (None, _) => continue,
            }
        }
    }

    let mut carried_count = 0u32;
    for slot in start_slot..start_slot + chain::slots_per_epoch() {
        if let Some(root) = roots.get(&slot) {
            last_known_root = Some(root.clone());
        } else if let Some(ref root) = last_known_root {
            roots.insert(slot, root.clone());
            carried_count += 1;
        } else {
            return Err(Error::InconsistentBeaconData(format!(
                "no canonical root available for slot {slot} (epoch starting at {start_slot}); \
                 carry-forward search found nothing",
            )));
        }
    }

    tracing::trace!(
        total_roots = roots.len(),
        actual_blocks = roots.len() as u32 - carried_count,
        carried_forward = carried_count,
        "Block root map complete"
    );

    Ok(roots)
}

/// Build the VoteContext for an epoch: canonical block roots + expected source/target checkpoints.
async fn build_vote_context(
    client: &BeaconClient,
    epoch: u64,
    start_slot: u64,
    blocks: &[(u64, Option<SignedBeaconBlock>)],
) -> Result<VoteContext> {
    let block_roots = build_block_root_map(client, start_slot, blocks).await?;

    let target_root = block_roots.get(&start_slot).cloned().ok_or_else(|| {
        Error::InconsistentBeaconData(format!(
            "target root missing for epoch-start slot {start_slot}"
        ))
    })?;

    let checkpoints = client
        .get_finality_checkpoints(&start_slot.to_string())
        .await?;

    tracing::trace!(
        epoch,
        target_root = %target_root,
        source_epoch = checkpoints.current_justified.epoch,
        source_root = %checkpoints.current_justified.root,
        finalized_epoch = checkpoints.finalized.epoch,
        "Vote context built"
    );

    Ok(VoteContext {
        block_roots,
        target_root,
        target_epoch: epoch,
        source_epoch: checkpoints.current_justified.epoch,
        source_root: checkpoints.current_justified.root,
    })
}

#[allow(clippy::too_many_arguments)]
async fn write_duty_rows(
    pool: &PgPool,
    epoch: u64,
    finalized: bool,
    scan_validators: &HashSet<u64>,
    attester_duties: &[AttesterDuty],
    inclusions: &HashMap<u64, AttestationInclusion>,
    rewards_map: &HashMap<u64, ValidatorAttestationReward>,
    missed_slots: &HashSet<u64>,
) -> Result<(u32, u32)> {
    let mut included_count = 0u32;
    let mut missed_count = 0u32;

    for duty in attester_duties {
        if !scan_validators.contains(&duty.validator_index) {
            continue;
        }

        let inclusion = inclusions.get(&duty.validator_index);
        let reward = rewards_map.get(&duty.validator_index);

        let effective_delay = inclusion.map(|inc| {
            effective_inclusion_delay(
                duty.slot,
                inc.inclusion_slot,
                inc.inclusion_delay,
                missed_slots,
            )
        });

        if let Some(inc) = inclusion {
            tracing::trace!(
                epoch,
                validator = duty.validator_index,
                assigned_slot = duty.slot,
                inclusion_slot = inc.inclusion_slot,
                inclusion_delay = inc.inclusion_delay,
                effective_inclusion_delay = effective_delay,
                head_correct = inc.head_correct,
                target_correct = inc.target_correct,
                source_correct = inc.source_correct,
                source_reward = reward.map(|r| r.source),
                target_reward = reward.map(|r| r.target),
                head_reward = reward.map(|r| r.head),
                "Attestation included"
            );
            included_count += 1;
        } else {
            tracing::trace!(
                epoch,
                validator = duty.validator_index,
                assigned_slot = duty.slot,
                source_reward = reward.map(|r| r.source),
                target_reward = reward.map(|r| r.target),
                head_reward = reward.map(|r| r.head),
                "Attestation missed"
            );
            missed_count += 1;
        }

        db::scanner::attestations::upsert_attestation_duty(
            pool,
            duty.validator_index as i64,
            epoch as i64,
            duty.slot as i64,
            duty.committee_index as i32,
            duty.validator_committee_index as i32,
            inclusion.is_some(),
            inclusion.map(|i| i.inclusion_slot as i64),
            inclusion.map(|i| i.inclusion_delay as i32),
            effective_delay.map(|d| d as i32),
            inclusion.map(|i| i.source_correct),
            inclusion.map(|i| i.target_correct),
            inclusion.map(|i| i.head_correct),
            reward.map(|r| r.source),
            reward.map(|r| r.target),
            reward.map(|r| r.head),
            reward.and_then(|r| r.inactivity),
            finalized,
        )
        .await?;
    }

    crate::metrics::SCANNER_ATT_DUTIES
        .with_label_values(&["dense", "included"])
        .inc_by(included_count as u64);
    crate::metrics::SCANNER_ATT_DUTIES
        .with_label_values(&["dense", "missed"])
        .inc_by(missed_count as u64);
    Ok((included_count, missed_count))
}
