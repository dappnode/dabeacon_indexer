//! Read queries for validator summaries used by the web API.

use sqlx::Row;

use crate::db::Pool;
use crate::error::Result;

/// One row of the aggregate validator summary. Raw counters; derived rates
/// (attestation_rate, head_correct_rate, …) are computed by the caller.
pub struct ValidatorSummary {
    pub validator_index: i64,
    pub pubkey_hex: String,
    pub activation_epoch: i64,
    pub exit_epoch: Option<i64>,
    pub last_scanned_epoch: Option<i64>,
    pub att_total: i64,
    pub att_included: i64,
    pub att_missed: i64,
    pub att_decided: i64,
    pub head_ok: i64,
    pub target_ok: i64,
    pub source_ok: i64,
    pub proposals_total: i64,
    pub proposals_missed: i64,
    pub sync_participated: i64,
    pub sync_missed: i64,
}

pub async fn list_validator_summaries(pool: &Pool) -> Result<Vec<ValidatorSummary>> {
    let rows = sqlx::query(
        r#"
        SELECT
            v.validator_index,
            encode(v.pubkey, 'hex') as pubkey,
            v.activation_epoch,
            v.exit_epoch,
            v.last_scanned_epoch,
            COALESCE(a.total, 0) as att_total,
            COALESCE(a.included, 0) as att_included,
            COALESCE(a.missed, 0) as att_missed,
            COALESCE(a.decided, 0) as att_decided,
            COALESCE(a.head_ok, 0) as head_ok,
            COALESCE(a.target_ok, 0) as target_ok,
            COALESCE(a.source_ok, 0) as source_ok,
            COALESCE(p.total, 0) as prop_total,
            COALESCE(p.missed, 0) as prop_missed,
            COALESCE(s.participated, 0) as sync_ok,
            COALESCE(s.missed, 0) as sync_missed
        FROM validators v
        LEFT JOIN LATERAL (
            SELECT
                COUNT(*) as total,
                COUNT(*) FILTER (WHERE included) as included,
                COUNT(*) FILTER (WHERE NOT included) as missed,
                COUNT(*) FILTER (WHERE included AND source_correct IS NOT NULL) as decided,
                COUNT(*) FILTER (WHERE head_correct) as head_ok,
                COUNT(*) FILTER (WHERE target_correct) as target_ok,
                COUNT(*) FILTER (WHERE source_correct) as source_ok
            FROM attestation_duties WHERE validator_index = v.validator_index
        ) a ON TRUE
        LEFT JOIN LATERAL (
            SELECT
                COUNT(*) as total,
                COUNT(*) FILTER (WHERE NOT proposed) as missed
            FROM block_proposals WHERE proposer_index = v.validator_index
        ) p ON TRUE
        LEFT JOIN LATERAL (
            SELECT
                COUNT(*) FILTER (WHERE participated) as participated,
                COUNT(*) FILTER (WHERE NOT participated) as missed
            FROM sync_duties WHERE validator_index = v.validator_index
        ) s ON TRUE
        ORDER BY v.validator_index
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| ValidatorSummary {
            validator_index: r.get("validator_index"),
            pubkey_hex: r.get("pubkey"),
            activation_epoch: r.get("activation_epoch"),
            exit_epoch: r.get("exit_epoch"),
            last_scanned_epoch: r.get("last_scanned_epoch"),
            att_total: r.get("att_total"),
            att_included: r.get("att_included"),
            att_missed: r.get("att_missed"),
            att_decided: r.get("att_decided"),
            head_ok: r.get("head_ok"),
            target_ok: r.get("target_ok"),
            source_ok: r.get("source_ok"),
            proposals_total: r.get("prop_total"),
            proposals_missed: r.get("prop_missed"),
            sync_participated: r.get("sync_ok"),
            sync_missed: r.get("sync_missed"),
        })
        .collect())
}
