//! Read queries used by the live-SSE endpoint.

use std::collections::HashMap;

use sqlx::Row;

use crate::db::Pool;
use crate::error::Result;

/// `(validator, slot) -> (included, inclusion_known)`.
pub type AttestationStatusMap = HashMap<(u64, u64), (bool, bool)>;
/// `(validator, slot) -> (participated, missed_block)`.
pub type SyncStatusMap = HashMap<(u64, u64), (bool, bool)>;
/// `slot -> (proposer, proposed)`.
pub type ProposalStatusMap = HashMap<u64, (u64, bool)>;

pub async fn processed_tip(pool: &Pool, tracked: &[i64]) -> Result<Option<u64>> {
    crate::db::scanner::live::processed_tip(pool, tracked).await
}

/// Only processed ancestry can establish a skipped slot; a slot API 404 alone
/// could also mean unavailable data or a fork-choice race.
pub async fn fetch_block_presence(
    pool: &Pool,
    tracked: &[i64],
    start: u64,
    end: u64,
) -> Result<HashMap<u64, bool>> {
    let rows: Vec<(i64, bool)> = sqlx::query_as(
        "SELECT DISTINCT c.slot,b.root IS NOT NULL
        FROM live_coverage c LEFT JOIN live_blocks b ON b.slot=c.slot
        WHERE c.validator_index=ANY($1) AND c.slot >= $2 AND c.slot < $3",
    )
    .bind(tracked)
    .bind(start as i64)
    .bind(end as i64)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(slot, present)| (slot as u64, present))
        .collect())
}

pub async fn fetch_attestation_status(
    pool: &Pool,
    tracked: &[i64],
    start_epoch: u64,
    current_epoch: u64,
) -> Result<AttestationStatusMap> {
    let rows = sqlx::query(
        r#"
        SELECT validator_index, assigned_slot, included, inclusion_known
        FROM attestation_duties
        WHERE validator_index = ANY($1) AND epoch >= $2 AND epoch <= $3
        "#,
    )
    .bind(tracked)
    .bind(start_epoch as i64)
    .bind(current_epoch as i64)
    .fetch_all(pool)
    .await?;

    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        let validator: i64 = row.try_get("validator_index")?;
        let slot: i64 = row.try_get("assigned_slot")?;
        let included: bool = row.try_get("included")?;
        let inclusion_known: bool = row.try_get("inclusion_known")?;
        map.insert((slot as u64, validator as u64), (included, inclusion_known));
    }
    Ok(map)
}

pub async fn fetch_sync_status(
    pool: &Pool,
    tracked: &[i64],
    start_slot: u64,
    end_slot: u64,
) -> Result<SyncStatusMap> {
    let rows = sqlx::query(
        r#"
        SELECT validator_index, slot, participated, missed_block
        FROM sync_duties
        WHERE validator_index = ANY($1) AND slot >= $2 AND slot < $3
        "#,
    )
    .bind(tracked)
    .bind(start_slot as i64)
    .bind(end_slot as i64)
    .fetch_all(pool)
    .await?;

    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        let validator: i64 = row.try_get("validator_index")?;
        let slot: i64 = row.try_get("slot")?;
        let participated: bool = row.try_get("participated")?;
        let missed_block: bool = row.try_get("missed_block")?;
        map.insert(
            (slot as u64, validator as u64),
            (participated, missed_block),
        );
    }
    Ok(map)
}

pub async fn fetch_proposal_status(
    pool: &Pool,
    start_slot: u64,
    end_slot: u64,
) -> Result<ProposalStatusMap> {
    let rows = sqlx::query(
        r#"
        SELECT slot, proposer_index, proposed
        FROM block_proposals
        WHERE slot >= $1 AND slot < $2
        "#,
    )
    .bind(start_slot as i64)
    .bind(end_slot as i64)
    .fetch_all(pool)
    .await?;

    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        let slot: i64 = row.try_get("slot")?;
        let proposer: i64 = row.try_get("proposer_index")?;
        let proposed: bool = row.try_get("proposed")?;
        map.insert(slot as u64, (proposer as u64, proposed));
    }
    Ok(map)
}
