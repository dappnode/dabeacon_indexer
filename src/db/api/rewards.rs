//! Read queries for the `/api/rewards` endpoint.

use std::collections::HashMap;

use sqlx::Row;

use crate::db::Pool;
use crate::error::Result;

/// Windowed reward sums for one validator. Attestation queries also populate
/// epoch counts; sync/proposal sums leave them zero (the caller typically
/// takes epoch counts from the attestation sums only, which are the same
/// "epochs active" domain).
#[derive(Clone, Copy, Default)]
pub struct WindowSums {
    pub d1: i64,
    pub d7: i64,
    pub d30: i64,
    pub all: i64,
    pub epochs_d1: i64,
    pub epochs_d7: i64,
    pub epochs_d30: i64,
    pub epochs_all: i64,
}

pub async fn latest_scanned_epoch(pool: &Pool) -> Result<i64> {
    let v = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COALESCE(MAX(epoch), 0) FROM attestation_duties",
    )
    .fetch_one(pool)
    .await?;
    Ok(v.unwrap_or(0))
}

pub async fn list_validator_indices(pool: &Pool) -> Result<Vec<i64>> {
    let v = sqlx::query_scalar::<_, i64>(
        "SELECT validator_index FROM validators ORDER BY validator_index",
    )
    .fetch_all(pool)
    .await?;
    Ok(v)
}

pub async fn attestation_reward_windows(
    pool: &Pool,
    cutoff_1d: i64,
    cutoff_7d: i64,
    cutoff_30d: i64,
) -> Result<HashMap<i64, WindowSums>> {
    let rows = sqlx::query(
        r#"
        SELECT
            validator_index,
            COALESCE(SUM(reward_total) FILTER (WHERE epoch >= $1), 0)::BIGINT AS d1,
            COALESCE(SUM(reward_total) FILTER (WHERE epoch >= $2), 0)::BIGINT AS d7,
            COALESCE(SUM(reward_total) FILTER (WHERE epoch >= $3), 0)::BIGINT AS d30,
            COALESCE(SUM(reward_total), 0)::BIGINT AS all_sum,
            COUNT(*) FILTER (WHERE epoch >= $1) AS e1,
            COUNT(*) FILTER (WHERE epoch >= $2) AS e7,
            COUNT(*) FILTER (WHERE epoch >= $3) AS e30,
            COUNT(*) AS e_all
        FROM (
            SELECT
                validator_index,
                epoch,
                COALESCE(source_reward,0) + COALESCE(target_reward,0)
                    + COALESCE(head_reward,0) + COALESCE(inactivity_penalty,0) AS reward_total
            FROM attestation_duties
        ) a
        GROUP BY validator_index
        "#,
    )
    .bind(cutoff_1d)
    .bind(cutoff_7d)
    .bind(cutoff_30d)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| {
            (
                r.get::<i64, _>("validator_index"),
                WindowSums {
                    d1: r.get("d1"),
                    d7: r.get("d7"),
                    d30: r.get("d30"),
                    all: r.get("all_sum"),
                    epochs_d1: r.get("e1"),
                    epochs_d7: r.get("e7"),
                    epochs_d30: r.get("e30"),
                    epochs_all: r.get("e_all"),
                },
            )
        })
        .collect())
}

pub async fn sync_reward_windows(
    pool: &Pool,
    slot_1d: i64,
    slot_7d: i64,
    slot_30d: i64,
) -> Result<HashMap<i64, WindowSums>> {
    let rows = sqlx::query(
        r#"
        SELECT
            validator_index,
            COALESCE(SUM(reward) FILTER (WHERE slot >= $1), 0)::BIGINT AS d1,
            COALESCE(SUM(reward) FILTER (WHERE slot >= $2), 0)::BIGINT AS d7,
            COALESCE(SUM(reward) FILTER (WHERE slot >= $3), 0)::BIGINT AS d30,
            COALESCE(SUM(reward), 0)::BIGINT AS all_sum
        FROM sync_duties
        GROUP BY validator_index
        "#,
    )
    .bind(slot_1d)
    .bind(slot_7d)
    .bind(slot_30d)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| {
            (
                r.get::<i64, _>("validator_index"),
                WindowSums {
                    d1: r.get("d1"),
                    d7: r.get("d7"),
                    d30: r.get("d30"),
                    all: r.get("all_sum"),
                    ..Default::default()
                },
            )
        })
        .collect())
}

pub async fn proposal_reward_windows(
    pool: &Pool,
    slot_1d: i64,
    slot_7d: i64,
    slot_30d: i64,
) -> Result<HashMap<i64, WindowSums>> {
    let rows = sqlx::query(
        r#"
        SELECT
            proposer_index,
            COALESCE(SUM(reward_total) FILTER (WHERE slot >= $1), 0)::BIGINT AS d1,
            COALESCE(SUM(reward_total) FILTER (WHERE slot >= $2), 0)::BIGINT AS d7,
            COALESCE(SUM(reward_total) FILTER (WHERE slot >= $3), 0)::BIGINT AS d30,
            COALESCE(SUM(reward_total), 0)::BIGINT AS all_sum
        FROM block_proposals
        GROUP BY proposer_index
        "#,
    )
    .bind(slot_1d)
    .bind(slot_7d)
    .bind(slot_30d)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| {
            (
                r.get::<i64, _>("proposer_index"),
                WindowSums {
                    d1: r.get("d1"),
                    d7: r.get("d7"),
                    d30: r.get("d30"),
                    all: r.get("all_sum"),
                    ..Default::default()
                },
            )
        })
        .collect())
}
