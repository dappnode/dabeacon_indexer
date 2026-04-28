use std::collections::{HashMap, HashSet};

use crate::beacon_client::BeaconClient;
use crate::beacon_client::types::SignedBeaconBlock;
use crate::chain::{self, epoch_start_slot};
use crate::db;
use crate::db::Pool as PgPool;
use crate::error::{Error, Result};
use futures::future::join_all;

/// Probe the beacon node once for the epoch's sync `participant_reward`
/// magnitude. Walks `slot_blocks` until it finds a block-present slot,
/// asks the rewards endpoint for that slot with our tracked committee
/// members, and returns the first non-zero `|reward|`.
///
/// The participant reward is a function of `total_active_balance`,
/// `SLOTS_PER_EPOCH`, and `SYNC_COMMITTEE_SIZE` only — it's uniform
/// across every member at every slot in the epoch. So one fetch beats
/// fanning out across all 32 slots.
///
/// Returns `Ok(None)` when every slot in the epoch was missed (no block
/// to probe). Returns `Err` if a block-present slot returned no usable
/// (non-zero) reward — that's a degenerate state that shouldn't happen
/// on a valid Altair chain.
async fn probe_sync_participant_reward(
    client: &BeaconClient,
    slot_blocks: &[(u64, Option<SignedBeaconBlock>)],
    relevant: &[u64],
) -> Result<Option<i64>> {
    for (slot, block) in slot_blocks {
        if block.is_none() {
            continue;
        }
        let rewards = client.get_sync_committee_rewards(*slot, relevant).await?;
        if let Some(magnitude) = rewards.iter().map(|r| r.reward.abs()).find(|&m| m > 0) {
            tracing::debug!(slot = *slot, magnitude, "Probed sync participant_reward");
            return Ok(Some(magnitude));
        }
        return Err(Error::InconsistentBeaconData(format!(
            "block-present slot {slot} returned no non-zero sync rewards \
             across {} tracked committee members",
            relevant.len(),
        )));
    }
    Ok(None)
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

    let magnitude = probe_sync_participant_reward(client, &slot_blocks, &relevant).await?;

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

                    for (pos, &validator_index) in sync_committee_validators.iter().enumerate() {
                        if !relevant_set.contains(&validator_index) {
                            continue;
                        }
                        let participated = bits[pos];
                        let reward = magnitude.map(|m| if participated { m } else { -m });

                        tracing::trace!(
                            slot,
                            validator = validator_index,
                            position = pos,
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
/// Rewards are left NULL; finalization rescan fills them in.
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
                    None,
                    true,
                    false,
                )
                .await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
