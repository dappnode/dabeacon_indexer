// Shared visual tokens — keep page styles consistent without duplicating
// gradient/ring strings all over. Import from `$lib/ui`.

export const cardSurface =
	'bg-gradient-to-br from-gray-900 to-gray-900/60 ring-1 ring-gray-800/80 rounded-lg p-4 shadow-sm';

export const tableSurface =
	'overflow-x-auto rounded-lg ring-1 ring-gray-800/80';

export const tableHeaderRow =
	'bg-gray-900/80 text-gray-400 uppercase text-xs';

export const tableBodyRow =
	'border-t border-gray-800/70 hover:bg-gray-900/60 transition-colors';

// Performance-tier utilities. 99%+ = good, 95%+ = warn, otherwise bad.
export type HealthTier = 'good' | 'warn' | 'bad';

export function healthTier(rate: number): HealthTier {
	if (rate >= 0.99) return 'good';
	if (rate >= 0.95) return 'warn';
	return 'bad';
}

export const tierText: Record<HealthTier, string> = {
	good: 'text-green-400',
	warn: 'text-yellow-400',
	bad: 'text-red-400'
};

export const tierBar: Record<HealthTier, string> = {
	good: 'bg-green-400',
	warn: 'bg-yellow-400',
	bad: 'bg-red-400'
};

export const tierDot: Record<HealthTier, string> = {
	good: 'bg-green-400 shadow-[0_0_12px_rgba(74,222,128,0.5)]',
	warn: 'bg-yellow-400 shadow-[0_0_12px_rgba(250,204,21,0.45)]',
	bad: 'bg-red-400 shadow-[0_0_12px_rgba(248,113,113,0.5)]'
};

/**
 * Section header with a thin divider line. Usage in a parent flex container:
 *   <h2 class={sectionHeader}>Title</h2><span class={sectionDivider}></span>
 * Keep them together in the same wrapper div for correct layout.
 */
export const sectionHeader =
	'text-[11px] uppercase tracking-wider text-gray-500 font-semibold';
export const sectionDivider = 'h-px flex-1 bg-gray-800';

// --- Buttons ---------------------------------------------------------------
// Soft tinted buttons: `bg-<tint>/10` + `ring-1 ring-<tint>/30` for a modern
// layered look. All share the same base spacing + transition so they align in
// button rows. Keep class strings literal so Tailwind can discover them.
const btnBase =
	'px-3 py-1.5 text-sm rounded-md transition-colors font-medium';

export const btnNeutral =
	`${btnBase} bg-gray-800/60 hover:bg-gray-800 ring-1 ring-gray-700/60 text-gray-300`;
export const btnDanger =
	`${btnBase} bg-red-500/10 hover:bg-red-500/20 ring-1 ring-red-500/30 text-red-300`;
export const btnWarn =
	`${btnBase} bg-yellow-500/10 hover:bg-yellow-500/20 ring-1 ring-yellow-500/30 text-yellow-300`;
export const btnAmber =
	`${btnBase} bg-amber-500/10 hover:bg-amber-500/20 ring-1 ring-amber-500/30 text-amber-300`;
export const btnOrange =
	`${btnBase} bg-orange-500/10 hover:bg-orange-500/20 ring-1 ring-orange-500/30 text-orange-300`;

// Segmented tri-state (All / Yes / No). Idle vs active style for each cell.
const segBase =
	'flex-1 px-2 py-1 text-xs rounded-md transition-colors';
export const segIdle =
	`${segBase} bg-gray-800/60 hover:bg-gray-800 ring-1 ring-gray-700/50 text-gray-400`;
export const segActive =
	`${segBase} bg-blue-500/20 text-blue-200 ring-1 ring-blue-400/40`;

// Tabs (Rewards time-window selector etc.) — same soft-accent idiom at a
// larger size than segmented controls.
const tabBase =
	'px-5 py-2 text-sm rounded-md font-medium transition-colors';
export const tabIdle =
	`${tabBase} bg-gray-800/60 hover:bg-gray-800 ring-1 ring-gray-700/50 text-gray-400 hover:text-gray-200`;
export const tabActive =
	`${tabBase} bg-blue-500/20 text-blue-200 ring-1 ring-blue-400/40`;

// Small tag-filter pills.
const tagBase = 'text-[10px] px-2 py-1 rounded-md transition-colors';
export const tagIdle =
	`${tagBase} bg-gray-800/60 hover:bg-gray-800 ring-1 ring-gray-700/50 text-gray-400`;
export const tagActive =
	`${tagBase} bg-blue-500/20 text-blue-200 ring-1 ring-blue-400/40`;
