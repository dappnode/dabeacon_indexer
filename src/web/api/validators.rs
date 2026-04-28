use axum::{Json, extract::State};
use serde::Serialize;

use crate::db::api::validators as db_validators;
use crate::web::AppState;

#[derive(Serialize)]
pub(super) struct ValidatorSummary {
    validator_index: i64,
    pubkey: String,
    activation_epoch: i64,
    exit_epoch: Option<i64>,
    last_scanned_epoch: Option<i64>,
    total_attestations: i64,
    missed_attestations: i64,
    attestation_rate: f64,
    head_correct_rate: f64,
    target_correct_rate: f64,
    source_correct_rate: f64,
    total_proposals: i64,
    missed_proposals: i64,
    sync_participated: i64,
    sync_missed: i64,
}

impl From<db_validators::ValidatorSummary> for ValidatorSummary {
    fn from(r: db_validators::ValidatorSummary) -> Self {
        Self {
            validator_index: r.validator_index,
            pubkey: format!("0x{}", r.pubkey_hex),
            activation_epoch: r.activation_epoch,
            exit_epoch: r.exit_epoch,
            last_scanned_epoch: r.last_scanned_epoch,
            total_attestations: r.att_included,
            missed_attestations: r.att_missed,
            attestation_rate: ratio(r.att_included, r.att_total),
            head_correct_rate: ratio(r.head_ok, r.att_decided),
            target_correct_rate: ratio(r.target_ok, r.att_decided),
            source_correct_rate: ratio(r.source_ok, r.att_decided),
            total_proposals: r.proposals_total,
            missed_proposals: r.proposals_missed,
            sync_participated: r.sync_participated,
            sync_missed: r.sync_missed,
        }
    }
}

pub(super) async fn get_validators(State(state): State<AppState>) -> Json<Vec<ValidatorSummary>> {
    let rows = db_validators::list_validator_summaries(&state.pool)
        .await
        .unwrap_or_default();
    Json(rows.into_iter().map(Into::into).collect())
}

fn ratio(num: i64, denom: i64) -> f64 {
    if denom > 0 {
        num as f64 / denom as f64
    } else {
        0.0
    }
}
