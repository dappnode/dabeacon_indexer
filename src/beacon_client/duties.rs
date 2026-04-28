//! Duty-endpoint wrappers. Cached per-epoch, or per `(epoch, validator-set-hash)`
//! for validator-scoped endpoints. A reorg crossing an epoch boundary
//! invalidates via `invalidate_duty_caches`.

use crate::beacon_client::types::{AttesterDuty, ProposerDuty, SyncDuty};
use crate::error::Result;

use super::{BeaconClient, DutiesKey};

impl BeaconClient {
    pub async fn get_attester_duties(
        &self,
        epoch: u64,
        validator_indices: &[u64],
    ) -> Result<Vec<AttesterDuty>> {
        let key = DutiesKey::new(epoch, validator_indices);
        if let Some(hit) = self.attester_duties_cache.read().await.peek(&key).cloned() {
            crate::metrics::record_cache("attester_duties", true);
            tracing::trace!(epoch, "attester_duties cache hit");
            return Ok(hit);
        }
        crate::metrics::record_cache("attester_duties", false);
        let body: Vec<String> = validator_indices.iter().map(|i| i.to_string()).collect();
        let fetched: Vec<AttesterDuty> = self
            .post(&format!("/eth/v1/validator/duties/attester/{epoch}"), &body)
            .await?;
        self.attester_duties_cache
            .write()
            .await
            .put(key, fetched.clone());
        Ok(fetched)
    }

    pub async fn get_proposer_duties(&self, epoch: u64) -> Result<Vec<ProposerDuty>> {
        if let Some(hit) = self
            .proposer_duties_cache
            .read()
            .await
            .peek(&epoch)
            .cloned()
        {
            crate::metrics::record_cache("proposer_duties", true);
            tracing::trace!(epoch, "proposer_duties cache hit");
            return Ok(hit);
        }
        crate::metrics::record_cache("proposer_duties", false);
        let fetched: Vec<ProposerDuty> = self
            .get(&format!("/eth/v1/validator/duties/proposer/{epoch}"))
            .await?;
        self.proposer_duties_cache
            .write()
            .await
            .put(epoch, fetched.clone());
        Ok(fetched)
    }

    pub async fn get_sync_duties(
        &self,
        epoch: u64,
        validator_indices: &[u64],
    ) -> Result<Vec<SyncDuty>> {
        let key = DutiesKey::new(epoch, validator_indices);
        if let Some(hit) = self.sync_duties_cache.read().await.peek(&key).cloned() {
            crate::metrics::record_cache("sync_duties", true);
            tracing::trace!(epoch, "sync_duties cache hit");
            return Ok(hit);
        }
        crate::metrics::record_cache("sync_duties", false);
        let body: Vec<String> = validator_indices.iter().map(|i| i.to_string()).collect();
        let fetched: Vec<SyncDuty> = self
            .post(&format!("/eth/v1/validator/duties/sync/{epoch}"), &body)
            .await?;
        self.sync_duties_cache
            .write()
            .await
            .put(key, fetched.clone());
        Ok(fetched)
    }
}
