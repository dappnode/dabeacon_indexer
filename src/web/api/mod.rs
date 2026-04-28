use axum::{Router, routing::get};

use crate::web::AppState;

mod attestations;
mod common;
mod epochs;
mod meta;
mod proposals;
mod rewards;
mod stats;
mod sync_duties;
mod validators;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/stats", get(stats::get_stats))
        .route("/validators", get(validators::get_validators))
        .route("/attestations", get(attestations::get_attestations))
        .route("/sync-duties", get(sync_duties::get_sync_duties))
        .route("/proposals", get(proposals::get_proposals))
        .route("/epochs", get(epochs::get_epoch_summary))
        .route("/rewards", get(rewards::get_rewards))
        .route("/meta", get(meta::get_meta))
}
