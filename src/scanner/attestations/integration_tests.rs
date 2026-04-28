//! Integration tests for the dense vs sparse attestation pipelines.
//!
//! Both tests are `#[ignore]`-gated and require external services:
//!
//! - `BEACON_URL` — a beacon node serving the target epoch (archive node
//!   if you're testing a finalized epoch).
//! - `DATABASE_URL` — Postgres for the scanner to write into. **The
//!   tests delete rows in `attestation_duties` for the test
//!   epoch/validators.** Don't point at production.
//! - `TEST_EPOCH` — epoch to scan.
//! - `TEST_VALIDATORS` — comma-separated validator indices to track.
//!
//! Run with:
//!
//! ```sh
//! BEACON_URL=https://… \
//! DATABASE_URL=postgres://… \
//! TEST_EPOCH=12345 \
//! TEST_VALIDATORS=1,2,3 \
//! cargo test --bin dabeacon_indexer integration_tests -- --ignored --nocapture
//! ```
//!
//! # Equivalence test
//!
//! [`dense_sparse_attestation_rows_match`] runs both modes against the
//! same `(epoch, validators)`, snapshots the resulting
//! `attestation_duties` rows, and compares field-by-field. Numeric and
//! reward fields must match exactly. The `*_correct` fields are
//! **expected** to differ on late inclusions (sparse uses
//! reward-eligibility semantics — head requires delay==1, source
//! requires delay≤5 — while dense uses pure vote correctness); the test
//! reports those without failing.
//!
//! # Performance benchmark
//!
//! [`dense_vs_sparse_perf_bench`] alternates the two modes for `RUNS`
//! iterations and prints per-run + median timings. No assertion on
//! which is faster — the right answer depends on tracked-set size
//! (sparse wins for small, dense for large).

use std::collections::HashSet;
use std::env;
use std::time::{Duration, Instant};

use sqlx::Row;

use super::{process_epoch_attestation_duties, process_epoch_attestation_duties_sparse};
use crate::beacon_client::BeaconClient;
use crate::db;

#[derive(Debug, Clone, PartialEq)]
struct DutyRow {
    validator_index: i64,
    included: bool,
    inclusion_slot: Option<i64>,
    inclusion_delay: Option<i32>,
    effective_inclusion_delay: Option<i32>,
    source_correct: Option<bool>,
    target_correct: Option<bool>,
    head_correct: Option<bool>,
    source_reward: Option<i64>,
    target_reward: Option<i64>,
    head_reward: Option<i64>,
    inactivity_penalty: Option<i64>,
}

struct TestEnv {
    beacon_url: String,
    db_url: String,
    epoch: u64,
    validators: HashSet<u64>,
}

/// Read all four env vars; `None` if any missing or malformed.
fn test_env() -> Option<TestEnv> {
    let beacon_url = env::var("BEACON_URL").ok()?;
    let db_url = env::var("DATABASE_URL").ok()?;
    let epoch = env::var("TEST_EPOCH").ok()?.parse::<u64>().ok()?;
    let validators: HashSet<u64> = env::var("TEST_VALIDATORS")
        .ok()?
        .split(',')
        .filter_map(|s| s.trim().parse::<u64>().ok())
        .collect();
    if validators.is_empty() {
        return None;
    }
    Some(TestEnv {
        beacon_url,
        db_url,
        epoch,
        validators,
    })
}

/// Wipe `attestation_duties` rows for the test set so each run inserts
/// fresh values rather than hitting the upsert's `finalized = FALSE`
/// guard.
async fn truncate_duties(pool: &db::Pool, epoch: i64, validators: &[i64]) {
    sqlx::query("DELETE FROM attestation_duties WHERE epoch = $1 AND validator_index = ANY($2)")
        .bind(epoch)
        .bind(validators)
        .execute(pool)
        .await
        .expect("failed to truncate test rows");
}

