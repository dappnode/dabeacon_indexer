**Beacon-node resilience investigation — 2026-09-11**

Recommendation: collect recent data before nodes prune it, persist its chain dependencies, and confirm it later from finalized ancestry. Keep the archive scanner as the historical reconstruction path. A single global “stay N slots behind head” setting cannot satisfy the different readiness windows of duties, block rewards, attestation rewards, and finality.

This investigation reviews the current staged working tree, including the preceding recovery fixes. It does not change the production pipeline.

**What was tested**

- Read the reward implementations in Lighthouse, Prysm, Teku, Nimbus, and Lodestar, pinned to the source commits linked below. Some are development branches; these observations are not certification of every released version.
- Attempted version, sync status, head, finality, and spec requests against both distinct endpoints configured in `config.toml` and `.env.test`. Neither endpoint was reachable from this environment. A public endpoint returned HTTP 403. No live reward measurements were obtained, and the frequency of near-head failures remains unmeasured.
- Ran two isolated Rust reproductions against the current code and synthetic local Beacon APIs. Both confirmed the problems described below. The snapshot and reproduction tests are in `/tmp/dabeacon-investigation/repro`; the project’s staged source files and database were not changed.
- Added `scripts/probe-beacon-readiness.py` for a real-node readiness/retention trace. It uses read-only requests, queries a specific validator, and never queries the indexer database. Its default validator is the initial head’s proposer, not the configured validator set.

**Assessment of the timing claim**

The observation is partly supported, but “incomplete” needs to be separated into four cases:

