//! Durable inputs are reusable only while their canonical anchor is unchanged.
//! We validate block headers, not historical states, so pruning states does not
//! prevent reuse after restart. Raw JSON retains endpoint metadata verbatim.
use serde::de::DeserializeOwned;
use sqlx::PgPool;
use std::time::{Duration, Instant};

use super::BeaconClient;
use super::types::{BeaconResponse, Root};
use crate::chain::epoch_start_slot;
use crate::error::{Error, Result};

#[derive(Clone)]
pub(super) struct CachedInput {
    anchor_slot: u64,
    anchor_root: Root,
    response: serde_json::Value,
}

impl BeaconClient {
    pub fn with_pool(mut self, pool: PgPool) -> Self {
        self.input_pool = Some(pool);
        self
    }

    /// The caller supplies the oldest epoch still needed by pending work.
    /// Only completed/abandoned input history before that safe floor is removed.
    pub async fn prune_inputs_before(&self, oldest_needed_epoch: u64) -> Result<()> {
        if let Some(pool) = &self.input_pool {
            sqlx::query("DELETE FROM beacon_inputs WHERE epoch < $1")
                .bind(oldest_needed_epoch as i64)
                .execute(pool)
                .await?;
        }
        Ok(())
    }

    // Use the boundary block (or its parent for skipped boundary slots). For
    // next-epoch prefetch, anchor to the currently observed head instead.
    async fn input_anchor(&self, epoch: u64) -> Result<(u64, Root)> {
        let head = self.get_head_header().await?;
        let boundary = epoch_start_slot(epoch);
        if head.header.message.slot <= boundary {
            return Ok((head.header.message.slot, head.root));
        }
        let mut slot = boundary;
        loop {
            match self.get_header(&slot.to_string()).await {
                Ok(header) => return Ok((slot, header.data.root)),
                Err(Error::BeaconApi { status: 404, .. }) => {
                    if slot == 0 {
                        break;
                    }
                    slot -= 1;
                }
                Err(error) => return Err(error),
            }
            // Do not turn a long unavailable range into an unbounded request loop.
            if boundary - slot > crate::chain::slots_per_epoch() * 2 {
                break;
            }
        }
        Err(Error::BeaconDataUnavailable(format!(
            "no input anchor at or before epoch {epoch} (slot {boundary}); history may be pruned"
        )))
    }

    async fn anchor_unchanged(&self, input: &CachedInput) -> Result<bool> {
        Ok(self
            .get_header(&input.anchor_slot.to_string())
            .await?
            .data
            .root
            == input.anchor_root)
    }

