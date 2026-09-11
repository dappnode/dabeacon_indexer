use std::collections::{HashMap, HashSet};

use crate::beacon_client::BeaconClient;
use crate::beacon_client::types::SignedBeaconBlock;
use crate::chain::{self, epoch_start_slot};
use crate::db;
use crate::db::Pool as PgPool;
use crate::error::{Error, Result};
use futures::future::join_all;

/// Probe the beacon node once for the epoch's sync `participant_reward`
/// magnitude. Walks `slot_blocks` until it finds a block-present slot
/// whose parent state is available, asks the rewards endpoint for that
/// slot with our tracked committee members, and divides a validator total
/// by its net participating positions to recover the per-position reward.
///
/// The participant reward is a function of `total_active_balance`,
/// `SLOTS_PER_EPOCH`, and `SYNC_COMMITTEE_SIZE` only — it's uniform
/// across every member at every slot in the epoch. So one fetch beats
/// fanning out across all 32 slots.
///
/// Returns `Ok(None)` when every block-present slot failed (no block to
/// probe, or every parent state was pruned). The caller writes
/// participation rows with `reward = NULL` when that happens.
async fn probe_sync_participant_reward(
    client: &BeaconClient,
    slot_blocks: &[(u64, Option<SignedBeaconBlock>)],
    relevant: &[u64],
    committee: &[u64],
) -> Result<Option<i64>> {
    for (slot, block) in slot_blocks {
        let Some(aggregate) = block.as_ref().and_then(|b| b.sync_aggregate()) else {
            continue;
        };
        let bits = decode_sync_committee_bits(&aggregate.sync_committee_bits)?;
        let outcomes = sync_outcomes(committee, &bits);
        let rewards = match client.get_sync_committee_rewards(*slot, relevant).await {
            Ok(r) => r,
            Err(Error::BeaconApi { status: 404, .. }) => {
                tracing::debug!(
                    slot,
                    "Sync rewards probe: parent state pruned, trying next slot"
                );
                continue;
            }
            Err(e) => return Err(e),
        };
        for reward in rewards {
            let Some(&(_, net_positions)) = outcomes.get(&reward.validator_index) else {
                continue;
            };
            if let Some(magnitude) = per_position_reward(reward.reward, net_positions)? {
                return Ok(Some(magnitude));
            }
        }
    }
    Ok(None)
}

/// Zero net positions cancel and cannot reveal the per-position amount.
fn per_position_reward(total: i64, net_positions: i64) -> Result<Option<i64>> {
    if net_positions == 0 {
        return Ok(None);
    }
    if total % net_positions != 0 || total / net_positions < 0 {
        return Err(Error::InconsistentBeaconData(
            "sync reward disagrees with participation".into(),
        ));
    }
    Ok(Some(total / net_positions))
}

/// One outcome per validator; a validator can occupy multiple positions.
/// Participation follows the live view (any signature); rewards sum every bit.
fn sync_outcomes(committee: &[u64], bits: &[bool]) -> HashMap<u64, (bool, i64)> {
    let mut outcomes = HashMap::new();
    for (&validator, &bit) in committee.iter().zip(bits) {
        let entry = outcomes.entry(validator).or_insert((false, 0));
        entry.0 |= bit;
        entry.1 += if bit { 1 } else { -1 };
    }
    outcomes
}

/// Decode `sync_committee_bits` from hex into a fixed-size bitvector.
///
/// A short bitvector would silently mark later committee members as
/// non-participating, so any length other than the spec-defined
/// `SYNC_COMMITTEE_SIZE` is rejected.
pub(crate) fn decode_sync_committee_bits(hex_str: &str) -> Result<Vec<bool>> {
    let expected = chain::sync_committee_size() as usize;
    let bits = super::bits::decode_bitvector(hex_str)?;
    if bits.len() != expected {
        return Err(Error::InconsistentBeaconData(format!(
            "sync_committee_bits: got {} bits, expected {expected}",
            bits.len(),
        )));
    }
    Ok(bits)
}

