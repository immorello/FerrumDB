/// Performance tests — run with `cargo test perf -- --nocapture` to see results.
///
/// These tests do not assert specific numbers since results vary by hardware.
/// They exist to give a baseline and to catch regressions when architecture changes.
use ferrumdb_core::store::{Store, Value};
use std::fs;
use std::time::Instant;

// Kept below the memtable flush threshold so these benchmarks measure the
// memtable + WAL path without an incidental auto-flush skewing results.
const N: usize = 1_000;

fn setup(name: &str) -> String {
    let dir = format!("./data/perf_{}", name);
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn teardown(dir: &str) {
    let _ = fs::remove_dir_all(dir);
}

// Sequential writes — one fsync per write (append + COMMIT).
// Expected bottleneck: fsync latency (~0.5–15ms per commit depending on storage).
#[test]
fn perf_sequential_write_throughput() {
    let dir = setup("write");
    let mut store = Store::open_with_dir(&dir).unwrap();

    let start = Instant::now();
    for i in 0..N {
        store.set_value(format!("key_{:06}", i), Value::Integer(i as i32)).unwrap();
    }
    let elapsed = start.elapsed();

    let ops_per_sec = N as f64 / elapsed.as_secs_f64();
    println!(
        "\n[write]      {} writes   in {:>8.2?}  →  {:>10.0} writes/sec   (bottleneck: fsync per write)",
        N, elapsed, ops_per_sec
    );

    teardown(&dir);
}

// Point reads from the in-memory memtable — no disk access.
#[test]
fn perf_read_throughput() {
    let dir = setup("read");
    let mut store = Store::open_with_dir(&dir).unwrap();

    for i in 0..N {
        store.set_value(format!("key_{:06}", i), Value::Integer(i as i32)).unwrap();
    }

    let start = Instant::now();
    for i in 0..N {
        let _ = store.get_value(&format!("key_{:06}", i));
    }
    let elapsed = start.elapsed();

    let ops_per_sec = N as f64 / elapsed.as_secs_f64();
    println!(
        "\n[read]       {} reads    in {:>8.2?}  →  {:>10.0} reads/sec",
        N, elapsed, ops_per_sec
    );

    teardown(&dir);
}

// WAL replay on open — simulates crash recovery with N committed entries.
#[test]
fn perf_wal_replay_on_open() {
    let dir = setup("replay");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        for i in 0..N {
            store.set_value(format!("key_{:06}", i), Value::Integer(i as i32)).unwrap();
        }
        // No flush — all N entries remain in the WAL.
    }

    let start = Instant::now();
    let store = Store::open_with_dir(&dir).unwrap();
    let elapsed = start.elapsed();

    println!(
        "\n[recovery]   {} entries  in {:>8.2?}  →  {:>10.0} entries/sec  (WAL replay)",
        store.get_data().len(), elapsed,
        store.get_data().len() as f64 / elapsed.as_secs_f64()
    );

    teardown(&dir);
}

// Flush — serialize the memtable to an SSTable (with CRCs) + fsync + WAL clear.
#[test]
fn perf_flush() {
    let dir = setup("flush");
    let mut store = Store::open_with_dir(&dir).unwrap();

    for i in 0..N {
        store.set_value(format!("key_{:06}", i), Value::Integer(i as i32)).unwrap();
    }

    let start = Instant::now();
    store.flush().unwrap();
    let elapsed = start.elapsed();

    println!(
        "\n[flush]      {} entries  in {:>8.2?}  (SSTable write + WAL clear)",
        N, elapsed
    );

    teardown(&dir);
}

// Batched transaction — N writes, one fsync. Shows the real benefit of COMMIT.
#[test]
fn perf_batched_transaction() {
    let dir = setup("batch");
    let mut store = Store::open_with_dir(&dir).unwrap();

    let start = Instant::now();
    let mut tx = store.begin_transaction();
    for i in 0..N {
        tx.set_value(format!("key_{:06}", i), Value::Integer(i as i32));
    }
    tx.commit().unwrap();
    let elapsed = start.elapsed();

    let ops_per_sec = N as f64 / elapsed.as_secs_f64();
    println!(
        "\n[batch tx]   {} writes   in {:>8.2?}  →  {:>10.0} writes/sec   (1 fsync for all)",
        N, elapsed, ops_per_sec
    );

    teardown(&dir);
}

// Mixed workload — alternating writes and reads, simulating real usage.
#[test]
fn perf_mixed_write_read() {
    let dir = setup("mixed");
    let mut store = Store::open_with_dir(&dir).unwrap();

    let start = Instant::now();
    for i in 0..N {
        store.set_value(format!("key_{:06}", i), Value::Integer(i as i32)).unwrap();
        let _ = store.get_value(&format!("key_{:06}", i));
    }
    let elapsed = start.elapsed();

    let ops_per_sec = (N * 2) as f64 / elapsed.as_secs_f64();
    println!(
        "\n[mixed]      {} ops      in {:>8.2?}  →  {:>10.0} ops/sec      (1 write + 1 read each)",
        N * 2, elapsed, ops_per_sec
    );

    teardown(&dir);
}
