//! Historical backfill: scan every epoch from each validator's start point up to
//! a fixed finality target. Designed to run alongside live tracking — the caller
//! freezes the target at startup, and live owns everything past it.
//!
//! The backfill task never touches epochs beyond `target_finalized_epoch`; it
//! writes every row with `finalized = true`, making its output immune to the
//! live head-tracker's reorg deletes and upsert guards (see
//! [`crate::scanner::scan_epoch`]).

use std::collections::{HashMap, HashSet};

use crate::db::Pool as PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::beacon_client::BeaconClient;
use crate::chain;
use crate::config::Config;
use crate::db;
use crate::error::{Error, Result};
use crate::live_updates::LiveUpdateEvent;
use crate::scanner;

/// Earliest epoch the backfill task will try to scan given the current
/// validator watermarks and config. Returns `None` if the scan state is
/// empty or every tracked validator has already exited before its start
/// epoch (nothing to backfill).
pub fn earliest_epoch_to_scan(
    config: &Config,
    validator_scan_state: &HashMap<u64, (u64, Option<u64>, Option<u64>)>,
) -> Option<u64> {
    let non_contig = config.non_contiguous_backfill;
    let compute_start = |activation: u64, last_scanned: Option<u64>| -> u64 {
        let base = if non_contig {
            activation
        } else {
            last_scanned.map_or(activation, |last| (last + 1).max(activation))
        };
        config.max_backfill_depth.map_or(base, |d| base.max(d))
    };
    validator_scan_state
        .values()
        .filter_map(|(act, ls, exit)| {
            let start = compute_start(*act, *ls);
            match exit {
                Some(e) if start >= *e => None,
                _ => Some(start),
            }
        })
        .min()
}

