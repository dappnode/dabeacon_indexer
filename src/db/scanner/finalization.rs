//! Finalization promotion + reorg cleanup — DB-wide maintenance tied to chain
//! lifecycle events, called from the live tracker.

use crate::chain;
use crate::db::Pool;
use crate::error::Result;

/// Promote every live-written row up to `epoch` (and its equivalent slot range
/// for the slot-keyed tables) from `finalized = FALSE` to `finalized = TRUE`.
///
/// The update is **not** scoped to a validator subset — it flips every
/// not-yet-finalized row in the DB, regardless of which instance wrote it.
/// This is only safe because every other caller (notably
/// [`crate::scanner::scan_epoch`] in backfill mode) writes `finalized = true`
/// directly; there are no long-lived `finalized = false` rows except those
/// freshly produced by a head-tracker. See `scan_epoch`'s doc for the
/// cross-instance invariant.
pub async fn finalize_up_to_epoch(pool: &Pool, epoch: i64) -> Result<()> {
    let max_slot = (epoch + 1) * chain::slots_per_epoch() as i64 - 1;

    let mut tx = pool.begin().await?;

    sqlx::query(
        "UPDATE attestation_duties SET finalized = TRUE WHERE epoch <= $1 AND finalized = FALSE",
    )
    .bind(epoch)
    .execute(&mut *tx)
    .await?;

    sqlx::query("UPDATE sync_duties SET finalized = TRUE WHERE slot <= $1 AND finalized = FALSE")
        .bind(max_slot)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "UPDATE block_proposals SET finalized = TRUE WHERE slot <= $1 AND finalized = FALSE",
    )
    .bind(max_slot)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Remove every row in `[from_slot, to_slot]` with `finalized = FALSE` — used
/// by the live reorg handler to clear orphaned data before re-scanning the new
/// canonical chain. Finalized rows (written by backfill or promoted by
/// [`finalize_up_to_epoch`]) are preserved, which is the guarantee that lets a
/// concurrent archive backfiller coexist with a live head-tracker on the same
/// DB. See [`crate::scanner::scan_epoch`] for the full invariant.
pub async fn delete_non_finalized_slots(pool: &Pool, from_slot: i64, to_slot: i64) -> Result<()> {
    let spe = chain::slots_per_epoch() as i64;
    let from_epoch = from_slot / spe;
    let to_epoch = to_slot / spe;

    let mut tx = pool.begin().await?;

    sqlx::query(
        "DELETE FROM attestation_duties WHERE epoch >= $1 AND epoch <= $2 AND finalized = FALSE",
    )
    .bind(from_epoch)
    .bind(to_epoch)
    .execute(&mut *tx)
    .await?;

    sqlx::query("DELETE FROM sync_duties WHERE slot >= $1 AND slot <= $2 AND finalized = FALSE")
        .bind(from_slot)
        .bind(to_slot)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "DELETE FROM block_proposals WHERE slot >= $1 AND slot <= $2 AND finalized = FALSE",
    )
    .bind(from_slot)
    .bind(to_slot)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}
