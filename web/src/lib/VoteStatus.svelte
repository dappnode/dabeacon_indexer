<script lang="ts">
	let { correct, included, finalized }: {
		correct: boolean | null;
		included: boolean | null;
		finalized: boolean;
	} = $props();
</script>

{#if included === false}
	<span class="text-gray-500" title="No attestation was included, so there is no vote to compare.">N/A</span>
{:else if correct === null}
	<span class="text-amber-300" title="Vote comparison is unavailable. This does not mean the vote was wrong or skipped.">{finalized ? 'Unknown' : 'Pending'}</span>
{:else if correct}
	<span class="text-green-400" title="The vote matches the chain. Reward eligibility also depends on inclusion timing.">OK</span>
{:else}
	<span class="text-red-400" title="The included vote does not match the canonical chain.">WRONG</span>
{/if}
