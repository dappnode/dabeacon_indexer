//! Read-side queries used by the web API. Each submodule mirrors a route family
//! in `web/api/`. Nothing in here is consumed by the scanner pipeline — writes
//! live in [`crate::db::scanner`].

pub mod attestations;
pub mod epochs;
pub mod live;
pub mod proposals;
pub mod rewards;
pub mod stats;
pub mod sync;
pub mod validators;
