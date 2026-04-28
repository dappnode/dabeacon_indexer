//! Write-side (and a few scanner-internal reads) queries consumed by the
//! scanner / backfill / live / main-loop pipelines. Parallel to `db::api`,
//! which holds the read-side queries behind the web endpoints.

pub mod attestations;
pub mod finalization;
pub mod instance;
pub mod proposals;
pub mod sync;
pub mod validators;
