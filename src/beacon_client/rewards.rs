use crate::beacon_client::types::{AttestationRewardsResponse, BlockRewards, SyncCommitteeReward};
use crate::error::Result;

use super::BeaconClient;

impl BeaconClient {
    pub async fn get_attestation_rewards(
        &self,
        epoch: u64,
        validator_indices: &[u64],
    ) -> Result<AttestationRewardsResponse> {
        let body: Vec<String> = validator_indices.iter().map(|i| i.to_string()).collect();
        self.post(
            &format!("/eth/v1/beacon/rewards/attestations/{}", epoch),
            &body,
        )
        .await
    }

    pub async fn get_sync_committee_rewards(
        &self,
        slot: u64,
        validator_indices: &[u64],
    ) -> Result<Vec<SyncCommitteeReward>> {
        let body: Vec<String> = validator_indices.iter().map(|i| i.to_string()).collect();
        self.post(
            &format!("/eth/v1/beacon/rewards/sync_committee/{}", slot),
            &body,
        )
        .await
    }

    pub async fn get_block_rewards(&self, slot: u64) -> Result<BlockRewards> {
        self.get(&format!("/eth/v1/beacon/rewards/blocks/{}", slot))
            .await
    }
}

impl BeaconClient {
    pub async fn get_attestation_rewards_response(
        &self,
        epoch: u64,
        validator_indices: &[u64],
    ) -> Result<super::types::BeaconResponse<AttestationRewardsResponse>> {
        let body: Vec<String> = validator_indices.iter().map(u64::to_string).collect();
        self.post_metadata(
            &format!("/eth/v1/beacon/rewards/attestations/{epoch}"),
            &body,
        )
        .await
    }

    pub async fn get_block_rewards_by_root(
        &self,
        root: &super::types::BlockRoot,
    ) -> Result<super::types::BeaconResponse<BlockRewards>> {
        self.get_metadata(&format!("/eth/v1/beacon/rewards/blocks/{root}"))
            .await
    }

    pub async fn get_sync_committee_rewards_by_root(
        &self,
        root: &super::types::BlockRoot,
        validator_indices: &[u64],
    ) -> Result<super::types::BeaconResponse<Vec<SyncCommitteeReward>>> {
        let body: Vec<String> = validator_indices.iter().map(u64::to_string).collect();
        self.post_metadata(
            &format!("/eth/v1/beacon/rewards/sync_committee/{root}"),
            &body,
        )
        .await
    }
}
