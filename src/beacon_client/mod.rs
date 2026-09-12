pub mod blocks;
pub mod committees;
pub mod duties;
mod inputs;
pub mod metadata;
pub mod rewards;
pub mod spec;
pub mod types;
pub mod validators;

use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use lru::LruCache;
use reqwest::Client;
use serde::de::DeserializeOwned;
use tokio::sync::{Mutex, RwLock};

use crate::beacon_client::types::{
    BeaconResponse, BlockRoot, FinalityCheckpoints, SignedBeaconBlock,
};
use crate::error::{Error, Result};

const BEACON_RETRY_MAX_ATTEMPTS: u32 = 3;
const BEACON_RETRY_BASE_DELAY_MS: u64 = 200;
const ROOT_BLOCK_CACHE_CAPACITY: usize = 128;
const SLOT_ROOT_CACHE_CAPACITY: usize = 512;

const INPUT_CACHE_CAPACITY: usize = 128;
const HEAD_SLOT_TTL: Duration = Duration::from_secs(2);
const HEAD_FINALITY_TTL: Duration = Duration::from_secs(10);

pub struct BeaconClient {
    client: Client,
    base_url: String,
    // Block caches keep `Mutex` because every `get()` on the LRU is a mutating
    // operation (updates recency), and the access pattern is write-skewed —
    // most hits populate the cache.
    root_block_cache: Mutex<LruCache<BlockRoot, SignedBeaconBlock>>,
    slot_root_cache: Mutex<LruCache<u64, BlockRoot>>,

    // Duty/committee caches are READ-dominated (SSE refresh hits the same keys
    // repeatedly). We store under `RwLock` and probe with `peek()` (read-only,
    // doesn't update recency) on the read path so concurrent SSE handlers
    // don't serialise. Writes still take the exclusive lock.
    pub(crate) head_slot_cache: RwLock<Option<(u64, Instant)>>,
    pub(crate) head_finality_cache: RwLock<Option<(FinalityCheckpoints, Instant)>>,
    input_cache: RwLock<LruCache<String, inputs::CachedInput>>,
    unavailable_inputs: Mutex<LruCache<String, (Instant, String)>>,
    input_pool: Option<sqlx::PgPool>,
}

impl BeaconClient {
    fn should_retry_http_error(err: &reqwest::Error) -> bool {
        err.is_timeout() || err.is_connect() || err.is_request()
    }

    fn should_retry_status(status: reqwest::StatusCode) -> bool {
        matches!(status.as_u16(), 408 | 425 | 429 | 500 | 502 | 503 | 504)
    }

    fn retry_delay(attempt: u32) -> Duration {
        let exp = attempt.saturating_sub(1).min(5);
        let factor = 1u64 << exp;
        Duration::from_millis(BEACON_RETRY_BASE_DELAY_MS * factor)
    }

