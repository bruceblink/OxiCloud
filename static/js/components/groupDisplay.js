// @ts-check

/**
 * Display helpers for ReBAC subject groups.
 *
 * Server-side names of virtual groups (`Internal`, future `Everyone`, …) are
 * fixed RFC 5321 local-part strings so they can be email-addressable. The UI
 * surfaces them with a localised, capitalised label and a distinct icon.
 *
 * "Add a new virtual group" — frontend cost is:
 *   1. Add an entry to `VIRTUAL_NAME_KEYS` mapping the well-known UUID to an
 *      `i18n` key.
 *   2. Add the i18n key + translations in the 16 locale files.
 *
 * Everything else (search results, vignettes, member rows, autocomplete)
 * picks the new group up automatically because the backend now returns
 * virtual groups in `/api/groups/search`.
 */

import { i18n } from '../core/i18n.js';
import { INTERNAL_GROUP_ID } from '../model/groups.js';

/**
 * Map of well-known virtual-group UUIDs → i18n key for the human-readable
 * display name. Anything not in this map falls back to `group.name`.
 *
 * @type {Record<string, string>}
 */
const VIRTUAL_NAME_KEYS = {
    [INTERNAL_GROUP_ID]: 'groups.virtual_internal_name'
};

/**
 * Minimal shape needed by the display helpers. Both `GroupItem` (from
 * `/api/groups`) and shareModal's `GroupSuggestion` satisfy it, so callers
 * can pass either without an awkward upcast.
 *
 * @typedef {{id: string, name: string, is_virtual: boolean}} GroupDisplay
 */

/**
 * Human-readable display name for a group. Virtual groups get a translated
 * label; user-defined groups display their raw name.
 *
 * @param {GroupDisplay} group
 * @returns {string}
 */
export function groupDisplayName(group) {
    if (group.is_virtual) {
        const key = VIRTUAL_NAME_KEYS[group.id];
        if (key) return i18n.t(key, group.name);
    }
    return group.name;
}

/** FA class for system-managed (virtual) groups. `fa-people-roof` evokes a
 *  shared roof / community, distinguishing virtual instance-wide groups
 *  (Internal, future Everyone, …) from user-defined groups. Change here to
 *  re-skin every virtual-group surface in the app in one place. */
const VIRTUAL_ICON = 'fa-people-roof';

/** FA class for user-defined groups. */
const REGULAR_ICON = 'fa-user-group';

/**
 * Pick the Font Awesome icon class for a group vignette. Virtual groups use
 * `VIRTUAL_ICON`; user-defined groups use `REGULAR_ICON`.
 *
 * @param {GroupDisplay} group
 * @returns {string}
 */
export function groupIconClass(group) {
    return group.is_virtual ? VIRTUAL_ICON : REGULAR_ICON;
}

/**
 * Same as `groupIconClass` but for call sites that hold only the
 * `is_virtual` boolean — e.g. `MemberEntry._isVirtual` in shareModal,
 * where the full `GroupItem` isn't kept around.
 *
 * @param {boolean | undefined} isVirtual
 * @returns {string}
 */
export function groupIconClassByVirtual(isVirtual) {
    return isVirtual ? VIRTUAL_ICON : REGULAR_ICON;
}
