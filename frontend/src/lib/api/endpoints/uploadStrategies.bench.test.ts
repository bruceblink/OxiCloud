/**
 * Benchmark: upload-dedup strategies compared.
 *
 *   BASELINE  — no instant upload: every file's bytes are sent.
 *   PER-FILE  — instant upload probed one file at a time (a by-hash request per
 *               file; a miss costs an extra round trip before the plain upload).
 *   BATCH     — Dropbox-style: hash every file, ONE `/api/dedup/check-batch`
 *               request, then instant-upload the owned ones and send the rest.
 *
 * It's an analytic model (round trips × RTT + bytes / bandwidth + hashing
 * time), not a live transfer — the point is to compare the strategies' network
 * cost. Run it to see the table:
 *   npm run test:unit -- uploadStrategies
 */
import { describe, expect, it } from 'vitest';

interface Cost {
	roundTrips: number;
	mbSent: number;
	mbHashed: number;
	seconds: number;
}
interface Scenario {
	name: string;
	files: number;
	sizeMB: number;
	ownedFrac: number;
	rttMs: number;
	mbps: number;
}

/** BLAKE3 + file read throughput on a typical client (MB/s). */
const HASH_MBPS = 1500;

function cost(roundTrips: number, mbSent: number, mbHashed: number, s: Scenario): Cost {
	const linkMBps = s.mbps / 8;
	const seconds = roundTrips * (s.rttMs / 1000) + mbSent / linkMBps + mbHashed / HASH_MBPS;
	return { roundTrips, mbSent, mbHashed, seconds };
}

function baseline(s: Scenario): Cost {
	return cost(s.files, s.files * s.sizeMB, 0, s);
}
function perFile(s: Scenario): Cost {
	const owned = Math.round(s.files * s.ownedFrac);
	const miss = s.files - owned;
	// owned → 1 by-hash create; miss → by-hash 404 + plain upload. Every file hashed.
	return cost(owned + miss * 2, miss * s.sizeMB, s.files * s.sizeMB, s);
}
function batch(s: Scenario): Cost {
	const owned = Math.round(s.files * s.ownedFrac);
	const miss = s.files - owned;
	// 1 batch check + owned creates + miss uploads. Every file hashed.
	return cost(1 + owned + miss, miss * s.sizeMB, s.files * s.sizeMB, s);
}

const SCENARIOS: Scenario[] = [
	{
		name: '200×4MB · 50% re-upload · home (40ms/50Mbps)',
		files: 200,
		sizeMB: 4,
		ownedFrac: 0.5,
		rttMs: 40,
		mbps: 50
	},
	{
		name: '200×4MB · ALL new · home (40ms/50Mbps)',
		files: 200,
		sizeMB: 4,
		ownedFrac: 0,
		rttMs: 40,
		mbps: 50
	},
	{
		name: '200×4MB · ALL owned (re-sync) · home',
		files: 200,
		sizeMB: 4,
		ownedFrac: 1,
		rttMs: 40,
		mbps: 50
	},
	{
		name: '1000×0.5MB · 30% owned · WAN (80ms/100Mbps)',
		files: 1000,
		sizeMB: 0.5,
		ownedFrac: 0.3,
		rttMs: 80,
		mbps: 100
	}
];

describe('upload-dedup strategies', () => {
	it('batch never sends more bytes than baseline and matches per-file dedup', () => {
		const rows: string[] = [];
		rows.push('');
		rows.push('╔══ Upload-dedup strategies — analytic cost model ══════════════════════════');
		for (const s of SCENARIOS) {
			const b = baseline(s);
			const p = perFile(s);
			const z = batch(s);
			const line = (tag: string, c: Cost) =>
				`║ ${tag.padEnd(9)} │ RT ${String(c.roundTrips).padStart(4)} │ sent ${c.mbSent
					.toFixed(0)
					.padStart(4)} MB │ ~${c.seconds.toFixed(1).padStart(6)} s`;
			rows.push(`╟─ ${s.name}`);
			rows.push(line('baseline', b));
			rows.push(line('per-file', p));
			rows.push(line('BATCH', z));
			const vsBase = (1 - z.seconds / b.seconds) * 100;
			const rtVsPerFile = p.roundTrips - z.roundTrips;
			rows.push(
				`║   → BATCH: ${vsBase.toFixed(0)}% faster than baseline · ${rtVsPerFile} fewer round trips than per-file`
			);

			// Invariants the strategies must satisfy:
			expect(z.mbSent).toBe(p.mbSent); // batch and per-file dedup identically
			expect(z.mbSent).toBeLessThanOrEqual(b.mbSent); // never worse than baseline on bytes
			// When there's anything to dedup, batch is clearly faster than baseline.
			// (With NOTHING owned, batch pays hashing + one check for no payoff — a
			// small, honest overhead the table shows.)
			if (s.ownedFrac > 0) expect(z.seconds).toBeLessThan(b.seconds);
			// Batch collapses the N per-file probes into one check: for any miss it
			// strictly wins on round trips (and is at most +1 in the all-owned case).
			if (s.ownedFrac < 1) expect(z.roundTrips).toBeLessThan(p.roundTrips);
		}
		rows.push('╚═══════════════════════════════════════════════════════════════════════════');
		console.log(rows.join('\n'));
	});
});
