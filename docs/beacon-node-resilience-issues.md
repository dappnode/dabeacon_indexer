Concrete implementation issues extracted from the [investigation](beacon-node-resilience-investigation.md).

Scope: correct live collection from ordinary non-archive nodes, while preserving finalized archive/backfill scanning. Multi-instance coordination (issue 5 from the earlier review) remains excluded. The identifiers below are a separate backlog. P1 means correctness or essential non-archive reliability; P2 means operational tuning and compatibility validation.

The issue descriptions below preserve the original problem statements. Implementation and validation are recorded in [the completion notes](beacon-node-resilience-implementation.md). Actual client timing remains unmeasured because the configured endpoints were unreachable.

**R1 — P1: Resolve live blocks strictly through parent roots**

Location: `src/live/head.rs::resolve_chain`, `fetch_by_root`, `fetch_by_slot`.

Problem: when a requested root is unavailable, the resolver substitutes slot lookups. It also fills every gap after the parent walk. A controlled reproduction returns blocks 100, 101, and 102 even though head 102 points directly to 100; block 101 belongs to another branch. This can create incorrect proposals and attestation inclusions.

Change:

- Resolve the selected head and its ancestry by root. Remove unverified slot substitutions and gap filling.
- A missing requested root leaves the affected range unresolved and retryable. Do not interpret a root lookup failure as a missed slot.
- Treat the interval between a known child and its resolved parent as skipped slots on that branch. Return enough coverage information to distinguish proven skips from unresolved history.

Acceptance:

- Head 102 → parent 100 never includes a competing slot-101 block, even if the slot API returns it.
- Unavailable head/parent roots do not create missed-duty rows or advance coverage through unresolved slots.
- A valid parent gap still produces the appropriate missed proposal when its proposer duty is known.

Dependencies: none. Port the isolated reproduction into a permanent regression test.

**R2 — P1: Preserve chain dependencies and validate responses before writing authoritative results**

Location: `src/beacon_client/{mod,types}.rs`, `src/live/{head,reorg}.rs`, scanner persistence and a new migration.

Problem: client methods discard response metadata; duty dependent roots are not retained. Unfinalized result rows cannot reliably identify the branch that produced them. Related API calls can straddle a reorg.

Change:

- Preserve endpoint-specific `dependent_root`, `execution_optimistic`, and `finalized` metadata where the API provides it. Missing metadata must not silently become a positive safety assertion.
- Persist recent block ancestry and result dependencies: proposal/sync block root, attestation inclusion block root, duty dependency, and attestation reward boundary/state reference.
- Request block rewards by root. For the epoch-only attestation reward API, check the relevant canonical boundary/state before and after fetching, then check its ancestry before committing.
- Use one serialized canonical write path. Atomically commit validated results and their progress; discard/retry responses whose dependencies became obsolete. Optimistic responses cannot establish authoritative completion.

Acceptance:

- A reorg between fetching and committing cannot attach old-branch rewards to replacement-branch rows.
- A changed reward boundary causes retry without overwriting valid data.
- Explicitly optimistic data cannot earn completion; missing optional metadata follows a documented verification policy.

Dependencies: R1 for ancestry. Assume one coherent node; Byzantine-node verification and multi-instance coordination are out of scope.

**R3 — P1: Track pending components instead of advancing the reward cursor after failures**

Location: `src/live/head.rs::process_head_scan`, `src/scanner/mod.rs::process_epoch_rewards`, progress persistence and metrics.

Problem: a failed epoch reward attempt advances `last_rewards_fetched_epoch`. Proposal/sync failures are logged while the overall function succeeds. Old epochs are capped away without durable repair work. A transient error can consume the only useful collection window.

Change:

- Persist collection progress separately for applicable attestation, proposal, and sync reward work, scoped to tracked validators and chain dependencies.
- Retain pending work after failures, with bounded backoff and a per-cycle request budget. New live collection must continue while older components retry.
- Do not make reward retries depend on processing a new slot; retry on the worker timer too.
- When recent recovery is no longer practical, retain an explicit `needs_backfill` gap. A retry budget is not proof that an endpoint is permanently unsupported or that data was pruned.
- Expose pending work, oldest gap, and last successful collection through existing diagnostics/metrics. Keep missing rewards unavailable, not zero.

Acceptance:

