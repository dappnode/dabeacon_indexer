use super::BeaconClient;
use crate::beacon_client::types::{Committee, SyncCommitteeData};
use crate::chain::epoch_start_slot;
use crate::error::Result;

impl BeaconClient {
    pub async fn get_committees(&self, epoch: u64) -> Result<Vec<Committee>> {
        let state_slot = epoch_start_slot(epoch);
        Ok(self
            .cached_input(
                epoch,
                &format!("/eth/v1/beacon/states/{state_slot}/committees?epoch={epoch}"),
                None,
            )
            .await?
            .data)
    }
    pub async fn get_sync_committee(&self, epoch: u64) -> Result<SyncCommitteeData> {
        let state_slot = epoch_start_slot(epoch);
        Ok(self
            .cached_input(
                epoch,
                &format!("/eth/v1/beacon/states/{state_slot}/sync_committees?epoch={epoch}"),
                None,
            )
            .await?
            .data)
    }
}
