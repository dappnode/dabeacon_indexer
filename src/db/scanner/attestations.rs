//! Attestation-duty writes + the non-contiguous-backfill coverage query.

use std::collections::HashSet;

use sqlx::Row;

use crate::db::Pool;
use crate::error::Result;

/// (validator_index, epoch, source_reward, target_reward, head_reward, inactivity_penalty)
pub type RewardTuple = (i64, i64, Option<i64>, Option<i64>, Option<i64>, Option<i64>);

/// Batch-update reward columns on non-finalized attestation_duties rows.
/// Used by the epoch-transition eager reward fetch to fill in rewards on rows
/// the head tracker already wrote (whose `ON CONFLICT` upsert would reject a
/// full row write due to the `inclusion_slot` guard).
///
/// Updates rows where rewards are NULL (head tracker wrote inclusion but not
/// rewards) or where rewards are all zero but the new data says otherwise
/// (a stale early fetch wrote 0s before the attestation was actually included).
pub async fn update_attestation_rewards_batch(pool: &Pool, rewards: &[RewardTuple]) -> Result<()> {
    if rewards.is_empty() {
        return Ok(());
    }

    let mut validator_indices = Vec::with_capacity(rewards.len());
    let mut epochs = Vec::with_capacity(rewards.len());
    let mut source_rewards: Vec<Option<i64>> = Vec::with_capacity(rewards.len());
    let mut target_rewards: Vec<Option<i64>> = Vec::with_capacity(rewards.len());
    let mut head_rewards: Vec<Option<i64>> = Vec::with_capacity(rewards.len());
    let mut inactivity_penalties: Vec<Option<i64>> = Vec::with_capacity(rewards.len());

    for &(vi, ep, source, target, head, inactivity) in rewards {
        validator_indices.push(vi);
        epochs.push(ep);
        source_rewards.push(source);
        target_rewards.push(target);
        head_rewards.push(head);
        inactivity_penalties.push(inactivity);
    }

    sqlx::query(
        r#"
        UPDATE attestation_duties AS ad SET
            source_reward = v.source_reward,
            target_reward = v.target_reward,
            head_reward = v.head_reward,
            inactivity_penalty = v.inactivity_penalty
        FROM UNNEST($1::BIGINT[], $2::BIGINT[], $3::BIGINT[], $4::BIGINT[], $5::BIGINT[], $6::BIGINT[])
            AS v(validator_index, epoch, source_reward, target_reward, head_reward, inactivity_penalty)
        WHERE ad.validator_index = v.validator_index
          AND ad.epoch = v.epoch
          AND ad.finalized = FALSE
          AND (ad.source_reward IS NULL
               OR (ad.source_reward = 0 AND ad.target_reward = 0 AND ad.head_reward = 0
                   AND (COALESCE(v.source_reward, 0) != 0 OR COALESCE(v.target_reward, 0) != 0 OR COALESCE(v.head_reward, 0) != 0)))
        "#,
    )
    .bind(&validator_indices)
    .bind(&epochs)
    .bind(&source_rewards)
    .bind(&target_rewards)
    .bind(&head_rewards)
    .bind(&inactivity_penalties)
    .execute(pool)
    .await?;

    Ok(())
}