/// Probe whether `client` can serve state-derived queries for `epoch`.
///
/// - `Ok(true)`: node returned the state (archival-capable at this depth, or
///   the epoch is within a non-archive node's retention window).
/// - `Ok(false)`: node returned 404 — the state has been pruned, so backfill
///   at this epoch will fail.
/// - `Err`: transport / non-404 API error. The caller decides whether to
///   treat this as fatal or just "proceed and let the real run fail loudly."
pub async fn probe_archival_capability(client: &BeaconClient, epoch: u64) -> Result<bool> {
    if epoch == 0 {
        // Genesis state is always available on every client.
        return Ok(true);
    }
    let state_id = (epoch * chain::slots_per_epoch()).to_string();
    match client.get_finality_checkpoints(&state_id).await {
        Ok(_) => Ok(true),
        Err(Error::BeaconApi { status: 404, .. }) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Backfill historical epochs up to (and optionally beyond) an initial finality
/// target.
///
/// - `initial_target_epoch`: upper bound for the first pass.
/// - `extend_on_finality_advance`: if `true`, after catching up to the current
///   target the function re-reads finality and runs another pass if the chain
///   has advanced. This is the `--backfill-only` mode where backfill is the
///   sole catch-up mechanism. In concurrent mode live handles new finality,
///   so callers pass `false` and the function exits after one pass.
///
/// The non-contiguous pre-pass (when `config.non_contiguous_backfill` is set)
/// runs once before the first contiguous pass and is not repeated on extension.
#[allow(clippy::too_many_arguments)]
pub async fn run_backfill(
    client: &BeaconClient,
    pool: &PgPool,
    config: &Config,
    mut validator_scan_state: HashMap<u64, (u64, Option<u64>, Option<u64>)>,
    initial_target_epoch: u64,
    instance_id: Uuid,
    live_updates_tx: broadcast::Sender<LiveUpdateEvent>,
    extend_on_finality_advance: bool,
) -> Result<()> {
    let mut non_contiguous_pending = config.non_contiguous_backfill;
    let mut target_finalized_epoch = initial_target_epoch;
    let scan_mode = config.scan_mode.resolve(validator_scan_state.len());
    tracing::info!(
        scan_mode = ?scan_mode,
        validator_count = validator_scan_state.len(),
        "Backfill resolved attestation scan mode"
    );

    // RAII guard flips `backfill_active` back to 0 on every exit path (success
    // and error), so a `?`-triggered early return doesn't leave a stale gauge.
    struct ActiveGuard;
    impl Drop for ActiveGuard {
        fn drop(&mut self) {
            crate::metrics::BACKFILL_ACTIVE.set(0);
        }
    }
    crate::metrics::BACKFILL_ACTIVE.set(1);
    let _active_guard = ActiveGuard;

    loop {
        let this_pass_non_contiguous = non_contiguous_pending;
        crate::metrics::BACKFILL_TARGET_EPOCH.set(target_finalized_epoch as i64);
        tracing::info!(
            target_finalized_epoch,
            non_contiguous = this_pass_non_contiguous,
            "Starting backfill pass"
        );
        let pass_start = std::time::Instant::now();

        // Activation floors the start even in contiguous mode, so a watermark
        // advanced past activation (e.g. by finalization fired before the
        // validator was active) can't make us scan pre-activation epochs.
        let compute_start = |activation: u64, last_scanned: Option<u64>| -> u64 {
            let base = if this_pass_non_contiguous {
                activation
            } else {
                last_scanned.map_or(activation, |last| (last + 1).max(activation))
            };
            config.max_backfill_depth.map_or(base, |d| base.max(d))
        };

        let min_start = validator_scan_state
            .values()
            .filter_map(|(activation, last_scanned, exit)| {
                let start = compute_start(*activation, *last_scanned);
                match exit {
                    Some(e) if start >= *e => None,
                    _ => Some(start),
                }
            })
            .min()
            .unwrap_or(target_finalized_epoch + 1);

        if min_start > target_finalized_epoch {
            tracing::info!("All validators up to date, no backfill needed");
            break;
        }

        crate::metrics::BACKFILL_MIN_START.set(min_start as i64);
        let total_epochs = target_finalized_epoch - min_start + 1;
        tracing::info!(
            from = min_start,
            to = target_finalized_epoch,
            total_epochs,
            "Backfill range"
        );

        let mut epochs_scanned: u64 = 0;
        let mut epochs_skipped_covered: u64 = 0;

        for epoch in min_start..=target_finalized_epoch {
            let candidates: HashSet<u64> = validator_scan_state
                .iter()
                .filter_map(|(idx, (activation, last_scanned, exit))| {
                    // Skip validators already exited at `epoch` — scanning them would
                    // only record misses.
                    if exit.is_some_and(|e| epoch >= e) {
                        return None;
                    }
                    (epoch >= compute_start(*activation, *last_scanned)).then_some(*idx)
                })
                .collect();

            if candidates.is_empty() {
                continue;
            }

            let scan_validators: HashSet<u64> = if this_pass_non_contiguous {
                let candidate_ids: Vec<i64> = candidates.iter().map(|&v| v as i64).collect();
                let covered = db::scanner::attestations::validators_with_finalized_attestation(
                    pool,
                    &candidate_ids,
                    epoch as i64,
                )
                .await?;
                let needs: HashSet<u64> = candidates
                    .into_iter()
                    .filter(|idx| !covered.contains(&(*idx as i64)))
                    .collect();
                tracing::debug!(
                    epoch,
                    covered = covered.len(),
                    to_scan = needs.len(),
                    "Non-contiguous gap check"
                );
                needs
            } else {
                candidates
            };

            if scan_validators.is_empty() {
                epochs_skipped_covered += 1;
                crate::metrics::BACKFILL_EPOCHS_SKIPPED.inc();
                continue;
            }

            scanner::scan_epoch(client, pool, epoch, &scan_validators, true, scan_mode)
                .await
                .map_err(|e| {
                    tracing::error!(
                        epoch,
                        error = %e,
                        "Backfill epoch scan failed; aborting to prevent inconsistent data"
                    );
                    e
                })?;

            // GREATEST semantics on the DB watermark mean a non-contiguous pass
            // scanning an older epoch won't rewind a higher last_scanned.
            let scan_validator_indices: Vec<i64> =
                scan_validators.iter().map(|&v| v as i64).collect();
            db::scanner::validators::update_validators_scanned_epoch(
                pool,
                &scan_validator_indices,
                epoch as i64,
            )
            .await?;
            for idx in &scan_validators {
                if let Some((_, last_scanned, _)) = validator_scan_state.get_mut(idx) {
                    *last_scanned = Some(last_scanned.map_or(epoch, |prev| prev.max(epoch)));
                }
            }

            let _ = live_updates_tx.send(LiveUpdateEvent::BackfillEpochProcessed);
            epochs_scanned += 1;
            crate::metrics::BACKFILL_EPOCHS_SCANNED.inc();

            if epoch % 10 == 0 {
                db::scanner::instance::update_heartbeat(pool, instance_id).await?;
                let progress_pct = ((epoch - min_start + 1) as f64 / total_epochs as f64) * 100.0;
                tracing::info!(
                    epoch,
                    total = target_finalized_epoch,
                    progress = format!("{:.1}%", progress_pct),
                    scanned = epochs_scanned,
                    skipped = epochs_skipped_covered,
                    "Backfill progress"
                );
            }
        }

        let pass_kind = if this_pass_non_contiguous {
            "non_contiguous"
        } else {
            "contiguous"
        };
        crate::metrics::BACKFILL_PASS_DURATION
            .with_label_values(&[pass_kind])
            .observe(pass_start.elapsed().as_secs_f64());
        tracing::info!(
            through_epoch = target_finalized_epoch,
            epochs_scanned,
            epochs_skipped_covered,
            "Backfill pass complete"
        );

        if this_pass_non_contiguous {
            // The sweep verified every (validator, epoch) pair up to the target.
            // Advance last_scanned for active validators so the next (contiguous)
            // pass resumes from target + 1.
            let active_ids: Vec<i64> = validator_scan_state
                .iter()
                .filter(|(_, (activation, _, exit))| {
                    *activation <= target_finalized_epoch
                        && exit.is_none_or(|e| target_finalized_epoch < e)
                })
                .map(|(&idx, _)| idx as i64)
                .collect();
            if !active_ids.is_empty() {
                db::scanner::validators::update_validators_scanned_epoch(
                    pool,
                    &active_ids,
                    target_finalized_epoch as i64,
                )
                .await?;
            }
            for (_, (activation, last_scanned, exit)) in validator_scan_state.iter_mut() {
                if *activation <= target_finalized_epoch
                    && exit.is_none_or(|e| target_finalized_epoch < e)
                {
                    *last_scanned = Some(
                        last_scanned
                            .map_or(target_finalized_epoch, |ls| ls.max(target_finalized_epoch)),
                    );
                }
            }
            non_contiguous_pending = false;
            tracing::info!(
                through_epoch = target_finalized_epoch,
                "Non-contiguous sweep finished; switching to contiguous catchup"
            );
            continue;
        }

        // Contiguous pass complete. Either we're done, or (in backfill-only
        // mode) the chain advanced while we were catching up and we should
        // pick up the new epochs too.
        if !extend_on_finality_advance {
            break;
        }
        let new_target = client
            .get_finality_checkpoints("head")
            .await?
            .finalized
            .epoch;
        if new_target <= target_finalized_epoch {
            tracing::info!(
                last_backfilled_epoch = target_finalized_epoch,
                "Backfill caught up to live finality"
            );
            break;
        }
        tracing::info!(
            from = target_finalized_epoch + 1,
            to = new_target,
            "Finality advanced during backfill; extending target"
        );
        target_finalized_epoch = new_target;
    }

    Ok(())
}
