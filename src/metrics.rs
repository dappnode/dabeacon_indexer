//! Prometheus metrics. Single module owns every metric so label taxonomies
//! stay consistent and cardinality stays bounded. Every label value is either
//! a fixed enum-like string or a classified beacon-path label (see
//! [`classify_beacon_path`]); raw paths are never used directly.
//!
//! Callers render text via [`render`] from `/metrics`.

use once_cell::sync::Lazy;
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, IntGaugeVec, Opts,
    Registry, TextEncoder, register_histogram_vec_with_registry,
    register_int_counter_vec_with_registry, register_int_counter_with_registry,
    register_int_gauge_vec_with_registry, register_int_gauge_with_registry,
};

/// Private registry — keeps our metrics out of the Prometheus default
/// `default_registry` so tests can spin up fresh state without cross-process
/// leakage.
pub(crate) static REGISTRY: Lazy<Registry> = Lazy::new(Registry::new);

pub fn render() -> Vec<u8> {
    let metric_families = REGISTRY.gather();
    let mut buffer = Vec::new();
    TextEncoder::new()
        .encode(&metric_families, &mut buffer)
        .ok();
    buffer
}

/// Run a tiny unauthenticated HTTP server that serves `/metrics`. Kept on its
/// own port (and deliberately off the main API router) so operators can bind
/// it to a private interface while the main API stays public + authenticated.
pub async fn run_server(addr: std::net::SocketAddr) -> anyhow::Result<()> {
    use axum::{Router, http::header, response::IntoResponse, routing::get};

    let app = Router::new().route(
        "/metrics",
        get(|| async {
            (
                [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
                render(),
            )
                .into_response()
        }),
    );
    tracing::info!(%addr, "Starting metrics server");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Bucket presets
// ---------------------------------------------------------------------------

/// For short-path operations like cache probes or individual DB upserts —
/// anything that should complete well under a second. Lower bound goes below
/// 1 ms so cache-hit paths still land in a meaningful bucket.
fn sub_second_buckets() -> Vec<f64> {
    vec![
        0.0001, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
    ]
}

/// Beacon HTTP calls. Tuned from observed `elapsed_ms` values in trace logs:
/// attester-duties POSTs ran ~80-110 ms, attestation-rewards ~1.5-1.8 s, per-
/// block fetches ~20-200 ms, whole 64-block concurrent fetches wall-timed at
/// 1.2-2.5 s. Retries and cold archive fetches push the tail to several
/// seconds; the 10 s bucket catches pathological cases.
fn beacon_request_buckets() -> Vec<f64> {
    vec![
        0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
    ]
}

/// Per-epoch / per-phase durations. Re-tuned from production histogram data:
/// the three phases share this bucket list but their actual distributions
/// differ by an order of magnitude —
/// - proposals phase clusters under 0.5 s (median ~100 ms),
/// - sync_committee phase clusters in 0.5-1 s,
/// - attestations phase clusters in 2-7.5 s with a tail to ~15 s,
/// - full-epoch totals sit in 2-20 s, finalized rescan loops can span
///   multiple epochs up to ~minutes.
///
/// The bucket list is therefore denser at both the sub-second end (to give
/// proposals and sync_committee useful resolution) and the 2-10 s band (the
/// meat of the attestation distribution) than a geometric sequence would be.
fn epoch_scan_buckets() -> Vec<f64> {
    vec![
        0.05, 0.1, 0.2, 0.5, 0.75, 1.0, 2.0, 3.0, 5.0, 7.5, 10.0, 15.0, 30.0, 60.0, 120.0,
    ]
}

/// Live per-slot head-scan durations. A single block fetch + decode + DB
/// writes — the trace didn't capture enough head-scan cycles for a robust
/// distribution, but the constituent operations (block fetch ~20-200 ms,
/// per-duty upsert ~sub-ms) should keep the median well under 500 ms; the
/// 10 s upper bucket is a safety net for beacon-node back-pressure.
fn head_scan_buckets() -> Vec<f64> {
    vec![0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
}

// ---------------------------------------------------------------------------
// Beacon client HTTP
// ---------------------------------------------------------------------------

/// Classify a beacon API path into a stable low-cardinality endpoint label.
/// Unknown shapes fall through to `"other"` so label explosion is impossible.
pub fn classify_beacon_path(path: &str) -> &'static str {
    // Strip query string before matching.
    let path = path.split('?').next().unwrap_or(path);
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match segments.as_slice() {
        ["eth", _, "beacon", "blocks", _, "root"] => "block_root",
        ["eth", _, "beacon", "blocks", ..] => "blocks",
        ["eth", _, "beacon", "headers", ..] => "headers",
        ["eth", _, "beacon", "states", _, "committees"] => "committees",
        ["eth", _, "beacon", "states", _, "sync_committees"] => "sync_committees",
        ["eth", _, "beacon", "states", _, "validators"] => "validators",
        ["eth", _, "beacon", "states", _, "finality_checkpoints"] => "finality_checkpoints",
        ["eth", _, "beacon", "rewards", "attestations", _] => "rewards_attestations",
        ["eth", _, "beacon", "rewards", "sync_committee", _] => "rewards_sync_committee",
        ["eth", _, "beacon", "rewards", "blocks", _] => "rewards_blocks",
        ["eth", _, "beacon", "genesis"] => "genesis",
        ["eth", _, "validator", "duties", "attester", _] => "duties_attester",
        ["eth", _, "validator", "duties", "proposer", _] => "duties_proposer",
        ["eth", _, "validator", "duties", "sync", _] => "duties_sync",
        ["eth", _, "config", "spec"] => "spec",
        ["eth", _, "events"] => "events",
        _ => "other",
    }
}

pub static BEACON_REQUESTS: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec_with_registry!(
        Opts::new(
            "beacon_requests_total",
            "Beacon API requests by endpoint, HTTP method, and outcome."
        ),
        &["endpoint", "method", "outcome"],
        REGISTRY
    )
    .expect("register beacon_requests_total")
});

