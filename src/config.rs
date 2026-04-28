use clap::Parser;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Attestation scan strategy. `Auto` resolves to `Sparse` when few validators
/// are tracked, otherwise `Dense` — see [`ScanMode::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    /// Fetch every block in the epoch + late window; derive correctness from
    /// attestations vs the canonical chain. Amortises well for 30+ validators.
    Dense,
    /// Derive correctness from attestation rewards; scan forward block-by-block
    /// only for duties the rewards show were included. Designed for 1–2 tracked
    /// validators where the dense flow's per-epoch block fetch is mostly wasted.
    Sparse,
    /// Resolve at startup based on validator count. Default.
    Auto,
}

/// Resolved mode — no `Auto`. What the scanner actually runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveScanMode {
    Dense,
    Sparse,
}

impl ScanMode {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "dense" => Ok(Self::Dense),
            "sparse" => Ok(Self::Sparse),
            "auto" => Ok(Self::Auto),
            other => anyhow::bail!("invalid scan_mode '{other}' (expected dense|sparse|auto)"),
        }
    }

    /// Threshold below which `Auto` picks `Sparse`. Tuned so a handful of
    /// validators skip the 64-block per-epoch fetch; once the scan set grows
    /// past this, dense amortisation pays off.
    pub const AUTO_SPARSE_MAX: usize = 5;

    pub fn resolve(self, validator_count: usize) -> EffectiveScanMode {
        match self {
            Self::Dense => EffectiveScanMode::Dense,
            Self::Sparse => EffectiveScanMode::Sparse,
            Self::Auto => {
                if validator_count <= Self::AUTO_SPARSE_MAX {
                    EffectiveScanMode::Sparse
                } else {
                    EffectiveScanMode::Dense
                }
            }
        }
    }
}

#[derive(Parser, Debug, Clone)]
#[command(
    name = "dabeacon-indexer",
    about = "ETH Beacon Chain Validator Indexer"
)]
pub struct CliConfig {
    /// Beacon node URL (live tracking)
    #[arg(long, env = "BEACON_URL")]
    pub beacon_url: Option<String>,

    /// Optional separate beacon node URL for historical backfill. Typically an
    /// archive node, while `beacon_url` points at a lighter non-archive node.
    /// Falls back to `beacon_url` when unset.
    #[arg(long, env = "BACKFILL_BEACON_URL")]
    pub backfill_beacon_url: Option<String>,

    /// PostgreSQL connection string
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: Option<String>,

    /// Validator indices to track (comma-separated)
    #[arg(long, env = "VALIDATOR_INDICES", value_delimiter = ',')]
    pub validators: Vec<u64>,

    /// Path to TOML config file with validator metadata and settings
    #[arg(long, env = "CONFIG_FILE")]
    pub config_file: Option<PathBuf>,

    /// Web UI port
    #[arg(long, env = "WEB_PORT", default_value = "3000")]
    pub web_port: u16,

    /// Port for the Prometheus `/metrics` endpoint. Served on a dedicated,
    /// unauthenticated HTTP server — expose only on a private network.
    #[arg(long, env = "METRICS_PORT", default_value = "9090")]
    pub metrics_port: u16,

    /// API key for authentication (empty = no auth)
    #[arg(long, env = "API_KEY", default_value = "")]
    pub api_key: String,

    /// Run backfill and exit without starting live tracking or web server
    #[arg(long, env = "BACKFILL_ONLY", default_value_t = false)]
    pub backfill_only: bool,

    /// Earliest epoch the backfill is allowed to start from. Validators whose
    /// activation (or last_scanned+1) is older than this are clamped to this
    /// epoch. Leave unset for unlimited depth.
    #[arg(long, env = "MAX_BACKFILL_DEPTH")]
    pub max_backfill_depth: Option<u64>,

    /// Walk every epoch in the backfill range and, per validator, only scan
    /// if there is no finalized attestation_duties row yet. Useful after
    /// changing max_backfill_depth or when the DB is known to have gaps.
    #[arg(long, env = "NON_CONTIGUOUS_BACKFILL", default_value_t = false)]
    pub non_contiguous_backfill: bool,

    /// Attestation scan mode: `dense` (fetch every block, vote-based correctness),
    /// `sparse` (rewards-based, scan forward per duty), or `auto` (sparse when
    /// ≤5 validators tracked, else dense).
    #[arg(long, env = "SCAN_MODE", default_value = "auto")]
    pub scan_mode: String,
}

/// TOML config file structure
#[derive(Debug, Deserialize, Default)]
pub struct FileConfig {
    /// Beacon node URL (used if not set via CLI/env)
    #[serde(default)]
    pub beacon_url: Option<String>,

