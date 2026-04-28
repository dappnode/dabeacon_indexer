<script lang="ts">
	import { flip } from 'svelte/animate';
	import { cubicOut } from 'svelte/easing';
	import { onMount } from 'svelte';
	import { fade, fly } from 'svelte/transition';
	import { getApiKey, timeSince } from '$lib/api';

	type ConnectionStatus = 'connecting' | 'connected' | 'disconnected';

	interface AttestationOutcome {
		validator_index: number;
		included: boolean | null;
	}

	interface SlotData {
		slot: number;
		block: boolean;
		skipped: boolean;
		proposer?: number | null;
		proposed?: boolean | null;
		attestations: AttestationOutcome[];
		// Positional: aligns with LiveUpdate.sync_committee by index.
		sync: Array<boolean | null>;
	}

	interface LiveUpdate {
		epoch: number;
		previous_epoch?: number | null;
		head_slot: number;
		start_slot: number;
		end_slot: number;
		// Tracked validators in any sync committee covering this window.
		// Per-slot `sync` arrays align with this vector by position.
		sync_committee: number[];
		slots: SlotData[];
	}

	type PillState = boolean | null;

	let currentData = $state<LiveUpdate | null>(null);
	let error = $state<string | null>(null);
	let lastUpdate = $state<Date | null>(null);
	let formattedLastUpdate = $state<string>('');
	let connectionStatus = $state<ConnectionStatus>('connecting');
	let tooltipAboveSlots = $state<Record<number, boolean>>({});
	let tooltipHorizontalAlign = $state<Record<number, 'left' | 'center' | 'right'>>({});

	const tooltipHeightEstimate = 250;
	const tooltipWidthEstimate = 288;
	const tooltipViewportPadding = 12;

	const statusClass: Record<ConnectionStatus, string> = {
		connecting: 'bg-amber-400',
		connected: 'bg-emerald-400',
		disconnected: 'bg-rose-500'
	};

	type FitTextOptions = {
		minRem?: number;
		maxRem?: number;
		stepRem?: number;
		paddingPx?: number;
	};

	function fitText(node: HTMLElement, options: FitTextOptions = {}) {
		let resizeObserver: ResizeObserver | null = null;
		let parentResizeObserver: ResizeObserver | null = null;
		let rafId: number | null = null;

		let settings = {
			minRem: options.minRem ?? 0.56,
			maxRem: options.maxRem ?? 0.875,
			stepRem: options.stepRem ?? 0.03125,
			paddingPx: options.paddingPx ?? 0
		};

		const scheduleFit = () => {
			if (rafId != null) {
				cancelAnimationFrame(rafId);
			}
			rafId = requestAnimationFrame(() => {
				rafId = null;

				const rootPx =
					parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
				const minPx = settings.minRem * rootPx;
				const maxPx = settings.maxRem * rootPx;
				const stepPx = Math.max(settings.stepRem * rootPx, 0.5);
				const measurementContainer = node.parentElement;
				const availableWidth = Math.max(
					(measurementContainer?.clientWidth ?? node.clientWidth) - settings.paddingPx,
					0
				);

				if (availableWidth <= 0) {
					return;
				}

				// First, test fit at the natural class-based font size.
				node.style.removeProperty('font-size');
				if (node.scrollWidth <= availableWidth + 0.5) {
					return;
				}

				const naturalFontSize = parseFloat(getComputedStyle(node).fontSize) || maxPx;
				const startPx = Math.min(maxPx, naturalFontSize);

				let nextSize = startPx;
				node.style.fontSize = `${nextSize}px`;

				while (nextSize > minPx && node.scrollWidth > availableWidth + 0.5) {
					nextSize = Math.max(minPx, nextSize - stepPx);
					node.style.fontSize = `${nextSize}px`;
				}
			});
		};

		resizeObserver = new ResizeObserver(() => scheduleFit());
		resizeObserver.observe(node);
		if (node.parentElement) {
			parentResizeObserver = new ResizeObserver(() => scheduleFit());
			parentResizeObserver.observe(node.parentElement);
		}
		window.addEventListener('resize', scheduleFit);
		scheduleFit();

		return {
			update(nextOptions: FitTextOptions = {}) {
				settings = {
					minRem: nextOptions.minRem ?? 0.56,
					maxRem: nextOptions.maxRem ?? 0.875,
					stepRem: nextOptions.stepRem ?? 0.03125,
					paddingPx: nextOptions.paddingPx ?? 0
				};
				scheduleFit();
			},
			destroy() {
				if (rafId != null) {
					cancelAnimationFrame(rafId);
				}
				if (resizeObserver) {
					resizeObserver.disconnect();
				}
				if (parentResizeObserver) {
					parentResizeObserver.disconnect();
				}
				window.removeEventListener('resize', scheduleFit);
				node.style.removeProperty('font-size');
			}
		};
	}

	onMount(() => {
		const url = new URL('/live/sse', window.location.origin);
		const apiKey = getApiKey();
		if (apiKey) {
			url.searchParams.set('api_key', apiKey);
		}

		const eventSource = new EventSource(url.toString());

		eventSource.addEventListener('open', () => {
			connectionStatus = 'connected';
			error = null;
		});

		eventSource.addEventListener('message', (event) => {
			try {
				currentData = JSON.parse(event.data) as LiveUpdate;
				lastUpdate = new Date();
				formattedLastUpdate = timeSince(lastUpdate);
				connectionStatus = 'connected';
				error = null;
			} catch (cause) {
				error = cause instanceof Error ? cause.message : 'Failed to parse SSE payload';
				connectionStatus = 'disconnected';
			}
		});

		eventSource.addEventListener('error', () => {
			error = 'SSE connection error';
			connectionStatus = 'disconnected';
		});

		// Update formatted time every second
		const interval = setInterval(() => {
			if (lastUpdate) {
				formattedLastUpdate = timeSince(lastUpdate);
			}
		}, 1000);

		return () => {
			eventSource.close();
			clearInterval(interval);
		};
	});

	function isFutureProposalPending(slot: SlotData, data: LiveUpdate): boolean {
		return slot.proposed === false && slot.slot > data.head_slot;
	}

	function proposalStatusText(slot: SlotData, data: LiveUpdate): string {
		if (slot.proposed == null || isFutureProposalPending(slot, data)) {
			return 'scheduled';
		}
		return slot.proposed ? 'proposed' : 'missed';
	}

	function proposalLabel(slot: SlotData, data: LiveUpdate): string {
		if (slot.proposed == null) return 'SCHEDULED';
		if (isFutureProposalPending(slot, data)) return 'PENDING';
		return slot.proposed ? 'PROPOSED' : 'PROPOSAL MISS';
	}

	function blkBadgeClass(slot: SlotData, data: LiveUpdate): string {
		if (slot.proposed === true) {
			return 'border-cyan-400 bg-cyan-950 text-cyan-200';
		}

		if (slot.proposed === false && !isFutureProposalPending(slot, data)) {
			return 'border border-rose-400 bg-rose-950/60 text-rose-200';
		}

		return 'border border-gray-600 bg-gray-900 text-gray-400';
	}

	// Skipped slots render every duty pill/row as neutral grey regardless of the
	// underlying outcome: the block didn't happen so there's nothing to reward
	// or punish. We surface the cause with a "NO BLOCK" label.
	function attestationLabel(outcome: AttestationOutcome, slotSkipped = false): string {
		if (slotSkipped) return 'NO BLOCK';
		if (outcome.included == null) return 'SCHEDULED';
		return outcome.included ? 'ATTESTED' : 'ATTEST MISS';
	}

	function syncLabel(state: PillState, slotSkipped = false): string {
		if (slotSkipped) return 'NO BLOCK';
		if (state == null) return 'SCHEDULED';
		return state ? 'SYNC OK' : 'SYNC MISS';
	}

	function rowColor(state: PillState, slotSkipped = false, okClass = 'bg-emerald-300'): string {
		if (slotSkipped || state == null)
			return 'border border-gray-700 bg-gray-900 text-gray-300';
		return state ? `${okClass} text-gray-950` : 'bg-rose-300 text-gray-950';
	}

	function proposalRowColor(slot: SlotData, data: LiveUpdate, slotSkipped = false): string {
		if (slotSkipped || isFutureProposalPending(slot, data) || slot.proposed == null) {
			return 'border border-gray-700 bg-gray-900 text-gray-300';
		}
		return slot.proposed ? 'bg-cyan-300 text-gray-950' : 'bg-rose-300 text-gray-950';
	}

	function pillClass(state: PillState, slotSkipped = false, okClass = 'bg-emerald-300'): string {
		if (slotSkipped || state == null) return 'border border-gray-700 bg-gray-950/80';
		return state
			? `${okClass} shadow-[0_0_10px_rgba(52,211,153,0.35)]`
			: 'bg-rose-300 shadow-[0_0_10px_rgba(251,113,133,0.35)]';
	}

	function totalDuties(data: LiveUpdate): number {
		let sum = data.sync_committee.length * data.slots.length;
		for (const s of data.slots) {
			sum += s.attestations.length;
			if (s.proposer != null) sum += 1;
		}
		return sum;
	}

	function countAttested(slot: SlotData): number {
		return slot.attestations.filter((a) => a.included === true).length;
	}

	function countMissedAttestations(slot: SlotData): number {
		return slot.attestations.filter((a) => a.included === false).length;
	}

	function countSyncOk(slot: SlotData): number {
		return slot.sync.filter((s) => s === true).length;
	}

	function countSyncMiss(slot: SlotData): number {
		return slot.sync.filter((s) => s === false).length;
	}

	// Slot color rules:
	//   1. green   — block proposed by an untracked validator
	//   2. grey    — block skipped by an untracked proposer (dimmer than future)
	//   3. red     — block skipped by a TRACKED proposer
	//   4. teal    — block proposed by a TRACKED validator (gets the "Blk" label)
	//   5. default — future slot, not yet decided
	function slotAccent(slot: SlotData, data: LiveUpdate): string {
		const trackedProposer = slot.proposer != null;
		const futureProposalPending = trackedProposer && isFutureProposalPending(slot, data);

		// Rule 3: tracked proposer didn't produce (past slot, confirmed miss).
		if (trackedProposer && slot.proposed === false && !futureProposalPending) {
			return 'border-rose-300 bg-[radial-gradient(circle_at_top_left,_rgba(251,113,133,0.14),_transparent_58%),linear-gradient(180deg,_rgba(69,10,10,0.28),_rgba(3,7,18,0.96))] shadow-[0_0_0_1px_rgba(251,113,133,0.2),0_10px_20px_rgba(127,29,29,0.14)]';
		}

		// Rule 2: any other skipped slot — dimmer than future, washed-out grey.
		if (slot.skipped) {
			return 'border-gray-700/70 bg-[linear-gradient(180deg,_rgba(51,65,85,0.08),_rgba(2,6,23,0.78))] shadow-[0_0_0_1px_rgba(71,85,105,0.12)]';
		}

		// Rule 4: tracked validator proposed.
		if (trackedProposer && slot.proposed === true) {
			return 'border-cyan-300 bg-[radial-gradient(circle_at_top_left,_rgba(34,211,238,0.16),_transparent_58%),linear-gradient(180deg,_rgba(8,47,73,0.45),_rgba(3,7,18,0.96))] shadow-[0_0_0_1px_rgba(103,232,249,0.2),0_12px_22px_rgba(14,116,144,0.14)]';
		}

		// Tracked proposer at a future slot — pending duty, dashed cyan.
		if (futureProposalPending) {
			return 'border-cyan-700/70 border-dashed bg-[radial-gradient(circle_at_top_left,_rgba(34,211,238,0.08),_transparent_60%),linear-gradient(180deg,_rgba(8,47,73,0.24),_rgba(3,7,18,0.96))]';
		}

		// Rule 1: block exists and no tracked proposer — untracked proposal, green.
		if (slot.block) {
			return 'border-emerald-500/50 bg-[radial-gradient(circle_at_top_left,_rgba(52,211,153,0.18),_transparent_55%),linear-gradient(180deg,_rgba(6,78,59,0.26),_rgba(3,7,18,0.96))] shadow-[0_10px_24px_rgba(6,78,59,0.14)]';
		}

		// Rule 5: future / undecided — default dark.
		return 'border-gray-800 bg-[linear-gradient(180deg,_rgba(17,24,39,0.88),_rgba(3,7,18,0.96))] hover:border-gray-700';
	}

	// A/S row inner rectangle: red only when the validator actually missed a
	// duty. Skipped slots keep it neutral because the miss is structural
	// (no block to attest to / participate in).
	function innerBoxClass(hasMiss: boolean): string {
		return hasMiss
			? 'border border-rose-500/60 bg-rose-950/30'
			: 'border border-white/5 bg-black/10';
	}


	function updateTooltipPosition(slotNumber: number, node: HTMLElement): void {
		const rect = node.getBoundingClientRect();
		const spaceBelow = window.innerHeight - rect.bottom;
		const centeredLeft = rect.left + rect.width / 2 - tooltipWidthEstimate / 2;
		const centeredRight = centeredLeft + tooltipWidthEstimate;

		let horizontalAlign: 'left' | 'center' | 'right' = 'center';
		if (centeredLeft < tooltipViewportPadding) {
			horizontalAlign = 'left';
		} else if (centeredRight > window.innerWidth - tooltipViewportPadding) {
			horizontalAlign = 'right';
		}

		tooltipAboveSlots = {
			...tooltipAboveSlots,
			[slotNumber]: spaceBelow < tooltipHeightEstimate
		};
		tooltipHorizontalAlign = {
			...tooltipHorizontalAlign,
			[slotNumber]: horizontalAlign
		};
	}

	function clearTooltipPosition(slotNumber: number): void {
		tooltipAboveSlots = {
			...tooltipAboveSlots,
			[slotNumber]: false
		};
		tooltipHorizontalAlign = {
			...tooltipHorizontalAlign,
			[slotNumber]: 'center'
		};
	}

	function isCurrentEpochBoundary(slot: number, data: LiveUpdate): boolean {
		const boundary = data.epoch * 32;
		return data.start_slot < boundary && slot === boundary;
	}

	function isHeadSlot(slot: number, data: LiveUpdate): boolean {
		return slot === data.head_slot;
	}

	function groupSlotsByEpoch(slots: SlotData[], data: LiveUpdate) {
		const groups: Array<{ epoch: number; slots: SlotData[] }> = [];
		let currentEpoch = Math.floor(data.start_slot / 32);
		let currentGroup: SlotData[] = [];

		for (const slot of slots) {
			const slotEpoch = Math.floor(slot.slot / 32);
			if (slotEpoch !== currentEpoch) {
				if (currentGroup.length > 0) {
					groups.push({ epoch: currentEpoch, slots: currentGroup });
				}
				currentEpoch = slotEpoch;
				currentGroup = [];
			}
			currentGroup.push(slot);
		}

		if (currentGroup.length > 0) {
			groups.push({ epoch: currentEpoch, slots: currentGroup });
		}

		return groups;
	}
