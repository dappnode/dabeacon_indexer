/// All recoverable failures produced inside the library. CLI-layer concerns
/// (config loading, argument parsing) stay in `anyhow` and don't go through
/// this type.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// Beacon node answered with a non-success status. `blocks.rs` matches on
    /// `status: 404` to translate "no block at this slot" into `Ok(None)`.
    #[error("Beacon API error: {status} - {message}")]
    BeaconApi { status: u16, message: String },

    /// Opportunistically-parsed payload (SSE events, etc.). Regular
    /// response-body parsing goes through `reqwest::Error` → `Http`.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    #[error("Invalid beacon block id: {0}")]
    InvalidBlockId(String),

    /// Required history could not be retrieved. This does not establish that
    /// the node returned contradictory data (it may have pruned the history).
    #[error("Beacon data unavailable: {0}")]
    BeaconDataUnavailable(String),

    #[error("Backfill epoch {epoch}: {source}")]
    BackfillEpoch {
        epoch: u64,
        #[source]
        source: Box<Error>,
    },

    /// Spec-level invariant violated by data from the beacon node (malformed
    /// SSZ, mismatched length, missing required entry). Fatal to the current
    /// scan so we don't persist inconsistent rows.
    #[error("Inconsistent beacon data: {0}")]
    InconsistentBeaconData(String),
}

impl Error {
    pub fn is_unavailable_input(&self) -> bool {
        matches!(
            self,
            Self::BeaconDataUnavailable(_) | Self::BeaconApi { status: 404, .. }
        )
    }
}

pub type Result<T> = std::result::Result<T, Error>;
