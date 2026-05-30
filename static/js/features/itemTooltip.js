// @ts-check

/**
 * Item tooltip — unified hover tooltip showing a stable "technical sheet" for
 * a hovered `.file-item`.
 *
 * Both rows are always rendered so the layout never shifts between items.
 * A "?" placeholder is shown when data is absent for a given row.
 *
 *   data-owner-id  → 👤 Owner   [userVignette]   (avatar + name, async)
 *   data-path      → ⊕  Path    Documents/Work   (monospace)
 *
 * The tooltip is shown only when at least one of the two attributes is present.
 * Lines are laid out in a 3-column CSS grid (icon | label | value) so values
 * are always left-aligned at the same x position.
 *
 * Replaces the former `pathTooltip` and `ownerTooltip` modules.
 *
 * Usage:
 *   import * as itemTooltip from '../features/itemTooltip.js';
 *   itemTooltip.init(containerEl)    — call after rendering items
 *   itemTooltip.destroy(containerEl) — call when leaving the section
 */

import { createUserVignette } from '../components/userVignette.js';
import { i18n } from '../core/i18n.js';
import { systemUsers } from '../model/systemUsers.js';

// ── Tooltip DOM ───────────────────────────────────────────────────────────────
//
// The tooltip element is a static node inside `.search-slot` (see index.html)
// and is absolutely positioned over `.search-container`. Show/hide is a
// simple `.hidden` toggle on the tooltip itself — the search bar underneath
// is never touched, so there's no two-element swap to keep in sync.

/** @returns {HTMLElement | null} */
function _getTooltip() {
    return document.getElementById('path-tooltip');
}

function _hide() {
    const el = _getTooltip();
    if (el) el.classList.add('hidden');
}

// ── Row builder ───────────────────────────────────────────────────────────────

/**
 * Append one grid row (icon | label | value) to the tooltip container.
 * The three cells are direct children of the CSS grid — column assignment
 * is automatic.
 *
 * @param {HTMLElement} tooltip
 * @param {string}      iconClass   FontAwesome class string, e.g. `"fas fa-user"`
 * @param {string}      labelText
 * @param {(el: HTMLElement) => void} populate   Fills the value cell.
 * @returns {HTMLElement}  The value cell.
 */
function _addRow(tooltip, iconClass, labelText, populate) {
    const icon = document.createElement('i');
    icon.className = `${iconClass} path-tooltip__icon`;
    tooltip.appendChild(icon);

    const label = document.createElement('span');
    label.className = 'path-tooltip__label';
    label.textContent = labelText;
    tooltip.appendChild(label);

    const value = document.createElement('span');
    value.className = 'path-tooltip__value';
    populate(value);
    tooltip.appendChild(value);

    return value;
}

/**
 * Append a "?" placeholder cell (used when data is unavailable).
 * @param {HTMLElement} el
 */
function _setUnknown(el) {
    el.classList.add('path-tooltip__value--unknown');
    el.textContent = '?';
}

// ── Event handler ─────────────────────────────────────────────────────────────

/**
 * @param {MouseEvent} e
 */
function _onEnter(e) {
    const item = /** @type {HTMLElement} */ (e.currentTarget);
    const ownerId = item.dataset.ownerId;
    const path = item.dataset.path;

    // Nothing to show — don't display an all-? tooltip.
    if (!ownerId && !path) return;

    const tooltip = _getTooltip();
    if (!tooltip) return;

    // Clear previous content.
    while (tooltip.firstChild) tooltip.removeChild(tooltip.firstChild);

    // ── Owner row (always rendered) ───────────────────────────────────────────
    _addRow(tooltip, 'fas fa-user', i18n.t('files.owner', 'Owner'), (el) => {
        if (ownerId && systemUsers.isAvailable()) {
            el.appendChild(createUserVignette(ownerId, 'xs'));
        } else {
            _setUnknown(el);
        }
    });

    // ── Path row (always rendered) ────────────────────────────────────────────
    _addRow(tooltip, 'fas fa-location-crosshairs', i18n.t('tooltip.path', 'Path'), (el) => {
        if (path) {
            el.classList.add('path-tooltip__value--path');
            el.textContent = path;
        } else {
            _setUnknown(el);
        }
    });

    tooltip.classList.remove('hidden');
}

function _onLeave() {
    _hide();
}

// ── Listener registry (WeakMap for leak-free cleanup) ────────────────────────

/**
 * @typedef {{ enter: (e: MouseEvent) => void, leave: () => void }} Handlers
 */

/** @type {WeakMap<HTMLElement, Handlers>} */
const _registry = new WeakMap();

// ── Public API ────────────────────────────────────────────────────────────────

/** Containers known to expose `data-path`/`data-owner-id` for tooltip use.
 *  Add new opt-in row classes here when other views want the tooltip — each
 *  one must stamp the dataset attributes itself. */
const _TOOLTIP_SELECTOR = '.file-item, .ms-resource-row, .ms-grant-row';

/**
 * Attach tooltip listeners to every tooltip-capable row inside `container`.
 * Rows with neither `data-owner-id` nor `data-path` will not trigger the
 * tooltip.  Safe to call repeatedly — already-wired elements are skipped.
 * @param {HTMLElement} container
 */
function init(container) {
    for (const item of container.querySelectorAll(_TOOLTIP_SELECTOR)) {
        const el = /** @type {HTMLElement} */ (item);
        if (_registry.has(el)) continue; // already wired

        const enter = (/** @type {MouseEvent} */ ev) => _onEnter(ev);
        const leave = () => _onLeave();

        el.addEventListener('mouseenter', enter);
        el.addEventListener('mouseleave', leave);
        _registry.set(el, { enter, leave });
    }
}

/**
 * Remove tooltip listeners from all tooltip-capable rows inside `container`
 * and hide any visible tooltip.
 * @param {HTMLElement} container
 */
function destroy(container) {
    for (const item of container.querySelectorAll(_TOOLTIP_SELECTOR)) {
        const el = /** @type {HTMLElement} */ (item);
        const h = _registry.get(el);
        if (h) {
            el.removeEventListener('mouseenter', h.enter);
            el.removeEventListener('mouseleave', h.leave);
            _registry.delete(el);
        }
    }
    _hide();
}

export { destroy, init };
