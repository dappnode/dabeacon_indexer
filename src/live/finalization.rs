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

    // --- Check-and-promote: only re-scan epochs whose rewards are missing ---
    // The epoch-transition eager fetch (in the head handler) writes rewards
    // with `finalized = false` as soon as an epoch completes. For those
    // epochs, finalization only needs to flip the `finalized` flag — no
    // beacon API calls required. If the eager fetch was missed (downtime,
    // SSE reconnect), we fall back to a full scan_epoch here.
    //
    // When rewards ARE present, we still attempt a full finalized rescan as
    // a best-effort upgrade: dense-mode vote correctness, inclusion_slot for
    // late-epoch attestations, and authoritative finalized rows. If the
    // beacon state is already pruned, the attempt fails silently and the
    // eager-fetch data (already present) is promoted as-is.
    let rescan_started_at = std::time::Instant::now();
    for epoch in from_epoch..=finalized.epoch {
        let active =
            db_scanner::validators::active_validators_at(pool, &tracked_indices, epoch as i64)
                .await?;
        if active.is_empty() {
            continue;
        }

        let has_rewards = scanner::epoch_has_rewards(pool, epoch)
            .await
            .unwrap_or(false);

        match scanner::scan_epoch(client, pool, epoch, &active, true, scan_mode).await {
            Ok(()) => {
                tracing::info!(epoch, "Finalized rescan succeeded");
            }
            Err(e) => {
                if has_rewards {
                    // Eager-fetch data is present — the failure only means we
                    // couldn't upgrade to authoritative finalized data (e.g.
                    // dense vote correctness, missing inclusion_slots). This
                    // is acceptable; the eager data will be promoted as-is.
                    tracing::debug!(
                        epoch,
                        error = %e,
                        "Finalized rescan failed but eager-fetch rewards are present; \
                         promoting existing data"
                    );
                } else {
                    // No rewards at all — state was pruned and eager fetch was
                    // missed. The head tracker's duty rows (inclusions, proposals,
                    // sync participation) will be promoted without reward data.
                    tracing::warn!(
                        epoch,
                        error = %e,
                        "Finalized rescan failed and no eager-fetch rewards; \
                         epoch will be finalized without reward data"
                    );
                    crate::metrics::LIVE_EPOCHS_INCOMPLETE.inc();
                }
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