</script>

<svelte:head><title>Live - Beacon Indexer</title></svelte:head>

<div class="min-h-screen bg-gray-950 text-gray-100">
	<section class="mx-auto w-full max-w-none px-3 py-6 lg:px-4">
		<header class="mb-3 flex flex-col gap-2 lg:flex-row lg:items-center lg:justify-between">
			<div class="min-w-0">
				<div class="flex flex-wrap items-center gap-x-3 gap-y-1">
					<p class="text-[10px] uppercase tracking-[0.35em] text-cyan-400">Live</p>
					<h1 class="text-xl font-semibold text-white lg:text-2xl">Current Epoch Slots</h1>
				</div>
				<p class="mt-1 max-w-2xl text-[11px] text-gray-400 lg:text-xs">
					Attestation markers are shown on the attested slot, not the inclusion slot.
				</p>
			</div>

			<div class="rounded-2xl ring-1 ring-gray-800/80 bg-gradient-to-br from-gray-900/90 to-gray-900/60 px-3 py-2 backdrop-blur">
				<div class="flex items-center gap-2 text-xs text-gray-300">
					<span class={`h-2.5 w-2.5 rounded-full ${statusClass[connectionStatus]}`}></span>
					<span>{connectionStatus}</span>
				</div>
				{#if formattedLastUpdate}
					<p class="mt-1 text-[10px] text-gray-500">updated {formattedLastUpdate}</p>
				{/if}
			</div>
		</header>

		{#if error}
			<div class="mb-6 rounded-2xl border border-rose-700 bg-rose-950/60 px-4 py-3 text-sm text-rose-200">
				{error}
			</div>
		{/if}

		{#if currentData && totalDuties(currentData) === 0}
			<div class="mb-6 rounded-2xl border border-amber-700 bg-amber-950/50 px-4 py-3 text-sm text-amber-100">
				No duties are currently present in the visible epoch yet. The grid below still reflects the current epoch and will update as live data arrives.
			</div>
		{/if}

		{#if currentData}
			<section class="grid gap-1.5 sm:grid-cols-4 md:grid-cols-8 lg:grid-cols-[repeat(16,minmax(0,1fr))]">
				{#each groupSlotsByEpoch(currentData.slots, currentData).reverse() as group (group.epoch)}
					<div
						class="col-span-full grid gap-1.5 sm:grid-cols-4 md:grid-cols-8 lg:grid-cols-[repeat(16,minmax(0,1fr))]"
						animate:flip={{ duration: 1500, easing: cubicOut }}
						in:fade={{ duration: 1500 }}
						out:fly={{ y: 56, duration: 1500, easing: cubicOut }}
					>
						<div class="col-span-full mb-2 mt-3 rounded-2xl border border-cyan-800/70 bg-cyan-950/30 px-3 py-2">
							<p class="text-[11px] font-semibold uppercase tracking-[0.22em] text-cyan-200">
								Epoch {group.epoch}
							</p>
						</div>

						{#each [...group.slots].reverse() as slot (slot.slot)}

					<article
						class={`group relative overflow-visible rounded-xl border p-2 transition-colors ${slotAccent(slot, currentData)} ${isHeadSlot(slot.slot, currentData) ? 'live-head-slot' : ''}`}
						onmouseenter={(event) => updateTooltipPosition(slot.slot, event.currentTarget as HTMLElement)}
						onmouseleave={() => clearTooltipPosition(slot.slot)}
					>
						<div class="mb-2 flex items-start justify-between gap-1.5">
							<div class="min-w-0 flex-1">
								<p class="text-[9px] uppercase tracking-[0.2em] text-gray-500">Slot</p>
								<h3
									class={`block w-full text-sm font-semibold leading-none ${isHeadSlot(slot.slot, currentData) ? 'head-slot-blink text-cyan-100' : 'text-white'}`}
									use:fitText={{ minRem: 0.56, maxRem: 0.875, stepRem: 0.03125, paddingPx: 0 }}
								>
									{slot.slot}
								</h3>
							</div>
							<div class="flex items-center gap-1.5">
								{#if slot.proposer != null}
									<div class={`rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-[0.18em] ${blkBadgeClass(slot, currentData)}`}>
										Blk
									</div>
								{/if}
							</div>
						</div>

						<div class="space-y-2 text-[11px]">
							<!-- A pills stay grey on skipped slots (rules 2/3: all duties grey). -->
							<div class={`rounded-xl ${innerBoxClass(!slot.skipped && countMissedAttestations(slot) > 0)} px-2 py-1.5`}>
								<div class="flex items-center gap-1.5">
									<span class="inline-flex h-4 w-4 items-center justify-center rounded-full border border-gray-700 bg-gray-900 text-[9px] leading-none text-gray-300">A</span>
								{#if slot.attestations.length > 0}
									<div class="flex flex-wrap gap-1.5">
										{#each slot.attestations as att (`att-pill-${slot.slot}-${att.validator_index}`)}
											<span class={`h-2.5 w-3.5 rounded-full ${pillClass(att.included, slot.skipped)}`}></span>
										{/each}
									</div>
								{/if}
								</div>
							</div>

							<div class={`rounded-xl ${innerBoxClass(!slot.skipped && countSyncMiss(slot) > 0)} px-2 py-1.5`}>
								<div class="flex items-center gap-1.5">
									<span class="inline-flex h-4 w-4 items-center justify-center rounded-full border border-gray-700 bg-gray-900 text-[9px] leading-none text-gray-300">S</span>
								{#if currentData.sync_committee.length > 0}
									<div class="flex flex-wrap gap-1.5">
										{#each currentData.sync_committee as vi, i (`sync-pill-${slot.slot}-${vi}`)}
											<span class={`h-2.5 w-3.5 rounded-full ${pillClass(slot.sync[i] ?? null, slot.skipped)}`}></span>
										{/each}
									</div>
								{/if}
								</div>
							</div>


						</div>

						<div class={`pointer-events-none absolute z-20 hidden w-64 group-hover:block xl:w-72 ${tooltipAboveSlots[slot.slot] ? 'bottom-full pb-2' : 'top-full pt-2'} ${tooltipHorizontalAlign[slot.slot] === 'left' ? 'left-0' : tooltipHorizontalAlign[slot.slot] === 'right' ? 'right-0' : 'left-1/2 -translate-x-1/2'}`}>
							<div class="rounded-2xl border border-gray-700 bg-gray-950/98 p-3 shadow-2xl backdrop-blur">
								<p class="mb-2 text-[10px] font-semibold uppercase tracking-[0.22em] text-gray-500">Slot {slot.slot} details</p>
								<div class="space-y-3 text-xs">
									<div>
										<p class="mb-2 text-[10px] font-semibold uppercase tracking-[0.18em] text-gray-500">Attestation duties</p>
										{#if slot.attestations.length > 0}
											<div class="space-y-1.5">
												{#each slot.attestations as att (`att-${slot.slot}-${att.validator_index}`)}
													<div class={`rounded-xl px-2.5 py-2 ${rowColor(att.included, slot.skipped)}`}>
														<div class="flex items-start justify-between gap-2">
															<span class="font-semibold">V{att.validator_index}</span>
															<span class="font-medium">{attestationLabel(att, slot.skipped)}</span>
														</div>
													</div>
												{/each}
											</div>
										{:else}
											<p class="text-gray-600">No attestation duty.</p>
										{/if}
									</div>

									<div>
										<p class="mb-2 text-[10px] font-semibold uppercase tracking-[0.18em] text-gray-500">Sync duties</p>
										{#if currentData.sync_committee.length > 0}
											<div class="space-y-1.5">
												{#each currentData.sync_committee as vi, i (`sync-${slot.slot}-${vi}`)}
													<div class={`rounded-xl px-2.5 py-2 ${rowColor(slot.sync[i] ?? null, slot.skipped)}`}>
														<div class="flex items-start justify-between gap-2">
															<span class="font-semibold">V{vi}</span>
															<span class="font-medium">{syncLabel(slot.sync[i] ?? null, slot.skipped)}</span>
														</div>
													</div>
												{/each}
											</div>
										{:else}
											<p class="text-gray-600">No sync duty.</p>
										{/if}
									</div>

									<div>
										<p class="mb-2 text-[10px] font-semibold uppercase tracking-[0.18em] text-gray-500">Proposal duties</p>
										{#if slot.proposer != null}
											<div class="space-y-1.5">
												<div class={`rounded-xl px-2.5 py-2 ${proposalRowColor(slot, currentData, slot.skipped)}`}>
													<div class="flex items-start justify-between gap-2">
														<span class="font-semibold">V{slot.proposer}</span>
														<span class="font-medium">{proposalLabel(slot, currentData)}</span>
													</div>
												</div>
											</div>
										{:else if slot.skipped}
											<p class="text-gray-600">Skipped slot without a tracked proposer duty.</p>
										{:else}
											<p class="text-gray-600">No proposal duty.</p>
										{/if}
									</div>


								</div>
							</div>
						</div>
					</article>
				{/each}
					</div>
			{/each}
			</section>
		{:else}
			<div class="flex min-h-[40vh] items-center justify-center rounded-3xl ring-1 ring-gray-800/80 bg-gradient-to-br from-gray-900 to-gray-900/60">
				<div class="text-center">
					<p class="text-sm text-gray-300">Waiting for live data…</p>
					<p class="mt-2 text-xs text-gray-500">SSE status: {connectionStatus}</p>
				</div>
			</div>
		{/if}
	</section>
</div>

<style>
	:global(html, body) {
		background-color: #030712;
	}

	@keyframes head-slot-blink {
		0%,
		45%,
		100% {
			opacity: 1;
		}
		50%,
		95% {
			opacity: 0.35;
		}
	}

	.live-head-slot {
		box-shadow: 0 0 0 1px rgba(34, 211, 238, 0.4), 0 0 20px rgba(34, 211, 238, 0.18);
	}

	.head-slot-blink {
		animation: head-slot-blink 1.2s step-end infinite;
	}
</style>
