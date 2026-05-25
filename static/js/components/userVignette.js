// @ts-check

/**
 * UserVignette — reusable user avatar + name inline component.
 *
 * Renders a coloured circle with initials (or photo when available) alongside
 * an asynchronously-resolved display name.  Used in:
 *  • Owner column (list view) via `ui.resolveOwnerCells()`
 *  • ShareModal member rows / chips / suggestion items
 *
 * Usage:
 *   import { createUserVignette } from './userVignette.js';
 *   cell.replaceChildren(createUserVignette(userId, 'sm'));
 */

import { systemUsers } from '../model/systemUsers.js';

// ── Helpers ────────────────────────────────────────────────────────────────────

/**
 * Get initials for an avatar (1-2 characters).
 * @param {string} name
 * @returns {string}
 */
export function _initials(name) {
    const parts = name.trim().split(/\s+/);
    if (parts.length >= 2) return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase();
    return name.slice(0, 2).toUpperCase();
}

/**
 * Deterministic color index 0-4 derived from a userId string.
 * Same userId always maps to the same color across all components.
 * @param {string} userId
 * @returns {number}
 */
export function _colorIndex(userId) {
    let hash = 0;
    for (let i = 0; i < userId.length; i++) {
        hash = (hash * 31 + userId.charCodeAt(i)) | 0;
    }
    return Math.abs(hash) % 5;
}

// ── Component ──────────────────────────────────────────────────────────────────

/**
 * @typedef {'xs'|'sm'|'md'|'lg'} VignetteSize
 */

/**
 * Create a user vignette element: a coloured initials circle + async-resolved
 * display name span.  The element is returned immediately with a short-UUID
 * placeholder; the name resolves in the background via `systemUsers`.
 *
 * @param {string}       userId  UUID of the user
 * @param {VignetteSize} [size='sm']
 * @returns {HTMLElement}
 */
export function createUserVignette(userId, size = 'sm') {
    const colorIdx = _colorIndex(userId);

    const wrapper = /** @type {HTMLElement} */ (document.createElement('span'));
    wrapper.className = `user-vignette user-vignette--${size}`;

    const avatar = document.createElement('span');
    avatar.className = `user-vignette__avatar uv-color-${colorIdx}`;
    // Temporary placeholder: first two chars of UUID
    avatar.textContent = userId.slice(0, 2).toUpperCase();

    const nameEl = document.createElement('span');
    nameEl.className = 'user-vignette__name';
    nameEl.textContent = `${userId.slice(0, 8)}…`;

    wrapper.appendChild(avatar);
    wrapper.appendChild(nameEl);

    // Resolve full name asynchronously and update both avatar initials and name
    systemUsers.getDisplayName(userId).then((name) => {
        avatar.textContent = _initials(name);
        nameEl.textContent = name;
    });

    return wrapper;
}
