//! Blob write syscall benchmark — per-chunk `stat` removal in `write_blob_bytes`.
//!
//! Isolates the one change: the new-chunk create path went from
//!   `try_exists` (stat) + `File::create` (open O_CREAT|O_TRUNC)   — 2 metadata syscalls
//! to
//!   `OpenOptions::create_new` (open O_CREAT|O_EXCL)               — 1 metadata syscall
//! each one a `spawn_blocking` round-trip on Tokio's blocking pool. The bench
//! writes N distinct content-addressed chunk files (scattered across the 256
//! hash-prefix dirs, like production) at the production fan-out
//! (`CHUNK_UPLOAD_CONCURRENCY = 8`), once per strategy, and reports write
//! throughput. "old" is the previous behaviour, "new" is the change.
//!
//! The gain is per-chunk and metadata-bound, so it scales **inversely with chunk
//! size** — largest for tiny chunks, smaller at the 256 KiB CDC average where the
//! data write dominates. The sweep shows that range.
//!
//! Run (no Postgres needed):
//!   cargo run --release --features bench --example bench_blob_write
//! Tunables (env): BENCH_CHUNKS (4000) BENCH_CHUNK_KB (8,64,256)
//!   BENCH_CONCURRENCY (8) BENCH_REPS (3)

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{StreamExt, stream};
use oxicloud::application::ports::blob_storage_ports::BlobStorageBackend;
use oxicloud::infrastructure::services::local_blob_backend::LocalBlobBackend;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[derive(Clone, Copy, PartialEq)]
enum Strategy {
    /// Old: try_exists (stat) + File::create (open O_CREAT|O_TRUNC).
    Old,
    /// New: OpenOptions::create_new (open O_CREAT|O_EXCL).
    New,
}

async fn write_one(strategy: Strategy, path: PathBuf, data: Arc<Vec<u8>>) {
    match strategy {
        Strategy::Old => {
            if tokio::fs::try_exists(&path).await.unwrap_or(false) {
                return;
            }
            let mut f = tokio::fs::File::create(&path).await.expect("create");
            f.write_all(&data).await.expect("write");
            f.flush().await.expect("flush");
        }
        Strategy::New => match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(mut f) => {
                f.write_all(&data).await.expect("write");
                f.flush().await.expect("flush");
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => panic!("open: {e}"),
        },
    }
}

async fn run_strategy(
    strategy: Strategy,
    paths: &[PathBuf],
    data: Arc<Vec<u8>>,
    concurrency: usize,
) -> Duration {
    let t = Instant::now();
    let mut s = stream::iter(paths.iter().cloned())
        .map(|p| write_one(strategy, p, data.clone()))
        .buffer_unordered(concurrency);
    while s.next().await.is_some() {}
    t.elapsed()
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let n: usize = env_or("BENCH_CHUNKS", 4000);
    let chunk_kbs: Vec<usize> = std::env::var("BENCH_CHUNK_KB")
        .ok()
        .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .filter(|v: &Vec<usize>| !v.is_empty())
        .unwrap_or_else(|| vec![8, 64, 256]);
    let concurrency: usize = env_or("BENCH_CONCURRENCY", 8);
    let reps: usize = env_or("BENCH_REPS", 3);

    // Distinct, well-distributed hashes → scattered across the 256 prefix dirs.
    let hashes: Vec<String> = (0..n)
        .map(|i| blake3::hash(&(i as u64).to_le_bytes()).to_hex().to_string())
        .collect();

    println!("\n############################################################");
    println!("# Blob write syscall benchmark — per-chunk stat removal");
    println!(
        "# {n} chunks/run, {concurrency}-way (CHUNK_UPLOAD_CONCURRENCY), median of {reps}"
    );
    println!("# old = try_exists+create (2 metadata syscalls); new = create_new (1)");
    println!("############################################################\n");
    println!(
        "| {:>8} | {:>12} | {:>12} | {:>12} | {:>10} |",
        "chunk", "old chunks/s", "new chunks/s", "new MB/s", "Δ chunks/s"
    );
    println!(
        "|{:-<10}|{:-<14}|{:-<14}|{:-<14}|{:-<12}|",
        "", "", "", "", ""
    );

    for &kb in &chunk_kbs {
        let data = Arc::new(vec![0xABu8; kb * 1024]);
        let mut old_rates: Vec<f64> = Vec::with_capacity(reps);
        let mut new_rates: Vec<f64> = Vec::with_capacity(reps);

        for rep in 0..reps {
            // Interleave the two strategies within each rep, alternating which
            // runs first, so any system drift (cache/tmpfs fill, thermal) hits
            // both equally instead of penalising whichever runs second.
            let order = if rep % 2 == 0 {
                [Strategy::Old, Strategy::New]
            } else {
                [Strategy::New, Strategy::Old]
            };
            for strategy in order {
                // Fresh dir per run so every chunk is genuinely new (no skips).
                let tmp = tempfile::tempdir().expect("tempdir");
                let backend = LocalBlobBackend::new(tmp.path());
                backend.initialize().await.expect("init");
                let paths: Vec<PathBuf> = hashes.iter().map(|h| backend.blob_path(h)).collect();
                let dur = run_strategy(strategy, &paths, data.clone(), concurrency).await;
                let rate = n as f64 / dur.as_secs_f64();
                match strategy {
                    Strategy::Old => old_rates.push(rate),
                    Strategy::New => new_rates.push(rate),
                }
            }
        }

        old_rates.sort_by(|a, b| a.partial_cmp(b).unwrap());
        new_rates.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let old = old_rates[old_rates.len() / 2];
        let new = new_rates[new_rates.len() / 2];
        let mb_s = new * (kb as f64) / 1024.0;
        let delta = (new / old - 1.0) * 100.0;
        println!(
            "| {:>6}K | {:>12.0} | {:>12.0} | {:>12.1} | {:>9.1}% |",
            kb, old, new, mb_s, delta
        );
    }
    println!(
        "\nΔ is the new (create_new) write throughput vs old (stat+create). The gain\n\
         is metadata-bound, so it shrinks as the chunk size (data-write cost) grows;\n\
         CDC chunks average 256 KiB.\n"
    );
}