/// Process sync committee participation for an epoch's blocks.
pub async fn process_epoch_sync(
    client: &BeaconClient,
    pool: &PgPool,
    epoch: u64,
    tracked_validators: &HashSet<u64>,
    finalized: bool,
) -> Result<()> {
    let scan_validator_indices: Vec<u64> = tracked_validators.iter().copied().collect();
    let duties = client
        .get_sync_duties(epoch, &scan_validator_indices)
        .await?;
    let sync_validator_set: HashSet<u64> = duties.iter().map(|d| d.validator_index).collect();

    tracing::debug!(
        epoch,
        in_sync_committee = sync_validator_set.len(),
        "Tracked validators in sync committee"
    );

    if sync_validator_set.is_empty() {
        tracing::trace!(epoch, "No tracked validators in sync committee this epoch");
        return Ok(());
    }

    let sc = client.get_sync_committee(epoch).await?;
    let sync_committee_validators: Vec<u64> = sc.validators.iter().map(|v| v.0).collect();
    let expected_sync_size = chain::sync_committee_size() as usize;
    if sync_committee_validators.len() != expected_sync_size {
        return Err(Error::InconsistentBeaconData(format!(
            "sync committee for epoch {epoch} has {} validators, expected {expected_sync_size}",
            sync_committee_validators.len(),
        )));
    }

    let relevant: Vec<u64> = sync_validator_set
        .iter()
        .filter(|v| tracked_validators.contains(v))
        .copied()
        .collect();

    if relevant.is_empty() {
        tracing::trace!("No relevant validators for sync committee processing");
        return Ok(());
    }

    let relevant_set: HashSet<u64> = relevant.iter().copied().collect();

    tracing::debug!(
        epoch,
        relevant_count = relevant.len(),
        "Processing sync committee participation"
    );

    let start_slot = epoch_start_slot(epoch);
    let slots: Vec<u64> = (start_slot..start_slot + chain::slots_per_epoch()).collect();
    let blocks = join_all(
        slots
            .iter()
            .copied()
            .map(|slot| async move { (slot, client.get_block(slot).await.map(|(b, _)| b)) }),
    )
    .await;

    let mut slot_blocks: Vec<(u64, Option<SignedBeaconBlock>)> = Vec::with_capacity(blocks.len());
    for (slot, block_res) in blocks {
        slot_blocks.push((slot, block_res?));
    }

    let has_blocks = slot_blocks.iter().any(|(_, b)| b.is_some());
    let magnitude =
        probe_sync_participant_reward(client, &slot_blocks, &relevant, &sync_committee_validators)
            .await?;

    for (slot, block_opt) in slot_blocks {
        match block_opt {
            Some(block) => {
                if let Some(sync_agg) = block.sync_aggregate() {
                    let bits = decode_sync_committee_bits(&sync_agg.sync_committee_bits)?;
                    // The function above validates length; re-assert so the
                    // `bits[pos]` indexing below is provably in-bounds.
                    debug_assert_eq!(bits.len(), expected_sync_size);
                    let total_participating = bits.iter().filter(|&&b| b).count();

                    tracing::trace!(
                        slot,
                        total_participating,
                        total_committee = expected_sync_size,
                        "Sync aggregate in block"
                    );

                    for (validator_index, (participated, net_positions)) in
                        sync_outcomes(&sync_committee_validators, &bits)
                    {
                        if !relevant_set.contains(&validator_index) {
                            continue;
                        }
                        let reward = magnitude.map(|m| m * net_positions);

                        tracing::trace!(
                            slot,
                            validator = validator_index,
                            net_positions,
                            participated,
                            reward,
                            "Sync committee participation"
                        );

                        crate::metrics::SCANNER_SYNC_PARTICIPATION
                            .with_label_values(&[if participated {
                                "participated"
                            } else {
                                "missed"
                            }])
                            .inc();
                        db::scanner::sync::upsert_sync_duty(
                            pool,
                            validator_index as i64,
                            slot as i64,
                            participated,
                            reward,
                            false,
                            finalized,
                        )
                        .await?;
                    }
                }
            }
            None => {
                tracing::trace!(
                    slot,
                    missed_validators = relevant.len(),
                    "Missed slot — all sync committee members missed"
                );
                crate::metrics::SCANNER_SYNC_PARTICIPATION
                    .with_label_values(&["missed_block"])
                    .inc_by(relevant.len() as u64);
                for &validator_index in &relevant {
                    db::scanner::sync::upsert_sync_duty(
                        pool,
                        validator_index as i64,
                        slot as i64,
                        false,
                        Some(0),
                        true,
                        finalized,
                    )
                    .await?;
                }
            }
        }
    }

    if magnitude.is_none() && has_blocks {
        return Err(Error::BeaconApi {
            status: 404,
            message: format!(
                "sync rewards unavailable for epoch {epoch}; participation saved, scan incomplete"
            ),
        });
    }
    tracing::debug!("Sync committee processing complete");
    Ok(())
}

