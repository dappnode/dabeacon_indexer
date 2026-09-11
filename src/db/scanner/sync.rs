//! Sync-committee duty writes.

use crate::db::Pool;
use crate::error::Result;

/// Upsert one sync-committee duty row. Like [`super::attestations::upsert_attestation_duty`],
/// the `ON CONFLICT` guard prevents live writes from clobbering finalized rows,
/// but allows finalized writes to repair rows from incomplete scans.
pub async fn upsert_sync_duty(
    pool: &Pool,
    validator_index: i64,
    slot: i64,
    participated: bool,
    reward: Option<i64>,
    missed_block: bool,
    finalized: bool,
) -> Result<()> {
    let started_at = std::time::Instant::now();
    sqlx::query(
        r#"
        INSERT INTO sync_duties (validator_index, slot, participated, reward, missed_block, finalized)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (validator_index, slot) DO UPDATE SET
            participated = EXCLUDED.participated,
            reward = COALESCE(EXCLUDED.reward, sync_duties.reward),
            missed_block = EXCLUDED.missed_block,
            finalized = EXCLUDED.finalized
        WHERE sync_duties.finalized = FALSE
           OR (EXCLUDED.finalized = TRUE AND NOT EXISTS (
               SELECT 1 FROM completed_scans c
               WHERE c.validator_index = sync_duties.validator_index
                 AND c.epoch = sync_duties.slot / $7))
        "#,
    )
    .bind(validator_index)
    .bind(slot)
    .bind(participated)
    .bind(reward)
    .bind(missed_block)
    .bind(finalized)
    .bind(crate::chain::slots_per_epoch() as i64)
    .execute(pool)
    .await
    .inspect_err(|_| {
        crate::metrics::DB_UPSERTS
            .with_label_values(&["sync_duties", "error"])
            .inc();
    })?;
    crate::metrics::DB_UPSERT_DURATION
        .with_label_values(&["sync_duties"])
        .observe(started_at.elapsed().as_secs_f64());
    crate::metrics::DB_UPSERTS
        .with_label_values(&["sync_duties", "ok"])
        .inc();
    Ok(())
}
