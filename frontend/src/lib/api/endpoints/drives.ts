/**
 * Drives endpoints. D0 ships read-only listing; mutations (create / rename /
 * member changes) land in D2/D3 and will be added here under the same shape.
 *
 * Consumers usually go through the `drives` store (`$lib/stores/drives.svelte`)
 * which dedupes the request and caches the list — touch this module directly
 * only when bypassing the cache is intentional (e.g. an explicit refresh).
 */
import { apiJson } from '$lib/api/client';
import type { Drive } from '$lib/api/types';

/** `GET /api/drives` — every drive the caller can read, default first by convention. */
export function listDrives(): Promise<Drive[]> {
	return apiJson<Drive[]>('/api/drives', { credentials: 'same-origin' });
}
