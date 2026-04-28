<script lang="ts">
	import { onMount } from 'svelte';
	import { getStats, getValidators, getMeta, pct, beaconchainUrl, epochTime, type Stats, type ValidatorSummary, type MetaResponse } from '$lib/api';
	import {
		cardSurface, tableSurface, tableHeaderRow, tableBodyRow,
		sectionHeader, sectionDivider,
		healthTier, tierText, tierBar, tierDot
	} from '$lib/ui';

	let stats: Stats | null = $state(null);
	let validators: ValidatorSummary[] = $state([]);
	let meta: MetaResponse = $state({ validators: {}, all_tags: [] });
	let loading = $state(true);

	function getTags(idx: number): string[] {
		const m = meta.validators[String(idx)];
		return m?.tags || [];
	}

	function syncRate(s: Stats): number {
		const total = s.total_sync_participated + s.total_sync_missed;
		return total > 0 ? s.total_sync_participated / total : 0;
	}

	const syncTotal = $derived<number>(stats != null ? (stats as Stats).total_sync_participated + (stats as Stats).total_sync_missed : 0);
	const syncRateVal = $derived<number>(stats != null ? syncRate(stats as Stats) : 0);
	const syncTier = $derived<'good' | 'warn' | 'bad'>(syncTotal > 0 ? healthTier(syncRateVal) : 'good');

	onMount(async () => {
		try {
			[stats, validators, meta] = await Promise.all([getStats(), getValidators(), getMeta()]);
		} catch (e) {
			console.error(e);
		}
		loading = false;
	});
</script>

<svelte:head><title>Dashboard - Beacon Indexer</title></svelte:head>

