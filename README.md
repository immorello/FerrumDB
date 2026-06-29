<p align="center">
  <img src="ferrumdb_logo_v3.svg" alt="FerrumDB" width="400"/>
</p>

# FerrumDB

FerrumDB is an embedded, no-SQL key-value storage engine written in Rust. It stores typed values (integer, float, text, boolean) accessed by key, with durable writes and crash recovery — without a query language and without a server.

The target is the space that SQLite owns for relational data, applied to key-value workloads: IoT devices, edge nodes, mobile applications, and local-first software that needs a lightweight but correct persistent store.

---

## Architecture

FerrumDB follows the **LSM-tree** (Log-Structured Merge-Tree) model, the same architecture used by LevelDB and RocksDB:

```
write
  │
  ├─ WAL (append-only log, fsynced on commit)  ← durability + atomicity
  └─ BTreeMap memtable (sorted, in RAM)         ← fast access
       │
       └─ flush when full
            │
            └─ SSTable (immutable sorted file on disk)
                 │
                 └─ compaction (merge SSTables, reclaim space)  ← [not yet implemented]
```

Reads check the memtable first, then SSTables from newest to oldest.

The `BTreeMap` is the memtable — it keeps keys in sorted order so that flushing to SSTable is a single in-order iteration with no extra sorting step.

---

## What Has Been Built

### Write-Ahead Log (WAL)

Every `PUT` and `DELETE` is appended to the WAL before touching in-memory state. The format is length-prefixed binary records (8-byte big-endian length followed by a protobuf payload), which makes truncated-tail detection safe after a crash.

Each entry carries a key, a typed value, and a monotonically increasing sequence number. A `COMMIT` entry marks the end of a transaction — entries without a following COMMIT are discarded on replay.

### Atomic Transactions

Operations can be grouped into explicit transactions:

```rust
let mut tx = store.begin_transaction();
tx.set_value("a".to_string(), Value::Integer(1));
tx.set_value("b".to_string(), Value::Integer(2));
tx.commit()?; // one fsync covers the entire batch
```

All entries are buffered in memory, written to the WAL together, then a single COMMIT marker is fsynced. Either all writes land or none do. Dropping a transaction without committing is a free rollback — nothing reaches disk.

Single-operation writes (`set_value`, `delete_value` directly on `Store`) are implicitly wrapped in their own commit and behave the same way.

### ACID Foundation

FerrumDB has a credible single-node ACID story for an embedded engine:

- **Atomicity** — multi-operation transactions via COMMIT marker in the WAL.
- **Consistency** — the BTreeMap only ever contains committed state.
- **Isolation** — one writer at a time via an exclusive file lock held for the lifetime of the `Store`. The borrow checker enforces single-transaction-at-a-time at compile time.
- **Durability** — every commit ends with an fsync. Crash recovery replays the WAL on next open.

The file lock is implemented with a direct `flock` syscall via `extern "C"` — zero crate dependencies, standard on every Linux target FerrumDB runs on.

### Store and Recovery

The `Store` struct owns the WAL, the memtable, the SSTables, and the lock:

- `set_value` and `delete_value` write to the WAL first, then update the memtable. In-memory state is never modified unless the WAL write succeeds.
- `flush()` writes the memtable to a new SSTable and clears the WAL (see SSTable Layer below). It replaces the earlier snapshot mechanism.
- `open_with_dir()` discovers the table's SSTables, replays uncommitted WAL entries on top, and sets the sequence counter to the highest value seen so new writes never reuse old sequence numbers.

### Table Model

Each table is an independent `Store` instance with its own files under `./data/<table>/`:

```
./data/users/
  ├── wal.log           ← committed writes since the last flush
  ├── LOCK              ← exclusive lock, held while the table is open
  └── sstable_*.sst     ← immutable on-disk tables (newest id wins)
```

Opening a table that is already open by another process fails immediately with an error. Different tables can be opened simultaneously without interference.

### Performance Baseline

Measured on an Apple Mac mini (APFS), 1000-key workloads:

