//! Read queries for the `/api/sync_duties` endpoint.

use sqlx::Row;

use crate::db::Pool;
use crate::error::Result;

#[derive(Default)]
pub struct SyncFilter {
    pub validator_indices: Option<Vec<i64>>,
    pub slot_from: Option<i64>,
    pub slot_to: Option<i64>,
    pub participated: Option<bool>,
    pub missed_block: Option<bool>,
    pub finalized: Option<bool>,
}

pub enum SyncSort {
    ValidatorIndex,
    Slot,
    Reward,
}

impl SyncSort {
    pub fn parse(s: &str) -> Self {
        match s {
            "validator_index" => Self::ValidatorIndex,
            "reward" => Self::Reward,
            _ => Self::Slot,
        }
    }

    fn column(&self) -> &'static str {
        match self {
            Self::ValidatorIndex => "validator_index",
            Self::Slot => "slot",
            Self::Reward => "reward",
        }
    }
}

pub enum SortOrder {
    Asc,
    Desc,
}

impl SortOrder {
    pub fn parse(s: &str) -> Self {
        if s == "asc" { Self::Asc } else { Self::Desc }
    }

    fn sql(&self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

pub struct SyncDutyRow {
    pub validator_index: i64,
    pub slot: i64,
    pub participated: bool,
    pub reward: Option<i64>,
    pub missed_block: bool,
    pub finalized: bool,
}

pub async fn list_sync_duties_paginated(
    pool: &Pool,
    filter: &SyncFilter,
    sort: SyncSort,
    order: SortOrder,
    limit: i64,
    offset: i64,
) -> Result<(Vec<SyncDutyRow>, i64)> {
    let where_clause = build_where(filter);

    let count_sql = format!("SELECT COUNT(*) FROM sync_duties {where_clause}");
    let data_sql = format!(
        "SELECT * FROM sync_duties {where_clause} \
         ORDER BY {} {} LIMIT {} OFFSET {}",
        sort.column(),
        order.sql(),
        limit,
        offset
    );

    macro_rules! bind_sync_filter {
        ($q:expr, $f:expr) => {{
            let mut q = $q;
            if let Some(ref vi) = $f.validator_indices {
                q = q.bind(vi);
            }
            if let Some(v) = $f.slot_from {
                q = q.bind(v);
            }
            if let Some(v) = $f.slot_to {
                q = q.bind(v);
            }
            if let Some(v) = $f.participated {
                q = q.bind(v);
            }
            if let Some(v) = $f.missed_block {
                q = q.bind(v);
            }
            if let Some(v) = $f.finalized {
                q = q.bind(v);
            }
            q
        }};
    }

    let count_query = bind_sync_filter!(sqlx::query_scalar::<_, Option<i64>>(&count_sql), filter);
    let data_query = bind_sync_filter!(sqlx::query(&data_sql), filter);

    let total = count_query.fetch_one(pool).await?.unwrap_or(0);
    let rows = data_query.fetch_all(pool).await?;

    let data = rows
        .iter()
        .map(|r| SyncDutyRow {
            validator_index: r.get("validator_index"),
            slot: r.get("slot"),
            participated: r.get("participated"),
            reward: r.get("reward"),
            missed_block: r.get("missed_block"),
            finalized: r.get("finalized"),
        })
        .collect();

    Ok((data, total))
}

fn build_where(f: &SyncFilter) -> String {
    let mut conds = Vec::new();
    let mut idx = 0u32;
    let mut next = || {
        idx += 1;
        idx
    };

    if f.validator_indices.is_some() {
        conds.push(format!("validator_index = ANY(${})", next()));
    }
    if f.slot_from.is_some() {
        conds.push(format!("slot >= ${}", next()));
    }
    if f.slot_to.is_some() {
        conds.push(format!("slot <= ${}", next()));
    }
    if f.participated.is_some() {
        conds.push(format!("participated = ${}", next()));
    }
    if f.missed_block.is_some() {
        conds.push(format!("missed_block = ${}", next()));
    }
    if f.finalized.is_some() {
        conds.push(format!("finalized = ${}", next()));
    }

    if conds.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conds.join(" AND "))
    }
}
