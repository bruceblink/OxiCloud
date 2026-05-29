/**
 * linkChip — inline clickable element representing a share link.
 *
 * Renders:  [🔒/🔗]  Link - ...{last4 of UUID} - {name}
 * Clicking copies the share URL to the clipboard.
 */

import { i18n } from '../core/i18n.js';
import { fileSharing } from '../features/sharing/fileSharing.js';

/** @import {OutgoingResourceGrant} from '../core/types.js' */

/**
 * Build an inline link chip. Clicking it copies the share URL.
 * @param {OutgoingResourceGrant} grant
 * @returns {HTMLButtonElement}
 */
function buildLinkChip(grant) {
    const btn = /** @type {HTMLButtonElement} */ (document.createElement('button'));
    btn.className = `link-chip${grant.has_password ? ' link-chip--locked' : ''}`;
    btn.type = 'button';
    btn.title = i18n.t('share.copyLink', 'Copy link');

    const icon = document.createElement('i');
    icon.className = grant.has_password ? 'fas fa-lock link-chip__icon' : 'fas fa-link link-chip__icon';
    btn.appendChild(icon);

    const last4 = grant.subject_id.slice(-4);
    const label = `${i18n.t('share.link', 'Link')} - ...${last4} - ${grant.subject_display}`;

    const text = document.createElement('span');
    text.className = 'link-chip__label';
    text.textContent = label;
    btn.appendChild(text);

    btn.addEventListener('click', async (e) => {
        e.preventDefault();
        e.stopPropagation();
        btn.disabled = true;
        try {
            const share = await fileSharing.getShareById(grant.subject_id);
            await fileSharing.copyLinkToClipboard(share.url);
        } catch (err) {
            console.error('linkChip: copy failed', err);
        } finally {
            btn.disabled = false;
        }
    });

    return btn;
}

export { buildLinkChip };