async fn read_duty_rows(pool: &db::Pool, epoch: i64, validators: &[i64]) -> Vec<DutyRow> {
    let rows = sqlx::query(
        r#"SELECT validator_index, included,
                  inclusion_slot, inclusion_delay, effective_inclusion_delay,
                  source_correct, target_correct, head_correct,
                  source_reward, target_reward, head_reward, inactivity_penalty
           FROM attestation_duties
           WHERE epoch = $1 AND validator_index = ANY($2)
           ORDER BY validator_index"#,
    )
    .bind(epoch)
    .bind(validators)
    .fetch_all(pool)
    .await
    .expect("failed to read attestation_duties");

    rows.into_iter()
        .map(|r| DutyRow {
            validator_index: r.get("validator_index"),
            included: r.get("included"),
            inclusion_slot: r.get("inclusion_slot"),
            inclusion_delay: r.get("inclusion_delay"),
            effective_inclusion_delay: r.get("effective_inclusion_delay"),
            source_correct: r.get("source_correct"),
            target_correct: r.get("target_correct"),
            head_correct: r.get("head_correct"),
            source_reward: r.get("source_reward"),
            target_reward: r.get("target_reward"),
            head_reward: r.get("head_reward"),
            inactivity_penalty: r.get("inactivity_penalty"),
        })
        .collect()
}

/// Initialise the chain spec from the beacon node. Idempotent — safe to
/// call from every test.
async fn init_chain(client: &BeaconClient) {
    let chain_spec = client
        .get_chain_spec()
        .await
        .expect("failed to fetch chain spec");
    crate::chain::init(chain_spec);
}

/// Seed the `validators` table for the test set. The
/// `attestation_duties.validator_index` column has an FK on
/// `validators(validator_index)` — without this, the scanner's first
/// upsert blows up with a 23503 violation.
///
/// Mirrors what `main.rs` does at startup: pull each validator's record
/// from the beacon node and upsert it.
async fn seed_validators(client: &BeaconClient, pool: &db::Pool, validators: &[u64]) {
    let beacon_validators = client
        .get_validators("head", validators)
        .await
        .expect("failed to fetch validator metadata for seed");
    for v in &beacon_validators {
        let pubkey_bytes = hex::decode(v.validator.pubkey.trim_start_matches("0x"))
            .expect("invalid pubkey hex from beacon node");
        let exit_epoch = if v.validator.exit_epoch == u64::MAX {
            None
        } else {
            Some(v.validator.exit_epoch as i64)
        };
        db::scanner::validators::upsert_validator(
            pool,
            v.index as i64,
            &pubkey_bytes,
            v.validator.activation_epoch as i64,
            exit_epoch,
        )
        .await
        .expect("failed to seed validator row");
    }
}

