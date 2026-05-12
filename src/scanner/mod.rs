mod attestations;
mod bits;
mod proposals;
mod sync_committee;

pub use attestations::scan_live_attestations_in_slot;
pub use proposals::upsert_live_proposal_in_slot;
pub use sync_committee::upsert_live_sync_in_slot;

use std::collections::HashSet;

use sqlx::Row;

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

/// Eagerly fetch rewards for a completed epoch while the beacon node still
/// has its state. Called with a 1-epoch lag: rewards for epoch N are fetched
/// at the epoch N+1→N+2 boundary. This ensures late-included attestations
/// from epoch N (which may be included in blocks during epoch N+1) are
/// reflected in the participation flags the rewards API reads.
///
/// This function intentionally uses sparse-mode attestation logic regardless
/// of the configured scan mode: sparse doesn't need the inclusion-window
/// blocks (which may not fully exist yet at the epoch boundary), and the
/// head tracker has already recorded inclusion data from block bodies.
///
/// Rows already written by the head tracker (with `inclusion_slot` set) may
/// reject the full upsert via the `ON CONFLICT … inclusion_slot` guard, so
/// we follow up with a targeted `update_attestation_rewards_batch` to fill
/// in just the reward columns on those rows.
///
/// Proposals and sync duties use `finalized = false` upserts — their ON
/// CONFLICT clauses allow overwriting non-finalized rows freely.
pub async fn process_epoch_rewards(
    client: &BeaconClient,
    pool: &PgPool,
    epoch: u64,
    scan_validators: &HashSet<u64>,
    head_slot: u64,
) -> Result<()> {
    if scan_validators.is_empty() {
        return Ok(());
    }

    let timer = std::time::Instant::now();
    let is_altair = epoch >= chain::altair_epoch();

    tracing::info!(
        epoch,
        validator_count = scan_validators.len(),
        is_altair,
        head_slot,
        "Eager epoch-transition reward fetch"
    );

    let scan_validator_indices: Vec<u64> = scan_validators.iter().copied().collect();

    // --- Attestation rewards ---
    // Always use sparse: it doesn't need the full inclusion window of blocks.
    // The full upsert handles new rows; the batch UPDATE handles rows the head
    // tracker already wrote (where the upsert's inclusion_slot guard rejects).
    let att_t = std::time::Instant::now();
    attestations::process_epoch_attestation_duties_sparse(
        client,
        pool,
        epoch,
        scan_validators,
        false,
    )
    .await?;

    // Fetch rewards again for the batch UPDATE on rows the upsert skipped.
    let att_rewards = client
        .get_attestation_rewards(epoch, &scan_validator_indices)
        .await?;
    let reward_tuples: Vec<crate::db::scanner::attestations::RewardTuple> = att_rewards
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
    crate::db::scanner::attestations::update_attestation_rewards_batch(pool, &reward_tuples)
        .await?;
    crate::metrics::LIVE_EPOCH_REWARDS_DURATION
        .with_label_values(&["attestations"])
        .observe(att_t.elapsed().as_secs_f64());

    // --- Proposal rewards ---
    // Non-fatal: if proposals fail (e.g. block pruned), attestation rewards
    // are already committed. Log and continue.
    let prop_t = std::time::Instant::now();
    if let Err(e) =
        proposals::process_epoch_proposals(client, pool, epoch, scan_validators, false).await
    {
        tracing::warn!(epoch, error = %e, "Proposal reward fetch failed; attestation rewards preserved");
    }
    crate::metrics::LIVE_EPOCH_REWARDS_DURATION
        .with_label_values(&["proposals"])
        .observe(prop_t.elapsed().as_secs_f64());

    // --- Sync committee rewards ---
    // Non-fatal: sync requires fetching all 32 blocks which is expensive and
    // may fail if blocks are pruned. Attestation rewards are already committed.
    if is_altair {
        let sync_t = std::time::Instant::now();
        if let Err(e) =
            sync_committee::process_epoch_sync(client, pool, epoch, scan_validators, false).await
        {
            tracing::warn!(epoch, error = %e, "Sync committee reward fetch failed; attestation rewards preserved");
        }
        crate::metrics::LIVE_EPOCH_REWARDS_DURATION
            .with_label_values(&["sync_committee"])
            .observe(sync_t.elapsed().as_secs_f64());
    }

    crate::metrics::LIVE_EPOCH_REWARDS_DURATION
        .with_label_values(&["total"])
        .observe(timer.elapsed().as_secs_f64());
    tracing::info!(
        epoch,
        elapsed_ms = timer.elapsed().as_millis() as u64,
        "Eager epoch-transition reward fetch complete"
    );
    Ok(())
}

/// Check whether reward data is already present for an epoch (i.e. the
/// epoch-transition eager fetch already ran). Returns `true` if at least one
/// tracked validator has a non-NULL `source_reward` for this epoch.
pub async fn epoch_has_rewards(pool: &PgPool, epoch: u64) -> Result<bool> {
    let row = sqlx::query(
        "SELECT EXISTS(
            SELECT 1 FROM attestation_duties
            WHERE epoch = $1 AND source_reward IS NOT NULL
        ) AS has_rewards",
    )
    .bind(epoch as i64)
    .fetch_one(pool)
    .await?;
    Ok(row.get::<bool, _>("has_rewards"))
}