pub static BEACON_REQUEST_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec_with_registry!(
        HistogramOpts::new(
            "beacon_request_duration_seconds",
            "Wall time per beacon request (success attempts only, incl. body download)."
        )
        .buckets(beacon_request_buckets()),
        &["endpoint", "method"],
        REGISTRY
    )
    .expect("register beacon_request_duration_seconds")
});

pub static BEACON_RETRIES: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec_with_registry!(
        Opts::new(
            "beacon_retries_total",
            "Beacon API retries by endpoint, method, and reason."
        ),
        &["endpoint", "method", "reason"],
        REGISTRY
    )
    .expect("register beacon_retries_total")
});

// ---------------------------------------------------------------------------
// Beacon client caches
// ---------------------------------------------------------------------------

pub static BEACON_CACHE_REQUESTS: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec_with_registry!(
        Opts::new(
            "beacon_cache_requests_total",
            "Beacon client cache lookups by cache and result."
        ),
        &["cache", "result"],
        REGISTRY
    )
    .expect("register beacon_cache_requests_total")
});

/// Record a cache access. `cache` is one of the fixed strings ("committees",
/// "attester_duties", etc.), `hit` picks the label value.
pub fn record_cache(cache: &str, hit: bool) {
    BEACON_CACHE_REQUESTS
        .with_label_values(&[cache, if hit { "hit" } else { "miss" }])
        .inc();
}

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

pub static SCANNER_EPOCH_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec_with_registry!(
        HistogramOpts::new(
            "scanner_epoch_duration_seconds",
            "Wall time for a full epoch scan (attestations + proposals + sync)."
        )
        .buckets(epoch_scan_buckets()),
        &["mode", "finalized"],
        REGISTRY
    )
    .expect("register scanner_epoch_duration_seconds")
});

