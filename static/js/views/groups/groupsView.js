// @ts-check

/**
 * Subject-group management view.
 *
 * Reached from the user-menu "Manage groups" entry. Opens a `Modal.openPanel`
 * with a two-state UI:
 *
 *   list    — paginated list of groups + Create button
 *   detail  — single-group editor: members, add/remove, rename, delete
 *
 * Mutations commit immediately (POST/DELETE per click). The two states are
 * rendered into the same panel body — `_renderListInto()` and
 * `_renderDetailInto()` swap the body element, the modal frame stays open.
 *
 * Action buttons (Create, Rename, Delete, Add/Remove member) read
 * `group.can_manage` from the backend DTO and are hidden/disabled when
 * `false`. v1 returns `can_manage = (role === "admin")`; v2 will return it
 * from per-group `Manage` grants — no JS change required at that point.
 */

import { groupDisplayName, groupIconClass } from '../../components/groupDisplay.js';
import { createGroupVignette } from '../../components/groupVignette.js';
import { Modal } from '../../components/modal.js';
import { createUserVignette } from '../../components/userVignette.js';
import { escapeHtml } from '../../core/formatters.js';
import { i18n } from '../../core/i18n.js';
import { addressBook, SYSTEM_BOOK_ID } from '../../model/addressBook.js';
import { groups } from '../../model/groups.js';

/**
 * @import {ContactItem, GroupItem, GroupMemberItem} from '../../core/types.js'
 */

const PAGE_SIZE = 50;

/**
 * Localised "(N members)" label for a group list row. The project's i18n
 * helper is key→string with no built-in pluralisation, so we branch on the
 * three forms named by the plan and substitute `{count}` ourselves.
 *
 * @param {number} count
 * @returns {string}
 */
function _memberCountLabel(count) {
    if (count === 0) return i18n.t('groups.member_count_zero', 'no members');
    if (count === 1) return i18n.t('groups.member_count_one', '1 member');
    return i18n.t('groups.member_count_other', '{count} members').replace('{count}', String(count));
}

