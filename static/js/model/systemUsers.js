// @ts-check

/**
 * System-users convenience layer.
 *
 * Thin wrapper over `addressBook.listContacts(SYSTEM_BOOK_ID)` that
 * provides a userId → display-name index.  Used wherever a grant's
 * `granted_by` UUID needs to be shown as a human-readable name
 * (owner tooltips, share dialogs, etc.).
 *
 * Falls back gracefully when the system address book is disabled
 * server-side (`OXICLOUD_EXPOSE_SYSTEM_USERS` not set): `isAvailable()`
 * returns false and `getDisplayName()` returns a shortened UUID.
 */

/** @import {ContactItem} from '../core/types.js' */

import { addressBook, SYSTEM_BOOK_ID } from './addressBook.js';

/** @type {Map<string, string> | null} userId → display name, built lazily */
let _index = null;

/**
 * Derive the best human-readable name from a contact.
 * Priority: "First Last" → full_name → primary email → shortened id.
 * @param {ContactItem} c
 * @returns {string}
 */
function _nameFor(c) {
    const parts = /** @type {string[]} */ ([c.first_name, c.last_name].filter(Boolean));
    if (parts.length) return parts.join(' ');
    if (c.full_name) return c.full_name;
    const mail = c.email?.find((e) => e.is_primary)?.email ?? c.email?.[0]?.email;
    if (mail) return mail;
    return `${c.id.slice(0, 8)}…`;
}

/**
 * Ensure the index is built (idempotent).
 * After loading contacts from the system address book, the current user
 * (from localStorage) is injected so owner cells resolve correctly even
 * when the server-side address book does not include the logged-in user.
 * @returns {Promise<void>}
 */
async function _ensureIndex() {
    if (_index !== null) return;
    const contacts = await addressBook.listContacts(SYSTEM_BOOK_ID);
    _index = new Map(contacts.map((c) => [c.id, _nameFor(c)]));

    // Inject the current user if they are not already in the index
    try {
        const raw = localStorage.getItem('oxicloud_user');
        if (raw) {
            const u = /** @type {{id?:string, display_name?:string, username?:string, email?:string}} */ (JSON.parse(raw));
            if (u?.id && !_index.has(u.id)) {
                const name = u.display_name || u.username || u.email || `${u.id.slice(0, 8)}…`;
                _index.set(u.id, name);
            }
        }
    } catch {
        // localStorage not available or JSON is invalid — silently skip
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/**
 * Start loading the system address book in the background.
 * Safe to call multiple times — subsequent calls are no-ops once loaded.
 */
function prefetch() {
    if (!addressBook.isSystemAvailable()) return;
    _ensureIndex(); // intentionally fire-and-forget
}

/**
 * Resolve a user UUID to a display name.
 * Awaits the first load if not yet cached; subsequent calls resolve instantly.
 *
 * @param {string} userId
 * @returns {Promise<string>}
 */
async function getDisplayName(userId) {
    await _ensureIndex();
    return _index?.get(userId) ?? `${userId.slice(0, 8)}…`;
}

/**
 * Returns `false` only after a confirmed 404 from the server (feature
 * disabled).  Returns `true` when status is unknown or the book loaded OK.
 * @returns {boolean}
 */
function isAvailable() {
    return addressBook.isSystemAvailable();
}

export const systemUsers = { prefetch, getDisplayName, isAvailable };
