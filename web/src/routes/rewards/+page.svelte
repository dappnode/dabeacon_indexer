<script lang="ts">
	import { onMount } from 'svelte';
	import { getRewards, getMeta, eth, beaconchainUrl, type RewardsResponse, type RewardWindow, type MetaResponse, type ValidatorRewards } from '$lib/api';
	import { tableSurface, tableHeaderRow, tableBodyRow, tabIdle, tabActive, tagIdle, tagActive } from '$lib/ui';

	let data: RewardsResponse | null = $state(null);
	let meta: MetaResponse = $state({ validators: {}, all_tags: [] });
	let loading = $state(true);
	let activeTab: 'day_1' | 'day_7' | 'day_30' | 'all_time' = $state('day_7');
	let activeTag = $state('');
	const PAGE_SIZE = 100;
	let visibleCount = $state(PAGE_SIZE);
	let sentinelEl: HTMLDivElement | null = $state(null);

	onMount(async () => {
		try {
			[data, meta] = await Promise.all([getRewards(), getMeta()]);
		} catch (e) {
			console.error(e);
		}
		loading = false;
	});

	function getWindow(v: { day_1: RewardWindow; day_7: RewardWindow; day_30: RewardWindow; all_time: RewardWindow }): RewardWindow {
		return v[activeTab];
	}

	function hasTag(validatorIndex: number, tag: string): boolean {
		const m = meta.validators[String(validatorIndex)];
		return !!m?.tags?.includes(tag);
	}

	function matchesTag(validatorIndex: number): boolean {
		if (!activeTag) return true;
		return hasTag(validatorIndex, activeTag);
	}

	function getFilteredValidators() {
		if (!data) return [];
		return data.validators.filter((v) => matchesTag(v.validator_index));
	}

	function loadMoreRows() {
		const filtered = getFilteredValidators();
		if (visibleCount < filtered.length) {
			visibleCount = Math.min(visibleCount + PAGE_SIZE, filtered.length);
		}
	}

	function sumFilteredWindow(vals: ValidatorRewards[]): RewardWindow {
		const total: RewardWindow = {
			attestation_reward: 0,
			sync_reward: 0,
			proposal_reward: 0,
			total: 0,
			epochs: 0,
		};

		for (const v of vals) {
			const w = getWindow(v);
			total.attestation_reward += w.attestation_reward;
			total.sync_reward += w.sync_reward;
			total.proposal_reward += w.proposal_reward;
			total.total += w.total;
			total.epochs = Math.max(total.epochs, w.epochs);
		}

		return total;
	}

	const tabs: { key: 'day_1' | 'day_7' | 'day_30' | 'all_time'; label: string }[] = [
		{ key: 'day_1', label: '24h' },
		{ key: 'day_7', label: '7d' },
		{ key: 'day_30', label: '30d' },
		{ key: 'all_time', label: 'All Time' },
	];

	$effect(() => {
		data;
		activeTab;
		activeTag;
		visibleCount = PAGE_SIZE;
	});

	$effect(() => {
		if (!sentinelEl) return;

		const observer = new IntersectionObserver(
			(entries) => {
				if (entries.some((e) => e.isIntersecting)) {
					loadMoreRows();
				}
			},
			{ root: null, rootMargin: '200px 0px', threshold: 0 }
		);

		observer.observe(sentinelEl);
		return () => observer.disconnect();
	});
</script>

<svelte:head><title>Rewards - Beacon Indexer</title></svelte:head>

<h1 class="text-2xl font-bold mb-4">Rewards</h1>

