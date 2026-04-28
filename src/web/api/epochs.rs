use axum::{
    Json,
    extract::{Query, State},
};
use serde::{Deserialize, Serialize};

use crate::db::api::epochs as db_epochs;
use crate::web::AppState;

use super::common::{PaginatedResponse, PaginationParams};

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct EpochFilters {
    epoch_from: Option<i64>,
    epoch_to: Option<i64>,
    validator_index: Option<String>,
    #[serde(flatten)]
    pagination: PaginationParams,
}

#[derive(Serialize)]
pub(super) struct EpochRow {
    epoch: i64,
    total_duties: i64,
    included: i64,
    missed: i64,
    /// `included / total_duties` (0.0 when `total_duties == 0`).
    attestation_rate: f64,
    head_correct: i64,
    target_correct: i64,
    source_correct: i64,
    total_reward: i64,
    sync_participated: i64,
    sync_missed: i64,
    proposals: i64,
    proposals_missed: i64,
}

impl From<db_epochs::EpochSummaryRow> for EpochRow {
    fn from(r: db_epochs::EpochSummaryRow) -> Self {
        let attestation_rate = if r.total_duties > 0 {
            r.included as f64 / r.total_duties as f64
        } else {
            0.0
        };
        Self {
            epoch: r.epoch,
            total_duties: r.total_duties,
            included: r.included,
            missed: r.missed,
            attestation_rate,
            head_correct: r.head_correct,
            target_correct: r.target_correct,
            source_correct: r.source_correct,
            total_reward: r.total_reward,
            sync_participated: r.sync_participated,
            sync_missed: r.sync_missed,
            proposals: r.proposals,
            proposals_missed: r.proposals_missed,
        }
    }
}

pub(super) async fn get_epoch_summary(
    State(state): State<AppState>,
    Query(f): Query<EpochFilters>,
) -> Json<PaginatedResponse<EpochRow>> {
    let validator_indices: Option<Vec<i64>> = f
        .validator_index
        .as_ref()
        .map(|s| s.split(',').filter_map(|v| v.trim().parse().ok()).collect());

    let filter = db_epochs::EpochFilter {
        validator_indices,
        epoch_from: f.epoch_from,
        epoch_to: f.epoch_to,
    };

    let page = f.pagination.page_num();
    let per_page = f.pagination.per_page_num();
    let offset = (page - 1) * per_page;

    let (rows, total) = db_epochs::list_epoch_summaries_paginated(
        &state.pool,
        &filter,
        db_epochs::SortOrder::parse(&f.pagination.order),
        per_page,
        offset,
    )
    .await
    .unwrap_or_else(|_| (Vec::new(), 0));

    Json(PaginatedResponse {
        data: rows.into_iter().map(Into::into).collect(),
        total,
        page,
        per_page,
    })
}
