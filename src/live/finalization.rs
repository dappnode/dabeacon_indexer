use std::collections::{HashMap, HashSet};

use crate::beacon_client::BeaconClient;
use crate::beacon_client::types::FinalizedCheckpointEvent;
use crate::config::EffectiveScanMode;
use crate::db::Pool as PgPool;
use crate::db::scanner as db_scanner;
use crate::error::{Error, Result};
use crate::exits;
use crate::scanner;

fn is_beacon_request_failure(e: &Error) -> bool {
    matches!(e, Error::Http(_) | Error::BeaconApi { .. })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn process_finalized_rescan(
    client: &BeaconClient,
    backfill_client: Option<&BeaconClient>,
    pool: &PgPool,
    scan_validators: &HashSet<u64>,
    validator_exits: &HashMap<u64, u64>,
    scan_mode: EffectiveScanMode,
    finalized: &FinalizedCheckpointEvent,
    last_finalized_rescanned_epoch: &mut u64,
) -> Result<()> {
    tracing::info!(
        epoch = finalized.epoch,
        block = %finalized.block,
        "Finalization checkpoint"
    );

    if finalized.epoch <= *last_finalized_rescanned_epoch {
        tracing::debug!(
            finalized_epoch = finalized.epoch,
            last_finalized_rescanned_epoch = *last_finalized_rescanned_epoch,
            "Skipping finalized rescan; epoch already processed"
        );
        return Ok(());
    }

    let from_epoch = last_finalized_rescanned_epoch.saturating_add(1);
    tracing::info!(
        from_epoch,
        to_epoch = finalized.epoch,
        "Catching up finalized rescans"
    );

    let rescan_started_at = std::time::Instant::now();
    for epoch in from_epoch..=finalized.epoch {
        let active = exits::active_at(scan_validators, validator_exits, epoch);
        if active.is_empty() {
            continue;
        }
        match scanner::scan_epoch(client, pool, epoch, &active, true, scan_mode).await {
            Ok(()) => {}
            Err(e) if is_beacon_request_failure(&e) && backfill_client.is_some() => {
                let bf = backfill_client.expect("checked above");
                tracing::warn!(
                    epoch,
                    error = %e,
                    "Live client failed re-scan; retrying with backfill client"
                );
                scanner::scan_epoch(bf, pool, epoch, &active, true, scan_mode)
                    .await
                    .map_err(|e2| {
                        tracing::error!(
                            epoch,
                            primary_error = %e,
                            backfill_error = %e2,
                            "Backfill client also failed re-scan; aborting"
                        );
                        e2
                    })?;
            }
            Err(e) => {
                tracing::error!(epoch, error = %e, "Failed to re-scan finalized epoch; aborting");
                return Err(e);
            }
        }
    }
    crate::metrics::LIVE_FINALIZED_RESCAN_DURATION
        .with_label_values(&["rescan_loop"])
        .observe(rescan_started_at.elapsed().as_secs_f64());

    let finalize_started_at = std::time::Instant::now();
    db_scanner::finalization::finalize_up_to_epoch(pool, finalized.epoch as i64)
        .await
        .map_err(|e| {
            tracing::error!(
                epoch = finalized.epoch,
                error = %e,
                "Failed to finalize in DB; aborting"
            );
            e
        })?;
    crate::metrics::LIVE_FINALIZED_RESCAN_DURATION
        .with_label_values(&["finalize_flip"])
        .observe(finalize_started_at.elapsed().as_secs_f64());

    // Only advance watermarks for validators still active at the finalized
    // epoch — exited ones have nothing to scan past their exit.
    let indices: Vec<i64> = scan_validators
        .iter()
        .filter(|&&v| exits::is_active_at(validator_exits, v, finalized.epoch))
        .map(|&v| v as i64)
        .collect();
    db_scanner::validators::update_validators_scanned_epoch(pool, &indices, finalized.epoch as i64)
        .await
        .map_err(|e| {
            tracing::error!(
                epoch = finalized.epoch,
                validators = indices.len(),
                error = %e,
                "Failed to update validator scan watermarks on finalization; aborting"
            );
            e
        })?;

    tracing::debug!(
        epoch = finalized.epoch,
        validators_updated = indices.len(),
        "Updated validator scan watermarks to finalized epoch"
    );

    *last_finalized_rescanned_epoch = finalized.epoch;

    Ok(())
}