    pub fn new(base_url: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(8)
            .build()
            .expect("Failed to create HTTP client");

        let nz = |n: usize| NonZeroUsize::new(n).expect("cache capacity must be > 0");
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            root_block_cache: Mutex::new(LruCache::new(nz(ROOT_BLOCK_CACHE_CAPACITY))),
            slot_root_cache: Mutex::new(LruCache::new(nz(SLOT_ROOT_CACHE_CAPACITY))),
            head_slot_cache: RwLock::new(None),
            head_finality_cache: RwLock::new(None),
            input_cache: RwLock::new(LruCache::new(nz(INPUT_CACHE_CAPACITY))),
            unavailable_inputs: Mutex::new(LruCache::new(nz(INPUT_CACHE_CAPACITY))),
            input_pool: None,
        }
    }

    /// Drop all duty/committee + head/finality caches. Called by the live
    /// reorg handler so a reorg crossing an epoch boundary can't serve
    /// pre-reorg duties out of cache.
    pub async fn invalidate_duty_caches(&self) {
        self.input_cache.write().await.clear();
        self.unavailable_inputs.lock().await.clear();
        *self.head_slot_cache.write().await = None;
        *self.head_finality_cache.write().await = None;
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub(crate) async fn get_response(&self, path: &str) -> Result<reqwest::Response> {
        let url = self.url(path);
        let endpoint = crate::metrics::classify_beacon_path(path);
        tracing::trace!(method = "GET", url = %url, "Beacon API request");

        for attempt in 1..=BEACON_RETRY_MAX_ATTEMPTS {
            let start = std::time::Instant::now();
            let resp = match self.client.get(&url).send().await {
                Ok(resp) => resp,
                Err(e) => {
                    let retryable = Self::should_retry_http_error(&e);
                    crate::metrics::BEACON_RETRIES
                        .with_label_values(&[
                            endpoint,
                            "GET",
                            if retryable { "transport" } else { "error" },
                        ])
                        .inc();
                    if retryable && attempt < BEACON_RETRY_MAX_ATTEMPTS {
                        let delay = Self::retry_delay(attempt);
                        tracing::warn!(
                            method = "GET",
                            url = %url,
                            attempt,
                            max_attempts = BEACON_RETRY_MAX_ATTEMPTS,
                            delay_ms = delay.as_millis() as u64,
                            error = %e,
                            "Transient beacon HTTP error, retrying"
                        );
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    crate::metrics::BEACON_REQUESTS
                        .with_label_values(&[endpoint, "GET", "error"])
                        .inc();
                    return Err(Error::Http(e));
                }
            };

            let status = resp.status();
            let elapsed = start.elapsed();
            crate::metrics::BEACON_REQUEST_DURATION
                .with_label_values(&[endpoint, "GET"])
                .observe(elapsed.as_secs_f64());

            tracing::trace!(
                method = "GET",
                url = %url,
                attempt,
                status = %status.as_u16(),
                elapsed_ms = elapsed.as_millis() as u64,
                "Beacon API response"
            );

            if status.is_success() {
                crate::metrics::BEACON_REQUESTS
                    .with_label_values(&[endpoint, "GET", "ok"])
                    .inc();
                return Ok(resp);
            }

            let body = resp.text().await.unwrap_or_default();
            let outcome = if status.is_client_error() {
                "4xx"
            } else {
                "5xx"
            };
            if Self::should_retry_status(status) && attempt < BEACON_RETRY_MAX_ATTEMPTS {
                crate::metrics::BEACON_RETRIES
                    .with_label_values(&[endpoint, "GET", "status"])
                    .inc();
                let delay = Self::retry_delay(attempt);
                tracing::warn!(
                    method = "GET",
                    url = %url,
                    attempt,
                    max_attempts = BEACON_RETRY_MAX_ATTEMPTS,
                    status = %status.as_u16(),
                    delay_ms = delay.as_millis() as u64,
                    body = %body,
                    "Transient beacon API status, retrying"
                );
                tokio::time::sleep(delay).await;
                continue;
            }

            crate::metrics::BEACON_REQUESTS
                .with_label_values(&[endpoint, "GET", outcome])
                .inc();
            tracing::debug!(
                method = "GET",
                url = %url,
                status = %status.as_u16(),
                body = %body,
                "Beacon API error response"
            );
            return Err(Error::BeaconApi {
                status: status.as_u16(),
                message: body,
            });
        }

        unreachable!("retry loop should always return")
    }

    pub(crate) async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self.get_response(path).await?;
        let beacon_resp: BeaconResponse<T> = resp.json().await.map_err(Error::Http)?;
        beacon_resp.ensure_not_optimistic()?;
        Ok(beacon_resp.data)
    }

    pub(crate) async fn post_response<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<reqwest::Response> {
        let url = self.url(path);
        let endpoint = crate::metrics::classify_beacon_path(path);
        tracing::trace!(method = "POST", url = %url, "Beacon API request");

        for attempt in 1..=BEACON_RETRY_MAX_ATTEMPTS {
            let start = std::time::Instant::now();
            let resp = match self.client.post(&url).json(body).send().await {
                Ok(resp) => resp,
                Err(e) => {
                    let retryable = Self::should_retry_http_error(&e);
                    crate::metrics::BEACON_RETRIES
                        .with_label_values(&[
                            endpoint,
                            "POST",
                            if retryable { "transport" } else { "error" },
                        ])
                        .inc();
                    if retryable && attempt < BEACON_RETRY_MAX_ATTEMPTS {
                        let delay = Self::retry_delay(attempt);
                        tracing::warn!(
                            method = "POST",
                            url = %url,
                            attempt,
                            max_attempts = BEACON_RETRY_MAX_ATTEMPTS,
                            delay_ms = delay.as_millis() as u64,
                            error = %e,
                            "Transient beacon HTTP error, retrying"
                        );
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    crate::metrics::BEACON_REQUESTS
                        .with_label_values(&[endpoint, "POST", "error"])
                        .inc();
                    return Err(Error::Http(e));
                }
            };

            let status = resp.status();
            let elapsed = start.elapsed();
            crate::metrics::BEACON_REQUEST_DURATION
                .with_label_values(&[endpoint, "POST"])
                .observe(elapsed.as_secs_f64());
            tracing::trace!(
                method = "POST",
                url = %url,
                attempt,
                status = %status.as_u16(),
                elapsed_ms = elapsed.as_millis() as u64,
                "Beacon API response"
            );

            if status.is_success() {
                crate::metrics::BEACON_REQUESTS
                    .with_label_values(&[endpoint, "POST", "ok"])
                    .inc();
                return Ok(resp);
            }

            let body = resp.text().await.unwrap_or_default();
            let outcome = if status.is_client_error() {
                "4xx"
            } else {
                "5xx"
            };
            if Self::should_retry_status(status) && attempt < BEACON_RETRY_MAX_ATTEMPTS {
                crate::metrics::BEACON_RETRIES
                    .with_label_values(&[endpoint, "POST", "status"])
                    .inc();
                let delay = Self::retry_delay(attempt);
                tracing::warn!(
                    method = "POST",
                    url = %url,
                    attempt,
                    max_attempts = BEACON_RETRY_MAX_ATTEMPTS,
                    status = %status.as_u16(),
                    delay_ms = delay.as_millis() as u64,
                    body = %body,
                    "Transient beacon API status, retrying"
                );
                tokio::time::sleep(delay).await;
                continue;
            }

            crate::metrics::BEACON_REQUESTS
                .with_label_values(&[endpoint, "POST", outcome])
                .inc();
            tracing::debug!(
                method = "POST",
                url = %url,
                status = %status.as_u16(),
                body = %body,
                "Beacon API error response"
            );
            return Err(Error::BeaconApi {
                status: status.as_u16(),
                message: body,
            });
        }

        unreachable!("retry loop should always return")
    }

    /// POST request with JSON body returning `BeaconResponse<T>.data`
    pub(crate) async fn post<T: DeserializeOwned, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let resp = self.post_response(path, body).await?;
        let beacon_resp: BeaconResponse<T> = resp.json().await.map_err(Error::Http)?;
        beacon_resp.ensure_not_optimistic()?;
        Ok(beacon_resp.data)
    }
}
