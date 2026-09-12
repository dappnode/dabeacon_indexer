//! Attestation-duty writes + the non-contiguous-backfill coverage query.

use std::collections::HashSet;

use sqlx::Row;

use crate::db::Pool;
use crate::error::Result;

/// (validator_index, epoch, source_reward, target_reward, head_reward, inactivity_penalty)
pub type RewardTuple = (i64, i64, Option<i64>, Option<i64>, Option<i64>, Option<i64>);

pub struct InclusionDetails {
    pub slot: i64,
    pub delay: i32,
    pub effective_delay: i32,
    pub source_correct: bool,
    pub target_correct: bool,
    pub head_correct: bool,
}

/// Called only after proving the inclusion block belongs to finalized ancestry.
/// Repair derived fields without replacing rewards or accepting a later duplicate.
pub async fn repair_finalized_inclusion(
    pool: &Pool,
    validator: i64,
    epoch: i64,
    details: &InclusionDetails,
) -> Result<()> {
    sqlx::query(
        "UPDATE attestation_duties SET inclusion_slot=$3,inclusion_delay=$4,
         effective_inclusion_delay=$5,source_correct=$6,target_correct=$7,head_correct=$8
         WHERE validator_index=$1 AND epoch=$2 AND included AND inclusion_slot >= $3
           AND (NOT EXISTS (SELECT 1 FROM completed_scans c
               WHERE c.validator_index=$1 AND c.epoch=$2)
               OR (inclusion_slot=$3 AND (effective_inclusion_delay IS NULL
                   OR source_correct IS NULL OR target_correct IS NULL OR head_correct IS NULL)))",
    )
    .bind(validator)
    .bind(epoch)
    .bind(details.slot)
    .bind(details.delay)
    .bind(details.effective_delay)
    .bind(details.source_correct)
    .bind(details.target_correct)
    .bind(details.head_correct)
    .execute(pool)
    .await?;
    Ok(())
}

/// Batch-update reward columns on non-finalized attestation_duties rows.
/// Used by the epoch-transition eager reward fetch to fill in rewards on rows
/// the head tracker already wrote (whose `ON CONFLICT` upsert would reject a
/// full row write due to the `inclusion_slot` guard).
///
/// The caller verifies chain dependencies first. Valid replacements may change
/// any reward, including nonzero to zero following a reorg; final rows are frozen.
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
            inactivity_penalty = v.inactivity_penalty,
            source_correct = COALESCE(ad.source_correct, CASE WHEN v.source_reward > 0 THEN TRUE END),
            target_correct = COALESCE(ad.target_correct, CASE WHEN v.target_reward > 0 THEN TRUE END),
            head_correct = COALESCE(ad.head_correct, CASE WHEN v.head_reward > 0 THEN TRUE END)
        FROM UNNEST($1::BIGINT[], $2::BIGINT[], $3::BIGINT[], $4::BIGINT[], $5::BIGINT[], $6::BIGINT[])
            AS v(validator_index, epoch, source_reward, target_reward, head_reward, inactivity_penalty)
        WHERE ad.validator_index = v.validator_index
          AND ad.epoch = v.epoch
          AND ad.finalized = FALSE
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

/// Live writes cannot replace finalized rows. An authoritative finalized scan
/// may replace rows until all scan stages have completed for this validator.
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
            assigned_slot = EXCLUDED.assigned_slot,
            committee_index = EXCLUDED.committee_index,
            committee_position = EXCLUDED.committee_position,
            included = EXCLUDED.included,
            inclusion_slot = EXCLUDED.inclusion_slot,
            inclusion_delay = EXCLUDED.inclusion_delay,
            effective_inclusion_delay = EXCLUDED.effective_inclusion_delay,
            source_correct = COALESCE(EXCLUDED.source_correct, attestation_duties.source_correct),
            target_correct = COALESCE(EXCLUDED.target_correct, attestation_duties.target_correct),
            head_correct = COALESCE(EXCLUDED.head_correct, attestation_duties.head_correct),
            source_reward = COALESCE(EXCLUDED.source_reward, attestation_duties.source_reward),
            target_reward = COALESCE(EXCLUDED.target_reward, attestation_duties.target_reward),
            head_reward = COALESCE(EXCLUDED.head_reward, attestation_duties.head_reward),
            inactivity_penalty = COALESCE(EXCLUDED.inactivity_penalty, attestation_duties.inactivity_penalty),
            finalized = EXCLUDED.finalized
        WHERE
          -- Path 1: non-finalized row can be updated by finalized writes
          -- (always win) or live writes with better inclusion data.
          (attestation_duties.finalized = FALSE
           AND (
             EXCLUDED.finalized = TRUE
             OR attestation_duties.inclusion_slot IS NULL
             OR EXCLUDED.inclusion_slot < attestation_duties.inclusion_slot
             OR (EXCLUDED.inclusion_slot = attestation_duties.inclusion_slot
                 AND EXCLUDED.effective_inclusion_delay IS NOT NULL)
           ))
          -- Path 2: an incomplete finalized scan can be repaired.
          -- Only finalized writes (archive backfill) are allowed here.
          OR (EXCLUDED.finalized = TRUE
              AND NOT EXISTS (SELECT 1 FROM completed_scans c
                  WHERE c.validator_index = attestation_duties.validator_index
                    AND c.epoch = attestation_duties.epoch))
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

