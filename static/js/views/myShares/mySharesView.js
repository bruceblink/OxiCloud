/**
 * OxiCloud – "My Shares" view.
 *
 * Renders resources the current user has shared with others, using the
 * cursor-paginated `GET /api/grants/outgoing/resources` endpoint.
 *
 * Group-by modes exposed to the navigation toolbar:
 *   'items'      — one card per resource (sort_by=type)    [default / None]
 *   'sharedWith' — swimlanes per subject (sort_by=subject)
 *
 * The "None" option (key='') from the toolbar maps to the default 'items' mode.
 */

import { ui } from '../../app/ui.js';
import { collectGroupSubjectIds, MySharesList } from '../../components/mySharesList.js';
import { shareModal } from '../../components/shareModal.js';
import { i18n } from '../../core/i18n.js';
import * as viewPrefs from '../../core/viewPrefs.js';
import * as itemTooltip from '../../features/itemTooltip.js';
import { grants } from '../../model/grants.js';
import { groups } from '../../model/groups.js';

/** @import {FileItem, FolderItem} from '../../core/types.js' */

/**
 * @typedef {{ key: string, label: string, icon?: string, orderBy: string }} GroupByDef
 * @typedef {'items'|'sharedWith'} ViewMode
 */

/**
 * @type {{ [key: string]: { orderBy: string, viewMode: ViewMode } }}
 */
const MODE_MAP = {
    '': { orderBy: 'type', viewMode: 'items' },
    items: { orderBy: 'type', viewMode: 'items' },
    sharedWith: { orderBy: 'subject', viewMode: 'sharedWith' }
};

/** @type {GroupByDef[]} */
const GROUP_BY_DEFS = [
    {
        key: '',
        get label() {
            return i18n.t('groupby.byFiles', 'By files');
        },
        icon: 'fas fa-layer-group',
        orderBy: 'type'
    },
    {
        key: 'sharedWith',
        get label() {
            return i18n.t('groupby.sharedWith', 'Shared with');
        },
        icon: 'fas fa-layer-group',
        orderBy: 'subject'
    }
];

/** ID of the "Load more" wrapper injected below `.files-container`. */
const LOAD_MORE_ID = 'ms-load-more-wrapper';

