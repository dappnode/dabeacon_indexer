//! Metadata-preserving requests. None means unknown, never confirmed safe.
use serde::de::DeserializeOwned;

use super::BeaconClient;
use super::types::{BeaconHeaderData, BeaconResponse, FinalityCheckpoints, Root};
use crate::error::{Error, Result};

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct RootData {
    pub root: Root,
}

impl BeaconClient {
    pub(crate) async fn get_metadata<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<BeaconResponse<T>> {
        let response: BeaconResponse<T> = self.get_response(path).await?.json().await?;
        response.ensure_not_optimistic()?;
        Ok(response)
    }

    pub(crate) async fn post_metadata<T: DeserializeOwned, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<BeaconResponse<T>> {
        let response: BeaconResponse<T> = self.post_response(path, body).await?.json().await?;
        response.ensure_not_optimistic()?;
        Ok(response)
    }

    /// Uncached: canonical checks must not reuse a previous head observation.
    pub async fn get_header(&self, block_id: &str) -> Result<BeaconResponse<BeaconHeaderData>> {
        let response: BeaconResponse<BeaconHeaderData> = self
            .get_metadata(&format!("/eth/v1/beacon/headers/{block_id}"))
            .await?;
        if !response.data.canonical {
            return Err(Error::InconsistentBeaconData(
                "noncanonical block header".into(),
            ));
        }
        Ok(response)
    }

    pub async fn get_head_header(&self) -> Result<BeaconHeaderData> {
        Ok(self.get_header("head").await?.data)
    }

    pub async fn get_state_root(&self, state_id: &str) -> Result<BeaconResponse<RootData>> {
        self.get_metadata(&format!("/eth/v1/beacon/states/{state_id}/root"))
            .await
    }

    pub async fn get_finality_checkpoints_response(
        &self,
        state_id: &str,
    ) -> Result<BeaconResponse<FinalityCheckpoints>> {
        self.get_metadata(&format!(
            "/eth/v1/beacon/states/{state_id}/finality_checkpoints"
        ))
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_metadata_remains_unknown_and_optimistic_is_rejected() {
        let unknown: BeaconResponse<Vec<u64>> = serde_json::from_str(r#"{"data":[]}"#).unwrap();
        assert_eq!(unknown.execution_optimistic, None);
        assert_eq!(unknown.finalized, None);
        assert_eq!(unknown.dependent_root, None);
        let optimistic: BeaconResponse<Vec<u64>> =
            serde_json::from_str(r#"{"data":[],"execution_optimistic":true}"#).unwrap();
        assert!(optimistic.ensure_not_optimistic().is_err());
    }

    #[test]
    fn persisted_duty_response_preserves_metadata_and_string_numbers() {
        let json = serde_json::json!({
            "dependent_root": format!("0x{}", "12".repeat(32)),
            "execution_optimistic": false,
            "data": [{"pubkey": "0x01", "validator_index": "42", "slot": "100"}]
        });
        let response: BeaconResponse<Vec<super::super::types::ProposerDuty>> =
            serde_json::from_value(json).unwrap();
        let encoded = serde_json::to_value(&response).unwrap();
        assert_eq!(encoded["data"][0]["validator_index"], "42");
        let decoded: BeaconResponse<Vec<super::super::types::ProposerDuty>> =
            serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.dependent_root, response.dependent_root);
    }
    #[test]
    fn sync_members_round_trip_through_persistent_json() {
        let json = serde_json::json!({"validators":["42","42"],"validator_aggregates":[["42"]]});
        let parsed: super::super::types::SyncCommitteeData = serde_json::from_value(json).unwrap();
        let encoded = serde_json::to_value(&parsed).unwrap();
        assert_eq!(encoded["validators"][0], "42");
        let decoded: super::super::types::SyncCommitteeData =
            serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.validators.len(), 2);
    }
}
