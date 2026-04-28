mod api;
mod live_sse;

use crate::db::Pool as PgPool;
use axum::{
    Router,
    extract::Request,
    http::StatusCode,
    middleware::{self, Next},
    response::Response,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

use crate::beacon_client::BeaconClient;
use crate::config::Config;
use crate::live_updates::LiveUpdateEvent;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub beacon_client: Arc<BeaconClient>,
    pub config: Arc<Config>,
    pub tracked_validators: Arc<Vec<u64>>,
    pub live_updates_tx: broadcast::Sender<LiveUpdateEvent>,
}

/// Prometheus wrapper for /api/* requests. Uses the matched route template
/// (`/api/attestations`, `/api/stats`, …) as the `route` label so cardinality
/// stays bounded; falls back to `unmatched` for routes without a template.
async fn metrics_middleware(req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "unmatched".to_string());
    let started_at = std::time::Instant::now();
    let resp = next.run(req).await;
    let status = resp.status().as_u16();
    let class = match status {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    };
    crate::metrics::WEB_API_REQUESTS
        .with_label_values(&[&route, class])
        .inc();
    crate::metrics::WEB_API_DURATION
        .with_label_values(&[&route])
        .observe(started_at.elapsed().as_secs_f64());
    resp
}

async fn auth_middleware(
    state: axum::extract::State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let api_key = &state.config.api_key;

    if api_key.is_empty() {
        return Ok(next.run(req).await);
    }

    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(header) => {
            let token = header.strip_prefix("Bearer ").unwrap_or(header);
            if token == api_key {
                Ok(next.run(req).await)
            } else {
                Err(StatusCode::UNAUTHORIZED)
            }
        }
        None => Err(StatusCode::UNAUTHORIZED),
    }
}

pub async fn run_server(
    pool: PgPool,
    beacon_client: Arc<BeaconClient>,
    tracked_validators: Arc<Vec<u64>>,
    live_updates_tx: broadcast::Sender<LiveUpdateEvent>,
    addr: SocketAddr,
    config: Config,
) -> anyhow::Result<()> {
    let state = AppState {
        pool,
        beacon_client,
        config: Arc::new(config),
        tracked_validators,
        live_updates_tx,
    };

    let api_routes = api::router()
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(middleware::from_fn(metrics_middleware))
        .with_state(state.clone());

    let live_sse = axum::routing::get(live_sse::live_sse);

    // Auth-check endpoint (no middleware — used by frontend to verify key)
    let auth_check = axum::routing::get(|| async { StatusCode::OK });

    // Meta endpoint returns auth_required flag (no auth needed to call this)
    let meta_state = state.clone();
    let auth_info = axum::routing::get(move || {
        let auth_required = !meta_state.config.api_key.is_empty();
        async move { axum::Json(serde_json::json!({ "auth_required": auth_required })) }
    });

    let spa = ServeDir::new("web/build").fallback(ServeFile::new("web/build/index.html"));

    let app = Router::new()
        .nest("/api", api_routes)
        .route("/live/sse", live_sse)
        .route(
            "/api/auth-check",
            auth_check.layer(middleware::from_fn_with_state(
                state.clone(),
                auth_middleware,
            )),
        )
        .route("/api/auth-info", auth_info)
        .fallback_service(spa)
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    tracing::info!(%addr, auth = !state.config.api_key.is_empty(), "Starting web server");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
