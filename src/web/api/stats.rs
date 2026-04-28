use axum::{Json, extract::State};
use serde::Serialize;

use crate::db::api::stats as db_stats;
use crate::web::AppState;

#[derive(Serialize)]
pub(super) struct Stats {
    total_validators: i64,
    total_epochs_scanned: i64,
    attestation_rate: f64,
    head_correct_rate: f64,
    target_correct_rate: f64,
    source_correct_rate: f64,
    total_attestations: i64,
    total_missed: i64,
    avg_inclusion_delay: Option<f64>,
    avg_effective_inclusion_delay: Option<f64>,
    total_proposals: i64,
    total_proposals_missed: i64,
    total_sync_participated: i64,
    total_sync_missed: i64,
    latest_scanned_epoch: Option<i64>,
    earliest_scanned_epoch: Option<i64>,
}

impl From<db_stats::DbStats> for Stats {
    fn from(s: db_stats::DbStats) -> Self {
        Self {
            total_validators: s.total_validators,
            total_epochs_scanned: s.total_epochs_scanned,
            attestation_rate: ratio(s.att_included, s.att_total),
            head_correct_rate: ratio(s.head_ok, s.att_decided),
            target_correct_rate: ratio(s.target_ok, s.att_decided),
            source_correct_rate: ratio(s.source_ok, s.att_decided),
            total_attestations: s.att_included,
            total_missed: s.att_missed,
            avg_inclusion_delay: s.avg_inclusion_delay,
            avg_effective_inclusion_delay: s.avg_effective_inclusion_delay,
            total_proposals: s.proposals_total,
            total_proposals_missed: s.proposals_missed,
            total_sync_participated: s.sync_participated,
            total_sync_missed: s.sync_missed,
            latest_scanned_epoch: s.latest_scanned_epoch,
            earliest_scanned_epoch: s.earliest_scanned_epoch,
        }
    }
}

pub(super) async fn get_stats(State(state): State<AppState>) -> Json<Stats> {
    let s = db_stats::fetch_stats(&state.pool).await.unwrap_or_default();
    Json(s.into())
}

fn ratio(num: i64, denom: i64) -> f64 {
    if denom > 0 {
        num as f64 / denom as f64
    } else {
        0.0
    }
}
