# dabeacon-indexer

An official [DAppNode](https://dappnode.com) project.

Ethereum validator indexer for the consensus (beacon) layer. Tracks a set of validators, writes per-epoch attestation / proposal / sync-committee outcomes rewards to Postgres, and exposes a web UI + JSON API + live SSE stream.

The design balances simplicity with completeness: the beacon node is treated as the source of truth via its `/rewards/*` and `/duties/*` endpoints, and beacon-spec logic is reimplemented locally only where it clearly pays off.

Designed to share a beacon node that's also doing validation duties: historical backfill can be pointed at a separate archive node so the live (non-archive) node keeps serving duties uninterrupted.

---

## Features

- **Per-epoch attestation, proposal, sync-committee tracking** — inclusion slot, vote correctness (head / target / source), rewards.
- **Live head tracking** via beacon SSE (`head`, `finalized_checkpoint`, `chain_reorg`). Reorgs delete non-finalized rows and re-scan.
- **Concurrent live + backfill**: recent collection starts independently of historical catch-up. The archive worker also repairs persisted live gaps after finality, including gaps created after startup.
- **Optional split beacon clients** — point `backfill_beacon_url` at an archive node while the live client stays on your validator's attached (non-archive) node. Combined mode starts live tracking independently and retries unavailable backfill every 60 seconds. Backfill-only mode probes historical state and exits on failure.
- **Chain-spec driven constants** — `SLOTS_PER_EPOCH`, `SECONDS_PER_SLOT`, `SYNC_COMMITTEE_SIZE`, `MAX_COMMITTEES_PER_SLOT`, `ALTAIR_FORK_EPOCH` are fetched from `/eth/v1/config/spec` at startup. Works unchanged on mainnet, holesky, hoodi, or any custom network.
- **Cross-fork block deserialization** — one typed struct per fork (phase0 → fulu). Electra attestation encoding (EIP-7549 `committee_bits`) is decoded correctly.
- **Single live writer per validator set** — completed finalized scans are immutable and reorg cleanup only removes unfinalized evidence. Coordination between overlapping live writers remains out of scope.
- **Strict data invariants** — malformed SSZ, size mismatches, missing committee entries, inclusion-slot < attestation-slot, etc. all surface as `Error::InconsistentBeaconData` and leave the epoch incomplete. Successful earlier writes are retained for retry.
- **Durable recent input cache** for committees and proposer/attester/sync duties, validated against canonical anchors before reuse after restart.

---

## Architecture at a glance

```
                   ┌───────────────┐
                   │ beacon node   │  ── /eth/v1/events (SSE)
                   │ (live client) │◄─ /eth/v2/beacon/blocks, /duties, …
                   └──────┬────────┘
                          │
                ┌─────────┴─────────┐
                │  beacon_client/   │  HTTP + retries + caches
                └─────────┬─────────┘
                          │
      ┌───────────────────┼───────────────────┐
      ▼                   ▼                   ▼
┌──────────┐      ┌────────────────┐   ┌────────────┐
│ live/    │      │ scanner/       │   │ backfill.rs│
│ head     │      │ (epoch scan)   │   │ (historical│
│ finality │      │ attestations   │   │  catch-up) │
│ reorg    │      │ proposals      │   │            │
└────┬─────┘      │ sync           │   └─────┬──────┘
     │            └────────┬───────┘         │
     │                     │                 │
     │                     ▼                 │
     │              ┌──────────────┐         │
     └─────────────►│ db/scanner/  │◄────────┘
                    │ (writes)     │
                    └──────┬───────┘
                           │
                           │ Postgres
                           ▼
                    ┌──────────────┐
                    │ db/api/      │
                    │ (reads)      │
                    └──────┬───────┘
                           │
                           ▼
                    ┌──────────────┐
                    │ web/         │   REST + /api/live/sse
                    │ (axum)       │
                    └──────────────┘
```

Module map:

- `beacon_client/` — Beacon API HTTP client, per-endpoint wrappers, wire types.
- `scanner/` — per-epoch scan pipeline (blocks → duties → rewards → DB writes).
- `live/` — SSE consumer for live head, finalization, chain_reorg.
- `backfill.rs` — historical catch-up, archival-capability probe.
- `db/scanner/` — write-side queries (upsert, finalize, delete).
- `db/api/` — read-side queries behind web endpoints.
- `web/` — Axum HTTP server, REST API, live SSE.
- `chain.rs` — chain-spec accessors (`slots_per_epoch()`, `altair_epoch()`, …).
- `config.rs` — CLI + TOML + env merge.
- `error.rs` — single `Error` enum shared across the crate.

---

## Requirements

- **PostgreSQL 14+**
- **Rust** (edition 2024; see `Cargo.toml`)
- **A beacon node** with `/eth/v1/events`, `/eth/v2/beacon/blocks/{id}`, `/eth/v1/validator/duties/*`, `/eth/v1/beacon/rewards/*`, `/eth/v1/config/spec`. Lighthouse, Prysm, Nimbus, Teku, Lodestar all work.

Optional but recommended when running in combined live + backfill mode:

- **A second, archive beacon node** (e.g. `lighthouse beacon ... --reconstruct-historic-states`) pointed at by `backfill_beacon_url`. Keeps the live node unburdened by deep-history queries.

---

## Quick start

```bash
# 1. Start Postgres
docker compose up -d db

# 2. Configure
cp config.example.toml config.toml
$EDITOR config.toml         # set validators, beacon_url, (optional) backfill_beacon_url

# 3. Set DB URL
export DATABASE_URL=postgres://dabeacon:dabeacon@localhost:5432/dabeacon

# 4. Run
cargo run --release
```

Open `http://localhost:3000`.

---

## Configuration

All settings may be passed via CLI flag, env var, or TOML file. Precedence: CLI > env > TOML > built-in default. See `config.example.toml`.

### Beacon nodes

| CLI / env | TOML | Default | Purpose |
|---|---|---|---|
| `--beacon-url` / `BEACON_URL` | `beacon_url` | `http://localhost:5052` | Live client (recent collection, finality verification, SSE). Requires recent blocks, duties, and applicable reward APIs; unavailable components remain explicit gaps. |
| `--backfill-beacon-url` / `BACKFILL_BEACON_URL` | `backfill_beacon_url` | *(shares live)* | Optional separate client for historical backfill. Must be archive-capable if set. |

### Database

| CLI / env | TOML | Required |
|---|---|---|
| `--database-url` / `DATABASE_URL` | `database_url` | Yes |

Migrations in `migrations/` apply automatically at startup.

### Validators to track

Either via CLI:

```bash
dabeacon-indexer --validators 123,456,789
```

or via TOML (with optional tags for UI grouping):

```toml
[[validators]]
index = 123
tags = ["pool-a", "node-1"]

[[validators]]
index = 456
tags = ["pool-a", "node-2"]
```

### Mode flags

| Flag | Default | Behaviour |
|---|---|---|
| `--mode` / `RUN_MODE` | `both` | Which workloads to run: `live` (head tracking + finality rescans + web server only — no historical backfill), `backfill` (one-shot historical catch-up, no live, no web), or `both`. See [Running modes](#running-modes). |
| `--max-backfill-depth` / `MAX_BACKFILL_DEPTH` | *(unlimited)* | Clamp the earliest epoch backfill will start from. Protects against accidentally re-scanning from genesis for a newly-added validator. |
| `--non-contiguous-backfill` / `NON_CONTIGUOUS_BACKFILL` | `false` | Walk every epoch in the backfill range and scan only those (validator, epoch) pairs that don't already have a completed scan. Use after widening validator set or reducing `max_backfill_depth`. |
| `--scan-mode` / `SCAN_MODE` | `auto` | Attestation scan strategy. `dense` fetches every block in the epoch and derives correctness from attestations vs the canonical chain — amortises well for 30+ validators. `sparse` scans forward per duty and compares observed votes with canonical duty/boundary roots; zero rewards never establish a miss. `auto` resolves to `sparse` when 5 or fewer validators are tracked. See [scan mode semantics](#attestation-scan-modes). |

### Web server

| Flag | Default | |
|---|---|---|
| `--web-port` / `WEB_PORT` | `3000` | HTTP port. Bound to `0.0.0.0`. |
| `--api-key` / `API_KEY` | *(empty)* | If set, required as `?api_key=…` on `/api/live/sse`. Read-only REST endpoints are always open. |

---

## Running modes

Selected via `--mode {live|backfill|both}` (env `RUN_MODE`, default `both`). Every mode is independently selectable; `live` and `backfill` are *not* shorthands for each other.

### `both` — default: live + backfill concurrently

```bash
cargo run --release
```

On startup the indexer:

1. Connects to the beacon node, fetches the chain spec, seeds validator metadata.
2. Reads the current finality checkpoint and sets the safe scan target `f₀` to its epoch minus two (no safe epoch yet if the checkpoint is below two).
3. Spawns a background task that backfills epochs `[…=f₀]` (using the backfill client — archive node if configured).
4. Starts recent live collection with durable per-slot coverage, independent reward jobs, and periodic reconciliation.
5. The web server runs from startup with both DB reads and the SSE stream.
6. A periodic reconcile task re-fetches active validators' state every epoch so exits land in the DB without a process restart.

Historical backfill and complete live scans use the same conservative completion boundary: epoch E is complete when checkpoint E+2 is finalized, because attestations from E can still be included through E+1. Individual positive duty outcomes can become final sooner: an observed attestation is final when its inclusion block is on finalized ancestry, and proposal/sync outcomes are final when their slot is behind the finalized checkpoint. Attestation absence remains unknown until the full inclusion window is covered. Reward availability does not delay these finality flags. `completed_scans` records complete data separately from on-chain finality. Incomplete finalized rows can be repaired by a later finalized scan; live writes cannot overwrite them.

### `backfill` only

```bash
cargo run --release -- --mode backfill
```

Runs the historical backfill until everything up to current finality is covered, then exits. Use this for the first run against an archive node to seed the database.

No web server, no live tracking. Re-extends finality if the chain advances mid-pass. The validator-state reconcile loop is skipped — backfill processes finalized history where state at any past epoch is fully determined; only the startup seed is needed.

### `live` only

```bash
cargo run --release -- --mode live
```

Recent collection + finality verification + web server, **no** historical backfill. Two situations where this is the right pick:

- **Multi-instance**: historical data is owned by a separate dedicated backfill instance writing to the same Postgres (typical: one head-tracker per validator's local non-archive node, one shared archive backfiller). Recent collection does not certify older history; use completed-scan coverage to identify gaps.
- **No archive client available**: you only have a non-archive beacon node and don't want to run an archive node. Live mode keeps working — but **the DB will only contain data from the moment this instance first started forward**, so any view spanning epochs older than that startup will show gaps. Acceptable for "I just want to track from now on", not for historical analysis.

### Split archive / non-archive nodes

Your validator's beacon node stays on live tracking. An archive node (or any beacon node configured with full historical state) handles the backfill.

```toml
# config.toml
beacon_url = "http://10.0.0.10:5052"           # non-archive, attached to validator
backfill_beacon_url = "http://10.0.0.20:5052"  # archive
```

In `both` mode, an unavailable archive node does not delay live startup or stop the process. Failed backfill attempts retry after 60 seconds, skipping completed epochs. After catch-up, the worker continues repairing persisted live gaps with a bounded budget. In `backfill` mode, failures still exit with an error so a one-shot job cannot report success with missing history.

### Resuming and repairing gaps

Normal backfill resumes from validator watermarks. A watermark records how far the indexer has progressed, not proof that every earlier epoch is complete. Run with `--non-contiguous-backfill` and an archive node to repair missing rewards, failed scan stages, or gaps from downtime. Completion is recorded only after attestations, proposals, and sync duties all succeed.

Without an archive node, live tracking collects recent block outcomes and proposal/sync rewards while their pre-states are available. Attestation rewards for E are fetched in E+2, with independent retries. Duties and committee mappings survive restart in a cache checked against canonical block anchors. Successful reward responses are staged even when their duty rows cannot yet be joined. Finalization verifies stored ancestry and coverage; it does not require another historical-state scan.

SSE wakes a single writer; polling and reconnect reconciliation recover missed events. Block ancestry is resolved by parent roots. Unavailable named blocks remain unresolved, and missing data never becomes a confirmed missed duty just because time has passed. Long outages leave gaps for archive repair while recent collection resumes. Retry limits control request effort; they do not prove an endpoint is permanently unsupported.

```toml
[live]
lag_slots = 2
poll_interval_seconds = 12
retry_window_epochs = 4
```

`--live-lag-slots` / `LIVE_LAG_SLOTS` overrides the block lag. The other live settings are TOML options. Two slots is an initial operational default, not a measured universal guarantee. State retention and reward endpoint availability vary across clients; there is no portable one-day retention assumption. The node must serve the recent block range used for reconciliation. Explicit optimistic responses are rejected. Missing optional metadata is not treated as proof of finality: authoritative completion additionally requires the node's finalized checkpoint and connected ancestry. The model assumes a coherent, trustworthy beacon endpoint.

`live_pending_jobs{status="pending"|"needs_backfill"}`, `live_gap_validator_epochs`, `live_oldest_gap_epoch`, and `live_last_reward_success_epoch` expose unresolved work and recent collection progress. Old gaps remain repairable; `--non-contiguous-backfill` also finds historical gaps predating live job records. In `live` mode, reward failures cannot be repaired automatically after their state is pruned.

**Existing databases:** the single pending migration adds completion, recent evidence, input cache, and job tables, plus `inclusion_known`. Legacy rows remain unverified until a successful scan. Confirmed inclusions remain visible; absence is exposed as unknown until coverage is proven. `001_initial.sql` is unchanged; all undeployed additions are consolidated into `002_epoch_scan_completion.sql`.

---

## Attestation scan modes

`--scan-mode` controls the attestation stage of finalized archive scans. Live scans are unaffected.

### Dense (default for >5 validators)

Fetches every block in the epoch and a one-epoch "late window" for inclusion discovery (~64 blocks), builds a canonical block-root map, and computes correctness by comparing each attestation's votes against the chain. Amortises well when most slots have at least one tracked duty.

### Sparse (default for ≤5 validators)

Fetches duties, rewards, and committees, then scans forward for every tracked duty until inclusion is found or the full inclusion window is covered. Zero or negative rewards do not skip inclusion discovery. Requests can still be cheaper for small validator sets, but the old 4–5-call estimate did not cover missed duties correctly.

### Correctness fields

Both modes use `*_correct` for vote correctness and compare observed votes with canonical roots. Live collection does the same immediately, independently of rewards. A correct but late head vote can earn zero head reward. Conflicting multiple attestations are not modeled as separate votes; the fields describe the earliest observed inclusion.

`included` is `true` for observed inclusion, `false` for a proven miss, and `null` for incomplete coverage in live views. The attestation list returns only included attestations and proven misses, excluding pending assignment seeds. Unknown duties are excluded from missed-duty counts and participation-rate denominators. Reward values remain nullable when unavailable. Live collection derives adjusted delay from resolved ancestry; legacy rows with unavailable ancestry show their raw delay and explicit unknown vote results. See [live collection and field semantics](docs/live-collection.md) for finality, repair, and readiness behavior.

---

## Multi-instance (sharing a Postgres)

Supported without writer coordination:

- **Two indexers tracking disjoint validator sets** (e.g. one handles indices 1–500, another handles 501–1000). Per-validator watermarks + row upserts guarantee independence.
Unsupported:

- Multiple writers on overlapping validator sets, including a separate live and archive process. Use `both` mode in one process when live and archive collection share validators.

Live writes cannot overwrite finalized rows. Finalized rescans can repair an incomplete epoch until its `completed_scans` marker is written. Multi-instance coordination remains outside the scope of these changes.

---

## API surface

### REST

All read-only, never requires auth (regardless of `api_key`).

| Method + path | Returns |
|---|---|
| `GET /api/validators` | Per-validator summary with rates |
| `GET /api/stats` | Global aggregate counts + rates |
| `GET /api/epochs` | Per-epoch summaries (paginated, filterable) |
| `GET /api/attestations` | Per-(validator, epoch) attestation duties (paginated, filterable) |
| `GET /api/proposals` | Per-slot block proposals (paginated, filterable) |
| `GET /api/sync_duties` | Per-(validator, slot) sync-committee duties (paginated, filterable) |
| `GET /api/rewards` | 1-day / 7-day / 30-day / all-time reward windows per validator + totals |
| `GET /api/meta` | Tracked validators list + tag map |

Common pagination params: `page`, `per_page`, `order=asc|desc`. Filters vary by endpoint (see `src/web/api/*.rs`).

### SSE

| Path | Purpose |
|---|---|
| `GET /api/live/sse?api_key=…` | Live per-slot view for the current + previous epoch: tracked attesters, proposer + proposal outcome, sync committee participation. Refreshes on every broadcast event (live slot scanned, finalization, backfill epoch processed) and at least every 6 s. |

### Static web UI

Served from `web/` at `/`. A minimal SvelteKit frontend consuming the REST endpoints above.

---

## Database

Schema in `migrations/`. Key tables:

| Table | Key | Contents |
|---|---|---|
| `validators` | `validator_index` | pubkey, activation/exit epochs, `last_scanned_epoch` |
| `attestation_duties` | `(validator_index, epoch)` | inclusion slot/delay, correctness, rewards |
| `sync_duties` | `(validator_index, slot)` | participated, reward, missed_block |
| `block_proposals` | `slot` | proposer_index, proposed, rewards |
| `completed_scans` | `(validator_index, epoch)` | verified completion across applicable components |
| `beacon_inputs` | `input_key` | durable assignments/mappings and canonical anchors |
| `live_blocks` / `live_coverage` | slot / `(validator_index, slot)` | recent ancestry and proven processing coverage |
| `live_jobs` | validator, epoch, slot, component | retry state, dependency root, staged reward response |
| `instances` | `instance_id` (UUID) | `heartbeat` (for observability only) |

`finalized` tracks on-chain finality on the three duty tables. `completed_scans`, keyed by `(validator_index, epoch)`, tracks successful scans across all three stages. Upserts protect completed finalized rows while allowing repair of incomplete ones.

---

## Development

```bash
cargo build
cargo test        # offline unit tests (uses captured block fixtures)
cargo clippy --all-targets
```

Fixtures live under `testdata/blocks/` — one captured block per fork (phase0 → fulu) from a live Lighthouse node. `cargo test` doesn't hit the network.

### Integration tests

`scripts/run-integration-tests.sh` spins up an ephemeral Postgres via `docker-compose.test.yml`, picks a random recent finalized epoch + a random proposer subset, and runs the dense-vs-sparse equivalence check and the performance bench against a real beacon node. Copy `.env.test.example` to `.env.test` and set `BEACON_URL` first; pin `TEST_EPOCH` / `TEST_VALIDATORS` there to reproduce a specific run. Both tests are `#[ignore]`d in `cargo test` since they need beacon + DB access. The recovery tests need only a fresh disposable PostgreSQL database: set `RECOVERY_TEST_DATABASE_URL` and run `cargo test -- --ignored --skip dense_sparse_attestation_rows_match --skip dense_vs_sparse_perf_bench`. These cover partial scans, finality boundaries, reorg cleanup, and a mock beacon node with unavailable historical state.

### Key invariants (before touching the scanner / DB)

1. **Backfill must always pass `finalized=true` to `scan_epoch`.** Only scan through `chain::finalized_scan_target`; finalized rows are immune to reorg deletes and live overwrites.
2. **Live writes remain provisional** until stored dependencies connect to finalized ancestry. Positive duty observations may then become final independently of rewards; proven misses and `completed_scans` retain their full coverage requirements.
3. **Upserts protect completed finalized scans.** Incomplete finalized rows remain repairable; live writes cannot replace finalized rows.
4. **Reorg deletes only match `finalized = FALSE`.** Same reasoning.
5. **Malformed data is always fatal to the epoch**, never silently tolerated. See `Error::InconsistentBeaconData`.

These are documented as doc-comments on the relevant functions in `src/db/scanner/*` and `src/scanner/mod.rs`.

---

## Errors

One error type, `crate::error::Error`. Variants:

- `Http(reqwest::Error)` — transport-layer failure.
- `BeaconApi { status, message }` — non-2xx from beacon node. Pattern-matched on `status: 404` where missing-slot vs error needs distinguishing.
- `Json(serde_json::Error)` — SSE payload parse failure.
- `Database(sqlx::Error)` / `Migration(sqlx::migrate::MigrateError)` — DB.
- `InvalidBlockId(String)` — locally-constructed id validation.
- `InconsistentBeaconData(String)` — spec-level data violation from the node.

CLI-level startup (config parsing) uses `anyhow`; beyond that everything is the typed enum.

---

## License

GNU General Public License v3.0 or later. See [LICENSE](LICENSE) for the full text or <https://www.gnu.org/licenses/gpl-3.0.html>.