| Operation | Debug | Release | Bound by |
|---|---|---|---|
| Single durable write | ~69/sec | ~66/sec | `fsync` (~14 ms/commit) |
| Batched transaction (1000 writes, 1 fsync) | ~181/sec | ~188/sec | WAL file open per append |
| Read (in-memory) | ~0.9M/sec | ~1.95M/sec | CPU |
| Recovery (WAL replay) | ~147k/sec | ~256k/sec | CPU |
| Flush 1000 entries → SSTable | ~16 ms | ~9 ms | CPU |

Two different walls explain these numbers:

- **Single writes are `fsync`-bound.** Every commit fsyncs, and APFS fsync is ~10–15 ms (it is ~1–5 ms on the Linux embedded storage FerrumDB actually targets). This is the same wall every durable embedded engine hits on the same hardware — the release build does not change it.
- **Batched writes are syscall-bound, not yet fsync-bound.** A batch pays a single fsync, so its cost is dominated by the WAL reopening the file on every append. Holding the WAL handle open (see Roadmap → Engine optimization) is expected to lift batched throughput into the thousands/sec.

Reads are currently served from the in-memory memtable. Once data ages into SSTables, reads involve disk plus the per-SSTable key-range skip; bloom filters and a block cache (Roadmap) are what will keep that fast.

### SSTable Layer

The memtable is flushed to immutable on-disk SSTables, which is what bounds memory on constrained devices. Each SSTable (`ferrumdb-core/src/sstable.rs`) is ~4 KB data blocks each protected by a CRC32, a sparse index (one entry per block), and a fixed footer carrying a magic number and format version. A reader loads only the footer and sparse index into RAM, then serves a point lookup with a single binary search and one block read. Full byte-level spec in [docs/sstable.md](docs/sstable.md).

- **Flush** — `Store::flush()` writes the whole memtable to a new SSTable, then clears the memtable and the WAL. An automatic flush fires once the memtable's live size passes a **byte budget** (tunable per table via `set_memtable_budget`), so memory stays bounded regardless of how much is written.
- **Layered reads** — a lookup checks the memtable first, then SSTables newest→oldest; the first hit wins. A tombstone in a newer layer shadows an older value. Each SSTable records its **key range**, so a lookup skips any table that cannot contain the key without touching the disk.
- **Crash safety** — the SSTable is fsynced before the WAL is cleared, so a crash in between is safe (the WAL entries simply replay on top of the SSTable).

SSTable flush replaces the earlier snapshot mechanism as the single path to disk.

### Compaction

As SSTables accumulate, reads must consult more files and deleted keys are never reclaimed. `Store::compact()` merges all SSTables into one, keeping the newest value per key and dropping tombstones (a full merge leaves nothing older for a tombstone to shadow). Compaction runs automatically once the SSTable count exceeds a threshold.

The merged SSTable is fsynced **before** the old files are deleted, so a crash in between is safe: the stale tables simply replay behind the newer merged table and lose to it on every read.

### Testing

Integration tests cover nine areas:

- `tests/wal.rs` — append, read-back, persistence across instances, clear
- `tests/recovery.rs` — WAL replay on restart, delete replay, flush clears WAL, recovery after flush, SSTable + WAL combined recovery, sequence continuity, tombstone shadowing across SSTables and from the WAL
- `tests/store.rs` — sorted iteration, sorted order after WAL replay, data survives flush and recovery, set/get/delete correctness, idempotent delete, overwrite stability
- `tests/lock.rs` — double-open rejection, lock release on drop, per-table isolation, LOCK file creation, multi-cycle reacquisition
- `tests/transaction.rs` — commit visibility, rollback on drop, crash recovery of committed transactions, uncommitted entry discard, mixed put/delete transactions
- `tests/sstable.rs` — flush/read roundtrip, missing keys, lookup across multiple blocks, tombstone roundtrip, CRC corruption detection, empty table, key-range reporting and out-of-range skip
- `tests/flush.rs` — flush creates an SSTable and empties the memtable, empty-flush no-op, memtable shadows SSTable, newest SSTable wins, layered read across many SSTables, byte-budget auto-flush bounds the memtable
- `tests/compaction.rs` — merge into one, newest value wins, deleted keys dropped, all-deleted leaves nothing, compaction survives recovery, auto-compaction bounds the SSTable count
- `tests/perf.rs` — write throughput, batched transaction throughput, read throughput, WAL replay time, flush time

