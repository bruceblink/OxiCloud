/**
 * API wire types — ported from static/js/core/types.js.
 *
 * This is a focused, hand-ported subset covering the core resources. The plan
 * is to regenerate the full set from the backend OpenAPI (`just openapi` +
 * `openapi-typescript`) so these track the Rust DTOs; until then, extend here.
 */

export type ItemType = 'file' | 'folder';

export interface LightItem {
	id: string;
	name: string;
	type: ItemType;
	parentId: string;
}

export interface FolderItem {
	category: string;
	created_at: number;
	icon_class: string;
	icon_special_class: string;
	id: string;
	is_root: boolean;
	modified_at: number;
	name: string;
	owner_id: string;
	parent_id: string | null;
	path: string;
	etag: string;
}

export interface FileItem {
	category: string;
	created_at: number;
	icon_class: string;
	icon_special_class: string;
	id: string;
	mime_type: string;
	modified_at: number;
	name: string;
	owner_id: string;
	folder_id: string;
	path: string;
	size: number;
	size_formatted: string;
	sort_date: number;
	etag: string;
	content_hash: string;
	/** Search-only: plain-text fragment around a content match. */
	snippet?: string;
	/** Search-only: "name" or "content". */
	match_source?: string;
}

export interface ShareItem {
	access_count: number;
	created_at: number;
	created_by: string;
	expires_at: number;
	has_password: boolean;
	id: string;
	item_id: string;
	item_name: string;
	item_type: ItemType;
	token: string | null;
	url: string;
}

export interface CreateShare {
	item_id: string;
	item_name?: string | null;
	item_type: ItemType;
	password: string | null;
	expires_at: number | null;
}

export interface UpdateShare {
	password?: string | null;
	expires_at?: number | null;
}

export interface FavoriteItem {
	id: string;
	user_id: string;
	item_id: string;
	item_type: ItemType;
	created_at: number;
	item_name: string | null;
	item_size: number | null;
	item_mime_type: string | null;
	parent_id: string | null;
	modified_at: number | null;
	item_path: string;
	icon_class: string;
	icon_special_class: string;
	category: string;
	size_formatted: string;
	owner_id: string | null;
}

export interface RecentItem {
	id: string;
	user_id: string;
	item_id: string;
	item_type: ItemType;
	accessed_at: number;
	item_name: string | null;
	item_size: number | null;
	item_mime_type: string | null;
	parent_id: string | null;
	item_path: string;
	icon_class: string;
	icon_special_class: string;
	category: string;
	size_formatted: string;
}

export interface TrashResourceItem {
	resource_type: ItemType;
	trashed_at: string;
	deletion_date: string;
	resource: FileItem | FolderItem;
}

export interface TrashResourcesResponse {
	items: TrashResourceItem[];
	next_cursor?: string;
}

export type Role = 'user' | 'admin';

/** Wire shape of `UserDto` (backend: src/application/dtos/user_dto.rs). */
export interface User {
	id: string;
	username?: string;
	email: string;
	role: string;
	storage_quota_bytes: number;
	storage_used_bytes: number;
	created_at: string;
	updated_at: string;
	last_login_at?: string | null;
	active: boolean;
	auth_provider: string;
	image?: string | null;
	can_edit_image: boolean;
	is_external: boolean;
	given_name?: string;
	family_name?: string;
	email_verified_at?: string;
	preferred_locale?: string;
	notify_on_share: boolean;
}

export interface AuthResponse {
	user: User;
	access_token: string;
	refresh_token: string;
	token_type: string;
	expires_in: number;
}

export type SortBy =
	| 'relevance'
	| 'name'
	| 'name_desc'
	| 'date'
	| 'date_desc'
	| 'size'
	| 'size_desc';

export interface SearchCriteria {
	sort_by: SortBy;
	recursive: boolean;
	limit: number;
	offset: number;
	name_contains?: string;
	file_types?: string[];
	folder_id?: string;
	min_size?: number;
	max_size?: number;
	created_before?: number;
	created_after?: number;
	modified_before?: number;
	modified_after?: number;
}

export interface SearchResults {
	files: FileItem[];
	folders: FolderItem[];
	total_count: number | null;
	limit: number;
	offset: number;
	has_more: boolean;
	query_time_ms: number;
	sort_by: string;
}

export type DriveKind = 'personal' | 'shared';

/**
 * One row from `GET /api/drives`. Mirrors `DriveDto` in
 * `src/application/dtos/drive_dto.rs`. `default_for_user` is the caller's
 * id when present, `null`/undefined otherwise — used to pick the default
 * personal drive without hard-coding name conventions.
 */
export interface Drive {
	id: string;
	name: string;
	kind: DriveKind;
	default_for_user?: string | null;
	root_folder_id: string;
	quota_bytes?: number | null;
	used_bytes: number;
	policies: Record<string, unknown>;
	created_at: string;
	updated_at: string;
}