- A first-attempt 404/500 followed by success is persisted without waiting for finalization, including when the head does not advance.
- A proposal/sync failure cannot mark that component complete or block successful attestation persistence.
- Restart preserves pending work; an old unresolved job does not prevent newer epochs progressing.

Dependencies: coordinate the progress schema with R2 and R7; a simple database table and bounded worker loop are sufficient.

**R4 — P1: Persist duties and committee mappings while their states are available**

Location: `src/beacon_client/` caches, `src/live/head.rs::fetch_epoch_duties`, attestation decoding, new persistence helpers.

Problem: later scans query historical duties and committees. Current caches are in memory and cleared on reorg, so restart or recovery can require already-pruned state.

Change:

- Capture current duties and the committee mappings needed to decode tracked validators' inclusions. Prefetch next-epoch data only where supported.
- Persist the required inputs with their chain dependencies; reuse them across restart and when a reorg leaves those dependencies unchanged.
- Invalidate and refetch assignments when their dependency changes. Record unavailable assignments as incomplete coverage, not absence of a duty.
- Keep storage bounded, retaining inputs still needed for unresolved work. Do not store full beacon states or introduce a consensus implementation.

Acceptance:

- After duties/mappings are captured, restart with historical committee endpoints returning 404 still permits processing the relevant recent inclusions.
- An unchanged dependency reuses stored mappings; a changed dependency does not reuse stale assignments.
- A failed next-epoch prefetch leaves current-epoch collection operational.

Dependencies: R2 for provenance; R3 for pending work.

**R5 — P1: Collect each reward type when ready, independently of historical reconstruction**

Location: `src/scanner/mod.rs::process_epoch_rewards`, `src/scanner/attestations/sparse.rs`, proposal/sync scanners, live worker.

Problem: the live reward job invokes the sparse archive-style scanner before saving rewards, then fetches attestation rewards a second time. A controlled reproduction obtains valid rewards but loses them when committees return 404. Proposal/sync rewards are also deferred until E+2, although their pre-state dependencies are older.

Change:

- Split live reward collection from full epoch reconstruction. Persist a validated reward response independently of whether inclusion decoding or duty joins are complete; use staging storage if final result rows do not yet exist.
- Collect proposal and sync rewards for recent resolved blocks by root. Collect attestation rewards for E near the start of E+2 and retry readiness failures through R3.
- Reuse R4's stored duties/mappings instead of requiring old committee requests. Remove the duplicate attestation reward fetch.
- Correct the comment claiming rewards are immutable at N→N+1: the relevant attestation calculation needs end-of-N+1 state and remains branch-dependent until finalized.

Acceptance:

- Valid rewards survive a committee 404 without falsely completing the epoch; later inclusion repair joins them successfully.
- Proposal/sync rewards are captured while recent block pre-states are available, even if those states are unavailable by E+2.
- An attestation reward failure does not prevent per-block reward collection. Repeated attempts remain idempotent.

Dependencies: R2–R4. Preserve the existing finalized archive reconstruction entry points.

**R6 — P1: Reconcile on polling and reconnect without blocking SSE ingestion**

Location: `src/live/mod.rs`, `src/live/{head,reorg}.rs`, startup cursor initialization.

Problem: reading SSE waits for full head/finalization scans. Reconnect reopens the stream without verifying persisted ancestry. Startup or long downtime can force live collection through unavailable history before reaching recent data.

Change:

- Keep SSE ingestion lightweight and use one live worker, woken by events and a slot-period poll. Coalesce notifications with bounded memory; the worker reads current head/finality and reconciles ancestry.
- On startup and reconnect, compare the selected chain with persisted roots, find the common ancestor, and invalidate/replay the orphaned suffix. Include preceding-epoch reward dependencies when affected.
- Reject stale in-flight work through R2. Do not rely solely on a `chain_reorg` event or its reported depth.
- If downtime exceeds available history, keep an explicit gap for backfill and establish a recent collection range without claiming the missing interval was scanned. Do not promote disconnected evidence until its finalized ancestry is established.

Acceptance:

- A reorg during SSE disconnection is repaired after reconnect even without a replayed reorg event.
- Slow or failed scans do not stop event ingestion; polling recovers a missed head/finality notification.
- A default non-archive node can start following recent data after a long outage while older gaps remain visible.

Dependencies: R1–R4. Avoid separate concurrent writers for each event type.

**R7 — P1: Finalize complete live evidence without requiring a historical rescan**

