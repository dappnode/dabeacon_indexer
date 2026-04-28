//! Centralised chain constants. Every chain-derived magic number the indexer
//! depends on lives here and is sourced from `/eth/v1/config/spec` at startup
//! via [`crate::beacon_client::BeaconClient::get_chain_spec`], so the code
//! works unchanged on mainnet, holesky, hoodi, or any custom network.
//!
//! Production: `main` calls [`init`] exactly once before spawning any task
//! that reads chain values. After that, the helper fns below ([`slots_per_epoch`],
//! [`altair_epoch`], …) are infallible.
//!
//! Tests: if [`init`] is never called, [`spec`] falls back to [`ChainSpec::MAINNET`]
//! so pure-logic unit tests work without network access.

use std::sync::OnceLock;

/// Every chain parameter the indexer reads at some point. Kept deliberately
/// small — add fields only when an actual call site needs them.
#[derive(Debug, Clone, Copy)]
pub struct ChainSpec {
    pub slots_per_epoch: u64,
    pub seconds_per_slot: u64,
    pub sync_committee_size: u64,
    pub max_committees_per_slot: u64,
    pub altair_fork_epoch: u64,
    pub epochs_per_sync_committee_period: u64,
}

impl ChainSpec {
    /// Mainnet values. Used as a fallback when [`init`] hasn't been called —
    /// happens in unit tests, never in production. Reachable only via the
    /// `cfg(test)` branch of [`spec`], hence the `allow(dead_code)`.
    #[allow(dead_code)]
    pub const MAINNET: Self = Self {
        slots_per_epoch: 32,
        seconds_per_slot: 12,
        sync_committee_size: 512,
        max_committees_per_slot: 64,
        altair_fork_epoch: 74_240,
        epochs_per_sync_committee_period: 256,
    };

    pub const fn epochs_per_day(&self) -> u64 {
        86_400 / (self.seconds_per_slot * self.slots_per_epoch)
    }
}

static SPEC: OnceLock<ChainSpec> = OnceLock::new();

/// Install the chain spec. First call wins; subsequent calls are no-ops
/// (safe for test harnesses that may call it from multiple places).
pub fn init(spec: ChainSpec) {
    let _ = SPEC.set(spec);
}

/// Handle to the installed spec.
///
/// - Production (`cfg(not(test))`): panics if [`init`] hasn't been called.
///   That's intentional — a caller running before spec fetch is a bug.
/// - Tests (`cfg(test)`): falls back to [`ChainSpec::MAINNET`] so unit tests
///   don't have to wire a mock beacon node.
pub fn spec() -> &'static ChainSpec {
    #[cfg(test)]
    {
        SPEC.get_or_init(|| ChainSpec::MAINNET)
    }
    #[cfg(not(test))]
    {
        SPEC.get()
            .expect("chain::init must be called at startup before any chain:: accessor is used")
    }
}

pub fn slots_per_epoch() -> u64 {
    spec().slots_per_epoch
}

pub fn altair_epoch() -> u64 {
    spec().altair_fork_epoch
}

pub fn sync_committee_size() -> u64 {
    spec().sync_committee_size
}

pub fn epochs_per_sync_committee_period() -> u64 {
    spec().epochs_per_sync_committee_period
}

/// The sync-committee period that contains `epoch`. Committee composition is
/// stable for the whole period, so this is the right cache key for
/// `/eth/v1/beacon/states/.../sync_committees`.
pub fn sync_committee_period(epoch: u64) -> u64 {
    epoch / epochs_per_sync_committee_period()
}

pub fn max_committees_per_slot() -> u64 {
    spec().max_committees_per_slot
}

pub fn epochs_per_day() -> u64 {
    spec().epochs_per_day()
}

pub fn epoch_start_slot(epoch: u64) -> u64 {
    epoch * slots_per_epoch()
}

pub fn slot_to_epoch(slot: u64) -> u64 {
    slot / slots_per_epoch()
}
