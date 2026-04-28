//! Aggregate read queries behind `/api/stats`.

use sqlx::Row;

use crate::db::Pool;
use crate::error::Result;

/// Aggregate counters across the entire DB. Rates and ratios are computed in
/// the caller; this struct is raw counts only.
#[derive(Default)]
pub struct DbStats {
    pub total_validators: i64,
    pub total_epochs_scanned: i64,
    pub att_total: i64,
    pub att_included: i64,
    pub att_missed: i64,
    pub att_decided: i64,
    pub head_ok: i64,
    pub target_ok: i64,
    pub source_ok: i64,
    pub avg_inclusion_delay: Option<f64>,
    pub avg_effective_inclusion_delay: Option<f64>,
    pub proposals_total: i64,
    pub proposals_missed: i64,
    pub sync_participated: i64,
    pub sync_missed: i64,
    pub latest_scanned_epoch: Option<i64>,
    pub earliest_scanned_epoch: Option<i64>,
}

pub async fn fetch_stats(pool: &Pool) -> Result<DbStats> {
    let att = sqlx::query(
        r#"
        SELECT
            COUNT(*) as total,
            COUNT(*) FILTER (WHERE included = TRUE) as included,
            COUNT(*) FILTER (WHERE included = FALSE) as missed,
            COUNT(*) FILTER (WHERE included = TRUE AND source_correct IS NOT NULL) as decided,
            COUNT(*) FILTER (WHERE head_correct = TRUE) as head_ok,
            COUNT(*) FILTER (WHERE target_correct = TRUE) as target_ok,
            COUNT(*) FILTER (WHERE source_correct = TRUE) as source_ok,
            (AVG(inclusion_delay) FILTER (WHERE included = TRUE))::DOUBLE PRECISION as avg_delay,
            (AVG(effective_inclusion_delay) FILTER (WHERE included = TRUE))::DOUBLE PRECISION as avg_effective_delay,
            MIN(epoch) as min_epoch,
            MAX(epoch) as max_epoch
        FROM attestation_duties
        "#,
    )
    .fetch_one(pool)
    .await?;

    let total_validators: i64 =
        sqlx::query_scalar::<_, Option<i64>>("SELECT COUNT(*) FROM validators")
            .fetch_one(pool)
            .await?
            .unwrap_or(0);

    let proposals = sqlx::query(
        r#"
        SELECT
            COUNT(*) as total,
            COUNT(*) FILTER (WHERE proposed = FALSE) as missed
        FROM block_proposals
        "#,
    )
    .fetch_one(pool)
    .await?;

    let sync = sqlx::query(
        r#"
        SELECT
            COUNT(*) FILTER (WHERE participated = TRUE AND missed_block = FALSE) as ok,
            COUNT(*) FILTER (WHERE participated = FALSE AND missed_block = FALSE) as missed
        FROM sync_duties
        "#,
    )
    .fetch_one(pool)
    .await?;

    let total_epochs_scanned: i64 = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COUNT(DISTINCT epoch) FROM attestation_duties",
    )
    .fetch_one(pool)
    .await?
    .unwrap_or(0);

    Ok(DbStats {
        total_validators,
        total_epochs_scanned,
        att_total: att.get("total"),
        att_included: att.get("included"),
        att_missed: att.get("missed"),
        att_decided: att.get("decided"),
        head_ok: att.get("head_ok"),
        target_ok: att.get("target_ok"),
        source_ok: att.get("source_ok"),
        avg_inclusion_delay: att.get("avg_delay"),
        avg_effective_inclusion_delay: att.get("avg_effective_delay"),
        proposals_total: proposals.get("total"),
        proposals_missed: proposals.get("missed"),
        sync_participated: sync.get("ok"),
        sync_missed: sync.get("missed"),
        latest_scanned_epoch: att.get("max_epoch"),
        earliest_scanned_epoch: att.get("min_epoch"),
    })
}