/// Upsert one attestation duty row. The `ON CONFLICT` clause enforces two
/// invariants:
///
/// 1. **Finalized rows are immutable** — a live head-tracker (`finalized=false`)
///    can never overwrite an archive backfiller's (`finalized=true`) output.
/// 2. **Reward backfill is allowed** — a finalized row that was promoted
///    without rewards (state was pruned when finalization fired) CAN be
///    overwritten by a subsequent finalized write (archive backfill) that
///    carries reward data. This lets `--non-contiguous-backfill` fill gaps.
///
/// See [`crate::scanner::scan_epoch`] for the wider cross-instance invariant.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_attestation_duty(
    pool: &Pool,
    validator_index: i64,
    epoch: i64,
    assigned_slot: i64,
    committee_index: i32,
    committee_position: i32,
    included: bool,
    inclusion_slot: Option<i64>,
    inclusion_delay: Option<i32>,
    effective_inclusion_delay: Option<i32>,
    source_correct: Option<bool>,
    target_correct: Option<bool>,
    head_correct: Option<bool>,
    source_reward: Option<i64>,
    target_reward: Option<i64>,
    head_reward: Option<i64>,
    inactivity_penalty: Option<i64>,
    finalized: bool,
) -> Result<()> {
    let _upsert_started_at = std::time::Instant::now();
    sqlx::query(
        r#"
        INSERT INTO attestation_duties (
            validator_index, epoch, assigned_slot, committee_index, committee_position,
            included, inclusion_slot, inclusion_delay, effective_inclusion_delay,
            source_correct, target_correct, head_correct,
            source_reward, target_reward, head_reward, inactivity_penalty,
            finalized
        ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)
        ON CONFLICT (validator_index, epoch) DO UPDATE SET
            included = EXCLUDED.included,
            inclusion_slot = EXCLUDED.inclusion_slot,
            inclusion_delay = EXCLUDED.inclusion_delay,
            effective_inclusion_delay = EXCLUDED.effective_inclusion_delay,
            source_correct = EXCLUDED.source_correct,
            target_correct = EXCLUDED.target_correct,
            head_correct = EXCLUDED.head_correct,
            source_reward = EXCLUDED.source_reward,
            target_reward = EXCLUDED.target_reward,
            head_reward = EXCLUDED.head_reward,
            inactivity_penalty = EXCLUDED.inactivity_penalty,
            finalized = EXCLUDED.finalized
        WHERE
          -- Path 1: non-finalized row can be updated by finalized writes
          -- (always win) or live writes with better inclusion data.
          (attestation_duties.finalized = FALSE
           AND (
             EXCLUDED.finalized = TRUE
             OR attestation_duties.inclusion_slot IS NULL
             OR EXCLUDED.inclusion_slot < attestation_duties.inclusion_slot
           ))
          -- Path 2: finalized row with missing rewards can be backfilled.
          -- Only finalized writes (archive backfill) are allowed here.
          OR (EXCLUDED.finalized = TRUE
              AND attestation_duties.source_reward IS NULL)
        "#,
    )
    .bind(validator_index)
    .bind(epoch)
    .bind(assigned_slot)
    .bind(committee_index)
    .bind(committee_position)
    .bind(included)
    .bind(inclusion_slot)
    .bind(inclusion_delay)
    .bind(effective_inclusion_delay)
    .bind(source_correct)
    .bind(target_correct)
    .bind(head_correct)
    .bind(source_reward)
    .bind(target_reward)
    .bind(head_reward)
    .bind(inactivity_penalty)
    .bind(finalized)
    .execute(pool)
    .await
    .inspect_err(|_e| {
        crate::metrics::DB_UPSERTS
            .with_label_values(&["attestation_duties", "error"])
            .inc();
    })?;
    crate::metrics::DB_UPSERT_DURATION
        .with_label_values(&["attestation_duties"])
        .observe(_upsert_started_at.elapsed().as_secs_f64());
    crate::metrics::DB_UPSERTS
        .with_label_values(&["attestation_duties", "ok"])
        .inc();
    Ok(())
}

/// Return the subset of `validator_indices` that already have a finalized
/// attestation_duties row for `epoch`. Used by non-contiguous backfill to skip
/// `(validator, epoch)` pairs that are already covered.
pub async fn validators_with_finalized_attestation(
    pool: &Pool,
    validator_indices: &[i64],
    epoch: i64,
) -> Result<HashSet<i64>> {
    if validator_indices.is_empty() {
        return Ok(HashSet::new());
    }
    let rows = sqlx::query(
        r#"
        SELECT validator_index
        FROM attestation_duties
        WHERE validator_index = ANY($1) AND epoch = $2 AND finalized = TRUE
        "#,
    )
    .bind(validator_indices)
    .bind(epoch)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(|r| r.get("validator_index")).collect())
}

/// Count covered `(validator_index, epoch)` pairs for the provided inclusive
/// per-validator scan ranges. Used at startup to detect data gaps without
/// collapsing coverage across different validators into the same epoch bucket.
pub async fn count_covered_validator_epochs(
    pool: &Pool,
    ranges: &[(i64, i64, i64)],
) -> Result<i64> {
    if ranges.is_empty() {
        return Ok(0);
    }

    let mut validator_indices = Vec::with_capacity(ranges.len());
    let mut from_epochs = Vec::with_capacity(ranges.len());
    let mut to_epochs = Vec::with_capacity(ranges.len());

    for &(validator_index, from_epoch, to_epoch) in ranges {
        validator_indices.push(validator_index);
        from_epochs.push(from_epoch);
        to_epochs.push(to_epoch);
    }

    let count: i64 = sqlx::query_scalar(
        r#"
        WITH requested_ranges AS (
            SELECT *
            FROM UNNEST($1::BIGINT[], $2::BIGINT[], $3::BIGINT[])
                AS r(validator_index, from_epoch, to_epoch)
        )
        SELECT COUNT(*)
        FROM requested_ranges AS r
        JOIN attestation_duties AS ad
          ON ad.validator_index = r.validator_index
         AND ad.epoch >= r.from_epoch
         AND ad.epoch <= r.to_epoch
        "#,
    )
    .bind(&validator_indices)
    .bind(&from_epochs)
    .bind(&to_epochs)
    .fetch_one(pool)
    .await?;
    Ok(count)
}
