/// Performance tests — run with `cargo test perf -- --nocapture` to see results.
///
/// These tests do not assert specific numbers since results vary by hardware.
/// They exist to give a baseline and to catch regressions when architecture changes.
use ferrumdb_core::store::{Store, Value};
use std::fs;
use std::time::Instant;

const N: usize = 1_000;

fn setup(name: &str) -> (String, String) {
    let dir = format!("./data/perf_{}", name);
    fs::create_dir_all(&dir).ok();
    let snapshot = format!("{}/snapshot.pb", dir);
    let wal      = format!("{}/wal.log", dir);
    let _ = fs::remove_file(&snapshot);
    let _ = fs::remove_file(&wal);
    let _ = fs::remove_file(format!("{}/LOCK", dir));
    (snapshot, wal)
}

fn teardown(snapshot: &str, wal: &str) {
    let _ = fs::remove_file(snapshot);
    let _ = fs::remove_file(wal);
    if let Some(parent) = std::path::Path::new(snapshot).parent() {
        let _ = fs::remove_file(parent.join("LOCK"));
        let _ = fs::remove_dir(parent);
    }
}

// Sequential writes — one fsync per write, one file open/close per write.
// This is the worst case: no batching, no buffering.
// Expected bottleneck: fsync latency (~0.5–5ms per call on SSD).
#[test]
fn perf_sequential_write_throughput() {
    let (snap, wal) = setup("write");
    let mut store = Store::open_with_paths(&snap, &wal).unwrap();

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

    teardown(&snap, &wal);
}

// Point reads from the in-memory BTreeMap — no disk access.
// Expected: very fast, limited only by BTreeMap O(log n) lookup.
#[test]
fn perf_read_throughput() {
    let (snap, wal) = setup("read");
    let mut store = Store::open_with_paths(&snap, &wal).unwrap();

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

    teardown(&snap, &wal);
}

// WAL replay on open — simulates a crash recovery with N uncommitted entries.
// Measures: protobuf decode + BTreeMap insert, times N.
#[test]
fn perf_wal_replay_on_open() {
    let (snap, wal) = setup("replay");

    {
        let mut store = Store::open_with_paths(&snap, &wal).unwrap();
        for i in 0..N {
            store.set_value(format!("key_{:06}", i), Value::Integer(i as i32)).unwrap();
        }
        // No checkpoint — all N entries remain in the WAL.
    }

    let start = Instant::now();
    let store = Store::open_with_paths(&snap, &wal).unwrap();
    let elapsed = start.elapsed();

    println!(
        "\n[recovery]   {} entries  in {:>8.2?}  →  {:>10.0} entries/sec  (WAL replay)",
        store.get_data().len(), elapsed,
        store.get_data().len() as f64 / elapsed.as_secs_f64()
    );

    teardown(&snap, &wal);
}

// Checkpoint — serialize BTreeMap to protobuf snapshot + fsync + WAL clear.
#[test]
fn perf_checkpoint() {
    let (snap, wal) = setup("checkpoint");
    let mut store = Store::open_with_paths(&snap, &wal).unwrap();

    for i in 0..N {
        store.set_value(format!("key_{:06}", i), Value::Integer(i as i32)).unwrap();
    }

    let start = Instant::now();
    store.checkpoint().unwrap();
    let elapsed = start.elapsed();

    println!(
        "\n[checkpoint] {} entries  in {:>8.2?}  (snapshot write + WAL clear)",
        N, elapsed
    );

    teardown(&snap, &wal);
}

// Mixed workload — alternating writes and reads, simulating real usage.
#[test]
fn perf_mixed_write_read() {
    let (snap, wal) = setup("mixed");
    let mut store = Store::open_with_paths(&snap, &wal).unwrap();

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

    teardown(&snap, &wal);
}
