use crate::beacon_client::types::{FinalityCheckpoints, ValidatorData};
use crate::error::Result;

use super::BeaconClient;

impl BeaconClient {
    pub async fn get_validators(
        &self,
        state_id: &str,
        validator_indices: &[u64],
    ) -> Result<Vec<ValidatorData>> {
        let ids: Vec<String> = validator_indices.iter().map(|i| i.to_string()).collect();
        let ids_param = ids.join(",");
        self.get(&format!(
            "/eth/v1/beacon/states/{}/validators?id={}",
            state_id, ids_param
        ))
        .await
    }

    pub async fn get_finality_checkpoints(&self, state_id: &str) -> Result<FinalityCheckpoints> {
        // Only the "head" variant is worth caching — other state_ids are
        // per-slot backfill queries unlikely to repeat within the TTL.
        if state_id == "head" {
            if let Some((fc, when)) = self.head_finality_cache.read().await.clone()
                && when.elapsed() < super::HEAD_FINALITY_TTL
            {
                crate::metrics::record_cache("head_finality", true);
                return Ok(fc);
            }
            crate::metrics::record_cache("head_finality", false);
        }
        let fc: FinalityCheckpoints = self
            .get(&format!(
                "/eth/v1/beacon/states/{state_id}/finality_checkpoints"
            ))
            .await?;
        if state_id == "head" {
            *self.head_finality_cache.write().await = Some((fc.clone(), std::time::Instant::now()));
        }
        Ok(fc)
    }
}
