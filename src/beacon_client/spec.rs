//! `/eth/v1/config/spec` — chain configuration pulled once at startup.

use serde::Deserialize;

use super::BeaconClient;
use super::types::deser_u64_string;
use crate::chain::ChainSpec;
use crate::error::Result;

impl BeaconClient {
    /// Must be called at startup before any [`crate::chain`] accessor runs;
    /// the chain accessors depend on values set by `chain::init`.
    pub async fn get_chain_spec(&self) -> Result<ChainSpec> {
        let wire: SpecWire = self.get("/eth/v1/config/spec").await?;
        Ok(ChainSpec {
            slots_per_epoch: wire.slots_per_epoch,
            seconds_per_slot: wire.seconds_per_slot,
            sync_committee_size: wire.sync_committee_size,
            max_committees_per_slot: wire.max_committees_per_slot,
            altair_fork_epoch: wire.altair_fork_epoch,
            epochs_per_sync_committee_period: wire.epochs_per_sync_committee_period,
        })
    }
}

/// Upstream returns uppercase-string keys with string values (even numeric
/// ones). We declare only the fields the indexer actually uses.
#[derive(Deserialize)]
struct SpecWire {
    #[serde(rename = "SLOTS_PER_EPOCH", deserialize_with = "deser_u64_string")]
    slots_per_epoch: u64,
    #[serde(rename = "SECONDS_PER_SLOT", deserialize_with = "deser_u64_string")]
    seconds_per_slot: u64,
    #[serde(rename = "SYNC_COMMITTEE_SIZE", deserialize_with = "deser_u64_string")]
    sync_committee_size: u64,
    #[serde(
        rename = "MAX_COMMITTEES_PER_SLOT",
        deserialize_with = "deser_u64_string"
    )]
    max_committees_per_slot: u64,
    #[serde(rename = "ALTAIR_FORK_EPOCH", deserialize_with = "deser_u64_string")]
    altair_fork_epoch: u64,
    #[serde(
        rename = "EPOCHS_PER_SYNC_COMMITTEE_PERIOD",
        deserialize_with = "deser_u64_string"
    )]
    epochs_per_sync_committee_period: u64,
}
