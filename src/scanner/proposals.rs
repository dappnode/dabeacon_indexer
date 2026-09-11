use std::collections::HashSet;

use crate::beacon_client::BeaconClient;
use crate::beacon_client::types::SignedBeaconBlock;
use crate::chain::{self, epoch_start_slot};
use crate::db;
use crate::db::Pool as PgPool;
use crate::error::Result;

/// Process block proposals for an epoch.
/// Only records entries for slots where a validator from `tracked_validators` was the proposer.
pub async fn process_epoch_proposals(
    client: &BeaconClient,
    pool: &PgPool,
    epoch: u64,
    tracked_validators: &HashSet<u64>,
    finalized: bool,
) -> Result<()> {
    let proposer_duties = client.get_proposer_duties(epoch).await?;

    let tracked_proposer_slots: std::collections::HashMap<u64, u64> = proposer_duties
        .iter()
        .filter(|d| tracked_validators.contains(&d.validator_index))
        .map(|d| (d.slot, d.validator_index))
        .collect();

    if tracked_proposer_slots.is_empty() {
        tracing::trace!("No tracked validators are proposers this epoch");
        return Ok(());
    }

    tracing::debug!(
        epoch,
        proposal_duties = tracked_proposer_slots.len(),
        "Tracked validators have proposal duties"
    );

    let start_slot = epoch_start_slot(epoch);
    let end_slot = start_slot + chain::slots_per_epoch();

    for slot in start_slot..end_slot {
        let Some(&proposer_index) = tracked_proposer_slots.get(&slot) else {
            continue;
        };

        let (block_opt, _is_finalized) = client.get_block(slot).await?;

        match block_opt {
            Some(_) => {
                crate::metrics::SCANNER_PROPOSALS
                    .with_label_values(&["proposed"])
                    .inc();
                let rewards = client.get_block_rewards(slot).await?;
                tracing::debug!(
                    slot,
                    validator = proposer_index,
                    total = rewards.total,
                    attestations = rewards.attestations,
                    sync_aggregate = rewards.sync_aggregate,
                    proposer_slashings = rewards.proposer_slashings,
                    attester_slashings = rewards.attester_slashings,
                    "Block proposed — rewards"
                );
                let reward_total = Some(rewards.total as i64);
                let reward_att = Some(rewards.attestations as i64);
                let reward_sync = Some(rewards.sync_aggregate as i64);
                let reward_slash =
                    Some((rewards.proposer_slashings + rewards.attester_slashings) as i64);

                db::scanner::proposals::upsert_block_proposal(
                    pool,
                    slot as i64,
                    proposer_index as i64,
                    true,
                    reward_total,
                    reward_att,
                    reward_sync,
                    reward_slash,
                    finalized,
                )
                .await?;
            }
            None => {
                crate::metrics::SCANNER_PROPOSALS
                    .with_label_values(&["missed"])
                    .inc();
                tracing::debug!(slot, validator = proposer_index, "Block proposal MISSED");

                db::scanner::proposals::upsert_block_proposal(
                    pool,
                    slot as i64,
                    proposer_index as i64,
                    false,
                    None,
                    None,
                    None,
                    None,
                    finalized,
                )
                .await?;
            }
        }
    }

    tracing::debug!("Proposal processing complete");
    Ok(())
}

/// Upsert a block_proposals row for a slot whose scheduled proposer is tracked.
/// Rewards are joined independently by the recent-block reward worker.
pub async fn upsert_live_proposal_in_slot(
    pool: &PgPool,
    slot: u64,
    proposer_index: u64,
    block: Option<&SignedBeaconBlock>,
) -> Result<()> {
    db::scanner::proposals::upsert_block_proposal(
        pool,
        slot as i64,
        proposer_index as i64,
        block.is_some(),
        None,
        None,
        None,
        None,
        false,
    )
    .await
}

/// Fetch by the root already resolved by the live chain worker, not a mutable slot.
pub async fn fetch_live_proposal_rewards(
    client: &BeaconClient,
    root: &crate::beacon_client::types::BlockRoot,
    block: &SignedBeaconBlock,
) -> Result<crate::beacon_client::types::BeaconResponse<crate::beacon_client::types::BlockRewards>>
{
    let response = client.get_block_rewards_by_root(root).await?;
    if response.data.proposer_index != block.proposer_index() {
        return Err(crate::error::Error::InconsistentBeaconData(
            "block reward proposer disagrees with resolved block".into(),
        ));
    }
    Ok(response)
}

/// Persist only after the caller verifies that the job's block remains canonical.
pub async fn persist_live_proposal_rewards(
    pool: &PgPool,
    slot: u64,
    rewards: &crate::beacon_client::types::BlockRewards,
) -> Result<()> {
    let signed = |value: u64| {
        i64::try_from(value).map_err(|_| {
            crate::error::Error::InconsistentBeaconData(
                "proposal reward exceeds database range".into(),
            )
        })
    };
    let slashings = rewards
        .proposer_slashings
        .checked_add(rewards.attester_slashings)
        .ok_or_else(|| {
            crate::error::Error::InconsistentBeaconData("proposal slashing reward overflow".into())
        })?;
    db::scanner::proposals::upsert_block_proposal(
        pool,
        slot as i64,
        rewards.proposer_index as i64,
        true,
        Some(signed(rewards.total)?),
        Some(signed(rewards.attestations)?),
        Some(signed(rewards.sync_aggregate)?),
        Some(signed(slashings)?),
        false,
    )
    .await
}
