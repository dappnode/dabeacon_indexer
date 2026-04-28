<script lang="ts">
	import { onMount } from 'svelte';
	import { getSyncDuties, getStats, getValidators, getMeta, eth, slotTime, timeAgo, beaconchainUrl, type SyncRow, type Paginated, type ValidatorSummary, type MetaResponse, type Stats } from '$lib/api';
	import ValidatorPicker from '$lib/ValidatorPicker.svelte';
	import RangeInput from '$lib/RangeInput.svelte';
	import { cardSurface, tableSurface, tableHeaderRow, tableBodyRow, btnNeutral, btnDanger, btnAmber, btnOrange, segIdle, segActive } from '$lib/ui';

	let validators: ValidatorSummary[] = $state([]);
	let meta: MetaResponse = $state({ validators: {}, all_tags: [] });
	let stats = $state<Stats | null>(null);
	let result: Paginated<SyncRow> | null = $state(null);
	let loading = $state(true);

	const epochMinPlaceholder = $derived<string>(stats?.earliest_scanned_epoch != null ? String(stats.earliest_scanned_epoch) : 'from');
	const epochMaxPlaceholder = $derived<string>(stats?.latest_scanned_epoch != null ? String(stats.latest_scanned_epoch) : 'to');

	let filterValidator = $state('');
	let filterEpochFrom = $state('');
	let filterEpochTo = $state('');
	let filterParticipated = $state('');
	let filterMissedBlock = $state('');
	let filterFinalized = $state('');
	let sortCol = $state('slot');
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
			const missedBlockFilter =
				filterMissedBlock === '' ? undefined : filterMissedBlock === 'true';
			const next = await getSyncDuties({
				validator_index: filterValidator || undefined,
				epoch_from: filterEpochFrom || undefined,
				epoch_to: filterEpochTo || undefined,
				participated: filterParticipated === '' ? undefined : filterParticipated === 'true',
				missed_block: missedBlockFilter,
				finalized: filterFinalized === '' ? undefined : filterFinalized === 'true',
				sort: sortCol, order: sortOrder, page, per_page: perPage,
			});

			let merged = next;
			// Defensive fallback: if backend ignores missed_block (e.g. stale server build),
			// enforce it client-side so the UI control still has an effect.
			if (missedBlockFilter !== undefined && next.data.some((r) => r.missed_block !== missedBlockFilter)) {
				const filtered = next.data.filter((r) => r.missed_block === missedBlockFilter);
				merged = { ...next, data: filtered, total: filtered.length };
			}

			if (append && result) {
				result = { ...merged, data: [...result.data, ...merged.data] };
			} else {
				result = merged;
			}
		} catch (e) { console.error(e); }
		if (append) loadingMore = false;
		else loading = false;
	}

	function applyFilters() { page = 1; load(); }
	function resetFilters() { filterValidator = ''; filterEpochFrom = ''; filterEpochTo = ''; filterParticipated = ''; filterMissedBlock = ''; filterFinalized = ''; page = 1; load(); }
	function applyMissedAllPreset() {
		filterParticipated = 'false';
		filterMissedBlock = '';
		applyFilters();
	}
	function applyBlockMissedPreset() {
		filterParticipated = 'false';
		filterMissedBlock = 'false';
		applyFilters();
	}
	function applyNoBlockMissesPreset() {
		filterParticipated = 'false';
		filterMissedBlock = 'true';
		applyFilters();
	}
	function setParticipatedFilter(val: string) {
		filterParticipated = val;
		applyFilters();
	}
	function setMissedBlockFilter(val: string) {
		filterMissedBlock = val;
		if (val === 'true') {
			filterParticipated = 'false';
		}
		applyFilters();
	}
	function sort(col: string) { if (sortCol === col) sortOrder = sortOrder === 'asc' ? 'desc' : 'asc'; else { sortCol = col; sortOrder = 'desc'; } page = 1; load(); }
	function hasMore(): boolean { return !!result && result.data.length < result.total; }
	async function loadMore() {
		if (loading || loadingMore || !hasMore()) return;
		page += 1;
		await load(true);
	}

	onMount(async () => {
		[validators, meta, stats] = await Promise.all([getValidators(), getMeta(), getStats()]);
		await load();
	});

	$effect(() => {
		if (!sentinelEl) return;
		const observer = new IntersectionObserver(
			(entries) => {
				if (entries.some((e) => e.isIntersecting)) loadMore();
			},
			{ root: null, rootMargin: '200px 0px', threshold: 0 }
		);
		observer.observe(sentinelEl);
		return () => observer.disconnect();
	});
