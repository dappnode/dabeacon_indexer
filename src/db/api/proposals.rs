//! Read queries for the `/api/proposals` endpoint.

use sqlx::Row;

use crate::db::Pool;
use crate::error::Result;

#[derive(Default)]
pub struct ProposalFilter {
    pub proposer_indices: Option<Vec<i64>>,
    pub slot_from: Option<i64>,
    pub slot_to: Option<i64>,
    pub proposed: Option<bool>,
    pub finalized: Option<bool>,
}

pub enum ProposalSort {
    ProposerIndex,
    Slot,
    RewardTotal,
}

impl ProposalSort {
    pub fn parse(s: &str) -> Self {
        match s {
            "proposer_index" => Self::ProposerIndex,
            "reward_total" => Self::RewardTotal,
            _ => Self::Slot,
        }
    }

    fn column(&self) -> &'static str {
        match self {
            Self::ProposerIndex => "proposer_index",
            Self::Slot => "slot",
            Self::RewardTotal => "reward_total",
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

pub struct ProposalRow {
    pub slot: i64,
    pub proposer_index: i64,
    pub proposed: bool,
    pub reward_total: Option<i64>,
    pub reward_attestations: Option<i64>,
    pub reward_sync: Option<i64>,
    pub reward_slashings: Option<i64>,
    pub finalized: bool,
}

pub async fn list_proposals_paginated(
    pool: &Pool,
    filter: &ProposalFilter,
    sort: ProposalSort,
    order: SortOrder,
    limit: i64,
    offset: i64,
) -> Result<(Vec<ProposalRow>, i64)> {
    let where_clause = build_where(filter);

    let count_sql = format!("SELECT COUNT(*) FROM block_proposals {where_clause}");
    let data_sql = format!(
        "SELECT * FROM block_proposals {where_clause} \
         ORDER BY {} {} NULLS LAST LIMIT {} OFFSET {}",
        sort.column(),
        order.sql(),
        limit,
        offset
    );

    macro_rules! bind_proposal_filter {
        ($q:expr, $f:expr) => {{
            let mut q = $q;
            if let Some(ref vi) = $f.proposer_indices {
                q = q.bind(vi);
            }
            if let Some(v) = $f.slot_from {
                q = q.bind(v);
            }
            if let Some(v) = $f.slot_to {
                q = q.bind(v);
            }
            if let Some(v) = $f.proposed {
                q = q.bind(v);
            }
            if let Some(v) = $f.finalized {
                q = q.bind(v);
            }
            q
        }};
    }

    let count_query =
        bind_proposal_filter!(sqlx::query_scalar::<_, Option<i64>>(&count_sql), filter);
    let data_query = bind_proposal_filter!(sqlx::query(&data_sql), filter);

    let total = count_query.fetch_one(pool).await?.unwrap_or(0);
    let rows = data_query.fetch_all(pool).await?;

    let data = rows
        .iter()
        .map(|r| ProposalRow {
            slot: r.get("slot"),
            proposer_index: r.get("proposer_index"),
            proposed: r.get("proposed"),
            reward_total: r.get("reward_total"),
            reward_attestations: r.get("reward_attestations"),
            reward_sync: r.get("reward_sync"),
            reward_slashings: r.get("reward_slashings"),
            finalized: r.get("finalized"),
        })
        .collect();

    Ok((data, total))
}

fn build_where(f: &ProposalFilter) -> String {
    let mut conds = Vec::new();
    let mut idx = 0u32;
    let mut next = || {
        idx += 1;
        idx
    };

    if f.proposer_indices.is_some() {
        conds.push(format!("proposer_index = ANY(${})", next()));
    }
    if f.slot_from.is_some() {
        conds.push(format!("slot >= ${}", next()));
    }
    if f.slot_to.is_some() {
        conds.push(format!("slot <= ${}", next()));
    }
    if f.proposed.is_some() {
        conds.push(format!("proposed = ${}", next()));
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
