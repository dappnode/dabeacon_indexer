#![allow(dead_code)]

use std::borrow::Cow;
use std::convert::TryFrom;
use std::fmt;

use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Deserialize)]
pub struct BeaconResponse<T> {
    pub data: T,
    #[serde(default)]
    pub execution_optimistic: Option<bool>,
    #[serde(default)]
    pub finalized: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockId {
    Slot(u64),
    Root(BlockRoot),
    Head,
    Genesis,
    Finalized,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct Root(String);

pub type BlockRoot = Root;
pub type StateRoot = Root;

impl Root {
    pub fn parse(value: &str) -> Result<Self> {
        normalize_prefixed_hex_root(value)
            .map(Self)
            .ok_or_else(|| Error::InvalidBlockId(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Root {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Root::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl AsRef<str> for Root {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<Root> for String {
    fn from(value: Root) -> Self {
        value.0
    }
}

fn normalize_prefixed_hex_root(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))?;

    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }

    Some(format!("0x{}", hex.to_ascii_lowercase()))
}

impl BlockId {
    pub(super) fn as_request_segment(&self) -> Cow<'_, str> {
        match self {
            Self::Slot(slot) => Cow::Owned(slot.to_string()),
            Self::Root(root) => Cow::Borrowed(root.as_str()),
            Self::Head => Cow::Borrowed("head"),
            Self::Genesis => Cow::Borrowed("genesis"),
            Self::Finalized => Cow::Borrowed("finalized"),
        }
    }
}

impl TryFrom<u64> for BlockId {
    type Error = Error;

    fn try_from(value: u64) -> Result<Self> {
        Ok(Self::Slot(value))
    }
}

impl TryFrom<String> for BlockId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        if value.eq_ignore_ascii_case("head") {
            return Ok(Self::Head);
        }
        if value.eq_ignore_ascii_case("genesis") {
            return Ok(Self::Genesis);
        }
        if value.eq_ignore_ascii_case("finalized") {
            return Ok(Self::Finalized);
        }
        if let Ok(slot) = value.parse::<u64>() {
            return Ok(Self::Slot(slot));
        }

        Ok(Self::Root(Root::parse(&value)?))
    }
}

impl TryFrom<&str> for BlockId {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self> {
        BlockId::try_from(value.to_string())
    }
}

impl TryFrom<&String> for BlockId {
    type Error = Error;

    fn try_from(value: &String) -> Result<Self> {
        BlockId::try_from(value.as_str())
    }
}

impl From<BlockId> for String {
    fn from(value: BlockId) -> Self {
        value.as_request_segment().into_owned()
    }
}

impl TryFrom<&BlockRoot> for BlockId {
    type Error = Error;

    fn try_from(value: &BlockRoot) -> Result<Self> {
        Ok(BlockId::Root(value.clone()))
    }
}

impl From<BlockRoot> for BlockId {
    fn from(value: BlockRoot) -> Self {
        BlockId::Root(value)
    }
}

/// Ethereum consensus fork, matching the `version` field returned by
/// `/eth/v2/beacon/blocks/{id}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BeaconFork {
    Phase0,
    Altair,
    Bellatrix,
    Capella,
    Deneb,
    Electra,
    Fulu,
}

/// Signed beacon block, dispatched on the fork the block was produced under.
/// Each variant carries a fork-specific body so new fields (execution_payload,
/// blob_kzg_commitments, execution_requests, …) can be added to a single body
/// type without touching consumers.
///
/// Consumers should use the getters rather than match on variants directly.
#[derive(Debug, Clone)]
pub enum SignedBeaconBlock {
    Phase0(SignedBlock<BeaconBlockBodyPhase0>),
    Altair(SignedBlock<BeaconBlockBodyAltair>),
    Bellatrix(SignedBlock<BeaconBlockBodyBellatrix>),
    Capella(SignedBlock<BeaconBlockBodyCapella>),
    Deneb(SignedBlock<BeaconBlockBodyDeneb>),
    Electra(SignedBlock<BeaconBlockBodyElectra>),
    Fulu(SignedBlock<BeaconBlockBodyFulu>),
}

#[derive(Debug, Clone, Deserialize)]
pub struct SignedBlock<B> {
    pub message: BeaconBlockMessage<B>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BeaconBlockMessage<B> {
    #[serde(deserialize_with = "deser_u64_string")]
    pub slot: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub proposer_index: u64,
    pub parent_root: BlockRoot,
    pub state_root: StateRoot,
    pub body: B,
}

// Per-fork block bodies. Each struct lists every field the fork adds, even
// unused ones — serde ignores unknown fields, so dropping a field here would
// be a silent deserialization mismatch when we later want to consume it.

#[derive(Debug, Clone, Deserialize)]
pub struct BeaconBlockBodyPhase0 {
    pub randao_reveal: String,
    pub eth1_data: Eth1Data,
    pub graffiti: String,
    pub proposer_slashings: Vec<ProposerSlashing>,
    pub attester_slashings: Vec<AttesterSlashing>,
    pub attestations: Vec<PreElectraAttestation>,
    pub deposits: Vec<Deposit>,
    pub voluntary_exits: Vec<SignedVoluntaryExit>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BeaconBlockBodyAltair {
    pub randao_reveal: String,
    pub eth1_data: Eth1Data,
    pub graffiti: String,
    pub proposer_slashings: Vec<ProposerSlashing>,
    pub attester_slashings: Vec<AttesterSlashing>,
    pub attestations: Vec<PreElectraAttestation>,
    pub deposits: Vec<Deposit>,
    pub voluntary_exits: Vec<SignedVoluntaryExit>,
    pub sync_aggregate: SyncAggregate,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BeaconBlockBodyBellatrix {
    pub randao_reveal: String,
    pub eth1_data: Eth1Data,
    pub graffiti: String,
    pub proposer_slashings: Vec<ProposerSlashing>,
    pub attester_slashings: Vec<AttesterSlashing>,
    pub attestations: Vec<PreElectraAttestation>,
    pub deposits: Vec<Deposit>,
    pub voluntary_exits: Vec<SignedVoluntaryExit>,
    pub sync_aggregate: SyncAggregate,
    pub execution_payload: ExecutionPayloadBellatrix,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BeaconBlockBodyCapella {
    pub randao_reveal: String,
    pub eth1_data: Eth1Data,
    pub graffiti: String,
    pub proposer_slashings: Vec<ProposerSlashing>,
    pub attester_slashings: Vec<AttesterSlashing>,
    pub attestations: Vec<PreElectraAttestation>,
    pub deposits: Vec<Deposit>,
    pub voluntary_exits: Vec<SignedVoluntaryExit>,
    pub sync_aggregate: SyncAggregate,
    pub execution_payload: ExecutionPayloadCapella,
    pub bls_to_execution_changes: Vec<SignedBLSToExecutionChange>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BeaconBlockBodyDeneb {
    pub randao_reveal: String,
    pub eth1_data: Eth1Data,
    pub graffiti: String,
    pub proposer_slashings: Vec<ProposerSlashing>,
    pub attester_slashings: Vec<AttesterSlashing>,
    pub attestations: Vec<PreElectraAttestation>,
    pub deposits: Vec<Deposit>,
    pub voluntary_exits: Vec<SignedVoluntaryExit>,
    pub sync_aggregate: SyncAggregate,
    pub execution_payload: ExecutionPayloadDeneb,
    pub bls_to_execution_changes: Vec<SignedBLSToExecutionChange>,
    pub blob_kzg_commitments: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BeaconBlockBodyElectra {
    pub randao_reveal: String,
    pub eth1_data: Eth1Data,
    pub graffiti: String,
    pub proposer_slashings: Vec<ProposerSlashing>,
    pub attester_slashings: Vec<AttesterSlashing>,
    pub attestations: Vec<Attestation>,
    pub deposits: Vec<Deposit>,
    pub voluntary_exits: Vec<SignedVoluntaryExit>,
    pub sync_aggregate: SyncAggregate,
    pub execution_payload: ExecutionPayloadDeneb,
    pub bls_to_execution_changes: Vec<SignedBLSToExecutionChange>,
    pub blob_kzg_commitments: Vec<String>,
    pub execution_requests: ExecutionRequests,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BeaconBlockBodyFulu {
    pub randao_reveal: String,
    pub eth1_data: Eth1Data,
    pub graffiti: String,
    pub proposer_slashings: Vec<ProposerSlashing>,
    pub attester_slashings: Vec<AttesterSlashing>,
    pub attestations: Vec<Attestation>,
    pub deposits: Vec<Deposit>,
    pub voluntary_exits: Vec<SignedVoluntaryExit>,
    pub sync_aggregate: SyncAggregate,
    pub execution_payload: ExecutionPayloadDeneb,
    pub bls_to_execution_changes: Vec<SignedBLSToExecutionChange>,
    pub blob_kzg_commitments: Vec<String>,
    pub execution_requests: ExecutionRequests,
}

impl SignedBeaconBlock {
    pub fn fork(&self) -> BeaconFork {
        match self {
            Self::Phase0(_) => BeaconFork::Phase0,
            Self::Altair(_) => BeaconFork::Altair,
            Self::Bellatrix(_) => BeaconFork::Bellatrix,
            Self::Capella(_) => BeaconFork::Capella,
            Self::Deneb(_) => BeaconFork::Deneb,
            Self::Electra(_) => BeaconFork::Electra,
            Self::Fulu(_) => BeaconFork::Fulu,
        }
    }

    pub fn slot(&self) -> u64 {
        match self {
            Self::Phase0(b) => b.message.slot,
            Self::Altair(b) => b.message.slot,
            Self::Bellatrix(b) => b.message.slot,
            Self::Capella(b) => b.message.slot,
            Self::Deneb(b) => b.message.slot,
            Self::Electra(b) => b.message.slot,
            Self::Fulu(b) => b.message.slot,
        }
    }

    pub fn proposer_index(&self) -> u64 {
        match self {
            Self::Phase0(b) => b.message.proposer_index,
            Self::Altair(b) => b.message.proposer_index,
            Self::Bellatrix(b) => b.message.proposer_index,
            Self::Capella(b) => b.message.proposer_index,
            Self::Deneb(b) => b.message.proposer_index,
            Self::Electra(b) => b.message.proposer_index,
            Self::Fulu(b) => b.message.proposer_index,
        }
    }

    pub fn parent_root(&self) -> &BlockRoot {
        match self {
            Self::Phase0(b) => &b.message.parent_root,
            Self::Altair(b) => &b.message.parent_root,
            Self::Bellatrix(b) => &b.message.parent_root,
            Self::Capella(b) => &b.message.parent_root,
            Self::Deneb(b) => &b.message.parent_root,
            Self::Electra(b) => &b.message.parent_root,
            Self::Fulu(b) => &b.message.parent_root,
        }
    }

    pub fn state_root(&self) -> &StateRoot {
        match self {
            Self::Phase0(b) => &b.message.state_root,
            Self::Altair(b) => &b.message.state_root,
            Self::Bellatrix(b) => &b.message.state_root,
            Self::Capella(b) => &b.message.state_root,
            Self::Deneb(b) => &b.message.state_root,
            Self::Electra(b) => &b.message.state_root,
            Self::Fulu(b) => &b.message.state_root,
        }
    }

    /// Attestations in this block, split by on-wire format.
    ///
    /// Pre-Electra forks (Phase 0 through Deneb) use [`PreElectraAttestation`] with a
    /// single committee per attestation identified by `data.index`. Electra and later
    /// use [`Attestation`] with EIP-7549 multi-committee encoding (`committee_bits`
    /// selects the committees, `aggregation_bits` spans them concatenated).
    pub fn attestations(&self) -> Attestations<'_> {
        match self {
            Self::Phase0(b) => Attestations::PreElectra(&b.message.body.attestations),
            Self::Altair(b) => Attestations::PreElectra(&b.message.body.attestations),
            Self::Bellatrix(b) => Attestations::PreElectra(&b.message.body.attestations),
            Self::Capella(b) => Attestations::PreElectra(&b.message.body.attestations),
            Self::Deneb(b) => Attestations::PreElectra(&b.message.body.attestations),
            Self::Electra(b) => Attestations::Electra(&b.message.body.attestations),
            Self::Fulu(b) => Attestations::Electra(&b.message.body.attestations),
        }
    }

    /// Number of attestations in this block, irrespective of fork format.
    pub fn attestations_len(&self) -> usize {
        match self.attestations() {
            Attestations::PreElectra(a) => a.len(),
            Attestations::Electra(a) => a.len(),
        }
    }

    /// Slots of every attestation in this block (`data.slot`), across both fork
    /// encodings. For callers that only need the target slots, not the fork-specific
    /// aggregation layout.
    pub fn attestation_slots(&self) -> Box<dyn Iterator<Item = u64> + '_> {
        match self.attestations() {
            Attestations::PreElectra(atts) => Box::new(atts.iter().map(|a| a.data.slot)),
            Attestations::Electra(atts) => Box::new(atts.iter().map(|a| a.data.slot)),
        }
    }

    /// `None` for pre-Altair blocks (Phase 0), otherwise the block's sync aggregate.
    pub fn sync_aggregate(&self) -> Option<&SyncAggregate> {
        match self {
            Self::Phase0(_) => None,
            Self::Altair(b) => Some(&b.message.body.sync_aggregate),
            Self::Bellatrix(b) => Some(&b.message.body.sync_aggregate),
            Self::Capella(b) => Some(&b.message.body.sync_aggregate),
            Self::Deneb(b) => Some(&b.message.body.sync_aggregate),
            Self::Electra(b) => Some(&b.message.body.sync_aggregate),
            Self::Fulu(b) => Some(&b.message.body.sync_aggregate),
        }
    }
}

/// Attestation as it appears in pre-Electra blocks (Phase 0 through Deneb).
///
/// A single committee per attestation is identified by `data.index`, and
/// `aggregation_bits` maps 1:1 to the validators of that committee.
#[derive(Debug, Deserialize, Clone)]
pub struct PreElectraAttestation {
    pub aggregation_bits: String,
    pub data: AttestationData,
    pub signature: String,
}

/// Attestation as it appears in Electra and later blocks (EIP-7549).
///
/// `data.index` is always 0; `committee_bits` is a bitvector selecting which
/// committees at the slot are included, and `aggregation_bits` spans the
/// concatenated validators of those committees in order.
#[derive(Debug, Deserialize, Clone)]
pub struct Attestation {
    pub aggregation_bits: String,
    pub committee_bits: String,
    pub data: AttestationData,
    pub signature: String,
}

/// Borrowed view of a block's attestations, dispatched by on-wire format.
#[derive(Debug, Clone, Copy)]
pub enum Attestations<'a> {
    PreElectra(&'a [PreElectraAttestation]),
    Electra(&'a [Attestation]),
}

#[derive(Debug, Deserialize, Clone)]
pub struct AttestationData {
    #[serde(deserialize_with = "deser_u64_string")]
    pub slot: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub index: u64,
    pub beacon_block_root: BlockRoot,
    pub source: Checkpoint,
    pub target: Checkpoint,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Checkpoint {
    #[serde(deserialize_with = "deser_u64_string")]
    pub epoch: u64,
    pub root: BlockRoot,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SyncAggregate {
    pub sync_committee_bits: String,
    pub sync_committee_signature: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Eth1Data {
    pub deposit_root: Root,
    #[serde(deserialize_with = "deser_u64_string")]
    pub deposit_count: u64,
    pub block_hash: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProposerSlashing {
    pub signed_header_1: SignedBeaconHeader,
    pub signed_header_2: SignedBeaconHeader,
}

/// Both `attesting_indices` list lengths (pre- vs post-Electra) serialize identically;
/// only the spec-level maximum differs (MAX_VALIDATORS_PER_COMMITTEE vs
/// MAX_VALIDATORS_PER_COMMITTEE * MAX_COMMITTEES_PER_SLOT), so one JSON type suffices.
#[derive(Debug, Deserialize, Clone)]
pub struct IndexedAttestation {
    pub attesting_indices: Vec<StringU64>,
    pub data: AttestationData,
    pub signature: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AttesterSlashing {
    pub attestation_1: IndexedAttestation,
    pub attestation_2: IndexedAttestation,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Deposit {
    pub proof: Vec<String>,
    pub data: DepositData,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DepositData {
    pub pubkey: String,
    pub withdrawal_credentials: String,
    #[serde(deserialize_with = "deser_u64_string")]
    pub amount: u64,
    pub signature: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SignedVoluntaryExit {
    pub message: VoluntaryExit,
    pub signature: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct VoluntaryExit {
    #[serde(deserialize_with = "deser_u64_string")]
    pub epoch: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub validator_index: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SignedBLSToExecutionChange {
    pub message: BLSToExecutionChange,
    pub signature: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BLSToExecutionChange {
    #[serde(deserialize_with = "deser_u64_string")]
    pub validator_index: u64,
    pub from_bls_pubkey: String,
    pub to_execution_address: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Withdrawal {
    #[serde(deserialize_with = "deser_u64_string")]
    pub index: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub validator_index: u64,
    pub address: String,
    #[serde(deserialize_with = "deser_u64_string")]
    pub amount: u64,
}

/// Bellatrix introduced the execution payload (EIP-3675, The Merge).
#[derive(Debug, Deserialize, Clone)]
pub struct ExecutionPayloadBellatrix {
    pub parent_hash: String,
    pub fee_recipient: String,
    pub state_root: String,
    pub receipts_root: String,
    pub logs_bloom: String,
    pub prev_randao: String,
    #[serde(deserialize_with = "deser_u64_string")]
    pub block_number: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub gas_limit: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub gas_used: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub timestamp: u64,
    pub extra_data: String,
    /// uint256, serialized as decimal string.
    pub base_fee_per_gas: String,
    pub block_hash: String,
    pub transactions: Vec<String>,
}

/// Capella added withdrawals to the execution payload (EIP-4895).
#[derive(Debug, Deserialize, Clone)]
pub struct ExecutionPayloadCapella {
    pub parent_hash: String,
    pub fee_recipient: String,
    pub state_root: String,
    pub receipts_root: String,
    pub logs_bloom: String,
    pub prev_randao: String,
    #[serde(deserialize_with = "deser_u64_string")]
    pub block_number: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub gas_limit: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub gas_used: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub timestamp: u64,
    pub extra_data: String,
    pub base_fee_per_gas: String,
    pub block_hash: String,
    pub transactions: Vec<String>,
    pub withdrawals: Vec<Withdrawal>,
}

/// Deneb added blob-gas fields (EIP-4844). Electra and Fulu reuse this shape;
/// new execution-requests in Electra live on the block body, not in the payload.
#[derive(Debug, Deserialize, Clone)]
pub struct ExecutionPayloadDeneb {
    pub parent_hash: String,
    pub fee_recipient: String,
    pub state_root: String,
    pub receipts_root: String,
    pub logs_bloom: String,
    pub prev_randao: String,
    #[serde(deserialize_with = "deser_u64_string")]
    pub block_number: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub gas_limit: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub gas_used: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub timestamp: u64,
    pub extra_data: String,
    pub base_fee_per_gas: String,
    pub block_hash: String,
    pub transactions: Vec<String>,
    pub withdrawals: Vec<Withdrawal>,
    #[serde(deserialize_with = "deser_u64_string")]
    pub blob_gas_used: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub excess_blob_gas: u64,
}

/// Electra, EIP-7685.
#[derive(Debug, Deserialize, Clone)]
pub struct ExecutionRequests {
    pub deposits: Vec<DepositRequest>,
    pub withdrawals: Vec<WithdrawalRequest>,
    pub consolidations: Vec<ConsolidationRequest>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DepositRequest {
    pub pubkey: String,
    pub withdrawal_credentials: String,
    #[serde(deserialize_with = "deser_u64_string")]
    pub amount: u64,
    pub signature: String,
    #[serde(deserialize_with = "deser_u64_string")]
    pub index: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct WithdrawalRequest {
    pub source_address: String,
    pub validator_pubkey: String,
    #[serde(deserialize_with = "deser_u64_string")]
    pub amount: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ConsolidationRequest {
    pub source_address: String,
    pub source_pubkey: String,
    pub target_pubkey: String,
}

/// Raw `/eth/v2/beacon/blocks/{id}` JSON response, dispatched on the `version` tag
/// so each fork is deserialized into its own body type.
#[derive(Debug, Deserialize)]
#[serde(tag = "version", rename_all = "lowercase")]
pub enum RawBlockResponse {
    Phase0 {
        #[serde(default)]
        finalized: bool,
        data: SignedBlock<BeaconBlockBodyPhase0>,
    },
    Altair {
        #[serde(default)]
        finalized: bool,
        data: SignedBlock<BeaconBlockBodyAltair>,
    },
    Bellatrix {
        #[serde(default)]
        finalized: bool,
        data: SignedBlock<BeaconBlockBodyBellatrix>,
    },
    Capella {
        #[serde(default)]
        finalized: bool,
        data: SignedBlock<BeaconBlockBodyCapella>,
    },
    Deneb {
        #[serde(default)]
        finalized: bool,
        data: SignedBlock<BeaconBlockBodyDeneb>,
    },
    Electra {
        #[serde(default)]
        finalized: bool,
        data: SignedBlock<BeaconBlockBodyElectra>,
    },
    Fulu {
        #[serde(default)]
        finalized: bool,
        data: SignedBlock<BeaconBlockBodyFulu>,
    },
}

impl RawBlockResponse {
    pub fn into_parts(self) -> (SignedBeaconBlock, bool) {
        match self {
            Self::Phase0 { finalized, data } => (SignedBeaconBlock::Phase0(data), finalized),
            Self::Altair { finalized, data } => (SignedBeaconBlock::Altair(data), finalized),
            Self::Bellatrix { finalized, data } => (SignedBeaconBlock::Bellatrix(data), finalized),
            Self::Capella { finalized, data } => (SignedBeaconBlock::Capella(data), finalized),
            Self::Deneb { finalized, data } => (SignedBeaconBlock::Deneb(data), finalized),
            Self::Electra { finalized, data } => (SignedBeaconBlock::Electra(data), finalized),
            Self::Fulu { finalized, data } => (SignedBeaconBlock::Fulu(data), finalized),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct BlockRootResponse {
    pub root: BlockRoot,
}

#[derive(Debug, Deserialize)]
pub struct RawBlockRootResponse {
    #[serde(default)]
    pub finalized: bool,
    pub data: BlockRootResponse,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Committee {
    #[serde(deserialize_with = "deser_u64_string")]
    pub index: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub slot: u64,
    pub validators: Vec<StringU64>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SyncCommitteeData {
    pub validators: Vec<StringU64>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AttesterDuty {
    pub pubkey: String,
    #[serde(deserialize_with = "deser_u64_string")]
    pub validator_index: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub committee_index: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub committee_length: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub committees_at_slot: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub validator_committee_index: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub slot: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProposerDuty {
    pub pubkey: String,
    #[serde(deserialize_with = "deser_u64_string")]
    pub validator_index: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub slot: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SyncDuty {
    pub pubkey: String,
    #[serde(deserialize_with = "deser_u64_string")]
    pub validator_index: u64,
    #[serde(default)]
    pub validator_sync_committee_indices: Vec<StringU64>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(transparent)]
pub struct StringU64(#[serde(deserialize_with = "deser_u64_string")] pub u64);

#[derive(Debug, Deserialize)]
pub struct AttestationRewardsResponse {
    pub ideal_rewards: Vec<IdealReward>,
    pub total_rewards: Vec<ValidatorAttestationReward>,
}

#[derive(Debug, Deserialize)]
pub struct IdealReward {
    #[serde(deserialize_with = "deser_u64_string")]
    pub effective_balance: u64,
    #[serde(deserialize_with = "deser_i64_string")]
    pub head: i64,
    #[serde(deserialize_with = "deser_i64_string")]
    pub target: i64,
    #[serde(deserialize_with = "deser_i64_string")]
    pub source: i64,
}

#[derive(Debug, Deserialize)]
pub struct ValidatorAttestationReward {
    #[serde(deserialize_with = "deser_u64_string")]
    pub validator_index: u64,
    #[serde(deserialize_with = "deser_i64_string")]
    pub head: i64,
    #[serde(deserialize_with = "deser_i64_string")]
    pub target: i64,
    #[serde(deserialize_with = "deser_i64_string")]
    pub source: i64,
    #[serde(default, deserialize_with = "deser_i64_string_opt")]
    pub inclusion_delay: Option<i64>,
    #[serde(default, deserialize_with = "deser_i64_string_opt")]
    pub inactivity: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct SyncCommitteeReward {
    #[serde(deserialize_with = "deser_u64_string")]
    pub validator_index: u64,
    #[serde(deserialize_with = "deser_i64_string")]
    pub reward: i64,
}

#[derive(Debug, Deserialize)]
pub struct BlockRewards {
    #[serde(deserialize_with = "deser_u64_string")]
    pub proposer_index: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub total: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub attestations: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub sync_aggregate: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub proposer_slashings: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub attester_slashings: u64,
}

#[derive(Debug, Deserialize)]
pub struct ValidatorData {
    #[serde(deserialize_with = "deser_u64_string")]
    pub index: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub balance: u64,
    pub status: String,
    pub validator: ValidatorDetails,
}

#[derive(Debug, Deserialize)]
pub struct ValidatorDetails {
    pub pubkey: String,
    #[serde(deserialize_with = "deser_u64_string")]
    pub effective_balance: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub activation_epoch: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub exit_epoch: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FinalityCheckpoints {
    pub previous_justified: Checkpoint,
    pub current_justified: Checkpoint,
    pub finalized: Checkpoint,
}

#[derive(Debug, Deserialize)]
pub struct BeaconHeaderData {
    pub root: BlockRoot,
    pub canonical: bool,
    pub header: SignedBeaconHeader,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SignedBeaconHeader {
    pub message: BeaconHeaderMessage,
    #[serde(default)]
    pub signature: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BeaconHeaderMessage {
    #[serde(deserialize_with = "deser_u64_string")]
    pub slot: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub proposer_index: u64,
    pub parent_root: BlockRoot,
    pub state_root: StateRoot,
    pub body_root: Root,
}

pub(super) fn deser_u64_string<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<u64, D::Error> {
    let s = String::deserialize(d)?;
    s.parse().map_err(serde::de::Error::custom)
}

fn deser_i64_string<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<i64, D::Error> {
    let s = String::deserialize(d)?;
    s.parse().map_err(serde::de::Error::custom)
}

fn deser_i64_string_opt<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<i64>, D::Error> {
    let opt: Option<String> = Option::deserialize(d)?;
    match opt {
        Some(s) => s.parse().map(Some).map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

// Payloads of the `/eth/v1/events?topics=...` stream.

#[derive(Debug, Deserialize)]
pub struct HeadEvent {
    #[serde(deserialize_with = "deser_u64_string")]
    pub slot: u64,
    pub block: BlockRoot,
    pub epoch_transition: bool,
}

#[derive(Debug, Deserialize)]
pub struct FinalizedCheckpointEvent {
    pub block: BlockRoot,
    #[serde(deserialize_with = "deser_u64_string")]
    pub epoch: u64,
}

#[derive(Debug, Deserialize)]
pub struct ChainReorgEvent {
    #[serde(deserialize_with = "deser_u64_string")]
    pub slot: u64,
    #[serde(deserialize_with = "deser_u64_string")]
    pub depth: u64,
    pub old_head_block: BlockRoot,
    pub new_head_block: BlockRoot,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_parse_accepts_lowercase_hex() {
        let s = "0xabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
        let r = Root::parse(s).unwrap();
        assert_eq!(r.as_str(), s);
    }

    #[test]
    fn root_parse_normalizes_uppercase_to_lowercase() {
        let upper = "0xABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD";
        let r = Root::parse(upper).unwrap();
        assert_eq!(
            r.as_str(),
            "0xabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd"
        );
    }

    #[test]
    fn root_parse_rejects_missing_prefix() {
        let no_prefix = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
        assert!(Root::parse(no_prefix).is_err());
    }

    #[test]
    fn root_parse_rejects_wrong_length() {
        assert!(Root::parse("0xabcd").is_err());
        assert!(Root::parse(&format!("0x{}", "a".repeat(63))).is_err());
        assert!(Root::parse(&format!("0x{}", "a".repeat(65))).is_err());
    }

    #[test]
    fn root_parse_rejects_non_hex() {
        let bad = "0xzzcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
        assert!(Root::parse(bad).is_err());
    }

    #[test]
    fn block_id_try_from_keywords() {
        assert!(matches!(BlockId::try_from("head").unwrap(), BlockId::Head));
        assert!(matches!(BlockId::try_from("HEAD").unwrap(), BlockId::Head));
        assert!(matches!(
            BlockId::try_from("genesis").unwrap(),
            BlockId::Genesis
        ));
        assert!(matches!(
            BlockId::try_from("finalized").unwrap(),
            BlockId::Finalized
        ));
    }

    #[test]
    fn block_id_try_from_slot() {
        let id = BlockId::try_from("12345").unwrap();
        assert!(matches!(id, BlockId::Slot(12345)));
    }

    #[test]
    fn block_id_try_from_root() {
        let root = "0xabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";
        let id = BlockId::try_from(root).unwrap();
        match id {
            BlockId::Root(r) => assert_eq!(r.as_str(), root),
            _ => panic!("expected BlockId::Root"),
        }
    }

    #[test]
    fn block_id_as_request_segment() {
        assert_eq!(BlockId::Slot(42).as_request_segment(), "42");
        assert_eq!(BlockId::Head.as_request_segment(), "head");
        assert_eq!(BlockId::Genesis.as_request_segment(), "genesis");
        assert_eq!(BlockId::Finalized.as_request_segment(), "finalized");
    }

    // Fixtures are real `/eth/v2/beacon/blocks/{slot}` responses captured from
    // a Lighthouse mainnet node — these tests catch drift between our per-fork
    // body types and the wire format without needing a live node.
    const PHASE0_FIXTURE: &str = include_str!("../../testdata/blocks/phase0.json");
    const ALTAIR_FIXTURE: &str = include_str!("../../testdata/blocks/altair.json");
    const BELLATRIX_FIXTURE: &str = include_str!("../../testdata/blocks/bellatrix.json");
    const CAPELLA_FIXTURE: &str = include_str!("../../testdata/blocks/capella.json");
    const DENEB_FIXTURE: &str = include_str!("../../testdata/blocks/deneb.json");
    const ELECTRA_FIXTURE: &str = include_str!("../../testdata/blocks/electra.json");
    const FULU_FIXTURE: &str = include_str!("../../testdata/blocks/fulu.json");

    fn parse(json: &str) -> (SignedBeaconBlock, bool) {
        serde_json::from_str::<RawBlockResponse>(json)
            .expect("fixture parses")
            .into_parts()
    }

    #[test]
    fn fixture_phase0_deserializes() {
        let (b, _) = parse(PHASE0_FIXTURE);
        assert_eq!(b.fork(), BeaconFork::Phase0);
        assert_eq!(b.slot(), 1_000_000);
        assert!(matches!(b.attestations(), Attestations::PreElectra(_)));
        assert!(b.sync_aggregate().is_none());
        assert!(matches!(b, SignedBeaconBlock::Phase0(_)));
    }

    #[test]
    fn fixture_altair_has_sync_aggregate_and_no_execution_payload() {
        let (b, _) = parse(ALTAIR_FIXTURE);
        assert_eq!(b.fork(), BeaconFork::Altair);
        assert_eq!(b.slot(), 3_500_000);
        assert!(b.sync_aggregate().is_some());
        assert!(matches!(b.attestations(), Attestations::PreElectra(_)));
    }

    #[test]
    fn fixture_bellatrix_has_execution_payload() {
        let (b, _) = parse(BELLATRIX_FIXTURE);
        assert_eq!(b.fork(), BeaconFork::Bellatrix);
        assert_eq!(b.slot(), 5_000_000);
        if let SignedBeaconBlock::Bellatrix(inner) = b {
            assert!(
                inner
                    .message
                    .body
                    .execution_payload
                    .block_hash
                    .starts_with("0x")
            );
        } else {
            panic!("expected Bellatrix variant");
        }
    }

    #[test]
    fn fixture_capella_has_withdrawals_field() {
        let (b, _) = parse(CAPELLA_FIXTURE);
        assert_eq!(b.fork(), BeaconFork::Capella);
        assert_eq!(b.slot(), 7_000_000);
        if let SignedBeaconBlock::Capella(inner) = b {
            let _ = &inner.message.body.execution_payload.withdrawals;
            let _ = &inner.message.body.bls_to_execution_changes;
        } else {
            panic!("expected Capella variant");
        }
    }

    #[test]
    fn fixture_deneb_has_blob_fields() {
        let (b, _) = parse(DENEB_FIXTURE);
        assert_eq!(b.fork(), BeaconFork::Deneb);
        if let SignedBeaconBlock::Deneb(inner) = b {
            let ep = &inner.message.body.execution_payload;
            let _ = ep.blob_gas_used;
            let _ = ep.excess_blob_gas;
            let _ = &inner.message.body.blob_kzg_commitments;
        } else {
            panic!("expected Deneb variant");
        }
    }

    #[test]
    fn fixture_electra_has_committee_bits_and_execution_requests() {
        let (b, _) = parse(ELECTRA_FIXTURE);
        assert_eq!(b.fork(), BeaconFork::Electra);
        match b.attestations() {
            Attestations::Electra(atts) => {
                assert!(!atts.is_empty(), "Electra block should have attestations");
                for a in atts {
                    assert!(a.committee_bits.starts_with("0x"));
                    // Electra sets data.index to 0 per EIP-7549.
                    assert_eq!(a.data.index, 0);
                }
            }
            _ => panic!("expected Electra attestations"),
        }
        if let SignedBeaconBlock::Electra(inner) = b {
            let _ = &inner.message.body.execution_requests;
        } else {
            panic!("expected Electra variant");
        }
    }

    #[test]
    fn fixture_fulu_matches_electra_shape() {
        let (b, _) = parse(FULU_FIXTURE);
        assert_eq!(b.fork(), BeaconFork::Fulu);
        assert!(matches!(b.attestations(), Attestations::Electra(_)));
        assert!(b.sync_aggregate().is_some());
    }
}
