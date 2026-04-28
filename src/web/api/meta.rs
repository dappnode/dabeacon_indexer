use axum::{Json, extract::State};
use serde::Serialize;

use crate::web::AppState;

#[derive(Serialize)]
pub(super) struct MetaResponse {
    validators: std::collections::HashMap<u64, crate::config::ValidatorMeta>,
    all_tags: Vec<String>,
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

    Json(MetaResponse {
        validators: meta.clone(),
        all_tags,
    })
}
