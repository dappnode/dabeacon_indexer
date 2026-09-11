mod attestations;
mod bits;
mod proposals;
mod sync_committee;

pub use attestations::scan_live_attestations_in_slot;
pub use proposals::{
    fetch_live_proposal_rewards, persist_live_proposal_rewards, upsert_live_proposal_in_slot,
};
pub use sync_committee::{
    fetch_live_sync_rewards, persist_live_sync_rewards, upsert_live_sync_in_slot,
};

use std::collections::HashSet;

use crate::beacon_client::BeaconClient;
use crate::chain;
use crate::config::EffectiveScanMode;
use crate::db::Pool as PgPool;
use crate::error::Result;

/// Scan a single epoch using a block-first approach.
///
/// Finalized callers must wait until the entire inclusion/reward window is
/// finalized (see `chain::finalized_scan_target`). Partial writes are retained
/// on failure, but only a successful scan records `completed_scans` entries.
/// Finalized retries can replace incomplete rows; live writes cannot replace
/// finalized rows. This preserves useful live data when historical state is
/// unavailable without mistaking it for a complete scan.
pub async fn scan_epoch(
    client: &BeaconClient,
    pool: &PgPool,
    epoch: u64,
    scan_validators: &HashSet<u64>,
    finalized: bool,
    mode: EffectiveScanMode,
) -> Result<()> {
    let epoch_timer = std::time::Instant::now();

    if scan_validators.is_empty() {
        tracing::trace!(epoch, "Skipping epoch — no validators need it");
        return Ok(());
    }

    let mode_label = match mode {
        EffectiveScanMode::Dense => "dense",
        EffectiveScanMode::Sparse => "sparse",
    };
    let finalized_label = if finalized { "true" } else { "false" };
    crate::metrics::SCANNER_EPOCHS_TOTAL
        .with_label_values(&[mode_label, finalized_label])
        .inc();

    let is_altair = epoch >= chain::altair_epoch();

    tracing::debug!(
        epoch,
        validator_count = scan_validators.len(),
        finalized,
        is_altair,
        scan_mode = ?mode,
        "Starting epoch scan"
    );

    let att_started_at = std::time::Instant::now();
    match mode {
        EffectiveScanMode::Dense => {
            attestations::process_epoch_attestation_duties(
                client,
                pool,
                epoch,
                scan_validators,
                finalized,
            )
            .await?;
        }
        EffectiveScanMode::Sparse => {
            attestations::process_epoch_attestation_duties_sparse(
                client,
                pool,
                epoch,
                scan_validators,
                finalized,
            )
            .await?;
        }
    }
    crate::metrics::SCANNER_PHASE_DURATION
        .with_label_values(&["attestations", mode_label, finalized_label])
        .observe(att_started_at.elapsed().as_secs_f64());

    tracing::debug!(epoch, "Processing epoch proposals");
    let prop_started_at = std::time::Instant::now();
    proposals::process_epoch_proposals(client, pool, epoch, scan_validators, finalized).await?;
    crate::metrics::SCANNER_PHASE_DURATION
        .with_label_values(&["proposals", mode_label, finalized_label])
        .observe(prop_started_at.elapsed().as_secs_f64());

    if is_altair {
        let sync_started_at = std::time::Instant::now();
        sync_committee::process_epoch_sync(client, pool, epoch, scan_validators, finalized).await?;
        crate::metrics::SCANNER_PHASE_DURATION
            .with_label_values(&["sync_committee", mode_label, finalized_label])
            .observe(sync_started_at.elapsed().as_secs_f64());
    } else {
        tracing::trace!(epoch, "Pre-Altair epoch, skipping sync committee");
    }

    if finalized {
        let indices: Vec<i64> = scan_validators.iter().map(|&v| v as i64).collect();
        crate::db::scanner::completion::mark_complete(pool, &indices, epoch as i64).await?;
    }

    let elapsed = epoch_timer.elapsed();
    crate::metrics::SCANNER_EPOCH_DURATION
        .with_label_values(&[mode_label, finalized_label])
        .observe(elapsed.as_secs_f64());
    tracing::debug!(
        epoch,
        elapsed_ms = elapsed.as_millis() as u64,
        "Epoch scan complete"
    );
    Ok(())
}

/// Fetch reward data without historical duties, committees, or block scanning.
/// The caller validates the epoch boundary before/after this request and stages
/// the response before joining it to duty rows. E needs end-of-E+1 state; the
/// result remains branch-dependent until checkpoint E+2 is finalized.
pub async fn fetch_live_attestation_rewards(
    client: &BeaconClient,
    epoch: u64,
    validators: &HashSet<u64>,
) -> Result<
    crate::beacon_client::types::BeaconResponse<
        crate::beacon_client::types::AttestationRewardsResponse,
    >,
