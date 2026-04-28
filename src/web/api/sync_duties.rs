use axum::{
    Json,
    extract::{Query, State},
};
use serde::{Deserialize, Serialize};

use crate::chain;
use crate::db::api::sync as db_sync;
use crate::web::AppState;

use super::common::{PaginatedResponse, PaginationParams};

#[derive(Deserialize)]
#[serde(default)]
pub(super) struct SyncFilters {
    validator_index: Option<String>,
    epoch_from: Option<i64>,
    epoch_to: Option<i64>,
    participated: Option<bool>,
    missed_block: Option<bool>,
    finalized: Option<bool>,
    sort: String,
    #[serde(flatten)]
    pagination: PaginationParams,
}

impl Default for SyncFilters {
    fn default() -> Self {
        Self {
            validator_index: None,
            epoch_from: None,
            epoch_to: None,
            participated: None,
            missed_block: None,
            finalized: None,
            sort: "slot".to_string(),
            pagination: PaginationParams::default(),
        }
    }
}

#[derive(Serialize)]
pub(super) struct SyncRow {
    validator_index: i64,
    slot: i64,
    /// Computed: `slot / slots_per_epoch`.
    epoch: i64,
    participated: bool,
    reward: Option<i64>,
    missed_block: bool,
    finalized: bool,
}

impl From<db_sync::SyncDutyRow> for SyncRow {
    fn from(r: db_sync::SyncDutyRow) -> Self {
        Self {
            validator_index: r.validator_index,
            slot: r.slot,
            epoch: r.slot / chain::slots_per_epoch() as i64,
            participated: r.participated,
            reward: r.reward,
            missed_block: r.missed_block,
            finalized: r.finalized,
        }
    }
}

pub(super) async fn get_sync_duties(
    State(state): State<AppState>,
    Query(f): Query<SyncFilters>,
) -> Json<PaginatedResponse<SyncRow>> {
    let validator_indices: Option<Vec<i64>> = f
        .validator_index
        .as_ref()
        .map(|s| s.split(',').filter_map(|v| v.trim().parse().ok()).collect());

    let spe = chain::slots_per_epoch() as i64;
    let slot_from = f.epoch_from.map(|e| e * spe);
    let slot_to = f.epoch_to.map(|e| (e + 1) * spe - 1);

    let filter = db_sync::SyncFilter {
        validator_indices,
        slot_from,
        slot_to,
        participated: f.participated,
        missed_block: f.missed_block,
        finalized: f.finalized,
    };

    let page = f.pagination.page_num();
    let per_page = f.pagination.per_page_num();
    let offset = (page - 1) * per_page;

    let (rows, total) = db_sync::list_sync_duties_paginated(
        &state.pool,
        &filter,
        db_sync::SyncSort::parse(&f.sort),
        db_sync::SortOrder::parse(&f.pagination.order),
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
