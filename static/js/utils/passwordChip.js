/**
 * buildPasswordChip — shared inline password editor chip.
 *
 * States:
 *   • "🔓 No password"      — unset (default)
 *   • "🔒 Password  ×"      — set; × clears it
 *
 * Clicking the chip shows a hidden <input type="password">.
 * Blur / Enter confirms; Escape cancels.
 * CSS classes (.smd-expiry-chip-wrap, .smd-expiry-chip, .smd-expiry-chip--set,
 * .smd-expiry-date-input) live in shareModal.css.
 */

import { i18n } from '../core/i18n.js';

/**
 * @param {boolean}              initialHasPassword
 * @param {(v: string) => void}  onChange  '' = remove, non-empty = set new password
 * @returns {HTMLElement}
 */
export function buildPasswordChip(initialHasPassword, onChange) {
    let hasPassword = initialHasPassword;

    const wrap = document.createElement('div');
    wrap.className = 'smd-expiry-chip-wrap';

    const chip = document.createElement('button');
    chip.type = 'button';

    const pwInput = document.createElement('input');
    pwInput.type = 'password';
    pwInput.className = 'smd-expiry-date-input hidden';
    pwInput.placeholder = i18n.t('dialogs.password', 'Password');
    pwInput.autocomplete = 'new-password';

    const updateChip = () => {
        if (hasPassword) {
            chip.className = 'smd-expiry-chip smd-expiry-chip--set';
            chip.innerHTML =
                `<i class="fas fa-lock"></i> ${i18n.t('share.passwordProtected', 'Password')}` +
                `<span class="smd-expiry-chip-clear" title="${i18n.t('actions.clear', 'Clear')}">×</span>`;
            chip.querySelector('.smd-expiry-chip-clear')?.addEventListener('click', (e) => {
                e.stopPropagation();
                hasPassword = false;
                onChange('');
                updateChip();
            });
        } else {
            chip.className = 'smd-expiry-chip';
            chip.innerHTML = `<i class="fas fa-lock-open"></i> ${i18n.t('share.noPassword', 'No password')}`;
        }
    };

    chip.addEventListener('click', () => {
        chip.classList.add('hidden');
        pwInput.value = '';
        pwInput.classList.remove('hidden');
        pwInput.focus();
    });

    const confirm = () => {
        const val = pwInput.value;
        pwInput.classList.add('hidden');
        chip.classList.remove('hidden');
        if (val) {
            hasPassword = true;
            onChange(val);
        }
        updateChip();
    };

    pwInput.addEventListener('blur', confirm);
    pwInput.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') {
            e.preventDefault();
            confirm();
        }
        if (e.key === 'Escape') {
            pwInput.classList.add('hidden');
            chip.classList.remove('hidden');
        }
    });

    updateChip();
    wrap.appendChild(chip);
    wrap.appendChild(pwInput);
    return wrap;
}
