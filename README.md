# dabeacon-indexer

An official [DAppNode](https://dappnode.com) project.

Ethereum validator indexer for the consensus (beacon) layer. Tracks a set of validators, writes per-epoch attestation / proposal / sync-committee outcomes rewards to Postgres, and exposes a web UI + JSON API + live SSE stream.

The design balances simplicity with completeness: the beacon node is treated as the source of truth via its `/rewards/*` and `/duties/*` endpoints, and beacon-spec logic is reimplemented locally only where it clearly pays off.

Designed to share a beacon node that's also doing validation duties: historical backfill can be pointed at a separate archive node so the live (non-archive) node keeps serving duties uninterrupted.

---

## Features

- **Per-epoch attestation, proposal, sync-committee tracking** — inclusion slot, vote correctness (head / target / source), rewards.
- **Live head tracking** via beacon SSE (`head`, `finalized_checkpoint`, `chain_reorg`). Reorgs delete non-finalized rows and re-scan.
- **Concurrent live + backfill** in the same process. Each owns a disjoint epoch range (`live` owns `(f₀, ∞)`, `backfill` owns `[…=f₀]`) so no row is scanned twice. Live has foreground priority.
- **Optional split beacon clients** — point `backfill_beacon_url` at an archive node while the live client stays on your validator's attached (non-archive) node. On startup the indexer probes the backfill node and warns/errors if it can't serve the earliest epoch you need.
- **Chain-spec driven constants** — `SLOTS_PER_EPOCH`, `SECONDS_PER_SLOT`, `SYNC_COMMITTEE_SIZE`, `MAX_COMMITTEES_PER_SLOT`, `ALTAIR_FORK_EPOCH` are fetched from `/eth/v1/config/spec` at startup. Works unchanged on mainnet, holesky, hoodi, or any custom network.
- **Cross-fork block deserialization** — one typed struct per fork (phase0 → fulu). Electra attestation encoding (EIP-7549 `committee_bits`) is decoded correctly.
- **Multiple indexer instances, one DB** — per-validator watermarks use `GREATEST`; finalized rows are immutable; reorg deletes target only non-finalized rows. A non-archive head-tracker and an archive backfiller can target the same validator set on the same DB.
- **Strict data invariants** — malformed SSZ, size mismatches, missing committee entries, inclusion-slot < attestation-slot, etc. all surface as `Error::InconsistentBeaconData` and abort the epoch rather than writing partial data.
- **In-memory caches** on the beacon client: head slot (2 s TTL), head finality checkpoints (10 s TTL), per-epoch committees + proposer/attester/sync duties. Invalidated on reorg.

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
| `--beacon-url` / `BEACON_URL` | `beacon_url` | `http://localhost:5052` | Live client (head tracking, finality rescan, SSE). Non-archive nodes are supported but **experimental**, and the beacon node has to be configured accordingly. |
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
| `--backfill-only` / `BACKFILL_ONLY` | `false` | Run backfill, then exit. No web server, no live tracking. Useful for first-time seed runs against an archive node. |
| `--max-backfill-depth` / `MAX_BACKFILL_DEPTH` | *(unlimited)* | Clamp the earliest epoch backfill will start from. Protects against accidentally re-scanning from genesis for a newly-added validator. |
| `--non-contiguous-backfill` / `NON_CONTIGUOUS_BACKFILL` | `false` | Walk every epoch in the backfill range and scan only those (validator, epoch) pairs that don't already have a finalized row. Use after widening validator set or reducing `max_backfill_depth`. |
| `--scan-mode` / `SCAN_MODE` | `auto` | Attestation scan strategy. `dense` fetches every block in the epoch and derives correctness from attestations vs the canonical chain — amortises well for 30+ validators. `sparse` derives correctness from rewards and scans forward block-by-block only for duties the rewards show were included — ~4–5 API calls per epoch vs ~100 for dense. `auto` resolves to `sparse` when 5 or fewer validators are tracked. See [scan mode semantics](#attestation-scan-modes). |

### Web server

| Flag | Default | |
|---|---|---|
| `--web-port` / `WEB_PORT` | `3000` | HTTP port. Bound to `0.0.0.0`. |
| `--api-key` / `API_KEY` | *(empty)* | If set, required as `?api_key=…` on `/api/live/sse`. Read-only REST endpoints are always open. |

---

## Running modes

### Default: live + backfill concurrently

```bash
cargo run --release
```

On startup the indexer:

1. Connects to the beacon node, fetches the chain spec.
2. Reads the current finality checkpoint `f₀`.
3. Spawns a background task that backfills epochs `[…=f₀]` (using the backfill client — archive node if configured).
4. Runs live head tracking in the foreground, owning epochs `(f₀, ∞)`.
5. The web server runs from startup with both DB reads and the SSE stream.

Live and backfill never touch the same epoch. `finalized=true` rows are immutable; live's `finalized=false` rows get promoted on the next `finalized_checkpoint` event.

### Backfill only

```bash
cargo run --release -- --backfill-only
```

Runs the historical backfill until everything up to current finality is covered, then exits. Use this for the first run against an archive node to seed the database.

Omit the web server. Re-extends finality if the chain advances mid-pass.

### Split archive / non-archive nodes

Your validator's beacon node stays on live tracking. An archive node (or any beacon node configured with full historical state) handles the backfill.

```toml
# config.toml
beacon_url = "http://10.0.0.10:5052"           # non-archive, attached to validator
backfill_beacon_url = "http://10.0.0.20:5052"  # archive
```

The startup flow probes the backfill client at the earliest epoch it will try to scan. If the probe fails **and** `backfill_beacon_url` is set, the indexer exits with a descriptive error (`backfill_beacon_url must be an archive node`). If no dedicated URL is set and the shared client can't serve the range, you get a warning that tells you exactly which flag to set.

### Resuming after downtime

Restarting any time just works; the indexer resumes from each validator's `last_scanned_epoch` watermark. If the gap exceeds the live node's retention (~1 day on a default Lighthouse) the startup probe warns you to set `backfill_beacon_url` + `--non-contiguous-backfill` to refill the gap.

---

## Attestation scan modes

`--scan-mode` controls the attestation stage of finalized scans (backfill + finalization rescan). Live scans are unaffected.

### Dense (default for >5 validators)

Fetches every block in the epoch and a one-epoch "late window" for inclusion discovery (~64 blocks), builds a canonical block-root map, and computes correctness by comparing each attestation's votes against the chain. Amortises well when most slots have at least one tracked duty.

### Sparse (default for ≤5 validators)

Fetches `/eth/v1/validator/duties/attester/{epoch}` + `/eth/v1/beacon/rewards/attestations/{epoch}` plus (cached) committees. For each tracked duty whose rewards show inclusion, scans forward block-by-block from the duty's slot until the including block is found. For duties the rewards show as missed, skips the block scan entirely.

Typical network cost drops from ~100 calls/epoch to ~4–5, making long backfills on tiny validator sets practical.

### Semantic difference to be aware of

Dense mode's `*_correct` columns mean "the vote was right". Sparse mode's mean "the validator earned the reward for that component", which requires correct vote AND timely inclusion (next slot for head, within ~5 for source, within 32 for target). A correct head vote included one slot late reads `head_correct = false` in sparse but `true` in dense. Operators typically care about the reward-qualifying definition; sparse mode surfaces that directly.

All reward columns, the `included` flag, `inclusion_slot`, and `inclusion_delay` match between modes (modulo rare beacon-node quirks where rewards show inclusion but the block scan can't locate it — then `inclusion_slot` is NULL and a warning is logged).

---

## Multi-instance (sharing a Postgres)

Safe scenarios:

- **Two indexers tracking disjoint validator sets** (e.g. one handles indices 1–500, another handles 501–1000). Per-validator watermarks + row upserts guarantee independence.
- **One head-tracker + one dedicated archive backfiller on the same set.** The backfiller writes `finalized=true` rows; the head-tracker writes `finalized=false` rows that get promoted on finality. Reorg deletes only non-finalized rows, so the backfiller's output is immutable against the head-tracker's reorg path.

Unsafe:

- Two head-trackers on overlapping validator sets — they'll churn on every reorg (each deletes the other's non-finalized rows).

The `finalized=true`-means-immutable invariant is load-bearing. It's documented on every write function in `src/db/scanner/*` and on `scanner::scan_epoch`. Don't violate it.

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
| `instances` | `instance_id` (UUID) | `heartbeat` (for observability only) |

`finalized` bool column on the three duty tables; upserts gate on `WHERE finalized = FALSE`, so finalized rows are immutable.

---

## Development

```bash
cargo build
cargo test        # 64 unit tests, offline (uses captured block fixtures)
cargo clippy --all-targets
```

Fixtures live under `testdata/blocks/` — one captured block per fork (phase0 → fulu) from a live Lighthouse node. `cargo test` doesn't hit the network.

### Integration tests

`scripts/run-integration-tests.sh` spins up an ephemeral Postgres via `docker-compose.test.yml`, picks a random recent finalized epoch + a random proposer subset, and runs the dense-vs-sparse equivalence check and the performance bench against a real beacon node. Copy `.env.test.example` to `.env.test` and set `BEACON_URL` first; pin `TEST_EPOCH` / `TEST_VALIDATORS` there to reproduce a specific run. Both tests are `#[ignore]`d in `cargo test` since they need beacon + DB access.

### Key invariants (before touching the scanner / DB)

1. **Backfill must always pass `finalized=true` to `scan_epoch`.** This makes its rows immune to reorg deletes and upsert overwrites.
2. **Live must pass `finalized=false`** and rely on `db::scanner::finalization::finalize_up_to_epoch` to promote them.
3. **Every write upsert has `WHERE finalized = FALSE`** — preserve that guard.
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
