/** Display helpers shared across list views. */

/**
 * Map an `icon_class` (e.g. "fas fa-folder", "fa-file-pdf") to an icon
 * registry name (the FA token without the `fa-` prefix).
 */
export function iconNameFromClass(iconClass: string | undefined | null): string {
	if (!iconClass) return 'file';
	const token = iconClass
		.split(/\s+/)
		.find((c) => c.startsWith('fa-') && c !== 'fa-fw' && c !== 'fa-lg');
	return token ? token.slice(3) : 'file';
}

/**
 * Coarse colour bucket for a resolved icon name (see {@link iconNameFromClass}).
 * One hue per broad file family so the grid/list glyphs render in type-specific
 * colours instead of a flat monochrome. Each bucket is backed by a
 * `--file-kind-*` token (variables.css) and consumed via the `.file-icon--*`
 * modifier classes (resourceList.css).
 */
export function fileIconKind(iconName: string): string {
	switch (iconName) {
		case 'folder':
		case 'folder-open':
			return 'folder';
		case 'file-pdf':
			return 'pdf';
		case 'file-word':
			return 'doc';
		case 'file-excel':
			return 'sheet';
		case 'file-powerpoint':
			return 'slides';
		case 'file-archive':
		case 'file-zipper':
			return 'archive';
		case 'file-code':
			return 'code';
		case 'file-image':
			return 'image';
		case 'file-video':
			return 'video';
		case 'file-audio':
			return 'audio';
		case 'file-alt':
		case 'file-lines':
			return 'text';
		default:
			return 'generic';
	}
}

/** `.file-icon` colour-bucket modifier class for a resolved icon name. */
export function fileIconKindClass(iconName: string): string {
	return `file-icon--${fileIconKind(iconName)}`;
}

/** Format a timestamp (epoch seconds/ms or ISO-8601 string) as a local date. */
export function formatDate(value: number | string | null | undefined): string {
	if (value === null || value === undefined) return '';
	let d: Date;
	if (typeof value === 'number') {
		// Heuristic: seconds vs milliseconds.
		d = new Date(value < 1e12 ? value * 1000 : value);
	} else {
		d = new Date(value);
	}
	if (Number.isNaN(d.getTime())) return '';
	return d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
}