/// Return the subset of `validator_indices` that already have a completed
/// scan for `epoch`. Used by non-contiguous backfill to skip
/// `(validator, epoch)` pairs that are already covered.
pub async fn validators_with_completed_scan(
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
        FROM completed_scans
        WHERE validator_index = ANY($1) AND epoch = $2
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
        JOIN completed_scans AS ad
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

#[cfg(test)]
mod live_join_tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires a disposable RECOVERY_TEST_DATABASE_URL"]
    async fn finalized_detail_repair_preserves_rewards_and_earliest_inclusion() {
        let pool = crate::db::isolated_test_pool().await;
        let validator = 99102;
        let epoch = 20;
        super::super::validators::upsert_validator(&pool, validator, &[10], 0, None)
            .await
            .unwrap();
        write(&pool, validator, epoch, Some(644)).await;
        update_attestation_rewards_batch(
            &pool,
            &[(validator, epoch, Some(10), Some(20), Some(0), Some(0))],
        )
        .await
        .unwrap();
        sqlx::query("UPDATE attestation_duties SET finalized=TRUE")
            .execute(&pool)
            .await
            .unwrap();
        let mut details = InclusionDetails {
            slot: 643,
            delay: 2,
            effective_delay: 1,
            source_correct: true,
            target_correct: true,
            head_correct: true,
        };
        repair_finalized_inclusion(&pool, validator, epoch, &details)
            .await
            .unwrap();
        details.slot = 645;
        details.head_correct = false;
        repair_finalized_inclusion(&pool, validator, epoch, &details)
            .await
            .unwrap();
        let row: (i64, i32, bool, i64, bool) = sqlx::query_as(
            "SELECT inclusion_slot,effective_inclusion_delay,head_correct,head_reward,finalized FROM attestation_duties"
        ).fetch_one(&pool).await.unwrap();
        assert_eq!(row, (643, 1, true, 0, true));
        pool.close().await;
    }

    #[tokio::test]
    #[ignore = "requires a disposable RECOVERY_TEST_DATABASE_URL"]
    async fn processed_list_omits_seeds_and_keeps_confirmed_misses() {
        use crate::db::api::attestations::{
            AttestationFilter, AttestationSort, SortOrder, list_attestation_duties_paginated,
        };
        let pool = crate::db::isolated_test_pool().await;
        super::super::validators::upsert_validator(&pool, 99103, &[11], 0, None)
            .await
            .unwrap();
        write(&pool, 99103, 20, None).await;
        write(&pool, 99103, 19, Some(610)).await;
        write(&pool, 99103, 18, None).await;
        sqlx::query("UPDATE attestation_duties SET inclusion_known=TRUE WHERE epoch=18")
            .execute(&pool)
            .await
            .unwrap();
        let (rows, total) = list_attestation_duties_paginated(
            &pool,
            &AttestationFilter {
                processed_only: true,
                ..Default::default()
            },
            AttestationSort::Epoch,
            SortOrder::Desc,
            50,
            0,
        )
        .await
        .unwrap();
        assert_eq!(total, 2);
        assert_eq!(
            rows.iter()
                .map(|r| (r.epoch, r.included))
                .collect::<Vec<_>>(),
            vec![(19, Some(true)), (18, Some(false))]
        );
        pool.close().await;
    }

    #[tokio::test]
    #[ignore = "requires a disposable RECOVERY_TEST_DATABASE_URL"]
    async fn pending_seed_and_later_inclusion_preserve_collected_rewards() {
        let pool = crate::db::isolated_test_pool().await;
        let validator = 99101;
        let epoch = 9001;
        super::super::validators::upsert_validator(&pool, validator, &[99, 101], 0, None)
            .await
            .unwrap();
        sqlx::query("DELETE FROM attestation_duties WHERE validator_index=$1")
            .bind(validator)
            .execute(&pool)
            .await
            .unwrap();
        // Seed, collect rewards, then repeat the seed before any inclusion.
        write(&pool, validator, epoch, None).await;
        update_attestation_rewards_batch(
            &pool,
            &[(validator, epoch, Some(10), Some(20), Some(5), Some(0))],
        )
        .await
        .unwrap();
        write(&pool, validator, epoch, None).await;
        assert_row(&pool, validator, false, None, 10).await;
        // A later inclusion and a repeated pending seed cannot erase the evidence.
        write(&pool, validator, epoch, Some(epoch * 32 + 2)).await;
        write(&pool, validator, epoch, None).await;
        assert_row(&pool, validator, true, Some(epoch * 32 + 2), 10).await;
        // A chain-validated replacement can legitimately change a positive reward to zero.
        update_attestation_rewards_batch(
            &pool,
            &[(validator, epoch, Some(0), Some(0), Some(0), Some(-1))],
        )
        .await
        .unwrap();
        assert_row(&pool, validator, true, Some(epoch * 32 + 2), 0).await;
        pool.close().await;
    }

    async fn write(pool: &Pool, validator: i64, epoch: i64, inclusion: Option<i64>) {
        upsert_attestation_duty(
            pool,
            validator,
            epoch,
            epoch * 32 + 1,
            0,
            0,
            inclusion.is_some(),
            inclusion,
            inclusion.map(|_| 1),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        )
        .await
        .unwrap();
    }

    async fn assert_row(
        pool: &Pool,
        validator: i64,
        included: bool,
        inclusion: Option<i64>,
        reward: i64,
    ) {
        let row: (bool, Option<i64>, i64) = sqlx::query_as(
            "SELECT included, inclusion_slot, source_reward FROM attestation_duties WHERE validator_index=$1",
        ).bind(validator).fetch_one(pool).await.unwrap();
        assert_eq!(row, (included, inclusion, reward));
    }
}
