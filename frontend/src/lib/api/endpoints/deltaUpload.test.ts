import { beforeEach, describe, expect, it, vi } from 'vitest';

// vi.mock is hoisted; build the spies with vi.hoisted so the factories can use them.
const { blake3Mock, byHashMock, batchMock } = vi.hoisted(() => ({
	blake3Mock: vi.fn(),
	byHashMock: vi.fn(),
	batchMock: vi.fn()
}));

vi.mock('$lib/vendor/hashWasm', () => ({ blake3HexOfFile: blake3Mock }));
vi.mock('$lib/api/endpoints/files', () => ({
	createFileByHash: byHashMock,
	dedupCheckBatch: batchMock
}));

import { DELTA_UPLOAD_MIN_SIZE, instantUploadOwned, resolveOwnedHashes } from './deltaUpload';

const fakeFile = (size: number, name = 'x.bin') => ({ size, name }) as unknown as File;
const hashOf = (name: string) => name.padEnd(64, '0');
const MB = 1024 * 1024;

describe('resolveOwnedHashes (batch check)', () => {
	beforeEach(() => {
		blake3Mock.mockReset();
		batchMock.mockReset();
		blake3Mock.mockImplementation((f: File) => Promise.resolve(hashOf(f.name)));
	});

	it('hits nothing when no files are in-band (empty / >= delta threshold)', async () => {
		const owned = await resolveOwnedHashes([
			fakeFile(0, 'empty'),
			fakeFile(DELTA_UPLOAD_MIN_SIZE, 'big')
		]);
		expect(owned.size).toBe(0);
		expect(blake3Mock).not.toHaveBeenCalled();
		expect(batchMock).not.toHaveBeenCalled();
	});

	it('hashes in-band files and maps only the server-owned subset in ONE batch call', async () => {
		const a = fakeFile(2 * MB, 'a');
		const b = fakeFile(3 * MB, 'b');
		batchMock.mockResolvedValue(new Set([hashOf('a')])); // server owns only "a"
		const owned = await resolveOwnedHashes([a, b]);
		expect(batchMock).toHaveBeenCalledTimes(1);
		expect(batchMock).toHaveBeenCalledWith([hashOf('a'), hashOf('b')]);
		expect(owned.get(a)).toBe(hashOf('a'));
		expect(owned.has(b)).toBe(false);
	});

	it('falls back to an empty map when client-side hashing fails', async () => {
		blake3Mock.mockRejectedValue(new Error('wasm down'));
		const owned = await resolveOwnedHashes([fakeFile(2 * MB, 'a')]);
		expect(owned.size).toBe(0);
		expect(batchMock).not.toHaveBeenCalled();
	});

	it('falls back to an empty map when the batch request fails', async () => {
		batchMock.mockRejectedValue(new Error('network'));
		const owned = await resolveOwnedHashes([fakeFile(2 * MB, 'a')]);
		expect(owned.size).toBe(0);
	});
});

describe('instantUploadOwned (zero-byte create)', () => {
	beforeEach(() => byHashMock.mockReset());

	it('reports zero-byte success on 201', async () => {
		byHashMock.mockResolvedValue({ ok: true, status: 201, data: { id: 'f1' } });
		const r = await instantUploadOwned('folder', fakeFile(2 * MB, 'a'), hashOf('a'));
		expect(r).toEqual({ ok: true, data: { id: 'f1' }, savedBytes: 2 * MB });
		expect(byHashMock).toHaveBeenCalledWith('folder', 'a', hashOf('a'));
	});

	it('falls back (null) when the blob vanished (404)', async () => {
		byHashMock.mockResolvedValue({ ok: false, status: 404 });
		expect(await instantUploadOwned('folder', fakeFile(2 * MB), hashOf('a'))).toBeNull();
	});

	it('surfaces a quota error on 507', async () => {
		byHashMock.mockResolvedValue({ ok: false, status: 507 });
		expect(await instantUploadOwned('folder', fakeFile(2 * MB), hashOf('a'))).toEqual({
			ok: false,
			isQuotaError: true,
			errorMsg: 'Storage quota exceeded'
		});
	});
});
