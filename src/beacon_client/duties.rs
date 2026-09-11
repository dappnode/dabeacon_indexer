//! Assignment responses retain dependent roots and use canonically anchored
//! durable inputs rather than trusting an epoch-only in-memory cache.
use super::BeaconClient;
use crate::beacon_client::types::{AttesterDuty, BeaconResponse, ProposerDuty, SyncDuty};
use crate::error::Result;

impl BeaconClient {
    pub async fn get_attester_duties_response(
        &self,
        epoch: u64,
        indices: &[u64],
    ) -> Result<BeaconResponse<Vec<AttesterDuty>>> {
        let body: Vec<String> = indices.iter().map(u64::to_string).collect();
        self.cached_input(
            epoch,
            &format!("/eth/v1/validator/duties/attester/{epoch}"),
            Some(&body),
        )
        .await
    }
    pub async fn get_attester_duties(
        &self,
        epoch: u64,
        indices: &[u64],
    ) -> Result<Vec<AttesterDuty>> {
        Ok(self
            .get_attester_duties_response(epoch, indices)
            .await?
            .data)
    }
    pub async fn get_proposer_duties_response(
        &self,
        epoch: u64,
    ) -> Result<BeaconResponse<Vec<ProposerDuty>>> {
        self.cached_input(
            epoch,
            &format!("/eth/v1/validator/duties/proposer/{epoch}"),
            None,
        )
        .await
    }
    pub async fn get_proposer_duties(&self, epoch: u64) -> Result<Vec<ProposerDuty>> {
        let duties = self.get_proposer_duties_response(epoch).await?.data;
        let start = crate::chain::epoch_start_slot(epoch);
        let end = crate::chain::epoch_start_slot(epoch + 1);
        let slots: std::collections::HashSet<u64> = duties.iter().map(|d| d.slot).collect();
        if duties.len() != crate::chain::slots_per_epoch() as usize
            || slots.len() != duties.len()
            || slots.iter().any(|slot| *slot < start || *slot >= end)
        {
            return Err(crate::error::Error::InconsistentBeaconData(
                "proposer response does not cover every slot in the requested epoch".into(),
            ));
        }
        Ok(duties)
    }
    pub async fn get_sync_duties_response(
        &self,
        epoch: u64,
        indices: &[u64],
    ) -> Result<BeaconResponse<Vec<SyncDuty>>> {
        let body: Vec<String> = indices.iter().map(u64::to_string).collect();
        self.cached_input(
            epoch,
            &format!("/eth/v1/validator/duties/sync/{epoch}"),
            Some(&body),
        )
        .await
    }
    pub async fn get_sync_duties(&self, epoch: u64, indices: &[u64]) -> Result<Vec<SyncDuty>> {
        Ok(self.get_sync_duties_response(epoch, indices).await?.data)
    }
}
