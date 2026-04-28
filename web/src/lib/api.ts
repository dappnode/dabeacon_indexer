const BASE = '/api';

let chainConfig = {
	genesis_time: 1606824023,
	seconds_per_slot: 12,
	slots_per_epoch: 32,
};

export function getApiKey(): string {
	return localStorage.getItem('api_key') || '';
}

export function setApiKey(key: string) {
	localStorage.setItem('api_key', key);
}

export async function fetchJson<T>(path: string, params?: Record<string, string | number | boolean | undefined>): Promise<T> {
	const url = new URL(path, window.location.origin);
	if (params) {
		for (const [k, v] of Object.entries(params)) {
			if (v !== undefined && v !== '') url.searchParams.set(k, String(v));
		}
	}
	const headers: Record<string, string> = {};
	const key = getApiKey();
	if (key) headers['Authorization'] = `Bearer ${key}`;
	const res = await fetch(url.toString(), { headers });
	if (res.status === 401) throw new AuthError();
	if (!res.ok) throw new Error(`API error ${res.status}`);
	return res.json();
}

export class AuthError extends Error {
	constructor() { super('Unauthorized'); this.name = 'AuthError'; }
}

export interface AuthInfo {
	auth_required: boolean;
}

export async function getAuthInfo(): Promise<AuthInfo> {
	const res = await fetch(`${BASE}/auth-info`);
	return res.json();
}

export async function checkAuth(): Promise<boolean> {
	const key = getApiKey();
	if (!key) return false;
	const res = await fetch(`${BASE}/auth-check`, {
		headers: { 'Authorization': `Bearer ${key}` }
	});
	return res.ok;
}

export interface ValidatorMetaEntry {
	tags: string[];
}

export interface ChainInfo {
	slots_per_epoch: number;
	seconds_per_slot: number;
	genesis_time: number;
}

export interface MetaResponse {
	validators: Record<string, ValidatorMetaEntry>;
	all_tags: string[];
	chain?: ChainInfo;
}

export async function getMeta(): Promise<MetaResponse> {
	const meta = await fetchJson<MetaResponse>(`${BASE}/meta`);
	if (meta.chain) {
		chainConfig = meta.chain;
	}
	return meta;
}

export function getChainConfig(): ChainInfo {
	return chainConfig;
}

export function slotsPerEpoch(): number {
	return chainConfig.slots_per_epoch;
}

/**
 * Current beacon-chain epoch derived from wall-clock + chain config.
 * Used for active/exiting/exited classification of validators in the UI;
 * mirrors the server-side `active_validators_at` SQL predicate
 * (`exit_epoch > current_epoch`).
 */
export function currentEpoch(): number {
	const now = Math.floor(Date.now() / 1000);
	const slot = Math.floor((now - chainConfig.genesis_time) / chainConfig.seconds_per_slot);
	return Math.floor(slot / chainConfig.slots_per_epoch);
}

/**
 * Validator lifecycle status derived from `exit_epoch`:
 *  - `active`   : exit_epoch is null (still active indefinitely)
 *  - `exiting`  : exit_epoch is set but not yet reached — still attesting
 *  - `exited`   : exit_epoch <= current_epoch — no longer attesting
 */
export type ValidatorStatus = 'active' | 'exiting' | 'exited';
export function validatorStatus(exit_epoch: number | null): ValidatorStatus {
	if (exit_epoch == null) return 'active';
	return exit_epoch > currentEpoch() ? 'exiting' : 'exited';
}

export interface Stats {
	total_validators: number;
	total_epochs_scanned: number;
	attestation_rate: number;
	head_correct_rate: number;
	target_correct_rate: number;
	source_correct_rate: number;
	total_attestations: number;
	total_missed: number;
	avg_inclusion_delay: number | null;
	avg_effective_inclusion_delay: number | null;
	total_proposals: number;
	total_proposals_missed: number;
	total_sync_participated: number;
	total_sync_missed: number;
	latest_scanned_epoch: number | null;
	earliest_scanned_epoch: number | null;
}

export interface ValidatorSummary {
	validator_index: number;
	pubkey: string;
	activation_epoch: number;
	exit_epoch: number | null;
	last_scanned_epoch: number | null;
	total_attestations: number;
	missed_attestations: number;
	attestation_rate: number;
	head_correct_rate: number;
	target_correct_rate: number;
	source_correct_rate: number;
	total_proposals: number;
	missed_proposals: number;
	sync_participated: number;
	sync_missed: number;
}

export interface Paginated<T> {
	data: T[];
	total: number;
	page: number;
	per_page: number;
}

