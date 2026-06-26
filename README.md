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

The `Store` struct owns the WAL, the memtable, and the lock:

- `set_value` and `delete_value` write to the WAL first, then update the BTreeMap. In-memory state is never modified unless the WAL write succeeds.
- `checkpoint()` writes a protobuf snapshot to disk and clears the WAL. This bounds WAL growth until SSTable flush is implemented.
- `open_with_paths()` loads the last snapshot, replays uncommitted WAL entries on top, and sets the sequence counter to the highest value seen so new writes never reuse old sequence numbers.

### Table Model

Each table is an independent `Store` instance with its own files under `./data/<table>/`:

```
./data/users/
  ├── snapshot.pb   ← latest checkpoint
  ├── wal.log       ← uncommitted writes since last checkpoint
  └── LOCK          ← exclusive lock, held while the table is open
```

Opening a table that is already open by another process fails immediately with an error. Different tables can be opened simultaneously without interference.

### Performance Baseline

Measured on macOS (debug build, APFS — pessimistic for fsync):

| Operation | Throughput |
|---|---|
| Single write | ~68 writes/sec |
| Batched transaction (1000 writes, 1 fsync) | ~184 writes/sec |
| Read (in-memory BTreeMap) | ~496k reads/sec |
| WAL replay on recovery | ~89k entries/sec |

The write bottleneck is fsync latency, which is ~10-15ms on macOS APFS and ~1-5ms on Linux embedded storage (the actual target). On real hardware single writes would be in the 200-1000 writes/sec range; batched transactions would reach 2,000-10,000 writes/sec depending on batch size.

Reads are pure in-memory and will change once the SSTable layer is in place — keys flushed out of the memtable will require disk access.

### SSTable Layer

The memtable is flushed to immutable on-disk SSTables, which is what bounds memory on constrained devices. Each SSTable (`ferrumdb-core/src/sstable.rs`) is ~4 KB data blocks each protected by a CRC32, a sparse index (one entry per block), and a fixed footer carrying a magic number and format version. A reader loads only the footer and sparse index into RAM, then serves a point lookup with a single binary search and one block read. Full byte-level spec in [docs/sstable.md](docs/sstable.md).

- **Flush** — `Store::flush()` writes the whole memtable to a new SSTable, then clears the memtable and the WAL. An automatic flush fires once the memtable passes a threshold, so memory stays bounded.
- **Layered reads** — a lookup checks the memtable first, then SSTables newest→oldest; the first hit wins. A tombstone in a newer layer shadows an older value.
- **Crash safety** — the SSTable is fsynced before the WAL is cleared, so a crash in between is safe (the WAL entries simply replay on top of the SSTable).

SSTable flush replaces the earlier snapshot mechanism as the single path to disk.

### Testing

Integration tests cover eight areas:

- `tests/wal.rs` — append, read-back, persistence across instances, clear
- `tests/recovery.rs` — WAL replay on restart, delete replay, flush clears WAL, recovery after flush, SSTable + WAL combined recovery, sequence continuity, tombstone shadowing across SSTables and from the WAL
- `tests/store.rs` — sorted iteration, sorted order after WAL replay, data survives flush and recovery, set/get/delete correctness, idempotent delete, overwrite stability
- `tests/lock.rs` — double-open rejection, lock release on drop, per-table isolation, LOCK file creation, multi-cycle reacquisition
- `tests/transaction.rs` — commit visibility, rollback on drop, crash recovery of committed transactions, uncommitted entry discard, mixed put/delete transactions
- `tests/sstable.rs` — flush/read roundtrip, missing keys, lookup across multiple blocks, tombstone roundtrip, CRC corruption detection, empty table
- `tests/flush.rs` — flush creates an SSTable and empties the memtable, empty-flush no-op, memtable shadows SSTable, newest SSTable wins, layered read across many SSTables, auto-flush bounds the memtable
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

### Step 2 — Buffer manager

- Track memtable size in bytes rather than entry count.
- Tune the flush threshold for the byte budget of the target device.
- Manage the list of live SSTable files and their key ranges (e.g. skip an SSTable whose key range excludes the lookup).

### Step 3 — Compaction

- As SSTables accumulate, reads degrade (more files to check per lookup) and deleted keys are never reclaimed.
- Compaction merges SSTables into fewer files, discarding tombstones and shadowed values.
- A simple size-triggered strategy (merge all files when there are more than N) is the starting point.
- Bloom filters can be added later to eliminate unnecessary SSTable reads for missing keys.

### Step 4 — API layer

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
