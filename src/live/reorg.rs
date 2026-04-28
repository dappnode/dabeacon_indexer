use crate::beacon_client::BeaconClient;
use crate::beacon_client::types::ChainReorgEvent;
use crate::db::Pool as PgPool;
use crate::db::scanner;
use crate::error::Result;

pub(super) async fn process_chain_reorg(
    client: &BeaconClient,
    pool: &PgPool,
    reorg: &ChainReorgEvent,
    last_scanned_slot: &mut Option<u64>,
) -> Result<()> {
    let revert_from = reorg.slot.saturating_sub(reorg.depth.saturating_sub(1));
    tracing::warn!(
        slot = reorg.slot,
        depth = reorg.depth,
        revert_from_slot = revert_from,
        old_head = %reorg.old_head_block,
        new_head = %reorg.new_head_block,
        "Chain reorg detected"
    );

    tracing::debug!(
        from_slot = revert_from,
        to_slot = reorg.slot,
        "Deleting non-finalized data for reverted slots"
    );
    scanner::finalization::delete_non_finalized_slots(pool, revert_from as i64, reorg.slot as i64)
        .await
        .map_err(|e| {
            tracing::error!(
                from_slot = revert_from,
                to_slot = reorg.slot,
                error = %e,
                "Failed to delete reverted data; aborting"
            );
            e
        })?;

    client.invalidate_duty_caches().await;

    // Roll the live cursor back so upcoming head events re-scan the reverted
    // slots on the new canonical chain. scan_epoch isn't usable here — it needs
    // post-epoch state for rewards which doesn't exist yet in the current epoch.
    if let Some(cur) = *last_scanned_slot {
        let target = revert_from.saturating_sub(1);
        if target < cur {
            tracing::info!(
                previous_last_scanned_slot = cur,
                new_last_scanned_slot = target,
                "Rolling back live cursor after reorg"
            );
            *last_scanned_slot = Some(target);
        }
    }

    Ok(())
}