    pub(super) async fn cached_input<T: DeserializeOwned>(
        &self,
        epoch: u64,
        path: &str,
        body: Option<&[String]>,
    ) -> Result<BeaconResponse<T>> {
        let mut indices = body.unwrap_or_default().to_vec();
        indices.sort_unstable();
        indices.dedup();
        let key = format!("{path}:{}", indices.join(","));
        let mut cached = self.input_cache.read().await.peek(&key).cloned();
        if cached.is_none()
            && let Some(pool) = &self.input_pool
        {
            let row: Option<(i64, String, String)> = sqlx::query_as(
                "SELECT anchor_slot, anchor_root, response FROM beacon_inputs WHERE input_key = $1",
            )
            .bind(&key)
            .fetch_optional(pool)
            .await?;
            if let Some((slot, root, response)) = row {
                cached = Some(CachedInput {
                    anchor_slot: slot as u64,
                    anchor_root: Root::parse(&root)?,
                    response: serde_json::from_str(&response)?,
                });
            }
        }
        if let Some(hit) = cached {
            // A future-epoch snapshot is reusable only while the head itself is
            // unchanged. Once that epoch starts, use its actual boundary anchor.
            let boundary = epoch_start_slot(epoch);
            let still_current = if hit.anchor_slot < boundary {
                self.input_anchor(epoch).await? == (hit.anchor_slot, hit.anchor_root.clone())
            } else {
                self.anchor_unchanged(&hit).await?
            };
            if still_current {
                let parsed: BeaconResponse<T> = serde_json::from_value(hit.response.clone())?;
                parsed.ensure_not_optimistic()?;
                if let Some(dependency) = &parsed.dependent_root {
                    self.get_header(dependency.as_str()).await?;
                }
                self.input_cache.write().await.put(key, hit);
                return Ok(parsed);
            }
        }
        // Many inclusion slots need the same epoch input. Share a short retry
        // delay so a pruned state is not requested once per slot on every poll.
        if let Some((since, message)) = self.unavailable_inputs.lock().await.peek(&key)
            && since.elapsed() < Duration::from_secs(30)
        {
            return Err(Error::BeaconApi {
                status: 404,
                message: message.clone(),
            });
        }
        let (anchor_slot, anchor_root) = self.input_anchor(epoch).await?;
        let fetched = match body {
            Some(_) => self.post_response(path, &indices).await,
            None => self.get_input_response(epoch, path).await,
        };
        let response: serde_json::Value = match fetched {
            Ok(response) => response.json().await?,
            Err(Error::BeaconApi {
                status: 404,
                message,
            }) => {
                self.unavailable_inputs
                    .lock()
                    .await
                    .put(key, (Instant::now(), message.clone()));
                return Err(Error::BeaconApi {
                    status: 404,
                    message,
                });
            }
            Err(error) => return Err(error),
        };
        self.unavailable_inputs.lock().await.pop(&key);
        let parsed: BeaconResponse<T> = serde_json::from_value(response.clone())?;
        parsed.ensure_not_optimistic()?;
        if let Some(dependency) = &parsed.dependent_root {
            self.get_header(dependency.as_str()).await?;
        }
        let input = CachedInput {
            anchor_slot,
            anchor_root,
            response,
        };
        // Recompute boundary as well: a previously skipped boundary slot could
        // become occupied in a reorg while its earlier anchor stays canonical.
        if self.input_anchor(epoch).await? != (anchor_slot, input.anchor_root.clone()) {
            return Err(Error::InconsistentBeaconData(
                "input dependency changed during request".into(),
            ));
        }
        if let Some(pool) = &self.input_pool {
            sqlx::query(
                "INSERT INTO beacon_inputs (input_key, epoch, anchor_slot, anchor_root, response) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT (input_key) DO UPDATE SET \
                 anchor_slot = EXCLUDED.anchor_slot, anchor_root = EXCLUDED.anchor_root, \
                 response = EXCLUDED.response",
            )
            .bind(&key)
            .bind(epoch as i64)
            .bind(anchor_slot as i64)
            .bind(input.anchor_root.as_str())
            .bind(serde_json::to_string(&input.response)?)
            .execute(pool)
            .await?;
        }
        self.input_cache.write().await.put(key, input);
        Ok(parsed)
    }

