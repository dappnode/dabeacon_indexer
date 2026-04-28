//! Validator metadata reconciliation.
//!
//! Pulls validator state (`activation_epoch`, `exit_epoch`, `pubkey`) from the
//! beacon node and writes it into the `validators` table — the source of
//! truth for [`crate::db::scanner::validators::active_validators_at`].
//!
//! Two entry points:
//!
//! - [`update`]: synchronous one-shot. Used at startup with the full
//!   tracked set so backfill / live can rely on a freshly-seeded table.
//! - [`run_update_loop`]: long-running task spawned only when live
//!   tracking runs (state can mutate as the chain advances). Once per
//!   epoch's wall-time it looks up validators currently *active* (per the
//!   DB) and re-pulls their beacon state, capturing exits without a
//!   process restart. Pending validators are skipped — when they activate
//!   they appear in the active set the next iteration and start being
//!   updated naturally.

use std::sync::Arc;
use std::time::Duration;

use crate::beacon_client::BeaconClient;
use crate::chain;
use crate::db::Pool as PgPool;
use crate::db::scanner as db_scanner;
use crate::error::Result;

/// Fetch the current beacon state for `indices` and upsert each row in the
/// `validators` table. `exit_epoch == u64::MAX` (FAR_FUTURE_EPOCH) collapses
/// to NULL — the column is the storage form of "still active".
pub async fn update(client: &BeaconClient, pool: &PgPool, indices: &[u64]) -> Result<()> {
    if indices.is_empty() {
        return Ok(());
    }

    let validators = client.get_validators("head", indices).await?;
    for v in &validators {
        let pubkey_bytes =
            hex::decode(v.validator.pubkey.trim_start_matches("0x")).unwrap_or_default();
        let exit_epoch = if v.validator.exit_epoch == u64::MAX {
            None
        } else {
            Some(v.validator.exit_epoch as i64)
        };
        db_scanner::validators::upsert_validator(
            pool,
            v.index as i64,
            &pubkey_bytes,
            v.validator.activation_epoch as i64,
            exit_epoch,
        )
        .await?;
    }
    Ok(())
}

/// Per-epoch reconciliation loop. Sleeps one epoch's wall-time between
/// iterations. Errors at any step are logged and don't abort the loop —
/// a transient beacon-API blip just defers reconciliation by one tick.
pub async fn run_update_loop(client: Arc<BeaconClient>, pool: PgPool, tracked: Arc<Vec<u64>>) {
    let spec = chain::spec();
    let epoch_duration = Duration::from_secs(spec.slots_per_epoch * spec.seconds_per_slot);
    let tracked_indices: Vec<i64> = tracked.iter().map(|&v| v as i64).collect();

    loop {
        tokio::time::sleep(epoch_duration).await;

        let head_slot = match client.get_head_slot().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "Validator update: failed to get head slot");
                continue;
            }
        };
        let head_epoch = chain::slot_to_epoch(head_slot);

        let active = match db_scanner::validators::active_validators_at(
            &pool,
            &tracked_indices,
            head_epoch as i64,
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    epoch = head_epoch,
                    "Validator update: failed to query active set"
                );
                continue;
            }
        };

        if active.is_empty() {
            tracing::trace!(epoch = head_epoch, "Validator update: no active validators");
            continue;
        }

        let active_vec: Vec<u64> = active.into_iter().collect();
        match update(&client, &pool, &active_vec).await {
            Ok(_) => tracing::debug!(
                count = active_vec.len(),
                epoch = head_epoch,
                "updated validator metadata"
            ),
            Err(e) => tracing::warn!(error = %e, "Validator update: upsert failed"),
        }
    }
}
