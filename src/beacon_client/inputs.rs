//! Durable inputs are reusable only while their canonical anchor is unchanged.
//! We validate block headers, not historical states, so pruning states does not
//! prevent reuse after restart. Raw JSON retains endpoint metadata verbatim.
use serde::de::DeserializeOwned;
use sqlx::PgPool;

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
                Err(Error::BeaconApi { status: 404, .. }) if slot > 0 => slot -= 1,
                Err(error) => return Err(error),
            }
            // Do not turn a long unavailable range into an unbounded request loop.
            if boundary - slot > crate::chain::slots_per_epoch() * 2 {
                return Err(Error::InconsistentBeaconData(
                    "input anchor unavailable".into(),
                ));
            }
        }
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
        let (anchor_slot, anchor_root) = self.input_anchor(epoch).await?;
        let response: serde_json::Value = match body {
            Some(_) => self.post_response(path, &indices).await?.json().await?,
            None => self.get_response(path).await?.json().await?,
        };
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
