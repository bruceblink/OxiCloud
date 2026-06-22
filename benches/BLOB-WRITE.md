# Blob write syscall benchmark — per-chunk `stat` removal

Measures the change to `write_blob_bytes` (`local_blob_backend.rs`): the new-chunk
create path went from `try_exists` (stat) + `File::create` (open `O_CREAT|O_TRUNC`)
— two metadata syscalls, each a `spawn_blocking` round-trip — to a single
`OpenOptions::create_new` (open `O_CREAT|O_EXCL`), treating `AlreadyExists` as the
idempotent skip. The bench writes N distinct content-addressed chunk files
scattered across the 256 hash-prefix dirs at the production fan-out
(`CHUNK_UPLOAD_CONCURRENCY = 8`), once per strategy, reporting write throughput.

## Reproduce

```bash
cargo run --release --features bench --example bench_blob_write
# tunables: BENCH_CHUNKS (4000) BENCH_CHUNK_KB (8,64,256) BENCH_CONCURRENCY (8) BENCH_REPS (3)
```

## Results — INCONCLUSIVE (effect below the noise floor)

Three 9-rep interleaved runs on the **same** ext4 device (`/dev/vda`, shared
container disk), Δ = new vs old chunks/s:

| run | 8 KiB | 64 KiB | 256 KiB |
|-----|------:|-------:|--------:|
| A (3 reps, sequential — order-biased, discard) | +28.7% | +9.3% | −12.2% |
| B (9 reps, interleaved, `/tmp`)                |  −5.4% | −1.0% |  +0.5% |
| C (9 reps, interleaved, repo dir)              |  +8.1% | +7.2% | +21.2% |

The 256 KiB delta swings **−12% → +0.5% → +21%** across runs of identical code on
identical storage. The run-to-run variance (each rep writes ~1 GiB to a shared
container disk) is larger than the effect, so **the throughput impact is not
measurable in this environment**. Run A is additionally invalid (all-old-then-
all-new lets system drift penalise whichever strategy runs second); B and C are
methodologically sound (interleaved, alternating order) but disagree — that
disagreement *is* the finding.

## What IS deterministic (from the code, not the timer)

- **One metadata syscall instead of two** per new chunk: `openat(O_CREAT|O_EXCL)`
  vs `newfstatat` + `openat(O_CREAT|O_TRUNC)`. On a large upload of unique data
  that is thousands of `stat`s — and `spawn_blocking` dispatches — removed. The
  wall-clock value of that is below the noise floor here because a negative
  `stat` on a warm ext4 dentry cache is ~µs, dwarfed by the chunk's
  create+write+flush (and, downstream, the fsync sweep).
- **Closes a TOCTOU race.** The old `try_exists`==false → `File::create`
  (`O_TRUNC`) pair could truncate a file a racing writer created in between;
  `O_EXCL` makes the check-and-create atomic and skips instead. (In practice the
  PG pin-or-classify serialises writes per content hash, so this race is already
  unreachable on the dedup path — the change is defence-in-depth.)

## Conclusion

This is a **code-quality / correctness micro-change**, not a measured throughput
win: `create_new` is the canonical Rust idiom replacing a check-then-create
anti-pattern, it is strictly fewer syscalls, and it has zero downside — but its
throughput effect is below what this shared-disk environment can resolve, and at
the realistic 256 KiB CDC chunk size the one saved `stat` is a tiny fraction of
the per-chunk cost regardless. Kept on those grounds, not on a benchmark number.

(Companion change *not* made: reusing the written `File` handle for the fsync
sweep instead of re-opening by path. The sweep is a single end-of-stream
`sync_blobs` over **all** the upload's new hashes (`dedup_service.rs:929`), so
retaining handles would hold thousands of FDs open for the whole upload — a 1 GiB
upload is ~4000 chunks > the default `ulimit -n` of 1024 → `EMFILE`. The
re-open-with-16-way-concurrency sweep is a deliberate FD-frugal design; the saved
`open` is negligible before the `fsync` it precedes anyway.)
