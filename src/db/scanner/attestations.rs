//! Attestation-duty writes + the non-contiguous-backfill coverage query.

use std::collections::HashSet;

use sqlx::Row;

use crate::db::Pool;
use crate::error::Result;

/// Upsert one attestation duty row. The `ON CONFLICT` clause **must** keep the
/// `finalized = FALSE` guard: finalized rows are authoritative and must be
/// immutable so an archive backfiller's output can't be overwritten by a
/// concurrent live head-tracker. See [`crate::scanner::scan_epoch`] for the
/// wider invariant.
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
        WHERE attestation_duties.finalized = FALSE
          AND (
            -- Finalized writes (scan_epoch) are authoritative and always win.
            -- Live writes must not clobber an earlier inclusion_slot with a
            -- later one when the same validator is re-aggregated into another block.
            EXCLUDED.finalized = TRUE
            OR attestation_duties.inclusion_slot IS NULL
            OR EXCLUDED.inclusion_slot < attestation_duties.inclusion_slot
          )
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