const groupsView = {
    // ── State ─────────────────────────────────────────────────────────────

    /** @type {HTMLElement|null} — current panel body container */
    _bodyEl: null,

    /** @type {GroupItem|null} — populated when in detail state */
    _currentGroup: null,

    /** @type {GroupMemberItem[]} — direct members of the current group */
    _members: [],

    /** @type {GroupItem[]} — most recent list page */
    _items: [],
    _nextOffset: 0,
    _hasMore: false,

    // ── Public entry ──────────────────────────────────────────────────────

    /** Open the management modal at the list view. */
    async open() {
        this._currentGroup = null;
        this._members = [];
        this._items = [];
        this._nextOffset = 0;
        this._hasMore = false;

        this._bodyEl = document.createElement('div');
        this._bodyEl.className = 'groups-modal';

        Modal.openPanel({
            title: i18n.t('groups.title', 'Manage groups'),
            icon: 'fa-user-group',
            content: this._bodyEl,
            confirmText: i18n.t('actions.close', 'Close'),
            cancelText: '',
            onConfirm: null
        });

        await this._renderListInto(this._bodyEl);
    },

    // ── List view ─────────────────────────────────────────────────────────

    /** @param {HTMLElement} root */
    async _renderListInto(root) {
        root.replaceChildren();

        const header = document.createElement('div');
        header.className = 'groups-modal__header';

        const subtitle = document.createElement('div');
        subtitle.className = 'groups-modal__subtitle';
        subtitle.textContent = i18n.t('groups.title', 'Manage groups');

        const createBtn = document.createElement('button');
        createBtn.type = 'button';
        createBtn.className = 'btn btn-primary groups-modal__create-btn';
        createBtn.innerHTML = `<i class="fas fa-plus"></i> ${escapeHtml(i18n.t('groups.create_button', 'Create group'))}`;
        createBtn.addEventListener('click', () => this._promptCreate());

        header.appendChild(subtitle);
        header.appendChild(createBtn);
        root.appendChild(header);

        const list = document.createElement('div');
        list.className = 'groups-modal__list';
        root.appendChild(list);

        const status = document.createElement('div');
        status.className = 'groups-modal__status';
        status.textContent = i18n.t('groups.loading', 'Loading…');
        list.appendChild(status);

        try {
            const page = await groups.list({ limit: PAGE_SIZE, offset: 0 });
            this._items = page.items;
            this._nextOffset = page.items.length;
            this._hasMore = page.items.length < page.total;
            list.replaceChildren();

            if (page.items.length === 0) {
                const empty = document.createElement('div');
                empty.className = 'groups-modal__empty';
                empty.innerHTML = `<i class="fas fa-user-group groups-modal__empty-icon"></i><p>${escapeHtml(i18n.t('groups.empty_state', 'No groups yet.'))}</p>`;
                list.appendChild(empty);
                return;
            }

            for (const g of page.items) {
                list.appendChild(this._buildListRow(g));
            }

            if (this._hasMore) {
                const more = document.createElement('button');
                more.type = 'button';
                more.className = 'btn btn-ghost groups-modal__load-more';
                more.textContent = i18n.t('groups.load_more', 'Load more');
                more.addEventListener('click', async () => {
                    more.disabled = true;
                    const next = await groups.list({ limit: PAGE_SIZE, offset: this._nextOffset });
                    more.remove();
                    for (const g of next.items) list.appendChild(this._buildListRow(g));
                    this._items = [...this._items, ...next.items];
                    this._nextOffset += next.items.length;
                    this._hasMore = this._items.length < next.total;
                    if (this._hasMore) list.appendChild(more);
                });
                list.appendChild(more);
            }
        } catch (err) {
            list.replaceChildren();
            const errEl = document.createElement('div');
            errEl.className = 'groups-modal__error';
            errEl.textContent = /** @type {Error} */ (err).message;
            list.appendChild(errEl);
        }
    },

    /**
     * @param {GroupItem} g
     * @returns {HTMLElement}
     */
    _buildListRow(g) {
        const row = document.createElement('div');
        row.className = 'groups-modal__row';
        row.tabIndex = 0;

        // Vignette + meta
        const main = document.createElement('div');
        main.className = 'groups-modal__row-main';
        main.appendChild(createGroupVignette(groupDisplayName(g), 'md', { icon: groupIconClass(g) }));
        if (g.description) {
            const desc = document.createElement('div');
            desc.className = 'groups-modal__row-desc';
            desc.textContent = g.description;
            main.appendChild(desc);
        }
        if (g.is_virtual) {
            const badge = document.createElement('span');
            badge.className = 'groups-modal__row-badge';
            badge.textContent = i18n.t('groups.virtual_badge', 'System');
            main.appendChild(badge);
        }
        // Virtual groups (Internal, future Everyone, …) have no direct members
        // by construction — membership is computed implicitly by the engine —
        // so a literal "no members" chip would be misleading. Skip it.
        if (!g.is_virtual) {
            const memberChip = document.createElement('span');
            memberChip.className = 'groups-modal__row-count';
            memberChip.textContent = _memberCountLabel(g.member_count);
            main.appendChild(memberChip);
        }
        row.appendChild(main);

        // Click row → open detail. Virtual groups are read-only but can still
        // be inspected.
        const openDetail = () => this._showDetail(g.id);
        row.addEventListener('click', openDetail);
        row.addEventListener('keydown', (e) => {
            if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                openDetail();
            }
        });

        return row;
    },

    /**
     * Render an inline name-entry form into the panel body. The Modal is a
     * singleton — calling `Modal.prompt()` from inside an open `openPanel()`
     * mutates the same overlay and doesn't surface a usable input field, so
     * we keep the create / rename flows inside this view's own body.
     * @param {HTMLElement} root
     */
    _renderCreateInto(root) {
        root.replaceChildren();

        const header = document.createElement('div');
        header.className = 'groups-modal__detail-header';

        const back = document.createElement('button');
        back.type = 'button';
        back.className = 'btn btn-ghost groups-modal__back-btn';
        back.setAttribute('aria-label', i18n.t('groups.back_to_list', 'Back'));
        back.innerHTML = '<i class="fas fa-arrow-left"></i>';
        back.addEventListener('click', () => this._renderListInto(root));
        header.appendChild(back);

        const title = document.createElement('div');
        title.className = 'groups-modal__subtitle';
        title.textContent = i18n.t('groups.create_dialog_title', 'New group');
        header.appendChild(title);

        root.appendChild(header);

        const form = document.createElement('div');
        form.className = 'groups-modal__form';

        const label = document.createElement('label');
        label.className = 'groups-modal__form-label';
        label.textContent = i18n.t('groups.name_label', 'Name');
        form.appendChild(label);

        const input = document.createElement('input');
        input.type = 'text';
        input.className = 'groups-modal__add-input';
        input.placeholder = i18n.t('groups.name_placeholder', 'engineering');
        input.autocomplete = 'off';
        label.appendChild(input);

        const err = document.createElement('div');
        err.className = 'groups-modal__inline-error hidden';
        form.appendChild(err);

        const actions = document.createElement('div');
        actions.className = 'groups-modal__form-actions';

        const cancel = document.createElement('button');
        cancel.type = 'button';
        cancel.className = 'btn btn-ghost';
        cancel.textContent = i18n.t('actions.cancel', 'Cancel');
        cancel.addEventListener('click', () => this._renderListInto(root));

        const save = document.createElement('button');
        save.type = 'button';
        save.className = 'btn btn-primary';
        save.textContent = i18n.t('actions.create', 'Create');

        const submit = async () => {
            const value = input.value.trim();
            if (!value) {
                err.textContent = i18n.t('errors.group_name_invalid', 'Invalid name.');
                err.classList.remove('hidden');
                input.focus();
                return;
            }
            save.disabled = true;
            try {
                await groups.create({ name: value });
                await this._renderListInto(root);
            } catch (e) {
                err.textContent = /** @type {Error} */ (e).message;
                err.classList.remove('hidden');
                save.disabled = false;
            }
        };

        save.addEventListener('click', submit);
        input.addEventListener('keydown', (e) => {
            if (e.key === 'Enter') {
                e.preventDefault();
                submit();
            } else if (e.key === 'Escape') {
                e.preventDefault();
                this._renderListInto(root);
            }
        });

        actions.appendChild(cancel);
        actions.appendChild(save);
        form.appendChild(actions);

        root.appendChild(form);

        // Focus the input on next tick so the panel finishes rendering first.
        setTimeout(() => input.focus(), 0);
    },

    _promptCreate() {
        if (this._bodyEl) this._renderCreateInto(this._bodyEl);
    },

    // ── Detail view ───────────────────────────────────────────────────────

    /** @param {string} groupId */
    async _showDetail(groupId) {
        if (!this._bodyEl) return;
        try {
            const [group, members] = await Promise.all([groups.get(groupId), groups.listMembers(groupId)]);
            this._currentGroup = group;
            this._members = members;
            this._renderDetailInto(this._bodyEl);
        } catch (err) {
            this._showFatalError(/** @type {Error} */ (err).message);
        }
    },

    /** @param {HTMLElement} root */
    _renderDetailInto(root) {
        const group = this._currentGroup;
        if (!group) return;
        root.replaceChildren();

        // ── Header (back arrow + name + meta) ─────────────────────────────
        const header = document.createElement('div');
        header.className = 'groups-modal__detail-header';

        const back = document.createElement('button');
        back.type = 'button';
        back.className = 'btn btn-ghost groups-modal__back-btn';
        back.setAttribute('aria-label', i18n.t('groups.back_to_list', 'Back'));
        back.innerHTML = '<i class="fas fa-arrow-left"></i>';
        back.addEventListener('click', () => this._renderListInto(root));
        header.appendChild(back);

        const titleWrap = document.createElement('div');
        titleWrap.className = 'groups-modal__detail-title-wrap';
        titleWrap.appendChild(createGroupVignette(groupDisplayName(group), 'md', { icon: groupIconClass(group) }));

        if (group.is_virtual) {
            const badge = document.createElement('span');
            badge.className = 'groups-modal__row-badge';
            badge.textContent = i18n.t('groups.virtual_badge', 'System');
            titleWrap.appendChild(badge);
        }
        header.appendChild(titleWrap);

        if (group.can_manage && !group.is_virtual) {
            const rename = document.createElement('button');
            rename.type = 'button';
            rename.className = 'btn btn-ghost';
            rename.innerHTML = `<i class="fas fa-pen"></i>`;
            rename.title = i18n.t('actions.rename', 'Rename');
            rename.addEventListener('click', () => this._promptRename(group));
            header.appendChild(rename);
        }

        root.appendChild(header);

        // ── Members section ───────────────────────────────────────────────
        const membersSection = document.createElement('div');
        membersSection.className = 'groups-modal__members';

        const membersHeader = document.createElement('div');
        membersHeader.className = 'groups-modal__section-title';
        membersHeader.textContent = i18n.t('groups.members_section', 'Members');
        membersSection.appendChild(membersHeader);

        if (this._members.length === 0) {
            const empty = document.createElement('div');
            empty.className = 'groups-modal__empty-line';
            empty.textContent = i18n.t('groups.no_members', 'No members yet.');
            membersSection.appendChild(empty);
        } else {
            for (const m of this._members) {
                membersSection.appendChild(this._buildMemberRow(m));
            }
        }
        root.appendChild(membersSection);

        // ── Add-member row (only if can_manage) ───────────────────────────
        if (group.can_manage && !group.is_virtual) {
            root.appendChild(this._buildAddMemberRow());
        }

        // ── Delete group (destructive footer) ─────────────────────────────
        if (group.can_manage && !group.is_virtual) {
            const footer = document.createElement('div');
            footer.className = 'groups-modal__footer';
            const del = document.createElement('button');
            del.type = 'button';
            del.className = 'btn btn-danger';
            del.innerHTML = `<i class="fas fa-trash"></i> ${escapeHtml(i18n.t('groups.delete_group', 'Delete group'))}`;
            del.addEventListener('click', () => this._confirmDelete(group));
            footer.appendChild(del);
            root.appendChild(footer);
        }
    },

    /**
     * @param {GroupMemberItem} m
     * @returns {HTMLElement}
     */
    _buildMemberRow(m) {
        const row = document.createElement('div');
        row.className = 'groups-modal__member-row';

        if (m.kind === 'user') {
            row.appendChild(createUserVignette(m.id, 'sm', { showEmail: true }));
        } else {
            // Nested group — show the vignette. We don't pre-load the name
            // (it's just the id from the API). To get the name we'd need an
            // extra fetch; for v1, show the id-as-name (small UX cost) and
            // upgrade once `list_direct_members` returns enriched rows.
            row.appendChild(createGroupVignette(m.id, 'sm'));
        }

        if (this._currentGroup?.can_manage && !this._currentGroup?.is_virtual) {
            const rm = document.createElement('button');
            rm.type = 'button';
            rm.className = 'btn btn-ghost groups-modal__member-remove';
            rm.innerHTML = '&times;';
            rm.title = i18n.t('groups.remove_member', 'Remove');
            rm.addEventListener('click', () => this._removeMember(m));
            row.appendChild(rm);
        }

        return row;
    },

    _buildAddMemberRow() {
        const group = this._currentGroup;
        if (!group) return document.createElement('div');

        const wrap = document.createElement('div');
        wrap.className = 'groups-modal__add-row';

        const input = document.createElement('input');
        input.type = 'text';
        input.className = 'groups-modal__add-input';
        input.placeholder = i18n.t('groups.add_member_placeholder', 'Add a user or group…');

        const dropdown = document.createElement('div');
        dropdown.className = 'groups-modal__add-dropdown hidden';

        /** @type {ReturnType<typeof setTimeout>|null} */
        let debounce = null;

        /** @param {GroupMemberItem} m */
        const knownMemberKey = (m) => `${m.kind}:${m.id}`;
        const seen = new Set(this._members.map(knownMemberKey));

        input.addEventListener('input', () => {
            if (debounce) clearTimeout(debounce);
            const q = input.value.trim();
            if (!q) {
                dropdown.classList.add('hidden');
                dropdown.replaceChildren();
                return;
            }
            debounce = setTimeout(async () => {
                try {
                    const [contacts, groupResults] = await Promise.all([addressBook.searchContacts(q, [SYSTEM_BOOK_ID]), groups.search(q)]);
                    // Filter out the current group itself + already-members.
                    const userHits = contacts.filter((c) => !seen.has(`user:${c.id}`)).slice(0, 5);
                    const groupHits = groupResults.filter((g) => g.id !== group.id && !seen.has(`group:${g.id}`)).slice(0, 5);
                    this._renderAddSuggestions(dropdown, userHits, groupHits);
                } catch (err) {
                    dropdown.replaceChildren();
                    const e = document.createElement('div');
                    e.className = 'groups-modal__error';
                    e.textContent = /** @type {Error} */ (err).message;
                    dropdown.appendChild(e);
                    dropdown.classList.remove('hidden');
                }
            }, 200);
        });

        document.addEventListener(
            'click',
            (e) => {
                if (!wrap.contains(/** @type {Node} */ (e.target))) {
                    dropdown.classList.add('hidden');
                }
            },
            { once: false }
        );

        wrap.appendChild(input);
        wrap.appendChild(dropdown);
        return wrap;
    },

    /**
     * @param {HTMLElement} dropdown
     * @param {ContactItem[]} userHits
     * @param {GroupItem[]} groupHits
     */
    _renderAddSuggestions(dropdown, userHits, groupHits) {
        dropdown.replaceChildren();
        if (userHits.length === 0 && groupHits.length === 0) {
            dropdown.classList.add('hidden');
            return;
        }
        const group = this._currentGroup;
        if (!group) return;

        for (const g of groupHits) {
            const item = document.createElement('div');
            item.className = 'groups-modal__add-item';
            item.tabIndex = 0;
            item.appendChild(createGroupVignette(groupDisplayName(g), 'sm', { icon: groupIconClass(g) }));
            item.addEventListener('click', async () => {
                dropdown.classList.add('hidden');
                await this._addGroupMember(g.id);
            });
            dropdown.appendChild(item);
        }
        for (const c of userHits) {
            const item = document.createElement('div');
            item.className = 'groups-modal__add-item';
            item.tabIndex = 0;
            item.appendChild(createUserVignette(c.id, 'sm', { showEmail: true }));
            item.addEventListener('click', async () => {
                dropdown.classList.add('hidden');
                await this._addUserMember(c.id);
            });
            dropdown.appendChild(item);
        }
        dropdown.classList.remove('hidden');
    },

    /**
     * Inline rename form. Replaces the detail view body. Same singleton-modal
     * constraint as `_renderCreateInto` — we keep all forms inside the panel.
     * @param {GroupItem} group
     */
    _promptRename(group) {
        if (!this._bodyEl) return;
        const root = this._bodyEl;
        root.replaceChildren();

        const header = document.createElement('div');
        header.className = 'groups-modal__detail-header';

        const back = document.createElement('button');
        back.type = 'button';
        back.className = 'btn btn-ghost groups-modal__back-btn';
        back.setAttribute('aria-label', i18n.t('groups.back_to_list', 'Back'));
        back.innerHTML = '<i class="fas fa-arrow-left"></i>';
        back.addEventListener('click', () => this._renderDetailInto(root));
        header.appendChild(back);

        const title = document.createElement('div');
        title.className = 'groups-modal__subtitle';
        title.textContent = i18n.t('groups.edit_dialog_title', 'Rename group');
        header.appendChild(title);
        root.appendChild(header);

        const form = document.createElement('div');
        form.className = 'groups-modal__form';

        const label = document.createElement('label');
        label.className = 'groups-modal__form-label';
        label.textContent = i18n.t('groups.name_label', 'Name');
        form.appendChild(label);

        const input = document.createElement('input');
        input.type = 'text';
        input.className = 'groups-modal__add-input';
        input.value = group.name;
        input.autocomplete = 'off';
        label.appendChild(input);

        const err = document.createElement('div');
        err.className = 'groups-modal__inline-error hidden';
        form.appendChild(err);

        const actions = document.createElement('div');
        actions.className = 'groups-modal__form-actions';

        const cancel = document.createElement('button');
        cancel.type = 'button';
        cancel.className = 'btn btn-ghost';
        cancel.textContent = i18n.t('actions.cancel', 'Cancel');
        cancel.addEventListener('click', () => this._renderDetailInto(root));

        const save = document.createElement('button');
        save.type = 'button';
        save.className = 'btn btn-primary';
        save.textContent = i18n.t('actions.rename', 'Rename');

        const submit = async () => {
            const value = input.value.trim();
            if (!value || value === group.name) {
                this._renderDetailInto(root);
                return;
            }
            save.disabled = true;
            try {
                const updated = await groups.rename(group.id, value);
                this._currentGroup = updated;
                this._renderDetailInto(root);
            } catch (e) {
                err.textContent = /** @type {Error} */ (e).message;
                err.classList.remove('hidden');
                save.disabled = false;
            }
        };

        save.addEventListener('click', submit);
        input.addEventListener('keydown', (e) => {
            if (e.key === 'Enter') {
                e.preventDefault();
                submit();
            } else if (e.key === 'Escape') {
                e.preventDefault();
                this._renderDetailInto(root);
            }
        });

        actions.appendChild(cancel);
        actions.appendChild(save);
        form.appendChild(actions);
        root.appendChild(form);

        setTimeout(() => {
            input.focus();
            input.select();
        }, 0);
    },

    /**
     * Inline confirmation form for group deletion. Requires the user to type
     * the group name (safer than DELETE keyword — name is visible above).
     * @param {GroupItem} group
     */
    _confirmDelete(group) {
        if (!this._bodyEl) return;
        const root = this._bodyEl;
        root.replaceChildren();

        const header = document.createElement('div');
        header.className = 'groups-modal__detail-header';

        const back = document.createElement('button');
        back.type = 'button';
        back.className = 'btn btn-ghost groups-modal__back-btn';
        back.setAttribute('aria-label', i18n.t('groups.back_to_list', 'Back'));
        back.innerHTML = '<i class="fas fa-arrow-left"></i>';
        back.addEventListener('click', () => this._renderDetailInto(root));
        header.appendChild(back);

        const title = document.createElement('div');
        title.className = 'groups-modal__subtitle';
        title.textContent = i18n.t('groups.delete_group', 'Delete group');
        header.appendChild(title);
        root.appendChild(header);

        const form = document.createElement('div');
        form.className = 'groups-modal__form';

        const warning = document.createElement('div');
        warning.className = 'groups-modal__inline-error';
        warning.textContent = i18n.t('groups.delete_confirm', 'Delete the group "{name}"?').replace('{name}', group.name);
        form.appendChild(warning);

        const label = document.createElement('label');
        label.className = 'groups-modal__form-label';
        label.textContent = i18n.t('groups.delete_confirm_label', 'Type the group name to confirm:');
        form.appendChild(label);

        const input = document.createElement('input');
        input.type = 'text';
        input.className = 'groups-modal__add-input';
        input.autocomplete = 'off';
        input.placeholder = group.name;
        label.appendChild(input);

        const err = document.createElement('div');
        err.className = 'groups-modal__inline-error hidden';
        form.appendChild(err);

        const actions = document.createElement('div');
        actions.className = 'groups-modal__form-actions';

        const cancel = document.createElement('button');
        cancel.type = 'button';
        cancel.className = 'btn btn-ghost';
        cancel.textContent = i18n.t('actions.cancel', 'Cancel');
        cancel.addEventListener('click', () => this._renderDetailInto(root));

        const del = document.createElement('button');
        del.type = 'button';
        del.className = 'btn btn-danger';
        del.textContent = i18n.t('actions.delete', 'Delete');

        const submit = async () => {
            if (input.value !== group.name) {
                err.textContent = i18n.t('groups.delete_confirm_mismatch', 'Type the group name exactly to confirm.');
                err.classList.remove('hidden');
                input.focus();
                return;
            }
            del.disabled = true;
            try {
                await groups.deleteGroup(group.id);
                this._currentGroup = null;
                this._members = [];
                this._renderListInto(root);
            } catch (e) {
                err.textContent = /** @type {Error} */ (e).message;
                err.classList.remove('hidden');
                del.disabled = false;
            }
        };

        del.addEventListener('click', submit);
        input.addEventListener('keydown', (e) => {
            if (e.key === 'Enter') {
                e.preventDefault();
                submit();
            } else if (e.key === 'Escape') {
                e.preventDefault();
                this._renderDetailInto(root);
            }
        });

        actions.appendChild(cancel);
        actions.appendChild(del);
        form.appendChild(actions);
        root.appendChild(form);

        setTimeout(() => input.focus(), 0);
    },

    /** @param {string} userId */
    async _addUserMember(userId) {
        const group = this._currentGroup;
        if (!group || !this._bodyEl) return;
        try {
            await groups.addUserMember(group.id, userId);
            this._members = await groups.listMembers(group.id);
            this._renderDetailInto(this._bodyEl);
        } catch (err) {
            this._showInlineError(/** @type {Error} */ (err).message);
        }
    },

    /** @param {string} groupId */
    async _addGroupMember(groupId) {
        const group = this._currentGroup;
        if (!group || !this._bodyEl) return;
        try {
            await groups.addGroupMember(group.id, groupId);
            this._members = await groups.listMembers(group.id);
            this._renderDetailInto(this._bodyEl);
        } catch (err) {
            this._showInlineError(/** @type {Error} */ (err).message);
        }
    },

    /** @param {GroupMemberItem} m */
    async _removeMember(m) {
        const group = this._currentGroup;
        if (!group || !this._bodyEl) return;
        try {
            if (m.kind === 'user') {
                await groups.removeUserMember(group.id, m.id);
            } else {
                await groups.removeGroupMember(group.id, m.id);
            }
            this._members = await groups.listMembers(group.id);
            this._renderDetailInto(this._bodyEl);
        } catch (err) {
            this._showInlineError(/** @type {Error} */ (err).message);
        }
    },

    /** @param {string} message */
    _showInlineError(message) {
        if (!this._bodyEl) return;
        const existing = this._bodyEl.querySelector('.groups-modal__inline-error');
        if (existing) existing.remove();
        const el = document.createElement('div');
        el.className = 'groups-modal__inline-error';
        el.textContent = message;
        this._bodyEl.prepend(el);
        setTimeout(() => el.remove(), 5000);
    },

    /** @param {string} message */
    _showFatalError(message) {
        if (!this._bodyEl) return;
        this._bodyEl.replaceChildren();
        const el = document.createElement('div');
        el.className = 'groups-modal__error';
        el.textContent = message;
        this._bodyEl.appendChild(el);
    }
};

export { groupsView };
