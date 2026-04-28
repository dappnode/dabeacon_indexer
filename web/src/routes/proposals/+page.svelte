<script lang="ts">
	import { onMount } from 'svelte';
	import { getProposals, getStats, getValidators, getMeta, eth, slotTime, timeAgo, beaconchainUrl, beaconchainSlotUrl, type ProposalRow, type Paginated, type ValidatorSummary, type MetaResponse, type Stats } from '$lib/api';
	import ValidatorPicker from '$lib/ValidatorPicker.svelte';
	import RangeInput from '$lib/RangeInput.svelte';
	import { cardSurface, tableSurface, tableHeaderRow, tableBodyRow, btnNeutral, btnDanger, segIdle, segActive } from '$lib/ui';

	let validators: ValidatorSummary[] = $state([]);
	let meta: MetaResponse = $state({ validators: {}, all_tags: [] });
	let stats = $state<Stats | null>(null);
	let result: Paginated<ProposalRow> | null = $state(null);
	let loading = $state(true);

	const epochMinPlaceholder = $derived<string>(stats?.earliest_scanned_epoch != null ? String(stats.earliest_scanned_epoch) : 'from');
	const epochMaxPlaceholder = $derived<string>(stats?.latest_scanned_epoch != null ? String(stats.latest_scanned_epoch) : 'to');

	let filterProposer = $state('');
	let filterEpochFrom = $state('');
	let filterEpochTo = $state('');
	let filterProposed = $state('');
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
			const next = await getProposals({
				proposer_index: filterProposer || undefined,
				epoch_from: filterEpochFrom || undefined,
				epoch_to: filterEpochTo || undefined,
				proposed: filterProposed === '' ? undefined : filterProposed === 'true',
				sort: sortCol, order: sortOrder, page, per_page: perPage,
			});
			if (append && result) {
				result = { ...next, data: [...result.data, ...next.data] };
			} else {
				result = next;
			}
		} catch (e) { console.error(e); }
		if (append) loadingMore = false;
		else loading = false;
	}

	function applyFilters() { page = 1; load(); }
	function resetFilters() { filterProposer = ''; filterEpochFrom = ''; filterEpochTo = ''; filterProposed = ''; page = 1; load(); }
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

<svelte:head><title>Proposals - Beacon Indexer</title></svelte:head>
<h1 class="text-2xl font-bold mb-4">Block Proposals</h1>

<div class="flex gap-2 mb-4">
	<button onclick={resetFilters} class={btnNeutral}>All</button>
	<button onclick={() => { filterProposed = 'false'; applyFilters(); }} class={btnDanger}>Missed Only</button>
</div>

<div class="mb-4 {cardSurface} space-y-4">
	<ValidatorPicker {validators} meta={meta.validators} allTags={meta.all_tags} bind:value={filterProposer} label="Proposers" onChange={applyFilters} />

	<div class="grid grid-cols-1 md:grid-cols-3 gap-3">
		<div>
			<span class="text-xs text-gray-500 block mb-1">Proposed</span>
			<div class="flex gap-1">
				{#each [['', 'All'], ['true', 'Yes'], ['false', 'Missed']] as [val, lbl]}
					<button onclick={() => { filterProposed = val; applyFilters(); }}
						class={filterProposed === val ? segActive : segIdle}>{lbl}</button>
				{/each}
			</div>
		</div>
		<div class="md:col-span-2">
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
					{#each [['slot','Slot'],['proposer_index','Proposer']] as [col, label]}
						<th class="px-3 py-2 text-left cursor-pointer hover:text-white select-none {col === 'proposer_index' ? 'w-[240px]' : ''}" onclick={() => sort(col)}>{label} {#if sortCol === col}{sortOrder === 'asc' ? '\u25B2' : '\u25BC'}{/if}</th>
					{/each}
					<th class="px-3 py-2 text-left">Epoch</th>
					<th class="px-3 py-2 text-left">Time</th>
					<th class="px-3 py-2 text-left">Status</th>
					<th class="px-3 py-2 text-right cursor-pointer hover:text-white select-none" onclick={() => sort('reward_total')}>Total {#if sortCol === 'reward_total'}{sortOrder === 'asc' ? '\u25B2' : '\u25BC'}{/if}</th>
					<th class="px-3 py-2 text-right">Att</th>
					<th class="px-3 py-2 text-right">Sync</th>
					<th class="px-3 py-2 text-right">Slashings</th>
					<th class="px-3 py-2 text-center">Finalized</th>
				</tr>
			</thead>
			<tbody>
				{#each result.data as r}
					{@const rowTags = getTags(r.proposer_index)}
					<tr class="{tableBodyRow} {!r.proposed ? 'bg-red-950/20' : ''}">
						<td class="px-3 py-1.5">
							<a href={beaconchainSlotUrl(r.slot)} target="_blank" rel="noopener" class="font-mono text-blue-400 hover:underline">{r.slot}</a>
						</td>
						<td class="w-[240px] max-w-[240px] px-3 py-1.5 align-top">
							<div class="flex flex-wrap items-center gap-1">
								<a href={beaconchainUrl(r.proposer_index)} target="_blank" rel="noopener" class="font-mono text-blue-400 hover:underline whitespace-nowrap pr-1.5">{r.proposer_index}</a>
								{#each rowTags as tag}
									<span class="inline-block text-[10px] px-1.5 py-0.5 rounded bg-purple-900/40 text-purple-300">{tag}</span>
								{/each}
							</div>
						</td>
						<td class="px-3 py-1.5 text-gray-400">{r.epoch}</td>
						<td class="px-3 py-1.5 text-xs text-gray-400" title={slotTime(r.slot)}>{timeAgo(r.slot)}</td>
						<td class="px-3 py-1.5">{#if r.proposed}<span class="text-green-400">Proposed</span>{:else}<span class="text-red-400 font-bold">MISSED</span>{/if}</td>
						<td class="px-3 py-1.5 text-right font-mono text-xs">{eth(r.reward_total)}</td>
						<td class="px-3 py-1.5 text-right font-mono text-xs">{eth(r.reward_attestations)}</td>
						<td class="px-3 py-1.5 text-right font-mono text-xs">{eth(r.reward_sync)}</td>
						<td class="px-3 py-1.5 text-right font-mono text-xs">{eth(r.reward_slashings)}</td>
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
