use std::collections::HashSet;

use crate::beacon_client::BeaconClient;
use crate::beacon_client::types::FinalizedCheckpointEvent;
use crate::config::EffectiveScanMode;
use crate::db::Pool as PgPool;
use crate::db::scanner as db_scanner;
use crate::error::Result;
use crate::scanner;

pub(super) async fn process_finalized_rescan(
    client: &BeaconClient,
    pool: &PgPool,
    scan_validators: &HashSet<u64>,
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

    let tracked_indices: Vec<i64> = scan_validators.iter().map(|&v| v as i64).collect();

    let rescan_started_at = std::time::Instant::now();
    for epoch in from_epoch..=finalized.epoch {
        let active =
            db_scanner::validators::active_validators_at(pool, &tracked_indices, epoch as i64)
                .await?;
        if active.is_empty() {
            continue;
        }
        scanner::scan_epoch(client, pool, epoch, &active, true, scan_mode)
            .await
            .map_err(|e| {
                tracing::error!(epoch, error = %e, "Failed to re-scan finalized epoch; aborting");
                e
            })?;
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
    let indices: Vec<i64> = db_scanner::validators::active_validators_at(
        pool,
        &tracked_indices,
        finalized.epoch as i64,
    )
    .await?
    .into_iter()
    .map(|v| v as i64)
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