{#if loading}
	<p class="text-gray-400">Loading...</p>
{:else if data}
	<!-- Time window tabs -->
	<div class="flex gap-1 mb-6">
		{#each tabs as tab}
			<button
				onclick={() => activeTab = tab.key}
				class={activeTab === tab.key ? tabActive : tabIdle}
			>{tab.label}</button>
		{/each}
	</div>

	{#if meta.all_tags.length > 0}
		<div class="flex items-center gap-2 mb-6 flex-wrap">
			<span class="text-xs text-gray-500">Filter by tag</span>
			<button
				onclick={() => (activeTag = '')}
				class={activeTag === '' ? tagActive : tagIdle}
			>All</button>
			{#each meta.all_tags as tag}
				<button
					onclick={() => (activeTag = tag)}
					class={activeTag === tag ? tagActive : tagIdle}
				>{tag}</button>
			{/each}
		</div>
	{/if}

	<!-- Totals summary -->
	{@const filteredValidators = getFilteredValidators()}
	{@const visibleValidators = filteredValidators.slice(0, visibleCount)}
	{@const tw = activeTag ? sumFilteredWindow(filteredValidators) : getWindow(data.totals)}
	<div class="bg-gradient-to-br from-gray-900 to-gray-900/60 ring-1 ring-gray-800/80 rounded-lg p-5 mb-6 shadow-sm">
		<h2 class="text-lg font-bold mb-3">{activeTag ? `Total (tag: ${activeTag})` : 'Total (all validators)'}</h2>
		<div class="grid grid-cols-2 md:grid-cols-5 gap-4">
			<div>
				<div class="text-xs text-gray-500 uppercase mb-1">Total Reward</div>
				<div class="text-2xl font-bold" class:text-green-400={tw.total > 0} class:text-red-400={tw.total < 0}>
					{eth(tw.total)} ETH
				</div>
			</div>
			<div>
				<div class="text-xs text-gray-500 uppercase mb-1">Attestation</div>
				<div class="text-lg font-semibold" class:text-green-400={tw.attestation_reward > 0}>
					{eth(tw.attestation_reward)} ETH
				</div>
			</div>
			<div>
				<div class="text-xs text-gray-500 uppercase mb-1">Sync Committee</div>
				<div class="text-lg font-semibold" class:text-green-400={tw.sync_reward > 0}>
					{eth(tw.sync_reward)} ETH
				</div>
			</div>
			<div>
				<div class="text-xs text-gray-500 uppercase mb-1">Proposals</div>
				<div class="text-lg font-semibold" class:text-green-400={tw.proposal_reward > 0}>
					{eth(tw.proposal_reward)} ETH
				</div>
			</div>
			<div>
				<div class="text-xs text-gray-500 uppercase mb-1">Epochs</div>
				<div class="text-lg font-semibold">{tw.epochs.toLocaleString()}</div>
				<div class="text-xs text-gray-500">Latest: {data.latest_epoch}</div>
			</div>
		</div>
	</div>

	<!-- Per-validator table -->
	<div class={tableSurface}>
		<table class="w-full text-sm">
			<thead class={tableHeaderRow}>
				<tr>
					<th class="px-3 py-2 text-left">Validator</th>
					<th class="px-3 py-2 text-right">Attestation</th>
					<th class="px-3 py-2 text-right">Sync</th>
					<th class="px-3 py-2 text-right">Proposals</th>
					<th class="px-3 py-2 text-right">Total</th>
					<th class="px-3 py-2 text-right">Epochs</th>
				</tr>
			</thead>
			<tbody>
				{#each visibleValidators as v}
					{@const w = getWindow(v)}
					<tr class={tableBodyRow}>
						<td class="px-3 py-2 font-mono"><a href={beaconchainUrl(v.validator_index)} target="_blank" rel="noopener" class="text-blue-400 hover:underline">{v.validator_index}</a></td>
						<td class="px-3 py-2 text-right font-mono text-xs" class:text-green-400={w.attestation_reward > 0} class:text-red-400={w.attestation_reward < 0}>{eth(w.attestation_reward)}</td>
						<td class="px-3 py-2 text-right font-mono text-xs" class:text-green-400={w.sync_reward > 0} class:text-red-400={w.sync_reward < 0}>{eth(w.sync_reward)}</td>
						<td class="px-3 py-2 text-right font-mono text-xs" class:text-green-400={w.proposal_reward > 0}>{eth(w.proposal_reward)}</td>
						<td class="px-3 py-2 text-right font-mono text-xs font-bold" class:text-green-400={w.total > 0} class:text-red-400={w.total < 0}>{eth(w.total)}</td>
						<td class="px-3 py-2 text-right text-gray-400">{w.epochs.toLocaleString()}</td>
					</tr>
				{/each}
			</tbody>
			<tfoot class="bg-gray-900/80 font-bold">
				<tr class="border-t-2 border-gray-700/70">
					<td class="px-3 py-2">TOTAL</td>
					<td class="px-3 py-2 text-right font-mono text-xs" class:text-green-400={tw.attestation_reward > 0}>{eth(tw.attestation_reward)}</td>
					<td class="px-3 py-2 text-right font-mono text-xs" class:text-green-400={tw.sync_reward > 0}>{eth(tw.sync_reward)}</td>
					<td class="px-3 py-2 text-right font-mono text-xs" class:text-green-400={tw.proposal_reward > 0}>{eth(tw.proposal_reward)}</td>
					<td class="px-3 py-2 text-right font-mono text-xs" class:text-green-400={tw.total > 0} class:text-red-400={tw.total < 0}>{eth(tw.total)}</td>
					<td class="px-3 py-2 text-right text-gray-400">{tw.epochs.toLocaleString()}</td>
				</tr>
			</tfoot>
		</table>
	</div>
	{#if visibleValidators.length < filteredValidators.length}
		<div bind:this={sentinelEl} class="py-4 text-center text-xs text-gray-500">
			Loading more validators...
		</div>
	{/if}
{/if}