Location: `src/live/finalization.rs`, `src/db/scanner/{completion,finalization}.rs`, `src/backfill.rs`.

Problem: `completed_scans` can currently be earned only by a successful finalized full scan. Data collected live can remain incomplete indefinitely because the finalization rescan needs state that has since been pruned.

Change:

- Define one completion predicate shared by live collection and archive reconstruction: known applicable duties, proven inclusion-window coverage, and every applicable reward component, all consistent with the selected finalized branch.
- Promote already-collected evidence after checking finalized ancestry. For attestation epoch E, retain the requirement for finalized checkpoint E+2 or later.
- Persist promotion and completion atomically. Keep finalized-but-incomplete epochs eligible for repair.
- Preserve the archive scanner as historical reconstruction and gap repair. Archive unavailability must not prevent complete live evidence being finalized or stop recent collection.

Acceptance:

- Collect a full epoch, then make historical state APIs fail: finalization still completes it using stored verified evidence.
- Missing rewards, unknown coverage, optimistic inputs, or orphaned dependencies prevent completion.
- Restore the archive endpoint: it repairs explicit gaps idempotently and produces the same completion outcome as live collection.

Dependencies: R2–R6 and R8's inclusion semantics. Do not include the deferred multi-instance promotion redesign.

**R8 — P1: Separate observed inclusion, missing data, and reward eligibility**

Location: `src/scanner/attestations/{sparse,dense,mod}.rs`, attestation persistence/API interpretation and documentation.

Problem: sparse mode skips inclusion scanning when rewards are non-positive and derives `included` from positive rewards. A participating validator can receive zero rewards during an inactivity leak. The same `*_correct` fields also have different meanings in dense and sparse modes.

Change:

- Preserve observed canonical block inclusion independently of reward values. Zero/non-positive rewards cannot establish non-inclusion.
- Only establish a miss after complete coverage of the applicable inclusion window. Represent incomplete knowledge explicitly, using collection status or nullable fields as appropriate.
- Document vote correctness versus timely reward eligibility in the API-facing contract. Either align the field semantics using available evidence or expose their distinction; do not silently switch meaning when archive repair runs.
- Do not reimplement reward calculation merely to handle rare cases: retaining unknown flags with an explanation is acceptable when evidence is insufficient.

Acceptance:

- An included attestation with zero component rewards remains included.
- A missing response/coverage gap is not surfaced as a confirmed miss or zero reward.
- A late correct vote has a documented, consistent interpretation before and after archive repair; normal positive-reward cases retain their expected values.

Dependencies: the observed-inclusion regression can be fixed independently; coordinate completeness with R3/R7.

**R9 — P2: Make live lag configurable and validate it against ordinary client configurations**

Location: `src/config.rs`, `src/live/head.rs`, `scripts/probe-beacon-readiness.py`, README and integration tests.

Problem: scanning starts only 100 ms after the SSE head. The investigation supports different readiness windows for different data, but does not establish the best delay or universal retention behavior.

Change:

- Add a modest configurable block lag based on imported head progress; start evaluation at two slots. Keep attestation reward scheduling tied to E+2, separately from block collection.
- Extend readiness measurements to compare block offsets 0/1/2/4 and repeated epoch reward responses with stable chain references. Cover Lighthouse, Prysm, Teku, Nimbus, and Lodestar, recording actual versions/settings.
- Compare collected results against an archive reference after finality. Exercise epoch boundaries, transient failures, missed boundary slots, cold caches, reconnect/reorg, and archive recovery with focused automated fixtures where practical.
- Document verified compatibility and explicit unsupported-data behavior. Remove the portable “about a day of state” assumption. Do not infer readiness or pruning solely from an HTTP status code.

Acceptance:

- Tests prove lag does not cause skipped processing ranges and that reward readiness failures are retried.
- A recorded compatibility matrix distinguishes live measurements from source review and states unavailable endpoints honestly.
- No delay setting relaxes ancestry/completion checks. If a client cannot supply a component, collection continues with a visible repairable gap.

Dependencies: R5–R7 for end-to-end comparison. Endpoint access is required to establish measured defaults; mock tests cannot certify client behavior.

Suggested sequence: fix R1 and R8's confirmed inclusion logic first; implement shared provenance/progress in R2–R3; capture early inputs and rewards in R4–R5; complete reconciliation and finalization in R6–R7; use R9 measurements to choose operational defaults. Build focused regression tests with each issue rather than postponing correctness testing to R9.
