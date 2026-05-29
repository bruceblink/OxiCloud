/**
 * OxiCloud - Shared format and escaping utilities
 * Centralized global helpers for date/size/text formatting and XSS-safe escaping.
 * Contains also checkers
 */

import { i18n } from './i18n.js';

/**
 *
 * @param {string} str
 * @returns {string}
 */
function escapeHtml(str) {
    if (typeof str !== 'string') return '';
    return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#039;');
}

/**
 *
 * @param {number} bytes
 * @returns {string}
 */
function formatFileSize(bytes) {
    if (bytes === 0) return '0 Bytes';

    const k = 1024;
    const sizes = ['Bytes', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));

    return `${parseFloat((bytes / k ** i).toFixed(2))} ${sizes[i]}`;
}

/// Formats a byte count for quota display. When bytes is 0, returns "∞" (unlimited).
/**
 *
 * @param {number} bytes
 * @returns {string}
 */
function formatQuotaSize(bytes) {
    if (bytes === 0) return '∞';
    return formatFileSize(bytes);
}

/**
 *
 * @param {Date | number| null} value
 * @returns {string}
 */
function formatDateTime(value) {
    if (!value) return '';
    let dateValue;
    if (value instanceof Date) {
        dateValue = value;
    } else if (typeof value === 'number') {
        dateValue = new Date(value < 1e12 ? value * 1000 : value);
    } else {
        dateValue = new Date(value);
    }
    if (Number.isNaN(dateValue.getTime())) return String(value);
    return `${dateValue.toLocaleDateString()} ${dateValue.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`;
}

/**
 *
 * @param {Date | number| null} value
 * @returns {string}
 */
function formatDateShort(value) {
    if (!value) return 'N/A';
    const dateValue = typeof value === 'number' ? new Date(value * 1000) : new Date(value);
    if (Number.isNaN(dateValue.getTime())) return String(value);
    return dateValue.toLocaleDateString(undefined, {
        year: 'numeric',
        month: 'short',
        day: 'numeric'
    });
}

const TEXT_TYPES = [
    'application/json',
    'application/xml',
    'application/javascript',
    'application/x-sh',
    'application/x-yaml',
    'application/toml',
    'application/x-toml',
    'application/sql'
];
// FIXME: move is to another file
/**
 *
 * @param {string} mimeType
 * @returns {boolean}
 */
function isTextViewable(mimeType) {
    if (!mimeType) return false;
    if (mimeType.startsWith('text/')) return true;

    return TEXT_TYPES.includes(mimeType);
}

/**
 * Chekif an email is valid
 * @param {string} email
 * @returns boolean
 */
function isEmailValid(email) {
    return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email);
}

/**
 * Normalize a date value into a human-readable bucket label.
 * Buckets (newest-first): Today | Last 7 days | Last 30 days | <YYYY>
 *
 * Accepts:
 *  - `string` — ISO-8601 date string (e.g. `granted_at` from the API)
 *  - `number` — Unix timestamp in **seconds** (e.g. `sort_date`, `modified_at`)
 *               Values < 1e12 are treated as seconds; larger values as milliseconds.
 *  - `Date`   — JavaScript Date object
 *
 * @param {string | number | Date} value
 * @returns {string}
 */
function normalizeDateBucket(value) {
    let date;
    if (value instanceof Date) {
        date = value;
    } else if (typeof value === 'number') {
        date = new Date(value < 1e12 ? value * 1000 : value);
    } else {
        date = new Date(value);
    }
    const diffDays = Math.floor((Date.now() - date.getTime()) / 86_400_000);
    if (diffDays === 0) return i18n.t('dateBucket.today', 'Today');
    if (diffDays <= 7) return i18n.t('dateBucket.last7days', 'Last 7 days');
    if (diffDays <= 30) return i18n.t('dateBucket.last30days', 'Last 30 days');
    return String(date.getFullYear());
}

/**
 * Normalize a future expiry value into a human-readable bucket label.
 * Buckets (soonest-first): Expired | Tomorrow | In less than 7 days | In less than 30 days | <YYYY> | No expiration
 *
 * Accepts the same input types as `normalizeDateBucket`.
 *
 * @param {string | number | Date | null | undefined} value
 * @returns {string}
 */
function normalizeExpiryBucket(value) {
    if (value === null || value === undefined) {
        return i18n.t('expiryBucket.noExpiry', 'No expiration');
    }
    /** @type {Date} */
    let date;
    if (value instanceof Date) {
        date = value;
    } else if (typeof value === 'number') {
        date = new Date(value < 1e12 ? value * 1000 : value);
    } else {
        date = new Date(value);
    }
    const daysUntil = Math.floor((date.getTime() - Date.now()) / 86_400_000);
    if (daysUntil < 0) return i18n.t('expiryBucket.expired', 'Expired');
    if (daysUntil <= 1) return i18n.t('expiryBucket.tomorrow', 'Tomorrow');
    if (daysUntil <= 7) return i18n.t('expiryBucket.week', 'In less than 7 days');
    if (daysUntil <= 30) return i18n.t('expiryBucket.month', 'In less than 30 days');
    return String(date.getFullYear());
}

/**
 * Maps a file size in bytes to a coarse, human-readable bucket label.
 *
 * Pass `-1` for folders — they sort before all files on the server and
 * receive the "Folders" label client-side.
 *
 * Buckets:
 *   -1                       → Folders
 *    0                       → Empty (0 B)
 *    1 – 1 048 575           → < 1 MB
 *    1 048 576 – 104 857 599 → 1 – 100 MB
 *    104 857 600 – 1 073 741 823 → 100 MB – 1 GB
 *    1 073 741 824 – 5 368 709 119 → 1 – 5 GB
 *    ≥ 5 368 709 120         → > 5 GB
 *
 * @param {number} bytes  File size in bytes, or -1 for folders.
 * @returns {string}
 */
// biome-ignore format: keep the following indent
function sizeBucket(bytes) {
    if (bytes < 0)                       return i18n.t('sizeBucket.folders', 'Folders');
    if (bytes === 0)                     return i18n.t('sizeBucket.empty',   'Empty (0 B)');
    if (bytes < 1_048_576)               return i18n.t('sizeBucket.tiny',    '< 1 MB');
    if (bytes < 104_857_600)             return i18n.t('sizeBucket.small',   '1 – 100 MB');
    if (bytes < 1_073_741_824)           return i18n.t('sizeBucket.medium',  '100 MB – 1 GB');
    if (bytes < 5 * 1_073_741_824)       return i18n.t('sizeBucket.large',   '1 – 5 GB');
    return                                      i18n.t('sizeBucket.huge',    '> 5 GB');
}

export {
    escapeHtml,
    formatDateShort,
    formatDateTime,
    formatFileSize,
    formatQuotaSize,
    isEmailValid,
    isTextViewable,
    normalizeDateBucket,
    normalizeExpiryBucket,
    sizeBucket
};
