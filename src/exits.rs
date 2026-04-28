//! Helpers for filtering tracked validators by exit status.
//!
//! A validator with `exit_epoch <= epoch` is no longer attesting, proposing,
//! or voting in the sync committee. Scanning it past its exit only wastes
//! beacon-API calls and writes rows that will forever be "missed".

use std::collections::{HashMap, HashSet};

/// True if `validator` is still active at `epoch`.
pub fn is_active_at(exits: &HashMap<u64, u64>, validator: u64, epoch: u64) -> bool {
    exits.get(&validator).is_none_or(|&e| epoch < e)
}

/// Restrict `tracked` to validators still active at `epoch`.
pub fn active_at(tracked: &HashSet<u64>, exits: &HashMap<u64, u64>, epoch: u64) -> HashSet<u64> {
    tracked
        .iter()
        .copied()
        .filter(|&v| is_active_at(exits, v, epoch))
        .collect()
}