    /// Optional separate beacon node URL for backfill (typically an archive node).
    #[serde(default)]
    pub backfill_beacon_url: Option<String>,

    /// PostgreSQL connection string (used if not set via CLI/env)
    #[serde(default)]
    pub database_url: Option<String>,

    /// API key for authentication (empty or omitted = no auth)
    #[serde(default)]
    pub api_key: Option<String>,

    /// Validator entries with metadata
    #[serde(default)]
    pub validators: Vec<ValidatorEntry>,

    /// Backfill tuning (optional).
    #[serde(default)]
    pub backfill: Option<BackfillFileConfig>,

    /// Attestation scan mode (dense|sparse|auto). Default auto.
    #[serde(default)]
    pub scan_mode: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct BackfillFileConfig {
    #[serde(default)]
    pub max_depth: Option<u64>,
    #[serde(default)]
    pub non_contiguous: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ValidatorEntry {
    pub index: u64,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Merged runtime config
#[derive(Debug, Clone)]
pub struct Config {
    pub beacon_url: String,
    /// Separate beacon URL used by the backfill task. `None` means "reuse
    /// `beacon_url`" — both tasks share the one client.
    pub backfill_beacon_url: Option<String>,
    pub database_url: String,
    pub web_port: u16,
    pub metrics_port: u16,
    pub api_key: String,
    pub backfill_only: bool,
    pub max_backfill_depth: Option<u64>,
    pub non_contiguous_backfill: bool,
    pub scan_mode: ScanMode,
    pub validator_indices: Vec<u64>,
    /// Metadata per validator index (from config file)
    pub validator_meta: HashMap<u64, ValidatorMeta>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ValidatorMeta {
    pub tags: Vec<String>,
}

impl Config {
    pub async fn load() -> anyhow::Result<Self> {
        let cli = CliConfig::parse();

        let file_config = if let Some(ref path) = cli.config_file {
            let content = tokio::fs::read_to_string(path).await?;
            toml::from_str::<FileConfig>(&content)?
        } else {
            let default_path = PathBuf::from("config.toml");
            if tokio::fs::try_exists(&default_path).await? {
                let content = tokio::fs::read_to_string(&default_path).await?;
                toml::from_str::<FileConfig>(&content)?
            } else {
                FileConfig::default()
            }
        };

        let mut validator_indices: Vec<u64> = cli.validators.clone();
        for entry in &file_config.validators {
            validator_indices.push(entry.index);
        }

        validator_indices.sort();
        validator_indices.dedup();

        let mut validator_meta: HashMap<u64, ValidatorMeta> = HashMap::new();
        for entry in &file_config.validators {
            validator_meta.insert(
                entry.index,
                ValidatorMeta {
                    tags: entry.tags.clone(),
                },
            );
        }

        // CLI/env takes precedence over the config file for all settings below.
        let api_key = if !cli.api_key.is_empty() {
            cli.api_key.clone()
        } else {
            file_config.api_key.unwrap_or_default()
        };

        let beacon_url = cli
            .beacon_url
            .or(file_config.beacon_url)
            .unwrap_or_else(|| "http://localhost:5052".to_string());
        let backfill_beacon_url = cli.backfill_beacon_url.or(file_config.backfill_beacon_url);

        let database_url = cli.database_url.or(file_config.database_url).ok_or_else(|| {
            anyhow::anyhow!(
                "database_url is required. Provide --database-url, set DATABASE_URL, or set database_url in config file"
            )
        })?;

        let file_backfill = file_config.backfill.unwrap_or_default();
        let max_backfill_depth = cli.max_backfill_depth.or(file_backfill.max_depth);
        let non_contiguous_backfill =
            cli.non_contiguous_backfill || file_backfill.non_contiguous.unwrap_or(false);

        // CLI `scan_mode` defaults to the literal "auto", so a non-"auto" value
        // is a user override and wins; "auto" falls through to the file setting.
        let scan_mode_str = if cli.scan_mode != "auto" {
            cli.scan_mode
        } else {
            file_config.scan_mode.unwrap_or_else(|| "auto".to_string())
        };
        let scan_mode = ScanMode::parse(&scan_mode_str)?;

        Ok(Config {
            beacon_url,
            backfill_beacon_url,
            database_url,
            web_port: cli.web_port,
            metrics_port: cli.metrics_port,
            api_key,
            backfill_only: cli.backfill_only,
            max_backfill_depth,
            non_contiguous_backfill,
            scan_mode,
            validator_indices,
            validator_meta,
        })
    }
}
