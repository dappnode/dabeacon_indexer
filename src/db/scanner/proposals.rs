//! Block-proposal writes.

use crate::db::Pool;
use crate::error::Result;

/// Upsert one block-proposal row. Like [`super::attestations::upsert_attestation_duty`],
/// the `ON CONFLICT` `WHERE finalized = FALSE` guard is load-bearing for the
/// multi-instance story — see [`crate::scanner::scan_epoch`].
#[allow(clippy::too_many_arguments)]
pub async fn upsert_block_proposal(
    pool: &Pool,
    slot: i64,
    proposer_index: i64,
    proposed: bool,
    reward_total: Option<i64>,
    reward_attestations: Option<i64>,
    reward_sync: Option<i64>,
    reward_slashings: Option<i64>,
    finalized: bool,
) -> Result<()> {
    let started_at = std::time::Instant::now();
    sqlx::query(
        r#"
        INSERT INTO block_proposals (
            slot, proposer_index, proposed,
            reward_total, reward_attestations, reward_sync, reward_slashings,
            finalized
        ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
        ON CONFLICT (slot) DO UPDATE SET
            proposed = EXCLUDED.proposed,
            reward_total = EXCLUDED.reward_total,
            reward_attestations = EXCLUDED.reward_attestations,
            reward_sync = EXCLUDED.reward_sync,
            reward_slashings = EXCLUDED.reward_slashings,
            finalized = EXCLUDED.finalized
        WHERE block_proposals.finalized = FALSE
        "#,
    )
    .bind(slot)
    .bind(proposer_index)
    .bind(proposed)
    .bind(reward_total)
    .bind(reward_attestations)
    .bind(reward_sync)
    .bind(reward_slashings)
    .bind(finalized)
    .execute(pool)
    .await
    .inspect_err(|_| {
        crate::metrics::DB_UPSERTS
            .with_label_values(&["block_proposals", "error"])
            .inc();
    })?;
    crate::metrics::DB_UPSERT_DURATION
        .with_label_values(&["block_proposals"])
        .observe(started_at.elapsed().as_secs_f64());
    crate::metrics::DB_UPSERTS
        .with_label_values(&["block_proposals", "ok"])
        .inc();
    Ok(())
}