---

## Roadmap

### Step 1 — SSTable layer ✅

SSTable flush is what makes FerrumDB viable on memory-constrained embedded devices, and it is now in place.

- ✅ The immutable on-disk SSTable format (blocks, sparse index, CRC, footer) with a reader and writer.
- ✅ Tombstones represented in the memtable and the format.
- ✅ A threshold on the memtable triggers a flush to a new SSTable on disk.
- ✅ Reads check the memtable first, then walk SSTables from newest to oldest.
- ✅ Flush replaces the snapshot mechanism as the single path to disk.

### Step 2 — Buffer manager ✅

- ✅ Track memtable size in bytes rather than entry count.
- ✅ The flush byte budget is tunable per table (`set_memtable_budget`) for the target device.
- ✅ Each SSTable records its key range; a lookup skips any SSTable whose range excludes the key, with no disk read.

### Step 3 — Compaction ✅

- ✅ Compaction merges all SSTables into one, keeping the newest value per key and dropping tombstones and shadowed values.
- ✅ A size-triggered strategy: a full merge runs once the SSTable count exceeds a threshold.
- ✅ Crash-safe: the merged table is fsynced before the old files are removed.

### Step 4 — Engine optimization (next)

The fundamentals are in place; the goal here is to make the engine as fast as it can be *before* an API freezes the hot paths. The performance numbers above point directly at the work:

- **WAL file-handle reuse** — the WAL currently reopens the log file on every append, which is the bottleneck for batched writes (a batch already pays only one fsync). Holding the handle open for the Store's lifetime should lift batched throughput into the thousands/sec.
- **Group commit** — let concurrent or queued writes share a single fsync.
- **Bloom filters** — a per-SSTable membership filter to skip block reads for keys that are definitely absent.
- **Block cache** — keep hot SSTable blocks in memory so reads that miss the memtable do not always hit disk.

### Step 5 — API layer

- A minimal public Rust API designed for embedding.
- A C FFI layer so FerrumDB can be used from C, Swift, Python, and other languages — the same way SQLite is embedded across ecosystems.

---

## Design Notes

**Why LSM and not B-Tree on disk?**

A B-Tree storage engine requires implementing disk pages, page splits, tree rebalancing, and a page cache. LSM separates the problem: writes go to an append-only log and a sorted in-memory buffer; disk files are written once and never modified. The implementation complexity is significantly lower, and LSM write performance is better on flash storage (common in embedded targets) because it avoids random writes.

**Why no SQL?**

SQL requires a parser, a query planner, and a schema layer. For the target use cases — IoT devices, edge nodes, mobile apps — the application already knows its data shape. A typed key-value API with fast key-based access is sufficient and keeps the engine small and embeddable.

**Why Rust?**

Memory safety without a garbage collector. Predictable latency. A small binary. Rust is increasingly the right language for systems software that needs to run reliably on constrained hardware for long periods.

**Why minimal dependencies?**

FerrumDB currently depends only on `prost` (protobuf encoding) and Rust's standard library. File locking uses a direct `flock` syscall rather than a crate. The goal is a binary that is small enough to ship on embedded targets without pulling in a dependency tree that dwarfs the engine itself.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

---

## Development Approach

FerrumDB is developed with assistance from Claude. The architecture, design decisions, and direction are mine — Claude accelerated the implementation and helped catch issues early. This is stated openly because it reflects how the project was actually built.

---

## License

FerrumDB is licensed under the Apache License, Version 2.0. See `LICENSE` for the full license text and `NOTICE` for the required attribution notice.