/// Wall time spent in each of the three top-level phases of an epoch scan:
/// `attestations`, `proposals`, `sync_committee`. Lets backfill dashboards see
/// which phase dominates; finer-grained HTTP timings live under
/// `beacon_request_duration_seconds` by endpoint, so we deliberately don't
/// duplicate sub-phase timers here.
pub static SCANNER_PHASE_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec_with_registry!(
        HistogramOpts::new(
            "scanner_phase_duration_seconds",
            "Per-phase wall time inside a single epoch scan."
        )
        .buckets(epoch_scan_buckets()),
        &["phase", "mode", "finalized"],
        REGISTRY
    )
    .expect("register scanner_phase_duration_seconds")
});

pub static SCANNER_EPOCHS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec_with_registry!(
        Opts::new(
            "scanner_epochs_scanned_total",
            "Total epochs scanned by mode and finalized flag."
        ),
        &["mode", "finalized"],
        REGISTRY
    )
    .expect("register scanner_epochs_scanned_total")
});

pub static SCANNER_ATT_DUTIES: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec_with_registry!(
        Opts::new(
            "scanner_attestation_duties_written_total",
            "Attestation duty rows written by the scanner, labelled by outcome."
        ),
        &["mode", "outcome"],
        REGISTRY
    )
    .expect("register scanner_attestation_duties_written_total")
});

pub static SCANNER_PROPOSALS: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec_with_registry!(
        Opts::new(
            "scanner_proposals_written_total",
            "Proposal rows written by the scanner, labelled by outcome."
        ),
        &["outcome"],
        REGISTRY
    )
    .expect("register scanner_proposals_written_total")
});

pub static SCANNER_SYNC_PARTICIPATION: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec_with_registry!(
        Opts::new(
            "scanner_sync_participation_written_total",
            "Sync-committee participation rows written, labelled by outcome."
        ),
        &["outcome"],
        REGISTRY
    )
    .expect("register scanner_sync_participation_written_total")
});

// ---------------------------------------------------------------------------
// Backfill
// ---------------------------------------------------------------------------

pub static BACKFILL_TARGET_EPOCH: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge_with_registry!(
        Opts::new(
            "backfill_target_epoch",
            "Current target finalized epoch the backfill is chasing."
        ),
        REGISTRY
    )
    .expect("register backfill_target_epoch")
});

pub static BACKFILL_MIN_START: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge_with_registry!(
        Opts::new(
            "backfill_min_start_epoch",
            "Lowest epoch the current backfill pass intends to scan."
        ),
        REGISTRY
    )
    .expect("register backfill_min_start_epoch")
});

pub static BACKFILL_EPOCHS_SCANNED: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter_with_registry!(
        Opts::new(
            "backfill_epochs_scanned_total",
            "Epochs backfill has actually scanned."
        ),
        REGISTRY
    )
    .expect("register backfill_epochs_scanned_total")
});

pub static BACKFILL_EPOCHS_SKIPPED: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter_with_registry!(
        Opts::new(
            "backfill_epochs_skipped_covered_total",
            "Epochs skipped because all in-scope validators were already covered."
        ),
        REGISTRY
    )
    .expect("register backfill_epochs_skipped_covered_total")
});

pub static BACKFILL_ACTIVE: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge_with_registry!(
        Opts::new(
            "backfill_active",
            "Whether a backfill pass is currently running (1 = yes, 0 = idle)."
        ),
        REGISTRY
    )
    .expect("register backfill_active")
});

pub static BACKFILL_PASS_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec_with_registry!(
        HistogramOpts::new(
            "backfill_pass_duration_seconds",
            "Wall time of a full backfill pass (each extension counts separately)."
        )
        .buckets(vec![
            0.5, 1.0, 5.0, 15.0, 60.0, 300.0, 900.0, 1800.0, 3600.0, 7200.0,
        ]),
        &["kind"],
        REGISTRY
    )
    .expect("register backfill_pass_duration_seconds")
});

// ---------------------------------------------------------------------------
// Live tracker
// ---------------------------------------------------------------------------

pub static LIVE_SSE_EVENTS: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec_with_registry!(
        Opts::new(
            "live_sse_events_total",
            "SSE events received from the beacon node by kind and outcome."
        ),
        &["kind", "outcome"],
        REGISTRY
    )
    .expect("register live_sse_events_total")
});

