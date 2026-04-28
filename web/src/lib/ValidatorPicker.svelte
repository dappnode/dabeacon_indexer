<!--
  Clickable toggle chips for validator selection with tag grouping.
  All validators are selected by default.
-->
<script lang="ts">
	import type { ValidatorSummary, ValidatorMetaEntry } from '$lib/api';

	let {
		validators,
		meta = {},
		allTags = [],
		value = $bindable(''),
		label = 'Validators',
		onChange,
	}: {
		validators: ValidatorSummary[];
		meta?: Record<string, ValidatorMetaEntry>;
		allTags?: string[];
		value: string;
		label?: string;
		onChange?: () => void;
	} = $props();

	let selected: Set<number> = $state(new Set());
	let activeTag = $state('');

	$effect(() => {
		if (validators.length > 0 && selected.size === 0) {
			selected = new Set(validators.map(v => v.validator_index));
			syncValue();
		}
	});

	function syncValue() {
		const prev = value;
		if (selected.size === validators.length) {
			value = '';
		} else {
			value = [...selected].sort((a, b) => a - b).join(',');
		}
		return prev !== value;
	}

	function syncAndNotify() {
		if (syncValue()) {
			onChange?.();
		}
	}

	function handleChipClick(e: MouseEvent, idx: number) {
		activeTag = '';
		// Modifier-click (⌘ / Ctrl / Alt) isolates: keep only the clicked
		// chip and deselect everything else. Plain click uses the toggle
		// rules below.
		if (e.metaKey || e.ctrlKey || e.altKey) {
			selected = new Set([idx]);
			syncAndNotify();
			return;
		}
		toggle(idx);
	}

	function toggle(idx: number) {
		// From the "all selected" state, clicking a chip just deselects that
		// one (set becomes "all except idx").
		if (isAll()) {
			const next = new Set(selected);
			next.delete(idx);
			selected = next;
			syncAndNotify();
			return;
		}
		// From the "exactly one selected" state, clicking that same chip
		// re-selects everything — there is no empty/None state to fall into.
		if (selected.size === 1 && selected.has(idx)) {
			selectAll();
			return;
		}
		const next = new Set(selected);
		if (next.has(idx)) {
			next.delete(idx);
		} else {
			next.add(idx);
		}
		selected = next;
		syncAndNotify();
	}

	function selectAll() {
		selected = new Set(validators.map(v => v.validator_index));
		activeTag = '';
		syncAndNotify();
	}

	function selectByTag(tag: string) {
		const matching = validators.filter(v => {
			const m = meta[String(v.validator_index)];
			return m && m.tags.includes(tag);
		});
		if (matching.length === 0) return;
		selected = new Set(matching.map(v => v.validator_index));
		activeTag = tag;
		syncAndNotify();
	}

	function isAll(): boolean {
		return selected.size === validators.length;
	}

	function getTags(idx: number): string[] {
		const m = meta[String(idx)];
		return m?.tags || [];
	}
</script>

<div>
	<div class="flex items-center gap-2 mb-1.5 flex-wrap">
		<span class="text-xs text-gray-500">{label}</span>
		<button
			onclick={selectAll}
			class="text-[10px] px-2 py-0.5 rounded-md ring-1 transition-colors {isAll() && !activeTag ? 'bg-blue-500/20 text-blue-200 ring-blue-400/40' : 'bg-gray-800/60 hover:bg-gray-800 text-gray-400 ring-gray-700/50'}"
		>All</button>
		{#if allTags.length > 0}
			<span class="text-[10px] text-gray-600 ml-1">|</span>
			{#each allTags as tag}
				<button
					onclick={() => selectByTag(tag)}
					class="text-[10px] px-2 py-0.5 rounded-md ring-1 transition-colors {activeTag === tag
						? 'bg-purple-500/20 text-purple-200 ring-purple-400/40'
						: 'bg-gray-800/60 hover:bg-gray-800 text-purple-300/80 ring-gray-700/50'}"
				>{tag}</button>
			{/each}
		{/if}
		{#if !isAll()}
			<span class="text-[10px] text-blue-300 ml-1">{selected.size}/{validators.length}</span>
		{/if}
	</div>
	<div class="flex flex-wrap gap-1">
		{#each validators as v}
			{@const vTags = getTags(v.validator_index)}
			<button
				onclick={(e) => handleChipClick(e, v.validator_index)}
				class="px-2 py-0.5 rounded-md text-xs font-mono ring-1 transition-colors {selected.has(v.validator_index)
					? 'bg-blue-500/20 text-blue-200 ring-blue-400/40'
					: 'bg-gray-800/60 text-gray-500 ring-gray-700/50 hover:bg-gray-800 hover:text-gray-300'}"
				title="{v.validator_index}{vTags.length ? ` [${vTags.join(', ')}]` : ''} — ⌘/Ctrl/Alt-click to isolate"
			>
				{v.validator_index}
			</button>
		{/each}
	</div>
</div>
