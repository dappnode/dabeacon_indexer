Implementation notes for [R1–R9](beacon-node-resilience-issues.md).

| Issue | Implemented behavior |
|---|---|
| R1 | Parent-root resolution proves skipped slots; named-root failures never trigger competing slot substitutions. |
| R2 | Metadata-preserving requests reject explicit optimism. Recent ancestry, input anchors, and staged reward dependencies are retained. Epoch rewards are bracketed with state/boundary checks. |
| R3 | Per-validator component jobs survive errors and restart, use bounded time-based retries, and retain old gaps for backfill. Metrics expose pending work and its oldest epoch. |
| R4 | Duties and mappings persist in a canonical-anchor-validated cache. Current inputs are prefetched before recent processing; future state IDs are not assumed available. Reorgs invalidate reuse when anchors change. |
| R5 | Attestation reward collection is independent of old committee lookup; proposal/sync rewards are collected by recent block root. Staging precedes result-table joins. |
| R6 | A bounded SSE notification channel and polling wake one writer. Root reconciliation detects a reorg even when its event was missed. New slots take priority over older coverage gaps. |
| R7 | Finality promotes connected stored evidence without historical-state rescans. Positive duty observations become final independently of rewards; misses and complete scans retain their coverage/readiness requirements. The separate archive worker continues repairing persisted live gaps after initial catch-up. |
| R8 | Zero rewards do not imply non-inclusion or a wrong vote. Live and sparse collection compare observed votes with canonical roots. The attestation list excludes pending seeds; live views retain pending assignments. |
| R9 | Configurable two-slot initial lag, retry window, and polling interval. The readiness probe compares offsets 0/1/2/4, sync rewards, and epoch reward readiness. Real-client timing is still unmeasured. |

The code deliberately retains a simple trust boundary: one coherent beacon endpoint supplies valid consensus data. It does not implement an independent consensus state transition or verify a Byzantine node. Recent blocks must remain available for the configured ancestry window. Missing mandatory inputs retain gaps; delay alone never establishes correctness. Multiple live writers remain outside scope.

Aggregate reward summaries sum available stored values; they are not proof that an entire period is complete. Consult nullable duty rewards and `completed_scans` for completeness. Sparse correctness flags summarize evidence and do not model multiple conflicting attestations separately. Those limitations are documented rather than introducing a new reward or vote model.

All pending schema work is consolidated in `migrations/002_epoch_scan_completion.sql`, including the completion, recent evidence, durable input, reward-job, and explicit-gap tables. `001_initial.sql` is unchanged.

Validation:

- 101 Rust tests passed, including recovery tests using fresh isolated schemas in disposable PostgreSQL. Two tests requiring a real beacon endpoint were excluded.
- Regressions cover fork mixing, reconnect/poll reorg recovery, durable inputs after pruning/restart, retry scheduling, zero-reward inclusion, staged rewards joined after duties arrive, early finalization of observed inclusions, complete-coverage finalization, and preservation of finalized archive rows.
- `cargo clippy --offline --all-targets -- -D warnings` passed.
- Frontend type-check and production build passed; the existing unrelated autofocus accessibility warning remains.
- Readiness probe smoke test passed against a local mock, including block offsets and sync rewards.
- Live verification on 2026-09-12 using the configured endpoint found no pending seeds in the attestation list and no missing vote/delay fields in newly collected epochs. Finalized rows also retained complete details. Correct late head votes with zero head reward were observed. Four final SSE samples had no holes through the processed tip, at a measured 4–5-slot distance from the node. This is one endpoint observation, not a client compatibility certification. Field definitions and the collection flow are documented in [live collection](live-collection.md).
- A later live run against an isolated copy of the local database reproduced the stale multi-component reward jobs and verified that they retire without cardinality errors, allowing finalization to resume. Regressions cover multiple expired jobs per gap, committee fallback with branch validation, shared missing-input retries, and saving current-epoch inclusions when older committees are unavailable. Slot 3918248 was confirmed skipped by the endpoint and its successor's parent root; the three tracked duties assigned there were included in 3918249 and subsequently finalized in the test copy. The run had zero warnings/errors and one missing-input notice; two SSE samples had no holes through the processed tip. No source database repair or reset was necessary.

Client compatibility measurement remains open:

| Client | Evidence available | Live default-settings run |
|---|---|---|
| Lighthouse | Reward implementation and pruning documentation reviewed | Not measured |
| Prysm | Reward implementation reviewed | Not measured |
| Teku | Reward implementation and storage documentation reviewed | Not measured |
| Nimbus | Reward implementation and history documentation reviewed | Not measured |
| Lodestar | Reward/cache implementation and storage documentation reviewed | Not measured |

Source revisions and limitations are in the [investigation](beacon-node-resilience-investigation.md). A local mock does not certify any released client version. Both configured endpoints were unreachable during that investigation; no empirical universal lag or retention guarantee is claimed.
