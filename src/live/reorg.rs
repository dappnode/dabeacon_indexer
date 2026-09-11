use crate::beacon_client::BeaconClient;
use crate::chain::{epoch_start_slot, slot_to_epoch};
use crate::db::Pool as PgPool;
use crate::db::scanner;
use crate::error::Result;

/// Invalidate the orphaned suffix and preceding epoch inclusion dependencies.
pub(super) async fn rollback(
    client: &BeaconClient,
    pool: &PgPool,
    first_changed_slot: u64,
    last_scanned_slot: &mut Option<u64>,
) -> Result<()> {
    let replay_from = reorg_replay_start(first_changed_slot);
    scanner::live::invalidate_from(pool, replay_from).await?;
    client.invalidate_duty_caches().await;
    *last_scanned_slot = replay_from.checked_sub(1);
    crate::metrics::LIVE_REORGS.inc();
    tracing::warn!(
        first_changed_slot,
        replay_from,
        "Replaying changed canonical ancestry"
    );
    Ok(())
}

fn reorg_replay_start(revert_from: u64) -> u64 {
    epoch_start_slot(slot_to_epoch(revert_from).saturating_sub(1))
}

#[cfg(test)]
mod tests {
    #[test]
    fn replay_covers_prior_epoch_inclusions_and_deleted_rows() {
        assert_eq!(super::reorg_replay_start(100), 64);
        assert_eq!(super::reorg_replay_start(96), 64);
        assert_eq!(super::reorg_replay_start(12), 0);
    }
}