{#if loading}
	<p class="text-gray-400">Loading...</p>
{:else if stats}
	<!-- Health hero -->
	{@const overallTier = healthTier(stats.attestation_rate)}
	<div class="relative overflow-hidden rounded-xl ring-1 ring-gray-800/80 bg-gradient-to-br from-gray-900 via-gray-900 to-blue-950/40 p-5 md:p-6 mb-8 shadow-lg">
		<div class="absolute -top-20 -right-20 w-64 h-64 bg-blue-500/10 rounded-full blur-3xl pointer-events-none"></div>
		<div class="relative flex flex-col md:flex-row md:items-end md:justify-between gap-4">
			<div>
				<div class="text-xs text-gray-400 uppercase tracking-wide mb-1 flex items-center gap-2">
					<span class="relative inline-flex w-2 h-2">
						<span class="absolute inline-flex w-full h-full rounded-full {tierDot[overallTier]} opacity-75 animate-ping"></span>
						<span class="relative inline-flex w-2 h-2 rounded-full {tierDot[overallTier]}"></span>
					</span>
					Overall attestation rate
				</div>
				<div class="flex items-baseline gap-3">
					<div class="text-4xl md:text-5xl font-bold {tierText[overallTier]} tabular-nums">{pct(stats.attestation_rate)}</div>
					<div class="text-sm text-gray-400">
						{stats.total_attestations.toLocaleString()} included
						{#if stats.total_missed > 0}
							· <span class="text-red-400">{stats.total_missed.toLocaleString()} missed</span>
						{/if}
					</div>
				</div>
			</div>
			<div class="grid grid-cols-3 gap-4 md:gap-8 text-sm md:text-right">
				<div>
					<div class="text-[10px] uppercase tracking-wide text-gray-500">Validators</div>
					<div class="font-semibold tabular-nums">{stats.total_validators}</div>
				</div>
				<div>
					<div class="text-[10px] uppercase tracking-wide text-gray-500">Epochs scanned</div>
					<div class="font-semibold tabular-nums">{stats.total_epochs_scanned.toLocaleString()}</div>
				</div>
				<div>
					<div class="text-[10px] uppercase tracking-wide text-gray-500">Range</div>
					<div class="font-semibold tabular-nums">{stats.earliest_scanned_epoch ?? '-'}–{stats.latest_scanned_epoch ?? '-'}</div>
				</div>
			</div>
		</div>
	</div>

	<!-- Performance -->
	<div class="mb-2 flex items-center gap-2">
		<h2 class={sectionHeader}>Performance</h2>
		<span class={sectionDivider}></span>
	</div>
	<div class="grid grid-cols-1 sm:grid-cols-3 gap-3 mb-8">
		{#each [
			{ label: 'Head Correct', rate: stats.head_correct_rate },
			{ label: 'Target Correct', rate: stats.target_correct_rate },
			{ label: 'Source Correct', rate: stats.source_correct_rate }
		] as item}
			{@const tier = healthTier(item.rate)}
			<div class={cardSurface}>
				<div class="text-xs text-gray-400 uppercase">{item.label}</div>
				<div class="mt-1 text-2xl font-bold tabular-nums {tierText[tier]}">{pct(item.rate)}</div>
				<div class="mt-2 h-1.5 w-full bg-gray-800 rounded-full overflow-hidden"><div class="h-full {tierBar[tier]} rounded-full transition-[width]" style:width={pct(item.rate)}></div></div>
			</div>
		{/each}
	</div>

	<!-- Participation -->
	<div class="mb-2 flex items-center gap-2">
		<h2 class={sectionHeader}>Participation</h2>
		<span class={sectionDivider}></span>
	</div>
	<div class="grid grid-cols-1 sm:grid-cols-3 gap-3 mb-8">
		<div class={cardSurface}>
			<div class="text-xs text-gray-400 uppercase">Proposals</div>
			<div class="mt-1 text-2xl font-bold tabular-nums">{stats.total_proposals}</div>
			{#if stats.total_proposals_missed > 0}
				<div class="text-xs text-red-400 mt-1">{stats.total_proposals_missed} missed</div>
			{:else if stats.total_proposals > 0}
				<div class="text-xs text-green-400 mt-1">None missed</div>
			{:else}
				<div class="text-xs text-gray-500 mt-1">No duties yet</div>
			{/if}
		</div>
		<div class={cardSurface}>
			<div class="text-xs text-gray-400 uppercase">Sync Committee</div>
			{#if syncTotal > 0}
				<div class="mt-1 text-2xl font-bold tabular-nums {tierText[syncTier]}">{pct(syncRateVal)}</div>
				<div class="text-xs text-gray-500 tabular-nums">{stats.total_sync_participated.toLocaleString()} / {syncTotal.toLocaleString()}</div>
				{#if stats.total_sync_missed > 0}
					<div class="mt-2 h-1.5 w-full bg-gray-800 rounded-full overflow-hidden"><div class="h-full {tierBar[syncTier]} rounded-full transition-[width]" style:width={pct(syncRateVal)}></div></div>
				{/if}
			{:else}
				<div class="mt-1 text-2xl font-bold text-gray-600">–</div>
				<div class="text-xs text-gray-500">No duties yet</div>
			{/if}
		</div>
		<div class={cardSurface}>
			<div class="text-xs text-gray-400 uppercase">Avg Effective Inclusion Delay</div>
			{#if stats.avg_effective_inclusion_delay != null}
				{@const delay = stats.avg_effective_inclusion_delay}
				{@const delayTier = delay <= 1.15 ? 'good' : delay <= 1.5 ? 'warn' : 'bad'}
				{@const tooltip = stats.avg_inclusion_delay != null
					? `Average slot delay, ignoring empty proposer slots. Without that adjustment the slot gap averages ${stats.avg_inclusion_delay.toFixed(4)}.`
					: 'Average slot delay, ignoring empty proposer slots.'}
				<div class="mt-1 text-2xl font-bold tabular-nums {tierText[delayTier]} cursor-help" title={tooltip}>{delay.toFixed(4)}<span class="text-sm font-medium text-gray-500 ml-1">slots</span></div>
				<div class="text-xs text-gray-500 mt-1">Lower is better · 1.00 = included at earliest available slot
					{#if stats.avg_inclusion_delay != null}
						<span class="text-gray-600"> · slot gap {stats.avg_inclusion_delay.toFixed(4)}</span>
					{/if}
				</div>
			{:else}
				<div class="mt-1 text-2xl font-bold text-gray-600">–</div>
				<div class="text-xs text-gray-500 mt-1">No included attestations yet</div>
			{/if}
		</div>
	</div>

	<!-- Validators section header -->
	<div class="mb-3 flex items-center gap-2">
		<h2 class={sectionHeader}>Validators</h2>
		<span class={sectionDivider}></span>
	</div>

	<!-- Validators table -->
	<div class={tableSurface}>
		<table class="w-full text-sm">
			<thead class={tableHeaderRow}>
				<tr>
					<th class="px-2 py-2"><span class="sr-only">Health</span></th>
					<th class="px-3 py-2 text-left">Index</th>
					<th class="px-3 py-2 text-left">Tags</th>
					<th class="px-3 py-2 text-left hidden md:table-cell">Pubkey</th>
					<th class="px-3 py-2 text-right">Att Rate</th>
					<th class="px-3 py-2 text-right">Head</th>
					<th class="px-3 py-2 text-right">Target</th>
					<th class="px-3 py-2 text-right">Source</th>
					<th class="px-3 py-2 text-right">Missed</th>
					<th class="px-3 py-2 text-right">Proposals</th>
					<th class="px-3 py-2 text-right">Sync</th>
					<th class="px-3 py-2 text-right">Status</th>
					<th class="px-3 py-2 text-right">Scanned To</th>
				</tr>
			</thead>
			<tbody>
				{#each validators as v}
					{@const vTier = healthTier(v.attestation_rate)}
					<tr class={tableBodyRow}>
						<td class="px-2 py-2"><span class="inline-block w-2 h-2 rounded-full {tierDot[vTier]}" title="Attestation rate {pct(v.attestation_rate)}"></span></td>
						<td class="px-3 py-2 font-mono"><a href={beaconchainUrl(v.validator_index)} target="_blank" rel="noopener" class="text-blue-400 hover:underline">{v.validator_index}</a></td>
						<td class="px-3 py-2">{#each getTags(v.validator_index) as tag}<span class="inline-block text-[10px] px-1.5 py-0.5 rounded bg-purple-900/40 text-purple-300 mr-1">{tag}</span>{/each}</td>
						<td class="px-3 py-2 font-mono text-xs text-gray-400 hidden md:table-cell"><a href={beaconchainUrl(v.validator_index)} target="_blank" rel="noopener" class="hover:text-blue-400">{v.pubkey.slice(0, 10)}...{v.pubkey.slice(-6)}</a></td>
						<td class="px-3 py-2 text-right" class:text-green-400={v.attestation_rate > 0.99} class:text-yellow-400={v.attestation_rate <= 0.99 && v.attestation_rate > 0.95} class:text-red-400={v.attestation_rate <= 0.95}>{pct(v.attestation_rate)}</td>
						<td class="px-3 py-2 text-right">{pct(v.head_correct_rate)}</td>
						<td class="px-3 py-2 text-right">{pct(v.target_correct_rate)}</td>
						<td class="px-3 py-2 text-right">{pct(v.source_correct_rate)}</td>
						<td class="px-3 py-2 text-right" class:text-red-400={v.missed_attestations > 0}>{v.missed_attestations}</td>
						<td class="px-3 py-2 text-right">{v.total_proposals}{#if v.missed_proposals > 0} <span class="text-red-400">({v.missed_proposals} missed)</span>{/if}</td>
						<td class="px-3 py-2 text-right">{v.sync_participated}{#if v.sync_missed > 0} <span class="text-red-400">/{v.sync_missed}</span>{/if}</td>
						<td class="px-3 py-2 text-right">
							{#if v.exit_epoch == null}
								<span class="inline-block text-[10px] px-1.5 py-0.5 rounded bg-green-900/40 text-green-300">Active</span>
							{:else}
								<span class="inline-block text-[10px] px-1.5 py-0.5 rounded bg-gray-800 text-gray-400" title="exit_epoch {v.exit_epoch}">Exited</span>
							{/if}
						</td>
						<td class="px-3 py-2 text-right text-gray-400" title={v.last_scanned_epoch ? epochTime(v.last_scanned_epoch) : ''}>{v.last_scanned_epoch ?? '-'}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
{/if}