</script>

<svelte:head><title>Sync Committee - Beacon Indexer</title></svelte:head>
<h1 class="text-2xl font-bold mb-4">Sync Committee Duties</h1>

<div class="flex flex-wrap gap-2 mb-4">
	<button onclick={resetFilters} class={btnNeutral}>Show All</button>
	<button onclick={applyMissedAllPreset} class={btnDanger}>Any Miss</button>
	<button onclick={applyBlockMissedPreset} class={btnAmber}>Missed (Block)</button>
	<button onclick={applyNoBlockMissesPreset} class={btnOrange}>Missed (No Block)</button>
</div>

<div class="mb-4 {cardSurface} space-y-4">
	<ValidatorPicker {validators} meta={meta.validators} allTags={meta.all_tags} bind:value={filterValidator} onChange={applyFilters} />

	<div class="grid grid-cols-2 md:grid-cols-5 gap-3">
		<div>
			<span class="text-xs text-gray-500 block mb-1">Participated</span>
			<div class="flex gap-1">
				{#each [['', 'All'], ['true', 'Yes'], ['false', 'No']] as [val, lbl]}
					<button onclick={() => setParticipatedFilter(val)}
						class={filterParticipated === val ? segActive : segIdle}>{lbl}</button>
				{/each}
			</div>
		</div>
		<div>
			<span class="text-xs text-gray-500 block mb-1">Missed Block</span>
			<div class="flex gap-1">
				{#each [['', 'All'], ['true', 'Yes'], ['false', 'No']] as [val, lbl]}
					<button onclick={() => setMissedBlockFilter(val)}
						class={filterMissedBlock === val ? segActive : segIdle}>{lbl}</button>
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
	</div>
</div>

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
					<th class="w-[240px] px-3 py-2 text-left cursor-pointer hover:text-white select-none" onclick={() => sort('validator_index')}>Validator {#if sortCol === 'validator_index'}{sortOrder === 'asc' ? '\u25B2' : '\u25BC'}{/if}</th>
					<th class="px-3 py-2 text-left cursor-pointer hover:text-white select-none" onclick={() => sort('slot')}>Slot {#if sortCol === 'slot'}{sortOrder === 'asc' ? '\u25B2' : '\u25BC'}{/if}</th>
					<th class="px-3 py-2 text-left">Epoch</th>
					<th class="px-3 py-2 text-left">Time</th>
					<th class="px-3 py-2 text-left">Status</th>
					<th class="px-3 py-2 text-right cursor-pointer hover:text-white select-none" onclick={() => sort('reward')}>Reward {#if sortCol === 'reward'}{sortOrder === 'asc' ? '\u25B2' : '\u25BC'}{/if}</th>
					<th class="px-3 py-2 text-center">Finalized</th>
				</tr>
			</thead>
			<tbody>
				{#each result.data as r}
					{@const rowTags = getTags(r.validator_index)}
					<tr class="{tableBodyRow} {!r.participated ? (r.missed_block ? 'bg-yellow-950/20' : 'bg-red-950/20') : ''}">
						<td class="w-[240px] max-w-[240px] px-3 py-1.5 align-top">
							<div class="flex flex-wrap items-center gap-1">
								<a href={beaconchainUrl(r.validator_index)} target="_blank" rel="noopener" class="font-mono text-blue-400 hover:underline whitespace-nowrap pr-1.5">{r.validator_index}</a>
								{#each rowTags as tag}
									<span class="inline-block text-[10px] px-1.5 py-0.5 rounded bg-purple-900/40 text-purple-300">{tag}</span>
								{/each}
							</div>
						</td>
						<td class="px-3 py-1.5">{r.slot}</td>
						<td class="px-3 py-1.5 text-gray-400">{r.epoch}</td>
						<td class="px-3 py-1.5 text-xs text-gray-400" title={slotTime(r.slot)}>{timeAgo(r.slot)}</td>
						<td class="px-3 py-1.5">
							{#if r.participated}
								<span class="text-green-400">Participated</span>
							{:else if r.missed_block}
								<span class="text-yellow-300 font-bold">NO BLOCK</span>
							{:else}
								<span class="text-red-400 font-bold">MISSED</span>
							{/if}
						</td>
						<td class="px-3 py-1.5 text-right font-mono text-xs" class:text-red-400={r.reward !== null && r.reward < 0}>{eth(r.reward)}</td>
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