    async fn get_input_response(&self, epoch: u64, path: &str) -> Result<reqwest::Response> {
        let result = self.get_response(path).await;
        // Committee assignment for the current/previous epoch can be obtained
        // from a recent state even when the exact epoch-boundary state is pruned.
        // Keep the stable epoch cache key and all normal anchor/optimism checks.
        if matches!(result, Err(Error::BeaconApi { status: 404, .. }))
            && path
                == format!(
                    "/eth/v1/beacon/states/{}/committees?epoch={epoch}",
                    epoch_start_slot(epoch)
                )
        {
            let head = self.get_head_header().await?;
            let head_epoch = crate::chain::slot_to_epoch(head.header.message.slot);
            if epoch >= head_epoch.saturating_sub(1) && epoch <= head_epoch {
                let response = self
                    .get_response(&format!(
                        "/eth/v1/beacon/states/{}/committees?epoch={epoch}",
                        head.header.message.state_root
                    ))
                    .await?;
                let after = self
                    .get_header(&head.header.message.slot.to_string())
                    .await?;
                if after.data.root != head.root || !after.data.canonical {
                    return Err(Error::InconsistentBeaconData(
                        "Committee fallback branch changed".into(),
                    ));
                }
                return Ok(response);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    #[derive(Clone, Default)]
    struct MockState {
        reorg: Arc<AtomicBool>,
        pruned: Arc<AtomicBool>,
        requests: Arc<AtomicUsize>,
    }

    async fn header(State(state): State<MockState>) -> Json<serde_json::Value> {
        let root = if state.reorg.load(Ordering::SeqCst) {
            "22"
        } else {
            "11"
        };
        let root = format!("0x{}", root.repeat(32));
        Json(serde_json::json!({"execution_optimistic":false,"data":{
            "root": root, "canonical":true,"header":{"message":{
                "slot":"32", "proposer_index":"42", "parent_root":root,
                "state_root":root,"body_root":root
            }}}
        }))
    }

    async fn duties(
        State(state): State<MockState>,
    ) -> std::result::Result<Json<serde_json::Value>, StatusCode> {
        state.requests.fetch_add(1, Ordering::SeqCst);
        if state.pruned.load(Ordering::SeqCst) {
            return Err(StatusCode::NOT_FOUND);
        }
        let duties: Vec<_> = (32..64)
            .map(|slot| {
                serde_json::json!({
                    "pubkey":"0x01", "validator_index":"42", "slot":slot.to_string()
                })
            })
            .collect();
        Ok(Json(
            serde_json::json!({"execution_optimistic":false,"data":duties}),
        ))
    }

    #[tokio::test]
    #[ignore = "requires a disposable RECOVERY_TEST_DATABASE_URL"]
    async fn durable_inputs_survive_restart_and_reject_reorg_after_pruning() {
        use sqlx::{Connection, Executor};
        let url = std::env::var("RECOVERY_TEST_DATABASE_URL").unwrap();
        let schema = format!("inputs_{}", uuid::Uuid::new_v4().simple());
        let mut admin = sqlx::PgConnection::connect(&url).await.unwrap();
        admin
            .execute(format!("CREATE SCHEMA {schema}").as_str())
            .await
            .unwrap();
        let options: sqlx::postgres::PgConnectOptions = url.parse().unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.options([("search_path", schema.as_str())]))
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let state = MockState::default();
        let app = Router::new()
            .route("/eth/v1/beacon/headers/{id}", get(header))
            .route("/eth/v1/validator/duties/proposer/1", get(duties))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = format!("http://{address}");
        let first = BeaconClient::new(&base).with_pool(pool.clone());
        first.get_proposer_duties(1).await.unwrap();
        drop(first);
        state.pruned.store(true, Ordering::SeqCst);
        let restarted = BeaconClient::new(&base).with_pool(pool.clone());
        assert_eq!(
            restarted.get_proposer_duties(1).await.unwrap()[0].validator_index,
            42
        );
        assert_eq!(state.requests.load(Ordering::SeqCst), 1);
        state.reorg.store(true, Ordering::SeqCst);
        assert!(restarted.get_proposer_duties(1).await.is_err());
        assert_eq!(state.requests.load(Ordering::SeqCst), 2);
        server.abort();
        pool.close().await;
        admin
            .execute(format!("DROP SCHEMA {schema} CASCADE").as_str())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn committee_fallback_is_recent_pinned_and_cached() {
        use axum::{extract::Request, response::IntoResponse};
        let state = MockState::default();
        let app_state = state.clone();
        let app = Router::new().fallback(move |request: Request| {
            let state = app_state.clone();
            async move {
                let path = request.uri().path();
                if path.starts_with("/eth/v1/beacon/headers/") {
                    let Json(mut response) = header(State(state)).await;
                    let id = path.rsplit('/').next().unwrap();
                    response["data"]["header"]["message"]["slot"] =
                        if id == "head" { "96" } else { id }.into();
                    return Json(response).into_response();
                }
                if path.contains("/states/0x") {
                    state.requests.fetch_add(1, Ordering::SeqCst);
                    // Simulate a branch switch while fetching the pinned state.
                    if state.pruned.load(Ordering::SeqCst) {
                        state.reorg.store(true, Ordering::SeqCst);
                    }
                    return Json(serde_json::json!({"execution_optimistic":false,
                        "data":[{"index":"0","slot":"64","validators":["42"]}]}))
                    .into_response();
                }
                StatusCode::NOT_FOUND.into_response()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = BeaconClient::new(&format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        assert_eq!(client.get_committees(2).await.unwrap()[0].slot, 64);
        client.get_committees(2).await.unwrap();
        assert_eq!(state.requests.load(Ordering::SeqCst), 1);
        // An old archive request must not silently use the head's committee set.
        assert!(matches!(
            client.get_committees(1).await,
            Err(Error::BeaconApi { status: 404, .. })
        ));
        assert_eq!(state.requests.load(Ordering::SeqCst), 1);
        client.invalidate_duty_caches().await;
        state.pruned.store(true, Ordering::SeqCst);
        assert!(matches!(
            client.get_committees(2).await,
            Err(Error::InconsistentBeaconData(_))
        ));
        server.abort();
    }

    #[tokio::test]
    async fn unavailable_inputs_share_retry_delay_and_recover_after_expiry() {
        let state = MockState::default();
        state.pruned.store(true, Ordering::SeqCst);
        let app = Router::new()
            .route("/eth/v1/beacon/headers/{id}", get(header))
            .route("/eth/v1/validator/duties/proposer/1", get(duties))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = BeaconClient::new(&format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for _ in 0..12 {
            assert!(matches!(
                client.get_proposer_duties(1).await,
                Err(Error::BeaconApi { status: 404, .. })
            ));
        }
        assert_eq!(state.requests.load(Ordering::SeqCst), 1);
        state.pruned.store(false, Ordering::SeqCst);
        for (_, (since, _)) in client.unavailable_inputs.lock().await.iter_mut() {
            *since = Instant::now() - Duration::from_secs(31);
        }
        assert!(client.get_proposer_duties(1).await.is_ok());
        assert_eq!(state.requests.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn missing_anchor_is_unavailable_and_search_is_bounded() {
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let app = Router::new()
            .route(
                "/eth/v1/beacon/headers/head",
                get(|| async {
                    let Json(mut response) = header(State(MockState::default())).await;
                    response["data"]["header"]["message"]["slot"] = "320".into();
                    Json(response)
                }),
            )
            .route(
                "/eth/v1/beacon/headers/{id}",
                get(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async { StatusCode::NOT_FOUND }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = BeaconClient::new(&format!("http://{address}"));
        assert!(matches!(
            client.input_anchor(3).await,
            Err(Error::BeaconDataUnavailable(_))
        ));
        assert_eq!(
            requests.load(Ordering::SeqCst),
            (crate::chain::slots_per_epoch() * 2 + 1) as usize
        );
        // A missing genesis anchor has the same classification, with no underflow.
        assert!(matches!(
            client.input_anchor(0).await,
            Err(Error::BeaconDataUnavailable(_))
        ));
        assert_eq!(
            requests.load(Ordering::SeqCst),
            (crate::chain::slots_per_epoch() * 2 + 2) as usize
        );
        server.abort();
    }

    #[tokio::test]
    async fn reuses_inputs_after_pruning_but_not_after_changed_anchor() {
        let state = MockState::default();
        let app = Router::new()
            .route("/eth/v1/beacon/headers/{id}", get(header))
            .route("/eth/v1/validator/duties/proposer/1", get(duties))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = BeaconClient::new(&format!("http://{address}"));
        assert_eq!(
            client.get_proposer_duties(1).await.unwrap()[0].validator_index,
            42
        );
        state.pruned.store(true, Ordering::SeqCst);
        assert!(client.get_proposer_duties(1).await.is_ok());
        assert_eq!(state.requests.load(Ordering::SeqCst), 1);
        state.reorg.store(true, Ordering::SeqCst);
        assert!(client.get_proposer_duties(1).await.is_err());
        assert_eq!(state.requests.load(Ordering::SeqCst), 2);
        server.abort();
    }
}
