<script lang="ts">
	import { onMount } from 'svelte';
	import { getAttestations, getStats, getValidators, getMeta, pct, eth, slotTime, timeAgo, beaconchainUrl, type AttestationRow, type Paginated, type ValidatorSummary, type MetaResponse, type Stats } from '$lib/api';
	import ValidatorPicker from '$lib/ValidatorPicker.svelte';
	import RangeInput from '$lib/RangeInput.svelte';
	import { cardSurface, tableSurface, tableHeaderRow, tableBodyRow, btnNeutral, btnDanger, btnWarn, btnOrange, segIdle, segActive } from '$lib/ui';

	let validators: ValidatorSummary[] = $state([]);
	let meta: MetaResponse = $state({ validators: {}, all_tags: [] });
	let stats = $state<Stats | null>(null);
	let result: Paginated<AttestationRow> | null = $state(null);
	let loading = $state(true);

	const epochMinPlaceholder = $derived<string>(stats?.earliest_scanned_epoch != null ? String(stats.earliest_scanned_epoch) : 'from');
	const epochMaxPlaceholder = $derived<string>(stats?.latest_scanned_epoch != null ? String(stats.latest_scanned_epoch) : 'to');

	// Filters
	let filterValidator = $state('');
	let filterEpochFrom = $state('');
	let filterEpochTo = $state('');
	let filterIncluded = $state('');
	let filterHeadCorrect = $state('');
	let filterTargetCorrect = $state('');
	let filterSourceCorrect = $state('');
	let filterFinalized = $state('');
	let filterMinEffectiveDelay = $state('');
	let filterMaxEffectiveDelay = $state('');
	let sortCol = $state('assigned_slot');
	let sortOrder = $state('desc');
	let page = $state(1);
	let perPage = $state(50);
	let loadingMore = $state(false);
	let sentinelEl: HTMLDivElement | null = $state(null);

	function getTags(idx: number): string[] {
		const m = meta.validators[String(idx)];
		return m?.tags || [];
	}

	async function load(append = false) {
		if (append) loadingMore = true;
		else loading = true;
		try {
			const next = await getAttestations({
				validator_index: filterValidator || undefined,
				epoch_from: filterEpochFrom || undefined,
				epoch_to: filterEpochTo || undefined,
				included: filterIncluded === '' ? undefined : filterIncluded === 'true',
				head_correct: filterHeadCorrect === '' ? undefined : filterHeadCorrect === 'true',
				target_correct: filterTargetCorrect === '' ? undefined : filterTargetCorrect === 'true',
				source_correct: filterSourceCorrect === '' ? undefined : filterSourceCorrect === 'true',
				finalized: filterFinalized === '' ? undefined : filterFinalized === 'true',
				min_effective_delay: filterMinEffectiveDelay || undefined,
				max_effective_delay: filterMaxEffectiveDelay || undefined,
				sort: sortCol,
				order: sortOrder,
				page,
				per_page: perPage,
			});

			if (append && result) {
				result = {
					...next,
					data: [...result.data, ...next.data],
				};
			} else {
				result = next;
			}
		} catch (e) { console.error(e); }
		if (append) loadingMore = false;
		else loading = false;
	}

	function applyFilters() { page = 1; load(); }
	function resetFilters() {
		filterValidator = ''; filterEpochFrom = ''; filterEpochTo = '';
		filterIncluded = ''; filterHeadCorrect = ''; filterTargetCorrect = '';
		filterSourceCorrect = ''; filterFinalized = '';
		filterMinEffectiveDelay = ''; filterMaxEffectiveDelay = '';
		page = 1; load();
	}
	function sort(col: string) {
		if (sortCol === col) sortOrder = sortOrder === 'asc' ? 'desc' : 'asc';
		else { sortCol = col; sortOrder = 'desc'; }
		page = 1;
		load();
	}
	function hasMore(): boolean {
		return !!result && result.data.length < result.total;
	}
	async function loadMore() {
		if (loading || loadingMore || !hasMore()) return;
		page += 1;
		await load(true);
	}

	function resetAllFiltersToAll() {
		filterValidator = '';
		filterEpochFrom = '';
		filterEpochTo = '';
		filterIncluded = '';
		filterHeadCorrect = '';
		filterTargetCorrect = '';
		filterSourceCorrect = '';
		filterFinalized = '';
		filterMinEffectiveDelay = '';
		filterMaxEffectiveDelay = '';
	}

	// Quick filters
	function showMissedOnly() {
		resetAllFiltersToAll();
		filterIncluded = 'false';
		applyFilters();
	}

	function showWrongHead() {
		resetAllFiltersToAll();
		filterHeadCorrect = 'false';
		applyFilters();
	}

	function showWrongTarget() {
		resetAllFiltersToAll();
		filterTargetCorrect = 'false';
		applyFilters();
	}

	function showWrongSource() {
		resetAllFiltersToAll();
		filterSourceCorrect = 'false';
		applyFilters();
	}

	function showHighDelay() {
		resetAllFiltersToAll();
		filterMinEffectiveDelay = '2';
		applyFilters();
	}

	function slotInEpoch(slot: number): number {
		return (slot % 32) + 1;
	}

	onMount(async () => {
		[validators, meta, stats] = await Promise.all([getValidators(), getMeta(), getStats()]);
		await load();
	});

	$effect(() => {
		if (!sentinelEl) return;

		const observer = new IntersectionObserver(
			(entries) => {
				if (entries.some((e) => e.isIntersecting)) {
					loadMore();
				}
			},
			{ root: null, rootMargin: '200px 0px', threshold: 0 }
		);

		observer.observe(sentinelEl);
		return () => observer.disconnect();
	});
