/** Group (ReBAC) endpoints — ported from model/groups.js. */
import { apiFetch, apiJson } from '$lib/api/client';
import { getCsrfHeaders } from '$lib/api/csrf';
import { t } from '$lib/i18n/index.svelte';

const JSON_HEADERS = { 'Content-Type': 'application/json' };
const enc = encodeURIComponent;

/**
 * Well-known UUID of the predefined "Internal" virtual group (matches the
 * Rust constant `INTERNAL_GROUP_ID` in `src/domain/entities/subject_group.rs`
 * and the legacy `model/groups.js`).
 */
export const INTERNAL_GROUP_ID = '00000000-0000-0000-0000-000000000001';

/**
 * Map of well-known virtual-group UUIDs → i18n key for the human-readable
 * display name. Anything not in this map falls back to `group.name`. Ported
 * from `components/groupDisplay.js`.
 */
const VIRTUAL_NAME_KEYS: Record<string, string> = {
	[INTERNAL_GROUP_ID]: 'groups.virtual_internal_name'
};

/**
 * Map of well-known virtual-group UUIDs → i18n key for a human-readable
 * description. Virtual groups are server-seeded and their `description` column
 * holds developer/schema notes (e.g. the Internal group's "…no rows in
 * subject_group_members."), which must never reach end users — so virtual
 * groups display a localized blurb instead of their raw `description`.
 */
const VIRTUAL_DESC_KEYS: Record<string, string> = {
	[INTERNAL_GROUP_ID]: 'groups.virtual_internal_explanation'
};

export interface GroupItem {
	id: string;
	name: string;
	description?: string | null;
	member_count?: number;
	is_virtual?: boolean;
	can_manage?: boolean;
}

/** The members endpoint returns a tagged union: `{ kind: 'user' | 'group', id }`. */
export interface GroupMember {
	kind: 'user' | 'group';
	id: string;
}

async function mutate(url: string, method: string, body?: unknown): Promise<void> {
	const res = await apiFetch(url, {
		method,
		credentials: 'same-origin',
		headers: { ...JSON_HEADERS, ...getCsrfHeaders() },
		body: body === undefined ? undefined : JSON.stringify(body)
	});
	if (!res.ok) throw new Error(`${method} ${url} failed: ${res.status}`);
}

/** A single page of groups plus the server-reported total (for "Load more"). */
export interface GroupPage {
	items: GroupItem[];
	total: number;
}

/**
 * Fetch one page of groups. The list endpoint may return an array or
 * `{ groups | items, total }`. When no total is provided we fall back to the
 * page length so pagination collapses gracefully to a single page.
 */
export async function listGroupsPage(limit = 50, offset = 0, q?: string): Promise<GroupPage> {
	const params = new URLSearchParams({ limit: String(limit), offset: String(offset) });
	if (q) params.set('q', q);
	const data = await apiJson<
		GroupItem[] | { groups?: GroupItem[]; items?: GroupItem[]; total?: number }
	>(`/api/groups?${params}`, { credentials: 'same-origin' });
	if (Array.isArray(data)) return { items: data, total: offset + data.length };
	const items = data.groups ?? data.items ?? [];
	return { items, total: data.total ?? offset + items.length };
}

/** Convenience wrapper returning just the items of the first page. */
export async function listGroups(limit = 50, offset = 0, q?: string): Promise<GroupItem[]> {
	return (await listGroupsPage(limit, offset, q)).items;
}

/**
 * Human-readable display name for a group. Virtual groups get a translated
 * label via the well-known UUID mapping; user-defined groups display their
 * raw name. Ported from `components/groupDisplay.js`.
 */
export function groupDisplayName(group: GroupItem): string {
	if (group.is_virtual) {
		const key = VIRTUAL_NAME_KEYS[group.id];
		if (key) return t(key, group.name);
	}
	return group.name;
}

/**
 * Human-readable description for a group row. Virtual groups render a localized
 * blurb (via the well-known UUID mapping) so their internal DB schema notes
 * never leak to the UI; a virtual group without a mapped key shows nothing.
 * User-defined groups display their raw `description` verbatim.
 */
export function groupDescription(group: GroupItem): string | null {
	if (group.is_virtual) {
		const key = VIRTUAL_DESC_KEYS[group.id];
		return key ? t(key, group.name) : null;
	}
	return group.description ?? null;
}

/**
 * Pick the icon registry name for a group avatar. Virtual (system-wide)
 * groups use `people-roof`; user-defined groups use `user-group`. Ported from
 * `components/groupDisplay.js`.
 */
export function groupIconName(group: Pick<GroupItem, 'is_virtual'>): string {
	return group.is_virtual ? 'people-roof' : 'user-group';
}

export function createGroup(name: string, description?: string | null): Promise<void> {
	return mutate('/api/groups', 'POST', { name, description: description ?? null });
}

export function renameGroup(id: string, name: string): Promise<void> {
	return mutate(`/api/groups/${enc(id)}`, 'PATCH', { name });
}

export function deleteGroup(id: string): Promise<void> {
	return mutate(`/api/groups/${enc(id)}`, 'DELETE');
}

export function listMembers(id: string): Promise<GroupMember[]> {
	return apiJson<GroupMember[]>(`/api/groups/${enc(id)}/members`, { credentials: 'same-origin' });
}

export function addUserMember(groupId: string, userId: string): Promise<void> {
	return mutate(`/api/groups/${enc(groupId)}/members`, 'POST', { user_id: userId });
}

/** Add another group as a nested member. Backend enforces cycle + depth limits. */
export function addGroupMember(groupId: string, memberGroupId: string): Promise<void> {
	return mutate(`/api/groups/${enc(groupId)}/members`, 'POST', { group_id: memberGroupId });
}

export function removeUserMember(groupId: string, userId: string): Promise<void> {
	return mutate(`/api/groups/${enc(groupId)}/members/user/${enc(userId)}`, 'DELETE');
}

export function removeGroupMember(groupId: string, memberGroupId: string): Promise<void> {
	return mutate(`/api/groups/${enc(groupId)}/members/group/${enc(memberGroupId)}`, 'DELETE');
}
