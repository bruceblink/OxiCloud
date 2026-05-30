// @ts-check

/**
 * @import {GroupItem, GroupListResponse, GroupMemberItem} from '../core/types.js'
 */

/**
 * Thin API client for ReBAC subject groups (`/api/groups/*`).
 *
 * Uses the global `fetch` (intercepted in `core/fetchWrapper.js` for
 * 401-refresh-retry and `ApiError` translation) and `getCsrfHeaders()` for
 * mutating verbs.
 *
 * v1: most endpoints are admin-only on the backend; `/api/groups/search` is
 * authenticated-only and powers the share-dialog recipient autocomplete.
 * v2 will relax the admin guard to per-group `Manage` permissions — every
 * caller here will keep working unchanged, but a non-admin may start
 * receiving 403s on `get`/`listMembers` for groups they can't see.
 */

import { getCsrfHeaders } from '../core/csrf.js';

/** Well-known UUID of the predefined Internal virtual group (matches the
 *  Rust constant `INTERNAL_GROUP_ID` in `src/domain/entities/subject_group.rs`). */
const INTERNAL_GROUP_ID = '00000000-0000-0000-0000-000000000001';

const groups = {
    /**
     * Paginated list. Admin-only on the server today.
     * @param {{limit?: number, offset?: number, q?: string|null}} [opts]
     * @returns {Promise<GroupListResponse>}
     */
    async list({ limit = 50, offset = 0, q = null } = {}) {
        const params = new URLSearchParams();
        params.set('limit', String(limit));
        params.set('offset', String(offset));
        if (q) params.set('q', q);
        const res = await fetch(`/api/groups?${params}`);
        if (!res.ok) throw await _err(res, 'list groups');
        return res.json();
    },

    /**
     * Search up to ~8 non-virtual groups whose name matches `q`.
     * Authenticated (not admin-gated) — used by the share-dialog autocomplete.
     * @param {string} q
     * @param {number} [limit=8]
     * @returns {Promise<GroupItem[]>}
     */
    async search(q, limit = 8) {
        const params = new URLSearchParams({ q, limit: String(limit) });
        const res = await fetch(`/api/groups/search?${params}`);
        if (!res.ok) throw await _err(res, 'search groups');
        return res.json();
    },

    /**
     * Resolve a set of group IDs to full `GroupItem` records. Used after
     * loading grants (which only carry `subject_id`) so the UI can render
     * the group's name and pick a virtual-aware icon.
     *
     * Strategy: a single `/api/groups/search` call with empty `q` and a
     * generous limit covers any caller (admin or not) without needing the
     * admin-gated `GET /api/groups/{id}`. Virtual groups are now returned
     * by the search endpoint, so no special-casing is needed here.
     *
     * Unresolved IDs (deleted groups, or beyond the search limit) get a
     * synthetic stub so call sites never have to handle missing entries.
     *
     * @param {Iterable<string>} ids
     * @returns {Promise<Record<string, GroupItem>>}
     */
    async resolveGroups(ids) {
        const wanted = new Set(ids);
        /** @type {Record<string, GroupItem>} */
        const out = {};
        if (wanted.size === 0) return out;

        try {
            const items = await this.search('', 200);
            for (const g of items) {
                if (wanted.has(g.id)) out[g.id] = g;
            }
        } catch {
            // Network / auth failure — fall through to the stub fallback below.
        }

        // Anything still unresolved → readable stub so the UI never shows a
        // raw UUID. `is_virtual: false` matches the safer (more restrictive)
        // visual treatment when in doubt.
        const now = new Date().toISOString();
        for (const id of wanted) {
            if (!(id in out)) {
                out[id] = {
                    id,
                    name: `Group ${id.slice(0, 8)}…`,
                    description: null,
                    is_virtual: false,
                    created_at: now,
                    updated_at: now,
                    can_manage: false,
                    member_count: 0
                };
            }
        }
        return out;
    },

    /**
     * @param {string} id
     * @returns {Promise<GroupItem>}
     */
    async get(id) {
        const res = await fetch(`/api/groups/${encodeURIComponent(id)}`);
        if (!res.ok) throw await _err(res, 'get group');
        return res.json();
    },

    /**
     * @param {{name: string, description?: string|null}} body
     * @returns {Promise<GroupItem>}
     */
    async create(body) {
        const res = await fetch('/api/groups', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', ...getCsrfHeaders() },
            body: JSON.stringify(body)
        });
        if (!res.ok) throw await _err(res, 'create group');
        return res.json();
    },

    /**
     * @param {string} id
     * @param {string} newName
     * @returns {Promise<GroupItem>}
     */
    async rename(id, newName) {
        const res = await fetch(`/api/groups/${encodeURIComponent(id)}`, {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json', ...getCsrfHeaders() },
            body: JSON.stringify({ name: newName })
        });
        if (!res.ok) throw await _err(res, 'rename group');
        return res.json();
    },

    /**
     * @param {string} id
     * @returns {Promise<void>}
     */
    async deleteGroup(id) {
        const res = await fetch(`/api/groups/${encodeURIComponent(id)}`, {
            method: 'DELETE',
            headers: getCsrfHeaders()
        });
        if (!res.ok) throw await _err(res, 'delete group');
    },

    /**
     * Direct members (one level only, not transitive).
     * @param {string} id
     * @returns {Promise<GroupMemberItem[]>}
     */
    async listMembers(id) {
        const res = await fetch(`/api/groups/${encodeURIComponent(id)}/members`);
        if (!res.ok) throw await _err(res, 'list members');
        return res.json();
    },

    /**
     * Add a user as a member.
     * @param {string} groupId
     * @param {string} userId
     * @returns {Promise<void>}
     */
    async addUserMember(groupId, userId) {
        const res = await fetch(`/api/groups/${encodeURIComponent(groupId)}/members`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', ...getCsrfHeaders() },
            body: JSON.stringify({ user_id: userId })
        });
        if (!res.ok) throw await _err(res, 'add user member');
    },

    /**
     * Add another group as a nested member.
     * Backend runs the cycle + depth checks at write time.
     * @param {string} groupId
     * @param {string} memberGroupId
     * @returns {Promise<void>}
     */
    async addGroupMember(groupId, memberGroupId) {
        const res = await fetch(`/api/groups/${encodeURIComponent(groupId)}/members`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', ...getCsrfHeaders() },
            body: JSON.stringify({ group_id: memberGroupId })
        });
        if (!res.ok) throw await _err(res, 'add group member');
    },

    /**
     * @param {string} groupId
     * @param {string} userId
     * @returns {Promise<void>}
     */
    async removeUserMember(groupId, userId) {
        const res = await fetch(`/api/groups/${encodeURIComponent(groupId)}/members/user/${encodeURIComponent(userId)}`, {
            method: 'DELETE',
            headers: getCsrfHeaders()
        });
        if (!res.ok) throw await _err(res, 'remove user member');
    },

    /**
     * @param {string} groupId
     * @param {string} memberGroupId
     * @returns {Promise<void>}
     */
    async removeGroupMember(groupId, memberGroupId) {
        const res = await fetch(`/api/groups/${encodeURIComponent(groupId)}/members/group/${encodeURIComponent(memberGroupId)}`, {
            method: 'DELETE',
            headers: getCsrfHeaders()
        });
        if (!res.ok) throw await _err(res, 'remove group member');
    }
};

/**
 * Build a thrown Error from a non-OK Response. The fetch interceptor turns
 * structured API errors into `ApiError`, so anything that lands here is
 * either a network failure or an error the interceptor already enriched.
 * @param {Response} res
 * @param {string} context
 * @returns {Promise<Error>}
 */
async function _err(res, context) {
    let detail = `${res.status} ${res.statusText}`;
    try {
        const body = await res.text();
        if (body) detail += `: ${body}`;
    } catch {
        // body unreadable — fall through with status only.
    }
    return new Error(`${context} failed: ${detail}`);
}

export { groups, INTERNAL_GROUP_ID };
