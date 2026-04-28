use axum::{
    Json,
    extract::{Query, State},
};
use serde::Deserialize;

use crate::db::api::attestations as db_att;
use crate::db::api::attestations::AttestationDutyRow;
use crate::web::AppState;

use super::common::{PaginatedResponse, PaginationParams};

#[derive(Deserialize)]
#[serde(default)]
pub(super) struct AttestationFilters {
    validator_index: Option<String>,
    epoch_from: Option<i64>,
    epoch_to: Option<i64>,
    included: Option<bool>,
    head_correct: Option<bool>,
    target_correct: Option<bool>,
    source_correct: Option<bool>,
    finalized: Option<bool>,
    min_effective_delay: Option<i32>,
    max_effective_delay: Option<i32>,
    sort: String,
    #[serde(flatten)]
    pagination: PaginationParams,
}

impl Default for AttestationFilters {
    fn default() -> Self {
        Self {
            validator_index: None,
            epoch_from: None,
            epoch_to: None,
            included: None,
            head_correct: None,
            target_correct: None,
            source_correct: None,
            finalized: None,
            min_effective_delay: None,
            max_effective_delay: None,
            sort: "epoch".to_string(),
            pagination: PaginationParams::default(),
        }
    }
}

pub(super) async fn get_attestations(
    State(state): State<AppState>,
    Query(f): Query<AttestationFilters>,
) -> Json<PaginatedResponse<AttestationDutyRow>> {
    let validator_indices: Option<Vec<i64>> = f
        .validator_index
        .as_ref()
        .map(|s| s.split(',').filter_map(|v| v.trim().parse().ok()).collect());

    let filter = db_att::AttestationFilter {
        validator_indices,
        epoch_from: f.epoch_from,
        epoch_to: f.epoch_to,
        included: f.included,
        head_correct: f.head_correct,
        target_correct: f.target_correct,
        source_correct: f.source_correct,
        finalized: f.finalized,
        min_effective_delay: f.min_effective_delay,
        max_effective_delay: f.max_effective_delay,
    };

    let page = f.pagination.page_num();
    let per_page = f.pagination.per_page_num();
    let offset = (page - 1) * per_page;

    let (data, total) = db_att::list_attestation_duties_paginated(
        &state.pool,
        &filter,
        db_att::AttestationSort::parse(&f.sort),
        db_att::SortOrder::parse(&f.pagination.order),
        per_page,
        offset,
    )
    .await
    .unwrap_or_else(|_| (Vec::new(), 0));

    Json(PaginatedResponse {
        data,
        total,
        page,
        per_page,
    })
}
