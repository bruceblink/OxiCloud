// @ts-check

/**
 * Inline element representing a ReBAC subject group: user-group icon +
 * the group's name. Used by the share dialog (to display groups as share
 * recipients) and by the group-management view (to display nested-group
 * members).
 *
 * Visually mirrors `createUserVignette` from `./userVignette.js` so a row
 * built from one can swap in the other without layout shift. Picks
 * `fa-user-group` (a *people* icon) rather than `fa-layer-group`, which is
 * reserved across the app for the *grouping operator* on group-by menu pills
 * — keeping the two concepts visually distinct.
 *
 *   subject group         (this file)        fa-user-group
 *   grouping operator     (group-by pills)   fa-layer-group
 */

import { escapeHtml } from '../core/formatters.js';

/**
 * Build the inline vignette.
 *
 * @param {string} name
 *   Display name of the group (escaped before injection).
 * @param {'xs'|'sm'|'md'|'list'} [size='sm']
 *   Matches the size scale of `createUserVignette`. The size class is
 *   `user-vignette--${size}`; see `static/css/components/userVignette.css`.
 * @param {{ icon?: string }} [opts]
 *   `icon`: FA class string without the `fa-` prefix (defaults to
 *   `'fa-user-group'`). Used to signal virtual groups visually — see
 *   `groupIconClass()` / `groupIconClassByVirtual()` in `./groupDisplay.js`
 *   return a distinct icon for system-wide virtual groups (Internal,
 *   future Everyone, …).
 * @returns {HTMLElement}
 */
export function createGroupVignette(name, size = 'sm', { icon = 'fa-user-group' } = {}) {
    const el = document.createElement('div');
    el.className = `user-vignette user-vignette-group user-vignette--${size}`;
    el.innerHTML = `<span class="user-vignette__avatar"><i class="fas ${escapeHtml(icon)}"></i></span><span class="user-vignette__name">${escapeHtml(name)}</span>`;
    return el;
}