/// Equivalence: dense and sparse must agree on every field except
/// `*_correct`, which has documented semantic differences (sparse
/// encodes reward-eligibility, dense encodes raw vote correctness).
///
/// Numeric and reward fields are asserted; `*_correct` mismatches are
/// reported but tolerated.
#[tokio::test]
#[ignore = "requires BEACON_URL, DATABASE_URL, TEST_EPOCH, TEST_VALIDATORS env vars"]
async fn dense_sparse_attestation_rows_match() {
    let env = test_env()
        .expect("set BEACON_URL, DATABASE_URL, TEST_EPOCH, TEST_VALIDATORS to run this test");

    let client = BeaconClient::new(&env.beacon_url);
    init_chain(&client).await;
    let pool = db::connect(&env.db_url)
        .await
        .expect("failed to connect to database");

    let validator_indices: Vec<i64> = env.validators.iter().map(|&v| v as i64).collect();
    let validator_u64: Vec<u64> = env.validators.iter().copied().collect();
    let epoch_i = env.epoch as i64;

    // Seed the `validators` table so the FK on attestation_duties holds.
    seed_validators(&client, &pool, &validator_u64).await;

    // Dense pass.
    truncate_duties(&pool, epoch_i, &validator_indices).await;
    process_epoch_attestation_duties(&client, &pool, env.epoch, &env.validators, false)
        .await
        .expect("dense scan failed");
    let dense_rows = read_duty_rows(&pool, epoch_i, &validator_indices).await;

    // Sparse pass.
    truncate_duties(&pool, epoch_i, &validator_indices).await;
    process_epoch_attestation_duties_sparse(&client, &pool, env.epoch, &env.validators, false)
        .await
        .expect("sparse scan failed");
    let sparse_rows = read_duty_rows(&pool, epoch_i, &validator_indices).await;

    eprintln!(
        "dense wrote {} rows; sparse wrote {} rows",
        dense_rows.len(),
        sparse_rows.len()
    );
    assert_eq!(
        dense_rows.len(),
        sparse_rows.len(),
        "row counts differ between modes"
    );

    let mut hard_failures: Vec<String> = Vec::new();
    let mut soft_diffs: Vec<String> = Vec::new();

    for (d, s) in dense_rows.iter().zip(sparse_rows.iter()) {
        assert_eq!(
            d.validator_index, s.validator_index,
            "row ordering mismatch"
        );
        let v = d.validator_index;

        // Hard equality — same beacon-node responses, same arithmetic.
        let mut hard = |field: &str, eq: bool, dense_v: String, sparse_v: String| {
            if !eq {
                hard_failures.push(format!(
                    "[v={v}] {field}: dense={dense_v}  sparse={sparse_v}"
                ));
            }
        };
        hard(
            "included",
            d.included == s.included,
            format!("{}", d.included),
            format!("{}", s.included),
        );
        hard(
            "inclusion_slot",
            d.inclusion_slot == s.inclusion_slot,
            format!("{:?}", d.inclusion_slot),
            format!("{:?}", s.inclusion_slot),
        );
        hard(
            "inclusion_delay",
            d.inclusion_delay == s.inclusion_delay,
            format!("{:?}", d.inclusion_delay),
            format!("{:?}", s.inclusion_delay),
        );
        hard(
            "effective_inclusion_delay",
            d.effective_inclusion_delay == s.effective_inclusion_delay,
            format!("{:?}", d.effective_inclusion_delay),
            format!("{:?}", s.effective_inclusion_delay),
        );
        hard(
            "source_reward",
            d.source_reward == s.source_reward,
            format!("{:?}", d.source_reward),
            format!("{:?}", s.source_reward),
        );
        hard(
            "target_reward",
            d.target_reward == s.target_reward,
            format!("{:?}", d.target_reward),
            format!("{:?}", s.target_reward),
        );
        hard(
            "head_reward",
            d.head_reward == s.head_reward,
            format!("{:?}", d.head_reward),
            format!("{:?}", s.head_reward),
        );
        hard(
            "inactivity_penalty",
            d.inactivity_penalty == s.inactivity_penalty,
            format!("{:?}", d.inactivity_penalty),
            format!("{:?}", s.inactivity_penalty),
        );

        // Soft equality — *_correct differs by definition for late
        // inclusions. Report but don't fail.
        let mut soft = |field: &str, eq: bool, dense_v: String, sparse_v: String| {
            if !eq {
                soft_diffs.push(format!(
                    "[v={v}] {field}: dense={dense_v}  sparse={sparse_v}"
                ));
            }
        };
        soft(
            "source_correct",
            d.source_correct == s.source_correct,
            format!("{:?}", d.source_correct),
            format!("{:?}", s.source_correct),
        );
        soft(
            "target_correct",
            d.target_correct == s.target_correct,
            format!("{:?}", d.target_correct),
            format!("{:?}", s.target_correct),
        );
        soft(
            "head_correct",
            d.head_correct == s.head_correct,
            format!("{:?}", d.head_correct),
            format!("{:?}", s.head_correct),
        );
    }

    if !soft_diffs.is_empty() {
        eprintln!(
            "\n{} *_correct differences (expected on late inclusions):",
            soft_diffs.len()
        );
        for d in &soft_diffs {
            eprintln!("  {d}");
        }
    }
    if !hard_failures.is_empty() {
        eprintln!("\n{} HARD MISMATCHES:", hard_failures.len());
        for f in &hard_failures {
            eprintln!("  {f}");
        }
        panic!(
            "{} hard-field mismatches between dense and sparse",
            hard_failures.len()
        );
    }
    eprintln!(
        "OK: {} rows, all numeric/reward fields agree",
        dense_rows.len()
    );
}

