use axum::{
    Json,
    extract::{Query, State},
};
use serde::{Deserialize, Serialize};

use crate::chain;
use crate::db::api::proposals as db_prop;
use crate::web::AppState;

use super::common::{PaginatedResponse, PaginationParams};

#[derive(Deserialize)]
#[serde(default)]
pub(super) struct ProposalFilters {
    proposer_index: Option<String>,
    epoch_from: Option<i64>,
    epoch_to: Option<i64>,
    proposed: Option<bool>,
    finalized: Option<bool>,
    sort: String,
    #[serde(flatten)]
    pagination: PaginationParams,
}

impl Default for ProposalFilters {
    fn default() -> Self {
        Self {
            proposer_index: None,
            epoch_from: None,
            epoch_to: None,
            proposed: None,
            finalized: None,
            sort: "slot".to_string(),
            pagination: PaginationParams::default(),
        }
    }
}

#[derive(Serialize)]
pub(super) struct ProposalRow {
    slot: i64,
    /// Computed: `slot / slots_per_epoch`. Not present on the DB row.
    epoch: i64,
    proposer_index: i64,
    proposed: bool,
    reward_total: Option<i64>,
    reward_attestations: Option<i64>,
    reward_sync: Option<i64>,
    reward_slashings: Option<i64>,
    finalized: bool,
}

impl From<db_prop::ProposalRow> for ProposalRow {
    fn from(r: db_prop::ProposalRow) -> Self {
        Self {
            slot: r.slot,
            epoch: r.slot / chain::slots_per_epoch() as i64,
            proposer_index: r.proposer_index,
            proposed: r.proposed,
            reward_total: r.reward_total,
            reward_attestations: r.reward_attestations,
            reward_sync: r.reward_sync,
            reward_slashings: r.reward_slashings,
            finalized: r.finalized,
        }
    }
}

pub(super) async fn get_proposals(
    State(state): State<AppState>,
    Query(f): Query<ProposalFilters>,
) -> Json<PaginatedResponse<ProposalRow>> {
    let proposer_indices: Option<Vec<i64>> = f
        .proposer_index
        .as_ref()
        .map(|s| s.split(',').filter_map(|v| v.trim().parse().ok()).collect());

    let spe = chain::slots_per_epoch() as i64;
    let slot_from = f.epoch_from.map(|e| e * spe);
    let slot_to = f.epoch_to.map(|e| (e + 1) * spe - 1);

    let filter = db_prop::ProposalFilter {
        proposer_indices,
        slot_from,
        slot_to,
        proposed: f.proposed,
        finalized: f.finalized,
    };

    let page = f.pagination.page_num();
    let per_page = f.pagination.per_page_num();
    let offset = (page - 1) * per_page;

    let (rows, total) = db_prop::list_proposals_paginated(
        &state.pool,
        &filter,
        db_prop::ProposalSort::parse(&f.sort),
        db_prop::SortOrder::parse(&f.pagination.order),
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
