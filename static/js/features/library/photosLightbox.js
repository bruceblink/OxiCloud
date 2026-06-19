/**
 * OxiCloud - Photos Lightbox
 * Full-screen image/video viewer with prev/next navigation.
 *
 * Media is never buffered in page memory: videos stream straight from the
 * API (the element's `src` is same-origin, so auth cookies travel
 * automatically and the browser issues Range requests — playback starts
 * progressively and seeking works without downloading the whole file).
 * Photos open with the server-cached `large` thumbnail; the full-resolution
 * original streams in only on demand via the toolbar expand button.
 */

import { Modal } from '../../components/modal.js';
import { getCsrfHeaders } from '../../core/csrf.js';
import { i18n } from '../../core/i18n.js';
import { favorites } from '../library/favorites.js';

/** @import {FileItem, FileMetadata} from '../../core/types.js' */
/** @typedef {typeof import('./photos.js').photosView} PhotosView */

export const photosLightbox = {
    /** @type {Array<FileItem>} Items array reference */
    items: [],
    /** @type {number} Current index */
    index: -1,
    /** @type {HTMLElement|null} */
    _overlay: null,
    /** @type {(ev: KeyboardEvent) => any|null} */
    _keyHandler: null,
    /** @type {PhotosView|null} Reference to photosView, set after both modules load */
    _photosView: null,
    /**
     * Monotonic token identifying the most recent {@link photosLightbox._show}
     * call. Image load/error callbacks fire asynchronously, so a rapid
     * prev/next must not let a superseded item commit its (stale) content
     * over the newer one.
     */
    _showGeneration: 0,

    /** @type {number} Current zoom factor (1 = fit) */
    _zoom: 1,
    /** @type {number} */
    _panX: 0,
    /** @type {number} */
    _panY: 0,
    /** @type {Map<number, {x: number, y: number}>} Active pointers (for pinch) */
    _pointers: new Map(),
    /** @type {number} */
    _pinchStartDist: 0,
    /** @type {number} */
    _pinchStartZoom: 1,
    /** @type {{x: number, y: number, panX: number, panY: number}|null} */
    _dragStart: null,
    /** @type {{x: number, y: number, t: number}|null} */
    _swipeStart: null,

    /**
     * Register the photosView reference (called from photos.js to avoid circular imports).
     * @param {any} pv
     */
    setPhotosView(pv) {
        this._photosView = pv;
    },

    /** Auth headers */
    _headers() {
        return getCsrfHeaders();
    },

    /**
     * Streaming URL of the original file. Same-origin, so media elements
     * send the auth cookie automatically and the browser handles Range.
     * @param {FileItem} item
     * @returns {string}
     */
    _originalUrl(item) {
        return `/api/files/${item.id}?inline=true`;
    },

    /**
     * URL of the server-cached `large` thumbnail (immutable, browser-cached).
     * @param {FileItem} item
     * @returns {string}
     */
    _thumbUrl(item) {
        return `/api/files/${item.id}/thumbnail/large`;
    },

    /**
     * Open lightbox at given index
     * @param {FileItem[]} items
     * @param {number} index
     */
    open(items, index) {
        this.items = items;
        this.index = index;
        this._createOverlay();
        this._show();
        this._bindKeys();
    },

    /** Close lightbox */
    close() {
        if (this._overlay) {
            this._overlay.classList.remove('active');
            setTimeout(() => {
                if (this._overlay) {
                    this._overlay.remove();
                    this._overlay = null;
                }
            }, 200);
        }
        this._resetZoom();
        this._unbindKeys();
    },

    /** Navigate to previous */
    prev() {
        if (this.index > 0) {
            this.index--;
            this._show();
        }
    },

    /** Navigate to next */
    next() {
        if (this.index < this.items.length - 1) {
            this.index++;
            this._show();
        }
    },

    /** Create the overlay DOM structure */
    _createOverlay() {
        if (this._overlay) this._overlay.remove();

        const el = document.createElement('div');
        el.className = 'photos-lightbox';
        el.innerHTML = `
            <div class="lightbox-info">
                <div class="lightbox-filename"></div>
                <div class="lightbox-meta"></div>
            </div>
            <button class="lightbox-close"><i class="fas fa-times"></i></button>
            <button class="lightbox-nav lightbox-prev"><i class="fas fa-chevron-left"></i></button>
            <div class="lightbox-content"></div>
            <button class="lightbox-nav lightbox-next"><i class="fas fa-chevron-right"></i></button>
            <div class="lightbox-toolbar">
                <button class="lb-fullres hidden" title="Full resolution"><i class="fas fa-expand"></i></button>
                <button class="lb-info" title="Info"><i class="fas fa-circle-info"></i></button>
                <button class="lb-download" title="Download"><i class="fas fa-download"></i></button>
                <button class="lb-favorite" title="Favorite"><i class="far fa-star"></i></button>
                <button class="lb-delete" title="Delete"><i class="fas fa-trash"></i></button>
            </div>
            <div class="lightbox-counter"></div>
            <div class="lightbox-infopanel hidden"></div>
        `;
        document.body.appendChild(el);
        this._overlay = el;

        // Event listeners
        /** @type {HTMLButtonElement} */ (el.querySelector('.lightbox-close')).onclick = () => this.close();
        /** @type {HTMLButtonElement} */ (el.querySelector('.lightbox-prev')).onclick = () => this.prev();
        /** @type {HTMLButtonElement} */ (el.querySelector('.lightbox-next')).onclick = () => this.next();

        // Click backdrop to close
        el.addEventListener('click', (e) => {
            if (e.target === el || /** @type {HTMLElement} */ (e.target).classList.contains('lightbox-content')) {
                this.close();
            }
        });

        // Toolbar actions (`.lb-fullres` is wired per-item in `_show`)
        /** @type {HTMLButtonElement} */ (el.querySelector('.lb-info')).onclick = () => this._toggleInfoPanel();
        /** @type {HTMLButtonElement} */ (el.querySelector('.lb-download')).onclick = () => this._download();
        /** @type {HTMLButtonElement} */ (el.querySelector('.lb-favorite')).onclick = () => this._toggleFavorite();
        /** @type {HTMLButtonElement} */ (el.querySelector('.lb-delete')).onclick = () => this._delete();

        // Animate in
        requestAnimationFrame(() => el.classList.add('active'));
    },

    /** Preload the immediate prev/next thumbnails so navigation is instant. */
    _preloadNeighbors() {
        [this.index - 1, this.index + 1].forEach((i) => {
            const it = this.items[i];
            if (it && !it.mime_type?.startsWith('video/')) {
                const pre = new Image();
                pre.src = this._thumbUrl(it);
            }
        });
    },

    /** Display the current item */
    _show() {
        if (!this._overlay || this.index < 0) return;
        const generation = ++this._showGeneration;
        this._resetZoom();

        const item = this.items[this.index];
        const content = this._overlay.querySelector('.lightbox-content');
        const filename = this._overlay.querySelector('.lightbox-filename');
        const meta = this._overlay.querySelector('.lightbox-meta');
        const counter = this._overlay.querySelector('.lightbox-counter');

        filename.textContent = item.name;
        counter.textContent = `${this.index + 1} / ${this.items.length}`;

        // Reflect the current favorite state on the toolbar star.
        const favBtn = this._overlay.querySelector('.lb-favorite');
        if (favBtn) {
            const isFav = favorites.isFavorite(item.id, 'file');
            favBtn.classList.toggle('active', isFav);
            const favIcon = favBtn.querySelector('i');
            if (favIcon) favIcon.className = isFav ? 'fas fa-star' : 'far fa-star';
        }

        // Format date
        const ts = (item.sort_date || item.created_at) * 1000;
        const dateStr = new Date(ts).toLocaleDateString(undefined, {
            year: 'numeric',
            month: 'short',
            day: 'numeric',
            hour: '2-digit',
            minute: '2-digit'
        });
        meta.textContent = `${dateStr} · ${item.size_formatted || ''}`;

        // Update nav button visibility
        /** @type {HTMLButtonElement} */ (this._overlay.querySelector('.lightbox-prev')).classList.toggle('hidden', !(this.index > 0));
        /** @type {HTMLButtonElement} */ (this._overlay.querySelector('.lightbox-next')).classList.toggle('hidden', !(this.index < this.items.length - 1));

        // Reset the full-resolution button for the new item
        const fullResBtn = /** @type {HTMLButtonElement} */ (this._overlay.querySelector('.lb-fullres'));
        fullResBtn.classList.add('hidden');
        fullResBtn.disabled = false;
        const fullResIcon = fullResBtn.querySelector('i');
        if (fullResIcon) fullResIcon.className = 'fas fa-expand';

        // Preload neighbours so prev/next is instant.
        this._preloadNeighbors();

        // Load content
        content.innerHTML = '<div class="photos-loading"><i class="fas fa-spinner"></i></div>';

        if (item.mime_type?.startsWith('video/')) {
            const video = document.createElement('video');
            video.controls = true;
            video.autoplay = true;
            // Instant first frame while metadata loads; a 204 (no cached
            // thumbnail yet) simply leaves the poster blank.
            video.poster = this._thumbUrl(item);
            video.src = this._originalUrl(item);
            video.addEventListener('error', () => {
                if (generation !== this._showGeneration) return;
                content.innerHTML = '<div class="photos-loading">Failed to load</div>';
            });
            // The native player has its own buffering UI — drop the spinner now.
            content.replaceChildren(video);
            this._loadMetadata(item.id, meta, dateStr, item.size_formatted || '');
            return;
        }

        // Photo: thumbnail first, original on demand. GIFs go straight to
        // the original — the thumbnail is a static JPEG and would lose the
        // animation.
        const isGif = item.mime_type === 'image/gif';
        let showingOriginal = isGif;
        const img = document.createElement('img');
        img.alt = item.name;
        this._wireZoomPan(img);

        img.addEventListener('load', () => {
            if (generation !== this._showGeneration) return;
            // First load replaces the spinner; the on-demand swap reuses the
            // already-attached element.
            if (!img.isConnected) content.replaceChildren(img);
            fullResBtn.classList.toggle('hidden', showingOriginal);
            fullResBtn.disabled = false;
            if (fullResIcon) fullResIcon.className = 'fas fa-expand';
        });

        img.addEventListener('error', () => {
            if (generation !== this._showGeneration) return;
            if (!showingOriginal) {
                // No server thumbnail (unsupported format or generation
                // failed) — fall back to the original.
                showingOriginal = true;
                img.src = this._originalUrl(item);
            } else {
                content.innerHTML = '<div class="photos-loading">Failed to load</div>';
                fullResBtn.classList.add('hidden');
            }
        });

        fullResBtn.onclick = () => {
            if (generation !== this._showGeneration || showingOriginal) return;
            showingOriginal = true;
            fullResBtn.disabled = true;
            if (fullResIcon) fullResIcon.className = 'fas fa-spinner fa-spin';
            img.src = this._originalUrl(item);
        };

        img.src = showingOriginal ? this._originalUrl(item) : this._thumbUrl(item);

        // Load EXIF metadata
        this._loadMetadata(item.id, meta, dateStr, item.size_formatted || '');
    },

    /**
     * Load EXIF metadata for info bar
     * @param {string} fileId
     * @param {Element} metaEl
     * @param {string} dateStr
     * @param {string} sizeStr
     */
    async _loadMetadata(fileId, metaEl, dateStr, sizeStr) {
        try {
            const res = await fetch(`/api/files/${fileId}/metadata`, {
                credentials: 'include',
                headers: this._headers()
            });
            if (res.ok) {
                const metadata = /** @type {FileMetadata} */ (await res.json());
                const parts = [dateStr];
                if (sizeStr) parts.push(sizeStr);
                if (metadata.camera_make || metadata.camera_model) {
                    parts.push([metadata.camera_make, metadata.camera_model].filter(Boolean).join(' '));
                }
                if (metadata.width && metadata.height) {
                    parts.push(`${metadata.width}×${metadata.height}`);
                }
                metaEl.textContent = parts.join(' · ');
                this._fillInfoPanel(metadata, dateStr, sizeStr);
            }
        } catch (_err) {
            // Non-critical, keep existing meta
        }
    },

    /** Download current item */
    _download() {
        const item = this.items[this.index];
        if (!item) return;
        const a = document.createElement('a');
        a.href = `/api/files/${item.id}`;
        a.download = item.name;
        document.body.appendChild(a);
        a.click();
        a.remove();
    },

    /** Toggle favorite on current item (via the favorites module so its
     *  cache stays in sync — the lightbox can then show the right initial
     *  star next time the item is opened). */
    async _toggleFavorite() {
        const item = this.items[this.index];
        if (!item) return;
        const isFav = favorites.isFavorite(item.id, 'file');
        try {
            if (isFav) {
                await favorites.removeFromFavorites(item.id, 'file', item.name);
            } else {
                await favorites.addToFavorites(item.id, item.name, 'file', null);
            }
            const btn = this._overlay?.querySelector('.lb-favorite');
            if (btn) {
                const nowFav = !isFav;
                btn.classList.toggle('active', nowFav);
                const icon = btn.querySelector('i');
                if (icon) icon.className = nowFav ? 'fas fa-star' : 'far fa-star';
            }
        } catch (err) {
            console.error('Favorite toggle failed:', err);
        }
    },

    /** Delete current item */
    async _delete() {
        const item = this.items[this.index];
        if (!item) return;
        const ok = await Modal.confirmDialog({
            title: i18n.t('photos.delete_title'),
            message: i18n.t('photos.delete_one_confirm', { name: item.name }),
            confirmText: i18n.t('actions.delete'),
            icon: 'fa-trash'
        });
        if (!ok) return;

        try {
            await fetch(`/api/files/${item.id}`, {
                method: 'DELETE',
                credentials: 'include',
                headers: this._headers()
            });
            // Remove from photosView items too
            if (this._photosView) {
                this._photosView.items = this._photosView.items.filter((f) => f.id !== item.id);
            }
            this.items.splice(this.index, 1);
            if (this.items.length === 0) {
                this.close();
                if (this._photosView) this._photosView._renderFull(); // will call renderEmpty() on this case
            } else {
                if (this.index >= this.items.length) this.index = this.items.length - 1;
                this._show();
                if (this._photosView) this._photosView._renderFull();
            }
        } catch (err) {
            console.error('Delete failed:', err);
        }
    },

    // ── Zoom / pan / swipe ──────────────────────────────────────────

    /** @returns {HTMLImageElement|null} The image element currently shown. */
    _currentImg() {
        return /** @type {HTMLImageElement|null} */ (this._overlay?.querySelector('.lightbox-content img') || null);
    },

    /** Reset zoom/pan state (per item and on close). */
    _resetZoom() {
        this._zoom = 1;
        this._panX = 0;
        this._panY = 0;
        this._pointers.clear();
        this._pinchStartDist = 0;
        this._dragStart = null;
        this._swipeStart = null;
    },

    _applyTransform() {
        const img = this._currentImg();
        if (img) img.style.transform = `translate(${this._panX}px, ${this._panY}px) scale(${this._zoom})`;
    },

    /**
     * Set the zoom factor (clamped 1–5), centered. Resets pan at 1.
     * @param {number} z
     */
    _setZoom(z) {
        z = Math.min(Math.max(z, 1), 5);
        if (z === 1) {
            this._panX = 0;
            this._panY = 0;
        }
        this._zoom = z;
        this._applyTransform();
        const img = this._currentImg();
        if (img) img.classList.toggle('is-zoomed', z > 1);
    },

    /**
     * Wire wheel-zoom, double-click zoom, drag-pan, pinch-zoom and (when not
     * zoomed) touch swipe-to-navigate onto a photo element.
     * @param {HTMLImageElement} img
     */
    _wireZoomPan(img) {
        img.style.transformOrigin = 'center center';
        img.style.touchAction = 'none';

        img.addEventListener(
            'wheel',
            (e) => {
                e.preventDefault();
                this._setZoom(this._zoom * (e.deltaY < 0 ? 1.2 : 1 / 1.2));
            },
            { passive: false }
        );

        img.addEventListener('dblclick', (e) => {
            e.preventDefault();
            this._setZoom(this._zoom > 1 ? 1 : 2.5);
        });

        img.addEventListener('pointerdown', (e) => {
            img.setPointerCapture?.(e.pointerId);
            this._pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
            if (this._pointers.size === 2) {
                const pts = [...this._pointers.values()];
                this._pinchStartDist = Math.hypot(pts[0].x - pts[1].x, pts[0].y - pts[1].y);
                this._pinchStartZoom = this._zoom;
            } else {
                this._dragStart = { x: e.clientX, y: e.clientY, panX: this._panX, panY: this._panY };
                this._swipeStart = { x: e.clientX, y: e.clientY, t: Date.now() };
            }
        });

        img.addEventListener('pointermove', (e) => {
            if (!this._pointers.has(e.pointerId)) return;
            this._pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
            if (this._pointers.size === 2 && this._pinchStartDist > 0) {
                const pts = [...this._pointers.values()];
                const dist = Math.hypot(pts[0].x - pts[1].x, pts[0].y - pts[1].y);
                this._setZoom(this._pinchStartZoom * (dist / this._pinchStartDist));
            } else if (this._zoom > 1 && this._dragStart) {
                this._panX = this._dragStart.panX + (e.clientX - this._dragStart.x);
                this._panY = this._dragStart.panY + (e.clientY - this._dragStart.y);
                this._applyTransform();
            }
        });

        const endPointer = (/** @type {PointerEvent} */ e) => {
            const wasPinch = this._pointers.size === 2;
            this._pointers.delete(e.pointerId);
            if (!wasPinch && this._zoom === 1 && this._swipeStart && e.pointerType === 'touch') {
                const dx = e.clientX - this._swipeStart.x;
                const dy = e.clientY - this._swipeStart.y;
                if (Math.abs(dx) > 50 && Math.abs(dx) > Math.abs(dy) * 1.5) {
                    if (dx > 0) this.prev();
                    else this.next();
                }
            }
            if (wasPinch) this._pinchStartDist = 0;
            this._dragStart = null;
            this._swipeStart = null;
        };
        img.addEventListener('pointerup', endPointer);
        img.addEventListener('pointercancel', endPointer);
    },

    // ── Info panel ──────────────────────────────────────────────────

    /** Toggle the EXIF info panel. */
    _toggleInfoPanel() {
        this._overlay?.querySelector('.lightbox-infopanel')?.classList.toggle('hidden');
    },

    /**
     * Populate the info panel from fetched EXIF metadata.
     * @param {FileMetadata} metadata
     * @param {string} dateStr
     * @param {string} sizeStr
     */
    _fillInfoPanel(metadata, dateStr, sizeStr) {
        const panel = this._overlay?.querySelector('.lightbox-infopanel');
        if (!panel) return;
        const item = this.items[this.index];
        const rows = [this._infoRow('fa-image', item?.name || ''), this._infoRow('fa-calendar', dateStr)];
        if (sizeStr) rows.push(this._infoRow('fa-hard-drive', sizeStr));
        if (metadata.width && metadata.height) {
            rows.push(this._infoRow('fa-ruler-combined', `${metadata.width} × ${metadata.height}`));
        }
        if (metadata.camera_make || metadata.camera_model) {
            rows.push(this._infoRow('fa-camera', [metadata.camera_make, metadata.camera_model].filter(Boolean).join(' ')));
        }
        if (metadata.latitude != null && metadata.longitude != null) {
            rows.push(this._infoRow('fa-location-dot', `${metadata.latitude.toFixed(5)}, ${metadata.longitude.toFixed(5)}`));
        }
        panel.innerHTML = rows.join('');
    },

    /**
     * @param {string} icon FontAwesome class
     * @param {string} text
     * @returns {string}
     */
    _infoRow(icon, text) {
        const d = document.createElement('div');
        d.textContent = text;
        return `<div class="lb-info-row"><i class="fas ${icon}"></i><span>${d.innerHTML}</span></div>`;
    },

    /** Keyboard navigation */
    _bindKeys() {
        this._keyHandler = (e) => {
            if (e.key === 'Escape') this.close();
            else if (e.key === 'ArrowLeft') this.prev();
            else if (e.key === 'ArrowRight') this.next();
        };
        document.addEventListener('keydown', this._keyHandler);
    },

    _unbindKeys() {
        if (this._keyHandler) {
            document.removeEventListener('keydown', this._keyHandler);
            this._keyHandler = null;
        }
    }
};