/// Performance comparison across an increasing tracked-set size. Treats
/// `TEST_VALIDATORS` as a pool, sweeps over sizes (powers of 2 up to the
/// pool size by default; override with `BENCH_SIZES=1,2,4,…`), and runs
/// `RUNS_PER_SIZE` iterations of each mode at each size for variance.
/// Reports a table of per-size medians + dense/sparse ratio.
///
/// No assertion — sparse is expected to win for small tracked sets and
/// lose for large ones; this test surfaces the crossover.
#[tokio::test]
#[ignore = "requires BEACON_URL, DATABASE_URL, TEST_EPOCH, TEST_VALIDATORS env vars"]
async fn dense_vs_sparse_perf_bench() {
    const RUNS_PER_SIZE: usize = 5;

    let env = test_env()
        .expect("set BEACON_URL, DATABASE_URL, TEST_EPOCH, TEST_VALIDATORS to run this bench");

    let client = BeaconClient::new(&env.beacon_url);
    init_chain(&client).await;
    let pool = db::connect(&env.db_url)
        .await
        .expect("failed to connect to database");

    // Use a stable order for subsetting so size=K always picks the same
    // K validators within one run.
    let mut validator_pool: Vec<u64> = env.validators.iter().copied().collect();
    validator_pool.sort();
    let max_size = validator_pool.len();
    let sizes = bench_sizes(max_size);
    let epoch_i = env.epoch as i64;

    // Seed all of them up front; subsequent passes only ever use a prefix.
    seed_validators(&client, &pool, &validator_pool).await;

    eprintln!(
        "Bench: epoch={} pool={} sizes={:?} runs/size={}",
        env.epoch, max_size, sizes, RUNS_PER_SIZE
    );
    eprintln!();
    eprintln!(
        "{:>5} | {:>15} | {:>15} | {:>10}",
        "N", "dense median", "sparse median", "ratio"
    );
    eprintln!("{:->5}-+-{:->15}-+-{:->15}-+-{:->10}", "", "", "", "");

    for &size in &sizes {
        let subset: HashSet<u64> = validator_pool[..size].iter().copied().collect();
        let subset_idx: Vec<i64> = validator_pool[..size].iter().map(|&v| v as i64).collect();

        // Warmup once at this size — the first call after a context
        // switch is dominated by cold caches and skews the first run.
        truncate_duties(&pool, epoch_i, &subset_idx).await;
        process_epoch_attestation_duties(&client, &pool, env.epoch, &subset, false)
            .await
            .expect("warmup dense failed");
        truncate_duties(&pool, epoch_i, &subset_idx).await;
        process_epoch_attestation_duties_sparse(&client, &pool, env.epoch, &subset, false)
            .await
            .expect("warmup sparse failed");

        let mut dense_times: Vec<Duration> = Vec::with_capacity(RUNS_PER_SIZE);
        let mut sparse_times: Vec<Duration> = Vec::with_capacity(RUNS_PER_SIZE);

        for run in 0..RUNS_PER_SIZE {
            truncate_duties(&pool, epoch_i, &subset_idx).await;
            let t0 = Instant::now();
            process_epoch_attestation_duties(&client, &pool, env.epoch, &subset, false)
                .await
                .expect("dense run failed");
            let dense = t0.elapsed();

            truncate_duties(&pool, epoch_i, &subset_idx).await;
            let t0 = Instant::now();
            process_epoch_attestation_duties_sparse(&client, &pool, env.epoch, &subset, false)
                .await
                .expect("sparse run failed");
            let sparse = t0.elapsed();

            eprintln!(
                "  size={:<3} run={}: dense {:>10?}  sparse {:>10?}",
                size, run, dense, sparse
            );
            dense_times.push(dense);
            sparse_times.push(sparse);
        }

        dense_times.sort();
        sparse_times.sort();
        let dense_median = dense_times[RUNS_PER_SIZE / 2];
        let sparse_median = sparse_times[RUNS_PER_SIZE / 2];
        let ratio = dense_median.as_secs_f64() / sparse_median.as_secs_f64();

        eprintln!(
            "{:>5} | {:>15?} | {:>15?} | {:>9.3}x",
            size, dense_median, sparse_median, ratio
        );
    }
}

/// Resolve the sweep sizes. Honours `BENCH_SIZES=1,2,4,8` if set,
/// otherwise picks powers of two up to `max_size` plus `max_size`
/// itself if it isn't already a power of two.
fn bench_sizes(max_size: usize) -> Vec<usize> {
    if let Ok(spec) = std::env::var("BENCH_SIZES") {
        let mut sizes: Vec<usize> = spec
            .split(',')
            .filter_map(|x| x.trim().parse::<usize>().ok())
            .filter(|&s| s > 0 && s <= max_size)
            .collect();
        sizes.sort();
        sizes.dedup();
        if !sizes.is_empty() {
            return sizes;
        }
    }
    let mut sizes: Vec<usize> = (0..)
        .map(|i| 1usize << i)
        .take_while(|&s| s <= max_size)
        .collect();
    if sizes.last() != Some(&max_size) {
        sizes.push(max_size);
    }
    sizes
}
