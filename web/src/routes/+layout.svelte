<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { getAuthInfo, checkAuth, setApiKey, type AuthInfo } from '$lib/api';

	let { children } = $props();

	let authInfo: AuthInfo | null = $state(null);
	let authenticated = $state(false);
	let loading = $state(true);
	let keyInput = $state('');
	let authError = $state(false);
	let mobileMenuOpen = $state(false);

	const navItems: { href: string; label: string }[] = [
		{ href: '/', label: 'Dashboard' },
		{ href: '/live', label: 'Live' },
		{ href: '/attestations', label: 'Attestations' },
		{ href: '/sync', label: 'Sync Committee' },
		{ href: '/proposals', label: 'Proposals' },
		{ href: '/epochs', label: 'Epochs' },
		{ href: '/rewards', label: 'Rewards' }
	];

	function isActive(href: string): boolean {
		const path = page.url.pathname;
		if (href === '/') return path === '/';
		return path === href || path.startsWith(href + '/');
	}

	async function handleLogin() {
		authError = false;
		setApiKey(keyInput);
		if (await checkAuth()) {
			authenticated = true;
		} else {
			authError = true;
			setApiKey('');
		}
	}

	function handleLogout() {
		setApiKey('');
		keyInput = '';
		authError = false;
		if (authInfo?.auth_required) {
			authenticated = false;
		}
	}

	function closeMobileMenu() {
		mobileMenuOpen = false;
	}

	onMount(async () => {
		authInfo = await getAuthInfo();
		if (!authInfo.auth_required) {
			authenticated = true;
		} else {
			authenticated = await checkAuth();
		}
		loading = false;
	});
</script>

<div class="min-h-screen bg-gray-950 text-gray-100">
	{#if loading}
		<div class="flex items-center justify-center h-screen">
			<p class="text-gray-400">Loading...</p>
		</div>
	{:else if !authenticated}
		<!-- Auth modal -->
		<div class="flex items-center justify-center h-screen">
			<div class="bg-gray-900 border border-gray-800 rounded-xl p-8 w-96 shadow-2xl">
				<h1 class="text-xl font-bold mb-1">Beacon Indexer</h1>
				<p class="text-gray-400 text-sm mb-6">Enter API key to continue</p>
				<form onsubmit={(e) => { e.preventDefault(); handleLogin(); }}>
					<input
						bind:value={keyInput}
						type="password"
						placeholder="API Key"
						class="w-full bg-gray-800 border border-gray-700 rounded-lg px-4 py-2.5 mb-3 text-sm focus:outline-none focus:border-blue-500"
						autofocus
					/>
					{#if authError}
						<p class="text-red-400 text-sm mb-3">Invalid API key</p>
					{/if}
					<button
						type="submit"
						class="w-full bg-blue-600 hover:bg-blue-500 text-white rounded-lg px-4 py-2.5 text-sm font-medium transition-colors"
					>Login</button>
				</form>
			</div>
		</div>
	{:else}
		<nav class="sticky top-0 z-40 backdrop-blur-md bg-gray-950/75 border-b border-gray-800/70 px-4 md:px-6">
			<div class="flex items-center h-14 gap-3">
				<a href="/" class="flex items-center gap-2 shrink-0 group" onclick={closeMobileMenu}>
					<img
						src="/dappnode-logo-only.png"
						alt="DAppNode"
						class="w-7 h-7 shrink-0 transition group-hover:opacity-90"
					/>
					<span class="text-lg font-bold tracking-tight bg-gradient-to-r from-blue-300 via-sky-300 to-cyan-300 bg-clip-text text-transparent">Beacon Indexer</span>
				</a>
				<div class="hidden md:flex md:items-center md:gap-1 md:ml-4">
					{#each navItems as item}
						<a
							href={item.href}
							onclick={closeMobileMenu}
							class={`px-3 py-1.5 rounded-md text-sm transition-colors ${isActive(item.href) ? 'bg-blue-500/15 text-blue-300 ring-1 ring-blue-400/25' : 'text-gray-300 hover:text-blue-300 hover:bg-gray-800/70'}`}
						>{item.label}</a>
					{/each}
				</div>
				<div class="ml-auto flex items-center gap-2">
					{#if authInfo?.auth_required}
						<button
							onclick={handleLogout}
							class="px-3 py-1.5 text-sm rounded-md bg-gray-800/70 hover:bg-gray-700 border border-gray-700/60 text-gray-300 transition-colors"
						>Logout</button>
					{/if}
					<button
						type="button"
						class="md:hidden p-1.5 rounded-md bg-gray-800/70 hover:bg-gray-700 border border-gray-700/60 text-gray-300 transition-colors"
						onclick={() => (mobileMenuOpen = !mobileMenuOpen)}
						aria-expanded={mobileMenuOpen}
						aria-label="Toggle navigation menu"
					>
						{#if mobileMenuOpen}
							<svg class="w-5 h-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 6l12 12M6 18L18 6"/></svg>
						{:else}
							<svg class="w-5 h-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 7h16M4 12h16M4 17h16"/></svg>
						{/if}
					</button>
				</div>
			</div>
			{#if mobileMenuOpen}
				<div class="md:hidden pb-3 flex flex-col gap-0.5">
					{#each navItems as item}
						<a
							href={item.href}
							onclick={closeMobileMenu}
							class={`px-3 py-2 rounded-md text-sm transition-colors ${isActive(item.href) ? 'bg-blue-500/15 text-blue-300' : 'text-gray-300 hover:text-blue-300 hover:bg-gray-800/70'}`}
						>{item.label}</a>
					{/each}
				</div>
			{/if}
		</nav>
		<main class="p-4 md:p-6">
			{@render children()}
		</main>
	{/if}
</div>
