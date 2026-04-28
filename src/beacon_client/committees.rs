use crate::beacon_client::types::{Committee, SyncCommitteeData};
use crate::chain::{epoch_start_slot, sync_committee_period};
use crate::error::Result;

use super::BeaconClient;

impl BeaconClient {
    /// Cached per-epoch. A reorg crossing the epoch boundary invalidates the
    /// cache via `invalidate_duty_caches`.
    pub async fn get_committees(&self, epoch: u64) -> Result<Vec<Committee>> {
        if let Some(hit) = self.committees_cache.read().await.peek(&epoch).cloned() {
            crate::metrics::record_cache("committees", true);
            tracing::trace!(epoch, "committees cache hit");
            return Ok(hit);
        }
        crate::metrics::record_cache("committees", false);
        let state_slot = epoch_start_slot(epoch);
        let fetched: Vec<Committee> = self
            .get(&format!(
                "/eth/v1/beacon/states/{state_slot}/committees?epoch={epoch}"
            ))
            .await?;
        self.committees_cache
            .write()
            .await
            .put(epoch, fetched.clone());
        Ok(fetched)
    }

    /// Cached by sync-committee period — a single fetch covers the whole
    /// period's epoch scans.
    pub async fn get_sync_committee(&self, epoch: u64) -> Result<SyncCommitteeData> {
        let period = sync_committee_period(epoch);
        if let Some(hit) = self
            .sync_committee_cache
            .read()
            .await
            .peek(&period)
            .cloned()
        {
            crate::metrics::record_cache("sync_committee", true);
            tracing::trace!(epoch, period, "sync_committee cache hit");
            return Ok(hit);
        }
        crate::metrics::record_cache("sync_committee", false);
        let state_slot = epoch_start_slot(epoch);
        let fetched: SyncCommitteeData = self
            .get(&format!(
                "/eth/v1/beacon/states/{state_slot}/sync_committees?epoch={epoch}"
            ))
            .await?;
        self.sync_committee_cache
            .write()
            .await
            .put(period, fetched.clone());
        Ok(fetched)
    }
}
