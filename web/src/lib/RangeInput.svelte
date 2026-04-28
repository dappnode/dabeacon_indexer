<script lang="ts">
	interface Props {
		label: string;
		from: string;
		to: string;
		onChange: () => void;
		fromPlaceholder?: string;
		toPlaceholder?: string;
	}

	let {
		label,
		from = $bindable(),
		to = $bindable(),
		onChange,
		fromPlaceholder = 'from',
		toPlaceholder = 'to'
	}: Props = $props();

	const hasValue = $derived(from !== '' || to !== '');

	function clear() {
		from = '';
		to = '';
		onChange();
	}
</script>

<div class="max-w-sm">
	<div class="flex items-center justify-between mb-1 min-h-[14px]">
		<span class="text-xs text-gray-500">{label}</span>
		{#if hasValue}
			<button
				onclick={clear}
				class="text-[10px] text-gray-500 hover:text-gray-200 transition-colors"
				title="Clear range"
			>Clear</button>
		{/if}
	</div>
	<div class="flex items-center rounded-md bg-gray-800/60 ring-1 ring-gray-700/60 focus-within:ring-blue-400/50 transition-colors">
		<input
			bind:value={from}
			onchange={onChange}
			type="number"
			inputmode="numeric"
			placeholder={fromPlaceholder}
			aria-label="{label} from"
			class="range-input w-full min-w-0 bg-transparent px-2 py-1 text-sm text-gray-100 placeholder:text-gray-600 outline-none"
		/>
		<span class="text-gray-600 text-xs px-1 select-none" aria-hidden="true">→</span>
		<input
			bind:value={to}
			onchange={onChange}
			type="number"
			inputmode="numeric"
			placeholder={toPlaceholder}
			aria-label="{label} to"
			class="range-input w-full min-w-0 bg-transparent px-2 py-1 text-sm text-gray-100 placeholder:text-gray-600 outline-none"
		/>
	</div>
</div>

<style>
	/* Strip the native number spinners — they don't match the custom styling
	   and are clumsy on desktop. Mobile still gets the numeric keyboard via
	   inputmode. */
	.range-input::-webkit-outer-spin-button,
	.range-input::-webkit-inner-spin-button {
		-webkit-appearance: none;
		margin: 0;
	}
	.range-input {
		-moz-appearance: textfield;
		appearance: textfield;
	}
</style>
