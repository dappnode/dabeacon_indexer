//! Read queries for the `/api/attestations` endpoint.

use serde::Serialize;
use sqlx::Row;

use crate::db::Pool;
use crate::error::Result;

/// Filters for `list_attestation_duties_paginated`. All fields are optional;
/// omitted fields yield no WHERE constraint.
#[derive(Default)]
pub struct AttestationFilter {
    pub validator_indices: Option<Vec<i64>>,
    pub epoch_from: Option<i64>,
    pub epoch_to: Option<i64>,
    pub included: Option<bool>,
    pub head_correct: Option<bool>,
    pub target_correct: Option<bool>,
    pub source_correct: Option<bool>,
    pub finalized: Option<bool>,
    pub min_effective_delay: Option<i32>,
    pub max_effective_delay: Option<i32>,
}

/// Sort key for the attestation list. Unknown strings fall back to `Epoch`.
pub enum AttestationSort {
    ValidatorIndex,
    Epoch,
    AssignedSlot,
    EffectiveInclusionDelay,
    Included,
}

impl AttestationSort {
    pub fn parse(s: &str) -> Self {
        match s {
            "validator_index" => Self::ValidatorIndex,
            "assigned_slot" => Self::AssignedSlot,
            "effective_inclusion_delay" => Self::EffectiveInclusionDelay,
            "included" => Self::Included,
            _ => Self::Epoch,
        }
    }

    fn column(&self) -> &'static str {
        match self {
            Self::ValidatorIndex => "validator_index",
            Self::Epoch => "epoch",
            Self::AssignedSlot => "assigned_slot",
            Self::EffectiveInclusionDelay => "effective_inclusion_delay",
            Self::Included => "included",
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

/// Row shape matching `attestation_duties` columns plus a derived `total_reward`.
/// Also serves as the web-API response shape — see `web::api::attestations`.
#[derive(Serialize)]
pub struct AttestationDutyRow {
    pub validator_index: i64,
    pub epoch: i64,
    pub assigned_slot: i64,
    pub committee_index: i32,
    pub committee_position: i32,
    pub included: bool,
    pub inclusion_slot: Option<i64>,
    pub inclusion_delay: Option<i32>,
    pub effective_inclusion_delay: Option<i32>,
    pub source_correct: Option<bool>,
    pub target_correct: Option<bool>,
    pub head_correct: Option<bool>,
    pub source_reward: Option<i64>,
    pub target_reward: Option<i64>,
    pub head_reward: Option<i64>,
    pub inactivity_penalty: Option<i64>,
    pub total_reward: Option<i64>,
    pub finalized: bool,
}

pub async fn list_attestation_duties_paginated(
    pool: &Pool,
    filter: &AttestationFilter,
    sort: AttestationSort,
    order: SortOrder,
    limit: i64,
    offset: i64,
) -> Result<(Vec<AttestationDutyRow>, i64)> {
    let where_clause = build_where(filter);

    let count_sql = format!("SELECT COUNT(*) FROM attestation_duties {where_clause}");
    let data_sql = format!(
        "SELECT * FROM attestation_duties {where_clause} \
         ORDER BY {} {} NULLS LAST LIMIT {} OFFSET {}",
        sort.column(),
        order.sql(),
        limit,
        offset
    );

    // Both queries bind the same parameters in the same order, so
    // `bind_attestation_filter!` is reused for count and data.
    macro_rules! bind_attestation_filter {
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
            if let Some(v) = $f.included {
                q = q.bind(v);
            }
            if let Some(v) = $f.head_correct {
                q = q.bind(v);
            }
            if let Some(v) = $f.target_correct {
                q = q.bind(v);
            }
            if let Some(v) = $f.source_correct {
                q = q.bind(v);
            }
            if let Some(v) = $f.finalized {
                q = q.bind(v);
            }
            if let Some(v) = $f.min_effective_delay {
                q = q.bind(v);
            }
            if let Some(v) = $f.max_effective_delay {
                q = q.bind(v);
            }
            q
        }};
    }

    let count_query =
        bind_attestation_filter!(sqlx::query_scalar::<_, Option<i64>>(&count_sql), filter);
    let data_query = bind_attestation_filter!(sqlx::query(&data_sql), filter);

    let total: i64 = count_query.fetch_one(pool).await?.unwrap_or(0);
    let rows = data_query.fetch_all(pool).await?;

    let data = rows
        .iter()
        .map(|r| {
            let source_reward: Option<i64> = r.get("source_reward");
            let target_reward: Option<i64> = r.get("target_reward");
            let head_reward: Option<i64> = r.get("head_reward");
            let inactivity: Option<i64> = r.get("inactivity_penalty");
            let total_reward = match (source_reward, target_reward, head_reward) {
                (Some(s), Some(t), Some(h)) => Some(s + t + h + inactivity.unwrap_or(0)),
                _ => None,
            };
            AttestationDutyRow {
                validator_index: r.get("validator_index"),
                epoch: r.get("epoch"),
                assigned_slot: r.get("assigned_slot"),
                committee_index: r.get("committee_index"),
                committee_position: r.get("committee_position"),
                included: r.get("included"),
                inclusion_slot: r.get("inclusion_slot"),
                inclusion_delay: r.get("inclusion_delay"),
                effective_inclusion_delay: r.get("effective_inclusion_delay"),
                source_correct: r.get("source_correct"),
                target_correct: r.get("target_correct"),
                head_correct: r.get("head_correct"),
                source_reward,
                target_reward,
                head_reward,
                inactivity_penalty: inactivity,
                total_reward,
                finalized: r.get("finalized"),
            }
        })
        .collect();

    Ok((data, total))
}

fn build_where(f: &AttestationFilter) -> String {
    let mut conds = Vec::new();
    let mut idx = 0u32;

    let mut next = || {
        idx += 1;
        idx
    };

    if f.validator_indices.is_some() {
        conds.push(format!("validator_index = ANY(${})", next()));
    }
    if f.epoch_from.is_some() {
        conds.push(format!("epoch >= ${}", next()));
    }
    if f.epoch_to.is_some() {
        conds.push(format!("epoch <= ${}", next()));
    }
    if f.included.is_some() {
        conds.push(format!("included = ${}", next()));
    }
    if f.head_correct.is_some() {
        conds.push(format!("head_correct = ${}", next()));
    }
    if f.target_correct.is_some() {
        conds.push(format!("target_correct = ${}", next()));
    }
    if f.source_correct.is_some() {
        conds.push(format!("source_correct = ${}", next()));
    }
    if f.finalized.is_some() {
        conds.push(format!("finalized = ${}", next()));
    }
    if f.min_effective_delay.is_some() {
        conds.push(format!("effective_inclusion_delay >= ${}", next()));
    }
    if f.max_effective_delay.is_some() {
        conds.push(format!("effective_inclusion_delay <= ${}", next()));
    }

    if conds.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conds.join(" AND "))
    }
}