</script>

<svelte:head><title>Attestations - Beacon Indexer</title></svelte:head>

<h1 class="text-2xl font-bold mb-4">Attestation Duties</h1>

<!-- Quick filter buttons -->
<div class="flex gap-2 mb-4 flex-wrap">
	<button onclick={resetFilters} class={btnNeutral}>All</button>
	<button onclick={showMissedOnly} class={btnDanger}>Missed Only</button>
	<button onclick={showWrongHead} class={btnWarn}>Wrong Head</button>
	<button onclick={showWrongTarget} class={btnWarn}>Wrong Target</button>
	<button onclick={showWrongSource} class={btnWarn}>Wrong Source</button>
	<button onclick={showHighDelay} class={btnOrange}>Delay 2+</button>
</div>

<!-- Filters -->
<div class="mb-4 {cardSurface} space-y-4">
	<ValidatorPicker {validators} meta={meta.validators} allTags={meta.all_tags} bind:value={filterValidator} onChange={applyFilters} />

	<div class="grid grid-cols-2 md:grid-cols-4 lg:grid-cols-8 gap-3">
		<div>
			<span class="text-xs text-gray-500 block mb-1">Included</span>
			<div class="flex gap-1">
				{#each [['', 'All'], ['true', 'Yes'], ['false', 'No']] as [val, lbl]}
					<button onclick={() => { filterIncluded = val; applyFilters(); }}
						class={filterIncluded === val ? segActive : segIdle}>{lbl}</button>
				{/each}
			</div>
		</div>
		<div>
			<span class="text-xs text-gray-500 block mb-1">Head</span>
			<div class="flex gap-1">
				{#each [['', 'All'], ['true', 'OK'], ['false', 'Wrong']] as [val, lbl]}
					<button onclick={() => { filterHeadCorrect = val; applyFilters(); }}
						class={filterHeadCorrect === val ? segActive : segIdle}>{lbl}</button>
				{/each}
			</div>
		</div>
		<div>
			<span class="text-xs text-gray-500 block mb-1">Target</span>
			<div class="flex gap-1">
				{#each [['', 'All'], ['true', 'OK'], ['false', 'Wrong']] as [val, lbl]}
					<button onclick={() => { filterTargetCorrect = val; applyFilters(); }}
						class={filterTargetCorrect === val ? segActive : segIdle}>{lbl}</button>
				{/each}
			</div>
		</div>
		<div>
			<span class="text-xs text-gray-500 block mb-1">Source</span>
			<div class="flex gap-1">
				{#each [['', 'All'], ['true', 'OK'], ['false', 'Wrong']] as [val, lbl]}
					<button onclick={() => { filterSourceCorrect = val; applyFilters(); }}
						class={filterSourceCorrect === val ? segActive : segIdle}>{lbl}</button>
				{/each}
			</div>
		</div>
		<div>
			<span class="text-xs text-gray-500 block mb-1">Finalized</span>
			<div class="flex gap-1">
				{#each [['', 'All'], ['true', 'Yes'], ['false', 'No']] as [val, lbl]}
					<button onclick={() => { filterFinalized = val; applyFilters(); }}
						class={filterFinalized === val ? segActive : segIdle}>{lbl}</button>
				{/each}
			</div>
		</div>
		<div class="col-span-2">
			<RangeInput
				label="Epoch Range"
				bind:from={filterEpochFrom}
				bind:to={filterEpochTo}
				onChange={applyFilters}
				fromPlaceholder={epochMinPlaceholder}
				toPlaceholder={epochMaxPlaceholder}
			/>
		</div>
		<div title="Effective delay subtracts missed proposer slots — a delay of 2 where the next slot was empty counts as 1.">
			<RangeInput
				label="Effective Delay"
				bind:from={filterMinEffectiveDelay}
				bind:to={filterMaxEffectiveDelay}
				onChange={applyFilters}
				fromPlaceholder="1"
				toPlaceholder="32"
			/>
		</div>
	</div>
</div>

<!-- Results -->
{#if loading && (!result || result.data.length === 0)}
	<p class="text-gray-400">Loading...</p>
{:else if result}
	<div class="flex justify-between items-center mb-2">
		<span class="text-sm text-gray-400">{result.total.toLocaleString()} results</span>
		<span class="text-sm text-gray-400">Loaded {result.data.length.toLocaleString()}</span>
	</div>

	<div class={tableSurface}>
		<table class="w-full text-sm">
			<thead class={tableHeaderRow}>
				<tr>
					{#each [['validator_index','Validator'],['epoch','Epoch'],['assigned_slot','Slot'],['assigned_slot','Time'],['included','Status'],['effective_inclusion_delay','Delay'],['head_correct','Head'],['target_correct','Target'],['source_correct','Source']] as [col, label]}
						<th class="px-3 py-2 text-left cursor-pointer hover:text-white select-none {col === 'validator_index' ? 'w-[240px]' : ''}" onclick={() => sort(col)}>
							{label} {#if sortCol === col}{sortOrder === 'asc' ? '\u25B2' : '\u25BC'}{/if}
						</th>
					{/each}
					<th class="px-3 py-2 text-right">Src</th>
					<th class="px-3 py-2 text-right">Tgt</th>
					<th class="px-3 py-2 text-right">Head</th>
					<th class="px-3 py-2 text-right">Total</th>
					<th class="px-3 py-2 text-center">Fin</th>
				</tr>
			</thead>
			<tbody>
				{#each result.data as r}
					{@const rowTags = getTags(r.validator_index)}
					<tr class="{tableBodyRow} {!r.included ? 'bg-red-950/20' : ''}">
						<td class="w-[240px] max-w-[240px] px-3 py-1.5 align-top">
							<div class="flex flex-wrap items-center gap-1">
								<a href={beaconchainUrl(r.validator_index)} target="_blank" rel="noopener" class="font-mono text-blue-400 hover:underline whitespace-nowrap pr-1.5">{r.validator_index}</a>
								{#each rowTags as tag}
									<span class="inline-block text-[10px] px-1.5 py-0.5 rounded bg-purple-900/40 text-purple-300">{tag}</span>
								{/each}
							</div>
						</td>
						<td class="px-3 py-1.5">{r.epoch}</td>
						<td class="px-3 py-1.5">
							{r.assigned_slot}
							<span
									class="ml-1 text-[11px] text-gray-500 cursor-help"
									title="Position of this slot within its epoch."
								>({slotInEpoch(r.assigned_slot)}/32)</span>
						</td>
						<td class="px-3 py-1.5 text-xs text-gray-400" title={slotTime(r.assigned_slot)}>{timeAgo(r.assigned_slot)}</td>
						<td class="px-3 py-1.5">
							{#if r.included}
								<span class="text-green-400">Included</span>
								{#if r.inclusion_delay !== null}
									<span class="text-gray-500 text-xs ml-1">(slot {r.inclusion_slot})</span>
								{/if}
							{:else}
								<span class="text-red-400 font-bold">MISSED</span>
							{/if}
						</td>
						<td class="px-3 py-1.5">
							{#if r.effective_inclusion_delay === null}
								-
							{:else}
								<span
									class="cursor-help"
									class:text-yellow-400={r.effective_inclusion_delay > 1}
									title="How late the attestation was included, ignoring slots where no block was proposed at all. 1 = included as soon as possible;"
								>{r.effective_inclusion_delay}</span>
								{#if r.inclusion_delay !== null}
									<span
										class="ml-1.5 text-xs text-gray-400 cursor-help"
										title="Actual slots between duty and inclusion counting slots where no block was proposed."
									>({r.inclusion_delay})</span>
								{/if}
							{/if}
						</td>
						<td class="px-3 py-1.5">
							{#if r.head_correct === null}-{:else if r.head_correct}<span class="text-green-400">OK</span>{:else}<span class="text-red-400">WRONG</span>{/if}
						</td>
						<td class="px-3 py-1.5">
							{#if r.target_correct === null}-{:else if r.target_correct}<span class="text-green-400">OK</span>{:else}<span class="text-red-400">WRONG</span>{/if}
						</td>
						<td class="px-3 py-1.5">
							{#if r.source_correct === null}-{:else if r.source_correct}<span class="text-green-400">OK</span>{:else}<span class="text-red-400">WRONG</span>{/if}
						</td>
						<td class="px-3 py-1.5 text-right font-mono text-xs" class:text-red-400={r.source_reward !== null && r.source_reward < 0}>{eth(r.source_reward)}</td>
						<td class="px-3 py-1.5 text-right font-mono text-xs" class:text-red-400={r.target_reward !== null && r.target_reward < 0}>{eth(r.target_reward)}</td>
						<td class="px-3 py-1.5 text-right font-mono text-xs" class:text-red-400={r.head_reward !== null && r.head_reward < 0}>{eth(r.head_reward)}</td>
						<td class="px-3 py-1.5 text-right font-mono text-xs font-bold" class:text-green-400={r.total_reward !== null && r.total_reward > 0} class:text-red-400={r.total_reward !== null && r.total_reward < 0}>{eth(r.total_reward)}</td>
						<td class="px-3 py-1.5 text-center">{r.finalized ? 'Y' : 'N'}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
	{#if loadingMore}
		<div class="py-3 text-center text-xs text-gray-500">Loading more...</div>
	{/if}
	{#if hasMore()}
		<div bind:this={sentinelEl} class="h-1"></div>
	{/if}
{/if}