pub static LIVE_HEAD_SCAN_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec_with_registry!(
        HistogramOpts::new(
            "live_head_scan_duration_seconds",
            "Wall time processing a single head event (fetch + decode + write)."
        )
        .buckets(head_scan_buckets()),
        &["stage"],
        REGISTRY
    )
    .expect("register live_head_scan_duration_seconds")
});

pub static LIVE_FINALIZED_RESCAN_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec_with_registry!(
        HistogramOpts::new(
            "live_finalized_rescan_duration_seconds",
            "Wall time for finalized-checkpoint rescan processing."
        )
        .buckets(epoch_scan_buckets()),
        &["stage"],
        REGISTRY
    )
    .expect("register live_finalized_rescan_duration_seconds")
});

pub static LIVE_LAST_SLOT: Lazy<IntGaugeVec> = Lazy::new(|| {
    register_int_gauge_vec_with_registry!(
        Opts::new(
            "live_last_slot",
            "Most recent slot observed / processed by the live tracker."
        ),
        &["kind"],
        REGISTRY
    )
    .expect("register live_last_slot")
});

pub static LIVE_REORGS: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter_with_registry!(
        Opts::new(
            "live_reorgs_total",
            "Chain reorg events handled by the live tracker."
        ),
        REGISTRY
    )
    .expect("register live_reorgs_total")
});

// ---------------------------------------------------------------------------
// Database
// ---------------------------------------------------------------------------

pub static DB_UPSERTS: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec_with_registry!(
        Opts::new(
            "db_upserts_total",
            "Scanner DB upserts by table and outcome."
        ),
        &["table", "outcome"],
        REGISTRY
    )
    .expect("register db_upserts_total")
});

pub static DB_UPSERT_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec_with_registry!(
        HistogramOpts::new(
            "db_upsert_duration_seconds",
            "Wall time for a single scanner upsert."
        )
        .buckets(sub_second_buckets()),
        &["table"],
        REGISTRY
    )
    .expect("register db_upsert_duration_seconds")
});

// ---------------------------------------------------------------------------
// Web
// ---------------------------------------------------------------------------

pub static WEB_API_REQUESTS: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec_with_registry!(
        Opts::new(
            "web_api_requests_total",
            "HTTP API requests by route template and response status class."
        ),
        &["route", "status_class"],
        REGISTRY
    )
    .expect("register web_api_requests_total")
});

pub static WEB_API_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec_with_registry!(
        HistogramOpts::new(
            "web_api_duration_seconds",
            "HTTP API request handler wall time by route template."
        )
        .buckets(sub_second_buckets()),
        &["route"],
        REGISTRY
    )
    .expect("register web_api_duration_seconds")
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_beacon_path_covers_known_shapes() {
        assert_eq!(
            classify_beacon_path("/eth/v2/beacon/blocks/12345"),
            "blocks"
        );
        assert_eq!(
            classify_beacon_path("/eth/v1/beacon/blocks/12345/root"),
            "block_root"
        );
        assert_eq!(
            classify_beacon_path("/eth/v1/beacon/states/head/committees?epoch=123"),
            "committees"
        );
        assert_eq!(
            classify_beacon_path("/eth/v1/beacon/rewards/attestations/123"),
            "rewards_attestations"
        );
        assert_eq!(
            classify_beacon_path("/eth/v1/validator/duties/attester/123"),
            "duties_attester"
        );
        assert_eq!(
            classify_beacon_path("/eth/v1/events?topics=head,finalized_checkpoint"),
            "events"
        );
        assert_eq!(classify_beacon_path("/eth/v1/something/unknown"), "other");
    }

    #[test]
    fn render_produces_some_output_after_touching_metrics() {
        // Touch the lazy metric so it's registered.
        BEACON_REQUESTS
            .with_label_values(&["blocks", "GET", "ok"])
            .inc();
        let out = render();
        assert!(!out.is_empty());
        assert!(String::from_utf8_lossy(&out).contains("beacon_requests_total"));
    }
}