> {
    let indices: Vec<u64> = validators.iter().copied().collect();
    if indices.is_empty() {
        return Err(crate::error::Error::InconsistentBeaconData(
            "refusing an unfiltered live reward request".into(),
        ));
    }
    let response = client
        .get_attestation_rewards_response(epoch, &indices)
        .await?;
    validate_reward_indices(
        validators,
        response
            .data
            .total_rewards
            .iter()
            .map(|r| r.validator_index),
    )?;
    Ok(response)
}

fn validate_reward_indices(
    expected: &HashSet<u64>,
    actual: impl Iterator<Item = u64>,
) -> Result<()> {
    let mut seen = HashSet::new();
    for index in actual {
        if !expected.contains(&index) || !seen.insert(index) {
            return Err(crate::error::Error::InconsistentBeaconData(
                "unexpected or duplicate validator in reward response".into(),
            ));
        }
    }
    if &seen != expected {
        return Err(crate::error::Error::InconsistentBeaconData(
            "missing validator in reward response".into(),
        ));
    }
    Ok(())
}

/// Join an already validated and durably staged response to existing duties.
/// Missing duties do not discard the staged response: the live worker replays
/// this join after assignments/inclusions become available.
pub async fn persist_live_attestation_rewards(
    pool: &PgPool,
    epoch: u64,
    rewards: &crate::beacon_client::types::AttestationRewardsResponse,
) -> Result<()> {
    let tuples: Vec<crate::db::scanner::attestations::RewardTuple> = rewards
        .total_rewards
        .iter()
        .map(|r| {
            (
                r.validator_index as i64,
                epoch as i64,
                Some(r.source),
                Some(r.target),
                Some(r.head),
                r.inactivity,
            )
        })
        .collect();
    crate::db::scanner::attestations::update_attestation_rewards_batch(pool, &tuples).await
}

/// Create pending assignments before an inclusion is observed. `included=false`
/// is provisional until full inclusion coverage earns the completion marker;
/// callers must expose incomplete epochs as unknown rather than confirmed misses.
pub async fn seed_live_attestation_duties(
    client: &BeaconClient,
    pool: &PgPool,
    epoch: u64,
    validators: &HashSet<u64>,
) -> Result<()> {
    let indices: Vec<u64> = validators.iter().copied().collect();
    if indices.is_empty() {
        return Ok(());
    }
    let duties = client.get_attester_duties(epoch, &indices).await?;
    validate_reward_indices(validators, duties.iter().map(|d| d.validator_index))?;
    for duty in duties {
        crate::db::scanner::attestations::upsert_attestation_duty(
            pool,
            duty.validator_index as i64,
            epoch as i64,
            duty.slot as i64,
            duty.committee_index as i32,
            duty.validator_committee_index as i32,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod live_reward_tests {
    use super::*;
    use axum::{
        Json, Router,
        http::StatusCode,
        routing::{get, post},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn reward_response_requires_exactly_one_result_per_requested_validator() {
        let expected = HashSet::from([42, 43]);
        assert!(validate_reward_indices(&expected, [42, 43].into_iter()).is_ok());
        assert!(validate_reward_indices(&expected, [42].into_iter()).is_err());
        assert!(validate_reward_indices(&expected, [42, 42, 43].into_iter()).is_err());
        assert!(validate_reward_indices(&expected, [42, 43, 44].into_iter()).is_err());
    }

    #[tokio::test]
    async fn available_rewards_do_not_depend_on_pruned_committees_or_duties() {
        let historical_requests = Arc::new(AtomicUsize::new(0));
        let historical_counter = historical_requests.clone();
        let app = Router::new()
            .route(
                "/eth/v1/beacon/rewards/attestations/{epoch}",
                post(|| async {
                    Json(serde_json::json!({
                        "execution_optimistic": false,
                        "finalized": false,
                        "data": {"ideal_rewards": [], "total_rewards": [{
                            "validator_index": "42", "source": "10", "target": "20", "head": "5"
                        }]}
                    }))
                }),
            )
            .fallback(get(move || {
                let counter = historical_counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NOT_FOUND
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = BeaconClient::new(&format!("http://{address}"));
        let response = fetch_live_attestation_rewards(&client, 100, &HashSet::from([42]))
            .await
            .unwrap();
        assert_eq!(response.data.total_rewards[0].target, 20);
        assert_eq!(historical_requests.load(Ordering::SeqCst), 0);
        server.abort();
    }
}
