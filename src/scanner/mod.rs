mod attestations;
mod bits;
mod proposals;
mod sync_committee;

pub use attestations::scan_live_attestations_in_slot;
pub use proposals::upsert_live_proposal_in_slot;
pub use sync_committee::upsert_live_sync_in_slot;

use std::collections::HashSet;

use crate::beacon_client::BeaconClient;
use crate::chain;
use crate::config::EffectiveScanMode;
use crate::db::Pool as PgPool;
use crate::error::Result;

/// Scan a single epoch using a block-first approach.
///
/// # `finalized` parameter — load-bearing invariant
///
/// `finalized` is written verbatim to the `finalized` column of every row this
/// call upserts (`attestation_duties`, `sync_duties`, `block_proposals`). Two
/// cross-instance guarantees depend on backfill callers passing `true`:
///
/// - **Reorg safety**: [`crate::db::scanner::finalization::delete_non_finalized_slots`]
///   wipes every row with `finalized = FALSE` in a slot range. Backfill rows
///   that advertise `finalized = true` are immune, so an archive backfiller
///   and a live head-tracker can coexist on the same DB without the latter's
///   reorg handler destroying the former's work.
/// - **Upsert precedence**: the `ON CONFLICT` clauses on all three duty tables
///   guard on `… .finalized = FALSE`. A finalized row is immutable; a live
///   write never clobbers it. This is what lets a non-archive head-tracker and
///   an archive backfiller target the same validator set safely.
///
/// **Rule**: callers performing historical backfill (post-finality epochs)
/// MUST pass `finalized = true`. Callers tracking the head MUST pass
/// `finalized = false` and rely on
/// [`crate::db::scanner::finalization::finalize_up_to_epoch`] to promote their rows once
/// the chain finalizes past them. Violating this breaks multi-instance
/// coordination silently — neither the DB nor the type system will catch it.
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