1. **The inclusion window is still open.** An attestation assigned in epoch E may appear through the end of E+1. Not seeing it in an early block does not establish a miss. This is protocol behavior, not an unreliable node. [Deneb attestation rules](https://github.com/ethereum/consensus-specs/blob/master/specs/deneb/beacon-chain.md#modified-process_attestation).
2. **The requested calculation is not ready or its inputs are unavailable.** Attestation reward handlers use the end-of-E+1 state. Calling during E or most of E+1 cannot produce the complete reward calculation. Different clients signal this differently: Prysm explicitly waits for two transitions; Lighthouse and Nimbus can return 404 for the required state; Teku can classify an unavailable state as an invalid epoch range; Lodestar can fail because the state is absent from cache. See the source table below.
3. **The chain changed.** A slot lookup before a reorg and a reward lookup after it can describe different blocks. Adding a delay reduces exposure but cannot make those requests consistent.
4. **A fixed block was allegedly incomplete.** For the supported Phase0–Fulu block format, a signed block’s contents are fixed by its root. Later attestations appear in later blocks; they do not get appended to that same block. I found no evidence here that successful responses for the same root routinely acquire additional contents later. Reproducing such behavior requires raw responses, roots, client version, and timing.

The code comment saying rewards for N are computed at N→N+1 and immutable afterwards is incorrect for the attestation rewards being indexed. The relevant previous-epoch calculation uses the end of N+1, and unfinalized results can still change with the branch.

**Client evidence and why defaults matter**

| Client/source revision | Attestation reward input | Operational consequence |
|---|---|---|
| [Lighthouse stable, e423a667](https://github.com/sigp/lighthouse/blob/e423a66763bb1bd780492d635123f208d80c3538/beacon_node/beacon_chain/src/attestation_rewards.rs#L48) | State at the last slot of E+1. | Its API marks this reward finalized only when checkpoint E+2 is finalized. |
| [Prysm develop, e7c2be4f](https://github.com/OffchainLabs/prysm/blob/e7c2be4fae5f3f6f973d120fec61e7395d1186af/beacon-chain/rpc/eth/rewards/handlers.go#L215) | Explicitly requires current epoch > E+1, then loads the end-of-E+1 state. | A near-boundary 404 is not proof that history has been pruned or that the validator missed its duty. |
| [Teku master, 5fe7cd31](https://github.com/Consensys/teku/blob/5fe7cd31e8418bb14e2c70125f1707ed7967c29a/data/provider/src/main/java/tech/pegasys/teku/api/ChainDataProvider.java#L864) | Computes start(E+2)−1 and obtains that state. | Availability errors do not use one universal status code across clients. |
| [Nimbus stable, 404a0001](https://github.com/status-im/nimbus-eth2/blob/404a0001561d1d83c5b5bf35dcbedbcb5fb86572/beacon_chain/rpc/rest_rewards_api.nim#L249) | Computes start(E+2)−1, checks imported head, then reconstructs the needed state. | Readiness should follow imported canonical progress, not just wall-clock time. |
| [Lodestar unstable, e22a22a1](https://github.com/ChainSafe/lodestar/blob/e22a22a11d2bc01ca481a1f4d74164ddbbff0264/packages/beacon-node/src/chain/chain.ts#L1835) | End-of-E+1 state; this handler disables regeneration and requires a cached state. | Generic historical-state support does not guarantee that the reward endpoint can use it. Missed boundary slots and cold caches deserve explicit testing. |

[Lighthouse documents that finalized states are pruned by default](https://lighthouse-book.sigmaprime.io/faq.html#how-can-i-construct-only-partial-state-history). [Teku documents `minimal` as its default, pruning finalized states and historical blocks](https://docs.teku.consensys.io/reference/cli#data-storage-mode). Thus “a non-archive node retains about a day of state” is not a portable assumption. The preceding README wording overstated how generally that applies.

[Nimbus documents a different rolling history policy](https://nimbus.guide/history.html). [Lodestar exposes both historical-state regeneration and archive/pruning controls](https://chainsafe.github.io/lodestar/contribution/dev-cli/#--servehistoricalstate), but its reward-handler cache requirement is the more relevant constraint here. Block retention, state regeneration, and reward-endpoint availability are separate capabilities. Do not infer them from the client name or one successful historical-state probe. I did not establish a universal default retention duration for Prysm.

**Recommended collection schedule**

These are proposed operational defaults, not measured guarantees. Correctness comes from ancestry and completeness checks, not the delay.

| Data | Collect when | Confirm when |
|---|---|---|
| Attester/proposer duties and required committee mappings | Fetch current epoch promptly; prefetch next epoch where the endpoint supports it. Persist the response and its dependent root. | Verify that the dependency remains on the selected branch. |
| Blocks, inclusion evidence, proposals, sync bits | Start with a target two slots behind imported head (about 24 seconds on mainnet). Request blocks by root and follow parent links. | The block is on finalized ancestry. |
| Proposal and sync rewards | Fetch for those same recent blocks, by block root, while their parent/pre-state is still available. | Their input block and state dependencies are finalized. |
| Attestation rewards for E | Start near the beginning of E+2, initially after two additional imported slots. Retry readiness failures within the recent-state window. | The calculation’s end-of-E+1 branch is anchored by finalized checkpoint E+2 or later. |

A short delay for block data is reasonable for reducing node/API races. Waiting until E+2 to collect all proposal and sync rewards is unnecessarily late. Those calculations depend on the block and its pre-state, rather than the end of the next epoch. [Block rewards API](https://github.com/ethereum/beacon-APIs/blob/master/apis/beacon/rewards/blocks.yaml), [Prysm block reward implementation](https://github.com/OffchainLabs/prysm/blob/e7c2be4fae5f3f6f973d120fec61e7395d1186af/beacon-chain/rpc/eth/rewards/service.go).

Example: at head E+2, attestation rewards for E need the end of E+1, which may still be recent. But re-running duties/committee queries for E and proposal reward queries needing early-E pre-states introduces older dependencies that may already be unavailable. This explains why reusing the archive epoch scanner is fragile even when the attestation rewards endpoint itself works.

**Comparison with the current code**

| Current behavior | Consequence | Recommended change |
|---|---|---|
| `src/live/head.rs`: starts scanning the SSE head after 100 ms. | Very close to API readiness races; delay is not a canonicality check. | Modest configurable lag plus root-based reconciliation. |
| `src/scanner/mod.rs`: eager rewards first invoke the sparse historical scanner, which fetches old duties and committees. | An available reward response can be lost because an unrelated old-state request fails. | Save reward responses independently; join them to duties already captured earlier. |
| Same function postpones proposal/sync rewards until the epoch reward job. | Their needed pre-states can be older than the attestation reward state. | Separate per-block reward collection. |
| `src/live/head.rs:159–170`: advances the reward watermark even after failure. | One transient boundary failure loses the best retry window. | Persist per-component pending work; failures do not advance successful-completion state. |
| `src/live/head.rs::resolve_chain`: fills root-walk gaps using slot lookups. | Can mix forks and turn an absent slot on one branch into a block from another. | Trust the parent walk. Unknown root responses remain unresolved; a known child/parent gap proves skipped slots on that branch. |
| `src/beacon_client/mod.rs`: returns only `.data`; duty dependent roots and optimistic/finality metadata are lost or unused. | Cannot establish that related responses are safe and belong together. | Preserve metadata and reject optimistic data from authoritative completion. |
| `src/live/mod.rs`: SSE reading waits for complete scans and reconnect only reopens the stream. | Slow scans delay newer events; a reorg during disconnect is not reconciled from persisted ancestry. | Continuous SSE ingestion plus periodic head/finality polling; reconcile on every reconnect. |
| Duty caches are in-memory, keyed by epoch/validator set, and cleared wholesale on reorg. | Restarts and reorg recovery can require states that are no longer available. | Persist recent duty/committee inputs; invalidate only changed dependencies. |
| `completed_scans` is written only after a successful finalized full scan. | Live collection cannot become complete solely from already collected evidence if the late rescan needs pruned state. | Let validated live collection earn the same completion marker after finality, without refetching old state. |

The earlier fixes improved repairability, finality boundaries, and background backfill behavior. They do not yet make default non-archive live operation self-sufficient.

The two isolated reproductions were:

- **Available rewards, pruned committee:** the mock returns valid duties and positive rewards for E, then 404 for committees. `process_epoch_rewards` returns 404 before any database access, despite having successfully fetched the rewards once. This reproduces the dependency coupling directly.
- **Mixed ancestry:** head block at slot 102 points directly to a parent at 100. A later slot query returns a competing block at 101. `resolve_chain` includes all three, even though 101 is not an ancestor of the supplied head. This reproduces the fallback’s consistency failure.

These tests demonstrate indexer behavior under controlled responses; they do not demonstrate that a particular production client emitted those responses during this session.

**Smallest architecture that preserves correctness**

Keep three responsibilities:

1. A lightweight SSE reader records the latest head/finality/reorg signals without awaiting scans. A slot-period poll and reconnect reconciliation cover missed signals. SSE is a notification mechanism; persisted ancestry is the source used to determine what changed.
2. One live worker reconciles the selected head, fetches recent inputs, and writes results. It can use bounded concurrent requests but has one canonical commit path. Persist a small recent block ancestry table and per-epoch collection progress, including the required duties and their dependencies. This is much smaller than storing full beacon states or implementing a consensus client.
3. The existing archive/backfill worker reconstructs historical gaps using finalized scans and the same result tables. Give live requests priority when both workers share an endpoint.

For each result, retain enough provenance to invalidate it: block root for a proposal/sync duty, inclusion block root for an attestation inclusion, duty dependent root for assignments, and the canonical boundary/state reference for an attestation reward calculation. The standard epoch reward endpoint cannot be pinned by root. Bracket its request with checks of the relevant canonical state/boundary, and validate that dependency again before commit and on reorg. This assumes one coherent, trustworthy node; load-balanced endpoints serving different backends require extra care and must not be treated as atomic snapshots.

Fetch outside a database transaction. Before committing results and progress together, verify that the response dependencies still belong to the selected chain. A result from an obsolete branch must not overwrite new-branch data.

For finalization, verify the finalized root against recorded ancestry and promote stored evidence. Full rescans become an optional archive verification/repair path. Preserve the distinction between `finalized` and `complete`; an incomplete epoch can be on a finalized chain without having every reward available.

For reorgs, find the common ancestor from roots, invalidate observations and reward calculations depending on the orphaned suffix, and replay the replacement branch. Attestation rewards for the preceding epoch can be affected. Reuse persisted duty/committee information where its dependent root is unchanged. Do this after reconnect as well as after a `chain_reorg` notification. [Duty API dependency contract](https://github.com/ethereum/beacon-APIs/blob/master/apis/validator/duties/attester.yaml).

For retries, track readiness separately per component. An attestation reward failure must not prevent block reward collection. Retry with bounded backoff while prioritizing new live inputs; eventually classify an unrecoverable old job as `needs_backfill` and keep following the head. HTTP status alone does not reliably distinguish not-ready, pruned, unsupported, or transient failure across clients. Record endpoint capability and last successful ages; do not call an epoch complete merely because retry attempts were exhausted.

**Meaning of correctness**

- An unknown slot is not a missed proposal. Prove the absence on the selected ancestry before deciding.
- A missing reward is `NULL`/unavailable, not zero.
- Zero attestation reward is not proof of non-inclusion. In an inactivity leak, correctly participating validators can receive zero component rewards; the sparse scanner currently uses positive rewards as its inclusion signal. Preserve observed block inclusions independently. [Altair reward rules](https://github.com/ethereum/consensus-specs/blob/master/specs/altair/beacon-chain.md#get_flag_index_deltas).
- Choose and document one meaning for vote correctness versus timely reward eligibility. The archive dense/sparse definitions currently differ; comparisons must respect that distinction.
- Optimistic execution data and a syncing/stale head should not become authoritative completed results. Preserve and check endpoint metadata rather than ignoring it. [Reward response contract](https://github.com/ethereum/beacon-APIs/blob/master/apis/beacon/rewards/attestations.yaml).
- Completion requires duties, coverage of the full inclusion window, and every applicable reward component. No sync membership or proposal duty is a valid “not applicable,” distinct from a failed lookup.

Exact results for every arbitrary node are not achievable if it never exposes the required data, silently returns incorrect values, or has already pruned a missed collection window. Supporting such nodes means continuing safely and showing explicit gaps, with optional archive repair. It does not mean fabricating a successful result. Independently calculating every reward from full beacon states would be a substantially larger project and is not the simple default recommendation.

**Validation before adopting a default delay**

Run the following against each actual supported client version with ordinary storage settings and its HTTP API enabled:

- Observe at least several epoch transitions, recording availability and latency for block rewards at head offsets 0/1/2/4 and epoch rewards throughout E+1 and E+2. Compare repeated successful responses for the same canonical calculation reference. Do not count a branch change as evidence of an incomplete API response.
- Compare recent collected results against a known archive node after the necessary finality boundary. Require identical rewards and inclusion facts; distinguish documented vote-flag semantics.
- Exercise a missed final slot of E+1, a node restart/cold cache, SSE disconnection during reorg, optimistic execution, finality delay, a first-attempt 404/500, and a successful reward response with unavailable older committees.
- Assert that failures leave explicit gaps, do not advance successful progress, cannot corrupt canonical rows, and do not block newer live work.
- Test archive unavailability followed by recovery, and confirm the existing finalized backfill path repairs those gaps.

The two-slot block lag and early-E+2 reward fetch are starting hypotheses. The cross-client measurements should determine the final defaults. Root consistency and completeness invariants must hold at every tested lag.

**Readiness trace**

From the project root, using the configured endpoint:

```sh
uv run scripts/probe-beacon-readiness.py --config config.toml --samples 20 --interval 24 > /tmp/beacon-readiness.jsonl
```

Alternatively set `BEACON_URL` in the environment. The first sample compares current/recent epochs, committees, duties, reward-state roots, and root-addressed block rewards. Following samples track the same epoch’s attestation rewards across the boundary. Output includes status, latency, roots, optimistic/finality flags, and reward values/digests, but no endpoint URLs or response error bodies. This is a limited readiness probe, not the full compatibility/fork-choice test suite described above.