const mySharesView = {
    // ── State ─────────────────────────────────────────────────────────────────

    /** @type {string|null} */
    _nextCursor: null,

    _loading: false,

    /** @type {MySharesList|null} */
    _component: null,

    /**
     * Cumulative id→GroupItem map of every group subject seen so far in this
     * session. Each `_loadPage()` merges newly resolved entries so subsequent
     * pages don't re-resolve already-known groups.
     * @type {Record<string, import('../../core/types.js').GroupItem>}
     */
    _knownGroupMeta: {},

    /** @type {string} */
    _groupBy: '',

    /** @type {boolean} */
    _reversed: false,

    // ── Public API ────────────────────────────────────────────────────────────

    /** @returns {GroupByDef[]} */
    get groupByDefs() {
        return GROUP_BY_DEFS;
    },

    /**
     * Change the active group-by dimension and reload from page 1.
     * Called by navigation.js when the user picks a pill.
     * Empty string '' maps to the default items mode.
     * @param {string} key
     */
    setGroupBy(key) {
        if (this._groupBy === key) return;
        this._groupBy = key;
        viewPrefs.save('shared', this._groupBy, this._reversed, viewPrefs.load('shared').view);
        this._nextCursor = null;
        this._component?.clear();
        this._loadPage();
    },

    /**
     * Flip sort direction and reload from page 1.
     * @param {boolean} reversed
     */
    setDirection(reversed) {
        if (this._reversed === reversed) return;
        this._reversed = reversed;
        viewPrefs.save('shared', this._groupBy, this._reversed, viewPrefs.load('shared').view);
        this._nextCursor = null;
        this._component?.clear();
        this._loadPage();
    },

    async init() {
        this._nextCursor = null;
        this._loading = false;
        const saved = viewPrefs.load('shared');
        this._groupBy = saved.groupBy || '';
        this._reversed = saved.reversed;

        this._ensureLoadMoreButton();

        ui.resetFilesList();
        ui.updateBreadcrumb();

        const filesList = document.getElementById('files-list');
        if (filesList) {
            if (!this._component) {
                this._component = new MySharesList(filesList, {
                    onResourceOpen: (resource, resourceType) => {
                        if (resourceType === 'folder') {
                            ui.openItem(resource);
                        } else {
                            // File: navigate to the parent folder in Files section.
                            const file = /** @type {FileItem} */ (resource);
                            const parts = (file.path || '').split('/').filter(Boolean);
                            const parentName = parts.length >= 2 ? parts[parts.length - 2] : '';
                            ui.openItem(/** @type {FolderItem} */ ({ id: file.folder_id, name: parentName }));
                        }
                    },
                    onShareEdit: (resource, resourceType) => {
                        shareModal.open(resource, /** @type {'file'|'folder'} */ (resourceType), () => {
                            this._nextCursor = null;
                            this._component?.clear();
                            this._loadPage();
                        });
                    }
                });
            }
        }

        await this._loadPage();
    },

    hide() {
        const w = document.getElementById(LOAD_MORE_ID);
        if (w) w.classList.add('hidden');
        const filesList = document.getElementById('files-list');
        if (filesList) itemTooltip.destroy(filesList);
    },

    // ── Internal helpers ──────────────────────────────────────────────────────

    async _loadPage() {
        if (this._loading) return;
        this._loading = true;

        const isFirstPage = this._nextCursor === null;

        try {
            const mode = MODE_MAP[this._groupBy] ?? MODE_MAP[''];

            const data = await grants.fetchMySharesPage({
                limit: 50,
                cursor: this._nextCursor ?? undefined,
                orderBy: mode.orderBy,
                reverse: this._reversed
            });

            this._nextCursor = data.next_cursor ?? null;

            if (data.items.length === 0 && isFirstPage) {
                ui.showError(`
                    <i class="fas fa-share-alt empty-state-icon"></i>
                    <p>${i18n.t('myshares.emptyStateTitle', "You haven't shared anything yet")}</p>
                    <p>${i18n.t('myshares.emptyStateDesc', 'Items you share with others will appear here')}</p>
                `);
                this._setLoadMoreVisible(false);
                return;
            }

            // Resolve full GroupItem records for any group-subject grants on
            // this page so lane headers and identity rows render the localised
            // name + virtual-aware icon. Cheap — one search call shared across
            // every group visible on the page.
            if (this._component) {
                const groupIds = collectGroupSubjectIds(data.items);
                if (groupIds.size > 0) {
                    try {
                        const resolved = await groups.resolveGroups(groupIds);
                        const merged = { ...this._knownGroupMeta, ...resolved };
                        this._component.setGroupMeta(merged);
                        this._knownGroupMeta = merged;
                    } catch (err) {
                        console.warn('mySharesView: failed to resolve group names', err);
                    }
                }
            }

            if (isFirstPage) {
                this._component?.render(data.items, mode.viewMode);
            } else {
                this._component?.append(data.items, mode.viewMode);
            }

            const filesList = document.getElementById('files-list');
            if (filesList) itemTooltip.init(filesList);

            this._setLoadMoreVisible(!!this._nextCursor);
        } catch (err) {
            ui.showError(`
                <i class="fas fa-exclamation-circle empty-state-icon error"></i>
                <p>${i18n.t('errors_loadFailed', 'Failed to load items')}</p>
            `);
            console.error('mySharesView: load error', err);
        } finally {
            this._loading = false;
        }
    },

    // ── "Load more" button ────────────────────────────────────────────────────

    _ensureLoadMoreButton() {
        if (document.getElementById(LOAD_MORE_ID)) return;

        const filesContainer = document.querySelector('.files-container');
        if (!filesContainer) return;

        const wrapper = document.createElement('div');
        wrapper.id = LOAD_MORE_ID;
        wrapper.className = 'ms-load-more-wrapper hidden';

        const btn = document.createElement('button');
        btn.id = 'ms-load-more';
        btn.className = 'button secondary';
        btn.textContent = i18n.t('myshares.loadMore', 'Load more');
        btn.addEventListener('click', () => this._loadPage());

        wrapper.appendChild(btn);
        filesContainer.after(wrapper);
    },

    /** @param {boolean} visible */
    _setLoadMoreVisible(visible) {
        const w = document.getElementById(LOAD_MORE_ID);
        if (w) w.classList.toggle('hidden', !visible);
    }
};

export { mySharesView };
