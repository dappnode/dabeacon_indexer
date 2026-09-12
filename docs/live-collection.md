# Live collection and attestation results

The live worker has three independent responsibilities: observe duties from recent blocks, collect rewards when their calculation becomes available, and confirm the stored evidence against finality. An unavailable reward endpoint must not hide an observed duty or delay its inclusion details.

## What the attestation page means

The list contains included attestations and proven misses. Assignment seeds are excluded by the database query, including the pagination count. It needs no beacon request or wall-clock cutoff to decide which rows to show. The Live page remains the place to view upcoming duties and pending observations.

The Live page groups attestations by their assigned duty slot, not the block containing them. A skipped proposal and successful attestations in the same column are compatible: those attestations were included in a later block. The tooltip shows the actual inclusion slot and explains skipped proposals. Known blocks remain visible while their duty inputs are incomplete; only proven coverage and the absence of a block establish a skipped slot.

| Field | Meaning and readiness |
| --- | --- |
| Included | A tracked aggregation bit was found in a block on the selected chain. Visible immediately after processing that block. |
| Missed | The full allowed inclusion window was processed and finalized without finding an inclusion. Absence near head is insufficient. |
| Delay | Number of slots between the assigned slot and the first observed inclusion. |
| Adjusted delay | Number of actual blocks after the assigned slot through inclusion. Skipped proposer slots do not count. Raw delay remains visible in parentheses, or as `raw` when adjustment is unavailable. |
| Head / Target | The included vote matches the canonical root at the duty slot / epoch boundary. Skipped slots carry forward their preceding root. These are vote comparisons, not reward flags. |
| Source | Live and sparse collection rely on the accepted block's consensus validation: inclusion requires a matching justified source. Dense archive collection also compares the epoch's justified checkpoint. |
| Rewards | Fetched independently. Attestation epoch E needs end-of-E+1 state, so reward jobs begin after the configured settling lag in E+2. Zero is a measured reward; `-` means unavailable. |
| Finalized | The inclusion block is on connected finalized ancestry, or the complete absence window has been proven there. This does not claim that every reward is present. |

`OK` and zero head reward are compatible. For example, a correct vote included two slots later does not qualify for the timely-head reward. A skipped intervening slot can make its adjusted delay 1 while its raw delay remains 2. Reward eligibility uses raw delay. See the [Altair participation rules](https://github.com/ethereum/consensus-specs/blob/master/specs/altair/beacon-chain.md#get_attestation_participation_flag_indices) and [Deneb's extended inclusion window](https://ethereum.github.io/consensus-specs/specs/deneb/beacon-chain/#modified-get_attestation_participation_flag_indices).

Unavailable vote comparisons display `Pending` before finality and `Unknown` afterwards, with an explanation. Proven misses display `N/A` because there is no included vote to compare. Unknown never means wrong or skipped.

## One serialized reconciliation loop

1. SSE notifications wake the worker; periodic polling covers disconnects and missed events. Notifications do not directly write chain data.
2. Read the canonical head and follow parent roots. Resolve the previous attestation epoch as well, including a predecessor when a boundary was skipped. The collection target remains `head - live.lag_slots`. Extra ancestry does not expand the configured slot-retry window.
3. Compare stored roots to that chain. On a change, invalidate provisional evidence and replay. Resolved context before tracking began is used for comparisons, not treated as missing collection work.
4. Process recent blocks and retry uncovered slots. Decode aggregation bits using cached committees/duties. Persist inclusion, raw/adjusted delay, and vote comparisons together. An earlier inclusion replaces a later observation; replay of the same inclusion can fill details. Coverage is recorded only after required inputs succeed.
5. Collect due proposal, sync, and epoch reward jobs. Responses retain their dependency root. A reward response can fill unknown vote flags when positive, but cannot overwrite a direct vote comparison. Zero reward never establishes a wrong vote or a missed duty.
6. Fetch finality and confirm connected ancestry. The vote/delay results derived from that same ancestry become final with their inclusion block. Missing details from partial collection or older releases are re-decoded from recent resolved blocks with a four-slot repair budget per poll. Repairs preserve rewards and reject later duplicate inclusions.
7. An epoch earns `completed_scans` only with full inclusion-window coverage, required rewards, and known details for included attestations. Finalized incomplete epochs remain repairable. Prune old working evidence after the retention boundary.

Recent collection does not fetch future epoch states, re-run historical reward calculations to establish inclusion, or infer correctness from time alone. The default two-slot lag is a settling allowance, not a guarantee that every client API is ready. Failures keep gaps/jobs for retry; long outages move recent collection forward and leave historical gaps for the archive worker.

If an epoch-boundary committee state returns 404, the current and previous epoch may instead use a pinned recent state root, with canonical-header and input-anchor checks. Older archive queries retain their historical-state requirement. The [committee API](https://github.com/ethereum/beacon-APIs/blob/master/apis/beacon/states/committee.yaml) accepts an epoch independently of its state ID. Unavailable inputs share a 30-second retry delay across inclusion slots; reorg invalidation clears that delay. In a block containing votes from two epochs, unavailable older inputs do not prevent storing newer inclusions. The slot stays incomplete until both epochs are processed.

Live reconciliation reports a persistent failure once, with repeat details at DEBUG and an INFO message on recovery. Database and data-integrity failures remain ERROR and take precedence over missing-input reports. Collection gaps do not prevent independent reward and finalization work. Expired reward jobs are grouped by validator/epoch before creating gap rows, and gap creation and job retirement commit together; multiple sync/proposal jobs cannot conflict with the same gap row inside one statement.

## Finality, repairs, and limits

All pre-finality observations describe the currently selected branch. Reorg cleanup removes provisional outcomes and their dependent jobs; replay calculates replacement fields. Finalization verifies ancestry instead of assuming that a row's age makes it correct. Full attestation completion and proven misses retain the checkpoint E+2 boundary.

The existing archive worker continues reconstructing complete epochs and repairing gaps. Sparse scans now compare observed votes against canonical roots too, so a zero reward no longer leaves a permanently blank head comparison. Live collection requires recent blocks and cached/retrievable committee inputs, not archive state. It trusts a coherent beacon endpoint's consensus validation rather than running an independent state transition. Failed web SSE refreshes emit keepalive comments, leaving the browser's last data and update timestamp unchanged.

Older rows outside retained ancestry may still lack details. Their raw delay is displayed when available; unavailable comparisons are explicitly unknown. Recovery of already-pruned input history needs an archive reconstruction. We do not invent final results when that evidence is unavailable.

In `both` mode, unavailable historical inputs pause backfill without stopping live collection or the independent minute-by-minute recent-gap repair pass. A missing anchor or historical API 404 is an availability failure, not evidence of corruption. The first failure is logged at INFO; subsequent failures of the same category are DEBUG. Retries wait 1, 2, 4, 8, 16, then at most 30 minutes. Routine pass setup is DEBUG; successful progress and completion remain INFO. Contradictory/malformed data is reported at ERROR, other failures at WARN, once per category change. Failed scans never advance completion; retries skip already completed epochs. Backfill-only mode still exits on failure, since it has no live workload to preserve.

The row represents the earliest observed inclusion for a validator/epoch. Multiple conflicting votes are not represented separately. If earlier slots were unavailable, the first observation is not proof of the earliest possible inclusion; full coverage is still required for completion. Multiple concurrent live writers remain outside scope.
