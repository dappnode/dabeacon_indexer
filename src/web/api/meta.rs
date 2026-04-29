use axum::{Json, extract::State};
use serde::Serialize;

use crate::chain;
use crate::web::AppState;

#[derive(Serialize)]
pub(super) struct ChainInfo {
    slots_per_epoch: u64,
    seconds_per_slot: u64,
    genesis_time: u64,
}

#[derive(Serialize)]
pub(super) struct MetaResponse {
    validators: std::collections::HashMap<u64, crate::config::ValidatorMeta>,
    all_tags: Vec<String>,
    chain: ChainInfo,
    explorer_url: String,
}

pub(super) async fn get_meta(State(state): State<AppState>) -> Json<MetaResponse> {
    let meta = &state.config.validator_meta;

    let mut all_tags: Vec<String> = meta
        .values()
        .flat_map(|m| m.tags.iter().cloned())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    all_tags.sort();

    let spec = chain::spec();

    Json(MetaResponse {
        validators: meta.clone(),
        all_tags,
        chain: ChainInfo {
            slots_per_epoch: spec.slots_per_epoch,
            seconds_per_slot: spec.seconds_per_slot,
            genesis_time: spec.genesis_time,
        },
        explorer_url: state.config.explorer_url.clone(),
    })
}
