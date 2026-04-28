//! Read queries for the `/api/epochs` endpoint (per-epoch aggregate summary).

use sqlx::Row;

use crate::chain;
use crate::db::Pool;
use crate::error::Result;

#[derive(Default)]
pub struct EpochFilter {
    pub validator_indices: Option<Vec<i64>>,
    pub epoch_from: Option<i64>,
    pub epoch_to: Option<i64>,
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

pub struct EpochSummaryRow {
    pub epoch: i64,
    pub total_duties: i64,
    pub included: i64,
    pub missed: i64,
    pub head_correct: i64,
    pub target_correct: i64,
    pub source_correct: i64,
    pub total_reward: i64,
    pub sync_participated: i64,
    pub sync_missed: i64,
    pub proposals: i64,
    pub proposals_missed: i64,
}

pub async fn list_epoch_summaries_paginated(
    pool: &Pool,
    filter: &EpochFilter,
    order: SortOrder,
    limit: i64,
    offset: i64,
) -> Result<(Vec<EpochSummaryRow>, i64)> {
    // Three separate WHERE clauses because sync / proposals filter by
    // validator_index / proposer_index while attestations use a.validator_index.
    let att_where = build_att_where(filter);
    let sync_where = if filter.validator_indices.is_some() {
        "WHERE validator_index = ANY($1)".to_string()
    } else {
        String::new()
    };
    let proposal_where = if filter.validator_indices.is_some() {
        "WHERE proposer_index = ANY($1)".to_string()
    } else {
        String::new()
    };

    let count_sql = format!("SELECT COUNT(DISTINCT a.epoch) FROM attestation_duties a {att_where}");

    let data_sql = format!(
        r#"
        SELECT
            att.epoch,
            att.total_duties,
            att.included,
            att.missed,
            att.head_correct,
            att.target_correct,
            att.source_correct,
            att.total_reward,
            COALESCE(sy.participated, 0) as sync_participated,
            COALESCE(sy.missed, 0) as sync_missed,
            COALESCE(pr.proposed, 0) as proposals,
            COALESCE(pr.missed, 0) as proposals_missed
        FROM (
            SELECT
                a.epoch,
                COUNT(*) as total_duties,
                COUNT(*) FILTER (WHERE a.included) as included,
                COUNT(*) FILTER (WHERE NOT a.included) as missed,
                COUNT(*) FILTER (WHERE a.head_correct AND a.included) as head_correct,
                COUNT(*) FILTER (WHERE a.target_correct AND a.included) as target_correct,
                COUNT(*) FILTER (WHERE a.source_correct AND a.included) as source_correct,
                COALESCE(SUM(COALESCE(a.source_reward,0)+COALESCE(a.target_reward,0)+COALESCE(a.head_reward,0)+COALESCE(a.inactivity_penalty,0)),0)::BIGINT as total_reward
            FROM attestation_duties a
            {att_where}
            GROUP BY a.epoch
        ) att
        LEFT JOIN (
            SELECT slot / {spe} as epoch,
                COUNT(*) FILTER (WHERE participated) as participated,
                COUNT(*) FILTER (WHERE NOT participated) as missed
            FROM sync_duties
            {sync_where}
            GROUP BY slot / {spe}
        ) sy ON sy.epoch = att.epoch
        LEFT JOIN (
            SELECT slot / {spe} as epoch,
                COUNT(*) FILTER (WHERE proposed) as proposed,
                COUNT(*) FILTER (WHERE NOT proposed) as missed
            FROM block_proposals
            {proposal_where}
            GROUP BY slot / {spe}
        ) pr ON pr.epoch = att.epoch
        ORDER BY att.epoch {order_sql}
        LIMIT {limit} OFFSET {offset}
        "#,
        order_sql = order.sql(),
        spe = chain::slots_per_epoch(),
    );

    // attestation-side bindings; sync/proposal subqueries reuse $1 when
    // validator_indices is set.
    macro_rules! bind_epoch_att {
        ($q:expr, $f:expr) => {{
            let mut q = $q;
            if let Some(ref vi) = $f.validator_indices {
                q = q.bind(vi);
            }
            if let Some(v) = $f.epoch_from {
                q = q.bind(v);
            }
            if let Some(v) = $f.epoch_to {
                q = q.bind(v);
            }
            q
        }};
    }

    let count_query = bind_epoch_att!(sqlx::query_scalar::<_, Option<i64>>(&count_sql), filter);
    let data_query = bind_epoch_att!(sqlx::query(&data_sql), filter);

    let total = count_query.fetch_one(pool).await?.unwrap_or(0);
    let rows = data_query.fetch_all(pool).await?;

    let data = rows
        .iter()
        .map(|r| EpochSummaryRow {
            epoch: r.get("epoch"),
            total_duties: r.get("total_duties"),
            included: r.get("included"),
            missed: r.get("missed"),
            head_correct: r.get("head_correct"),
            target_correct: r.get("target_correct"),
            source_correct: r.get("source_correct"),
            total_reward: r.get("total_reward"),
            sync_participated: r.get("sync_participated"),
            sync_missed: r.get("sync_missed"),
            proposals: r.get("proposals"),
            proposals_missed: r.get("proposals_missed"),
        })
        .collect();

    Ok((data, total))
}

fn build_att_where(f: &EpochFilter) -> String {
    let mut conds = Vec::new();
    let mut idx = 0u32;
    let mut next = || {
        idx += 1;
        idx
    };

    if f.validator_indices.is_some() {
        conds.push(format!("a.validator_index = ANY(${})", next()));
    }
    if f.epoch_from.is_some() {
        conds.push(format!("a.epoch >= ${}", next()));
    }
    if f.epoch_to.is_some() {
        conds.push(format!("a.epoch <= ${}", next()));
    }

    if conds.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conds.join(" AND "))
    }
}
