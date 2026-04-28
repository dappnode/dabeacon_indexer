<script lang="ts">
	import { onMount } from 'svelte';
	import { getEpochs, getStats, getValidators, getMeta, pct, eth, epochTime, timeAgo, type EpochRow, type Paginated, type ValidatorSummary, type MetaResponse, type Stats } from '$lib/api';
	import ValidatorPicker from '$lib/ValidatorPicker.svelte';
	import RangeInput from '$lib/RangeInput.svelte';
	import { cardSurface, tableSurface, tableHeaderRow, tableBodyRow, healthTier, tierText } from '$lib/ui';

	let validators: ValidatorSummary[] = $state([]);
	let meta: MetaResponse = $state({ validators: {}, all_tags: [] });
	let stats = $state<Stats | null>(null);
	let result: Paginated<EpochRow> | null = $state(null);
	let loading = $state(true);

	const epochMinPlaceholder = $derived<string>(stats?.earliest_scanned_epoch != null ? String(stats.earliest_scanned_epoch) : 'from');
	const epochMaxPlaceholder = $derived<string>(stats?.latest_scanned_epoch != null ? String(stats.latest_scanned_epoch) : 'to');

	let filterEpochFrom = $state('');
	let filterEpochTo = $state('');
	let filterValidator = $state('');
	let sortOrder = $state('desc');
	let page = $state(1);
	let perPage = $state(50);
	let loadingMore = $state(false);
	let sentinelEl: HTMLDivElement | null = $state(null);

	async function load(append = false) {
		if (append) loadingMore = true;
		else loading = true;
		try {
			const next = await getEpochs({
				epoch_from: filterEpochFrom || undefined,
				epoch_to: filterEpochTo || undefined,
				validator_index: filterValidator || undefined,
				order: sortOrder, page, per_page: perPage,
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
	function resetFilters() { filterEpochFrom = ''; filterEpochTo = ''; filterValidator = ''; page = 1; load(); }
	function toggleOrder() { sortOrder = sortOrder === 'asc' ? 'desc' : 'asc'; page = 1; load(); }
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

<svelte:head><title>Epochs - Beacon Indexer</title></svelte:head>
<h1 class="text-2xl font-bold mb-4">Epoch Summary</h1>

<div class="mb-4 {cardSurface} space-y-4">
	<ValidatorPicker {validators} meta={meta.validators} allTags={meta.all_tags} bind:value={filterValidator} onChange={applyFilters} />

	<div class="grid grid-cols-1 md:grid-cols-2 gap-3">
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

{#if loading && (!result || result.data.length === 0)}
	<p class="text-gray-400">Loading...</p>
{:else if result}
	<div class="flex justify-between items-center mb-2">
		<span class="text-sm text-gray-400">{result.total.toLocaleString()} epochs</span>
		<span class="text-sm text-gray-400">Loaded {result.data.length.toLocaleString()}</span>
	</div>
	<div class={tableSurface}>
		<table class="w-full text-sm">
			<thead class={tableHeaderRow}>
				<tr>
					<th class="px-3 py-2 text-left cursor-pointer hover:text-white select-none" onclick={toggleOrder}>Epoch {sortOrder === 'asc' ? '\u25B2' : '\u25BC'}</th>
					<th class="px-3 py-2 text-left">Time</th>
					<th class="px-3 py-2 text-right">Duties</th>
					<th class="px-3 py-2 text-right">Incl</th>
					<th class="px-3 py-2 text-right">Miss</th>
					<th class="px-3 py-2 text-right">Att Rate</th>
					<th class="px-3 py-2 text-right">Head</th>
					<th class="px-3 py-2 text-right">Target</th>
					<th class="px-3 py-2 text-right">Source</th>
					<th class="px-3 py-2 text-right">Sync</th>
					<th class="px-3 py-2 text-right">Proposals</th>
					<th class="px-3 py-2 text-right">Reward</th>
				</tr>
			</thead>
			<tbody>
				{#each result.data as r}
					{@const rowTier = healthTier(r.attestation_rate)}
					<tr class="{tableBodyRow} {r.missed > 0 || r.proposals_missed > 0 ? 'bg-red-950/20' : ''}">
						<td class="px-3 py-1.5 font-mono">{r.epoch}</td>
						<td class="px-3 py-1.5 text-xs text-gray-400" title={epochTime(r.epoch)}>{timeAgo(r.epoch * 32)}</td>
						<td class="px-3 py-1.5 text-right">{r.total_duties}</td>
						<td class="px-3 py-1.5 text-right text-green-400">{r.included}</td>
						<td class="px-3 py-1.5 text-right" class:text-red-400={r.missed > 0}>{r.missed}</td>
						<td class="px-3 py-1.5 text-right {tierText[rowTier]}">{pct(r.attestation_rate)}</td>
						<td class="px-3 py-1.5 text-right">{r.head_correct}/{r.included}</td>
						<td class="px-3 py-1.5 text-right">{r.target_correct}/{r.included}</td>
						<td class="px-3 py-1.5 text-right">{r.source_correct}/{r.included}</td>
						<td class="px-3 py-1.5 text-right">{#if r.sync_participated + r.sync_missed > 0}<span class="text-green-400">{r.sync_participated}</span>{#if r.sync_missed > 0}/<span class="text-red-400">{r.sync_missed}</span>{/if}{:else}<span class="text-gray-600">-</span>{/if}</td>
						<td class="px-3 py-1.5 text-right">{#if r.proposals + r.proposals_missed > 0}<span class="text-green-400">{r.proposals}</span>{#if r.proposals_missed > 0}/<span class="text-red-400">{r.proposals_missed}</span>{/if}{:else}<span class="text-gray-600">-</span>{/if}</td>
						<td class="px-3 py-1.5 text-right font-mono text-xs" class:text-green-400={r.total_reward > 0} class:text-red-400={r.total_reward < 0}>{eth(r.total_reward)}</td>
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
