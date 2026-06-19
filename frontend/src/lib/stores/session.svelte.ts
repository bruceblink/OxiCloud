/**
 * Session store — the authenticated user and derived flags.
 *
 * Replaces the user-related fields of the original `app` state object
 * (isExternalUser, userHomeFolderId/Name). `isExternalUser` drives default
 * routing: externals (magic-link / OIDC-only / OCM recipients) have no home
 * folder and land on the shared-with-me view.
 */
import { fetchMe, tryRefresh } from '$lib/api/endpoints/auth';
import { drives } from '$lib/stores/drives.svelte';
import type { User } from '$lib/api/types';

class SessionStore {
	user = $state<User | null>(null);
	loaded = $state(false);
	homeFolderId = $state<string | null>(null);
	homeFolderName = $state<string | null>(null);

	isExternalUser = $derived(this.user?.is_external ?? false);
	isAuthenticated = $derived(this.user !== null);

	/**
	 * Resolve the session once. Probes /api/auth/me; on 401 it makes a single
	 * refresh attempt and re-probes. Never redirects — the layout guard decides
	 * what to do with an unauthenticated result. Idempotent: subsequent calls
	 * return the cached result (so client-side navigation doesn't re-probe).
	 */
	async load(): Promise<User | null> {
		if (this.loaded) return this.user;
		try {
			let me = await fetchMe();
			if (!me && (await tryRefresh())) {
				me = await fetchMe();
			}
			this.user = me;
		} catch {
			this.user = null;
		}
		this.loaded = true;
		return this.user;
	}

	/**
	 * Resolve the caller's default personal drive's root folder — the landing
	 * point for `/files` and the `/` redirect. Externals (grant-only) have no
	 * personal drive, so this is skipped for them.
	 *
	 * Identifies the default via `default_for_user`, not folder name: users
	 * can rename "Personal" without breaking this lookup.
	 */
	async loadHomeFolder(): Promise<string | null> {
		if (this.homeFolderId) return this.homeFolderId;
		if (this.isExternalUser) return null;
		await drives.load();
		const def = drives.findDefault();
		if (def) {
			this.homeFolderId = def.root_folder_id;
			this.homeFolderName = def.name;
		}
		return this.homeFolderId;
	}

	reset(): void {
		this.user = null;
		this.homeFolderId = null;
		this.homeFolderName = null;
	}
}

export const session = new SessionStore();