/// Upsert sync_duties rows for tracked validators that sit in the current
/// period's sync committee. `tracked_positions` maps each tracked committee
/// member to its position(s) inside the 512-slot sync committee.
///
/// - Block present with sync_aggregate: participated = any tracked position set.
/// - Block present without sync_aggregate (pre-Altair): no-op.
/// - Missed slot (block=None): participated=false, missed_block=true.
///
/// Rewards are joined independently by the recent-block reward worker.
pub async fn upsert_live_sync_in_slot(
    pool: &PgPool,
    slot: u64,
    block: Option<&SignedBeaconBlock>,
    tracked_positions: &HashMap<u64, Vec<u64>>,
) -> Result<()> {
    if tracked_positions.is_empty() {
        return Ok(());
    }

    match block {
        Some(block) => {
            let Some(sync_agg) = block.sync_aggregate() else {
                return Ok(());
            };
            let bits = decode_sync_committee_bits(&sync_agg.sync_committee_bits)?;
            for (&validator_index, positions) in tracked_positions {
                for &p in positions {
                    if (p as usize) >= bits.len() {
                        return Err(Error::InconsistentBeaconData(format!(
                            "tracked sync-committee position {p} for validator {validator_index} \
                             is out of range (bits.len()={})",
                            bits.len(),
                        )));
                    }
                }
                let participated = positions.iter().any(|&p| bits[p as usize]);
                db::scanner::sync::upsert_sync_duty(
                    pool,
                    validator_index as i64,
                    slot as i64,
                    participated,
                    None,
                    false,
                    false,
                )
                .await?;
            }
        }
        None => {
            for &validator_index in tracked_positions.keys() {
                db::scanner::sync::upsert_sync_duty(
                    pool,
                    validator_index as i64,
                    slot as i64,
                    false,
                    Some(0),
                    true,
                    false,
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// Fetch validator totals independently of proposal/attestation reward requests.
/// These totals already include every position occupied by repeated members.
pub async fn fetch_live_sync_rewards(
    client: &BeaconClient,
    root: &crate::beacon_client::types::BlockRoot,
    members: &[u64],
) -> Result<
    crate::beacon_client::types::BeaconResponse<
        Vec<crate::beacon_client::types::SyncCommitteeReward>,
    >,
> {
    if members.is_empty() {
        return Err(Error::InconsistentBeaconData(
            "refusing an unfiltered live sync reward request".into(),
        ));
    }
    let response = client
        .get_sync_committee_rewards_by_root(root, members)
        .await?;
    super::validate_reward_indices(
        &members.iter().copied().collect(),
        response.data.iter().map(|r| r.validator_index),
    )?;
    Ok(response)
}

/// Join validated totals to participation already decoded from the same root.
/// The live worker stages the response so a missing participation row can be
/// joined later without refetching the block's historical pre-state.
pub async fn persist_live_sync_rewards(
    pool: &PgPool,
    slot: u64,
    rewards: &[crate::beacon_client::types::SyncCommitteeReward],
) -> Result<()> {
    let indices: Vec<i64> = rewards.iter().map(|r| r.validator_index as i64).collect();
    let values: Vec<i64> = rewards.iter().map(|r| r.reward).collect();
    sqlx::query(
        "UPDATE sync_duties AS duty SET reward = reward_data.reward \
         FROM UNNEST($1::BIGINT[], $2::BIGINT[]) AS reward_data(validator_index, reward) \
         WHERE duty.validator_index = reward_data.validator_index AND duty.slot = $3 \
         AND duty.finalized = FALSE",
    )
    .bind(indices)
    .bind(values)
    .bind(slot as i64)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_normalizes_validator_totals_before_reusing_the_reward() {
        assert_eq!(per_position_reward(20, 2).unwrap(), Some(10));
        assert_eq!(per_position_reward(-20, -2).unwrap(), Some(10));
        assert_eq!(per_position_reward(0, 0).unwrap(), None);
        assert!(per_position_reward(21, 2).is_err());
        assert!(per_position_reward(-20, 2).is_err());
    }

    #[test]
    fn repeated_members_sum_rewards_and_preserve_any_participation() {
        let outcomes = sync_outcomes(&[1, 2, 1, 3, 2], &[true, false, true, false, true]);
        assert_eq!(outcomes[&1], (true, 2));
        assert_eq!(outcomes[&2], (true, 0));
        assert_eq!(outcomes[&3], (false, -1));
    }

    #[test]
    fn sync_committee_bits_lsb_first() {
        let hex = format!("0x01{}", "00".repeat(63));
        let bits = decode_sync_committee_bits(&hex).unwrap();
        assert_eq!(bits.len(), chain::sync_committee_size() as usize);
        assert!(bits[0]);
        assert!(bits[1..].iter().all(|&b| !b));
    }

    #[test]
    fn sync_committee_bits_all_ones() {
        let hex = format!("0x{}", "ff".repeat(64));
        let bits = decode_sync_committee_bits(&hex).unwrap();
        assert_eq!(bits.len(), chain::sync_committee_size() as usize);
        assert!(bits.iter().all(|&b| b));
    }

    #[test]
    fn sync_committee_bits_byte_order_preserved() {
        let mut bytes = [0u8; 64];
        bytes[1] = 0b1000_0001;
        let hex = format!("0x{}", hex::encode(bytes));
        let bits = decode_sync_committee_bits(&hex).unwrap();
        assert!(bits[8]);
        assert!(bits[15]);
        for (i, &bit) in bits.iter().enumerate() {
            if i != 8 && i != 15 {
                assert!(!bit, "unexpected set bit at {i}");
            }
        }
    }

    #[test]
    fn sync_committee_bits_malformed_hex_errors() {
        assert!(matches!(
            decode_sync_committee_bits("0xzz"),
            Err(Error::InconsistentBeaconData(_))
        ));
    }

    #[test]
    fn sync_committee_bits_wrong_length_errors() {
        let hex = format!("0x{}", "ff".repeat(32));
        assert!(matches!(
            decode_sync_committee_bits(&hex),
            Err(Error::InconsistentBeaconData(_))
        ));
    }
}