export interface AttestationRow {
	validator_index: number;
	epoch: number;
	assigned_slot: number;
	committee_index: number;
	committee_position: number;
	included: boolean;
	inclusion_slot: number | null;
	inclusion_delay: number | null;
	effective_inclusion_delay: number | null;
	source_correct: boolean | null;
	target_correct: boolean | null;
	head_correct: boolean | null;
	source_reward: number | null;
	target_reward: number | null;
	head_reward: number | null;
	inactivity_penalty: number | null;
	total_reward: number | null;
	finalized: boolean;
}

export interface SyncRow {
	validator_index: number;
	slot: number;
	epoch: number;
	participated: boolean;
	reward: number | null;
	missed_block: boolean;
	finalized: boolean;
}

export interface ProposalRow {
	slot: number;
	epoch: number;
	proposer_index: number;
	proposed: boolean;
	reward_total: number | null;
	reward_attestations: number | null;
	reward_sync: number | null;
	reward_slashings: number | null;
	finalized: boolean;
}

export interface EpochRow {
	epoch: number;
	total_duties: number;
	included: number;
	missed: number;
	attestation_rate: number;
	head_correct: number;
	target_correct: number;
	source_correct: number;
	total_reward: number;
	sync_participated: number;
	sync_missed: number;
	proposals: number;
	proposals_missed: number;
}

export const getStats = () => fetchJson<Stats>(`${BASE}/stats`);
export const getValidators = () => fetchJson<ValidatorSummary[]>(`${BASE}/validators`);

export const getAttestations = (params: Record<string, string | number | boolean | undefined>) =>
	fetchJson<Paginated<AttestationRow>>(`${BASE}/attestations`, params);

export const getSyncDuties = (params: Record<string, string | number | boolean | undefined>) =>
	fetchJson<Paginated<SyncRow>>(`${BASE}/sync-duties`, params);

export const getProposals = (params: Record<string, string | number | boolean | undefined>) =>
	fetchJson<Paginated<ProposalRow>>(`${BASE}/proposals`, params);

export const getEpochs = (params: Record<string, string | number | boolean | undefined>) =>
	fetchJson<Paginated<EpochRow>>(`${BASE}/epochs`, params);

export interface RewardWindow {
	attestation_reward: number;
	sync_reward: number;
	proposal_reward: number;
	total: number;
	epochs: number;
}

export interface ValidatorRewards {
	validator_index: number;
	day_1: RewardWindow;
	day_7: RewardWindow;
	day_30: RewardWindow;
	all_time: RewardWindow;
}

export interface RewardsResponse {
	validators: ValidatorRewards[];
	totals: {
		day_1: RewardWindow;
		day_7: RewardWindow;
		day_30: RewardWindow;
		all_time: RewardWindow;
	};
	latest_epoch: number;
}

export const getRewards = () => fetchJson<RewardsResponse>(`${BASE}/rewards`);

export function pct(n: number): string {
	return (n * 100).toFixed(2) + '%';
}

/** Format a gwei amount as ETH with fixed decimals. No suffix — callers add "ETH"
 * once in the column header or inline where needed. */
export function eth(n: number | null, decimals = 6): string {
	if (n === null) return '-';
	return (n / 1_000_000_000).toFixed(decimals);
}

/** Convert a slot number to a UTC timestamp string */
export function slotTime(slot: number): string {
	const ts = chainConfig.genesis_time + slot * chainConfig.seconds_per_slot;
	return new Date(ts * 1000).toISOString().replace('T', ' ').replace('.000Z', ' UTC');
}

/** Convert an epoch number to a UTC timestamp string (start of epoch) */
export function epochTime(epoch: number): string {
	return slotTime(epoch * chainConfig.slots_per_epoch);
}

/** Shorter relative time: "2m ago", "3h ago", "5d ago" */
export function timeAgo(slot: number): string {
	const ts = chainConfig.genesis_time + slot * chainConfig.seconds_per_slot;
	const diff = Math.floor(Date.now() / 1000) - ts;
	return `${formatDuration(diff)} ago`;
}

/** Format a duration in seconds as "5m", "2h", "3d" */
export function formatDuration(seconds: number): string {
	if (seconds < 60) return `${seconds}s`;
	if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
	if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`;
	return `${Math.floor(seconds / 86400)}d`;
}

/** Format time since a given Date: "5m ago", "2h ago", "3d ago" */
export function timeSince(date: Date): string {
	const diff = Math.floor(Date.now() / 1000) - Math.floor(date.getTime() / 1000);
	return `${formatDuration(diff)} ago`;
}

export function beaconchainUrl(validatorIndex: number): string {
	return `https://beaconcha.in/validator/${validatorIndex}`;
}

export function beaconchainSlotUrl(slot: number): string {
	return `https://beaconcha.in/slot/${slot}`;
}
