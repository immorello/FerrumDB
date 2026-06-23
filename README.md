# FerrumDB

FerrumDB is an embedded, no-SQL key-value storage engine written in Rust. It stores typed values (integer, float, text, boolean) accessed by key, with durable writes and crash recovery — without a query language and without a server.

The target is the space that SQLite owns for relational data, applied to key-value workloads: IoT devices, edge nodes, mobile applications, and local-first software that needs a lightweight but correct persistent store.

---

## Architecture

FerrumDB follows the **LSM-tree** (Log-Structured Merge-Tree) model, the same architecture used by LevelDB and RocksDB:

```
write
  │
  ├─ WAL (append-only log, fsynced)       ← durability
  └─ BTreeMap memtable (sorted, in RAM)   ← fast access
       │
       └─ flush when full
            │
            └─ SSTable (immutable sorted file on disk)
                 │
                 └─ compaction (merge SSTables, reclaim space)
```

Reads check the memtable first, then SSTables from newest to oldest.

The `BTreeMap` is the memtable data structure — it keeps keys in sorted order so that flushing to SSTable is a single in-order iteration with no extra sorting step.

---

## What Has Been Built

### Write-Ahead Log (WAL)

Every `PUT` and `DELETE` is appended to the WAL before touching in-memory state. The format is length-prefixed binary records (8-byte big-endian length followed by a protobuf payload), which makes truncated-tail detection safe after a crash.

Each entry carries a key, a typed value, and a monotonically increasing sequence number.

### Store with WAL Integration

The `Store` struct owns the WAL and the memtable:

- `set_value` and `delete_value` write to the WAL first, then update the `BTreeMap`. The in-memory state is never touched unless the WAL write succeeds.
- Keys are kept in sorted lexicographic order in the `BTreeMap` at all times.
- `checkpoint()` writes a protobuf snapshot to disk (fsynced) and clears the WAL.

### Crash Recovery

`Store::open()` is the production entry point. On startup it:

1. Loads the last snapshot from disk if one exists.
2. Replays all WAL entries on top of the snapshot.
3. Sets the sequence counter to the highest sequence seen, so new writes never reuse old numbers.

### Testing

Integration tests cover three areas:

- `tests/wal.rs` — append, read-back, persistence across instances, clear
- `tests/recovery.rs` — WAL replay on restart, delete replay, checkpoint clears WAL, snapshot-only recovery, snapshot + WAL combined recovery, sequence continuity across restarts
- `tests/store.rs` — sorted iteration, sorted order after WAL replay and checkpoint, set/get/delete correctness, overwrite stability

---

## Roadmap

### Step 1 — ACID foundation (next)

The WAL handles durability. What is missing is atomicity across multiple operations and protection against concurrent access from two processes.

- **COMMIT marker** — a `COMMIT` entry type in the WAL. Multiple operations are buffered in memory and written to the WAL together, followed by a COMMIT. On replay, entries without a following COMMIT are discarded. This gives multi-operation atomicity: either all writes in a transaction land or none do.
- **Write lock** — a lockfile (`./data/LOCK`) acquired exclusively when `Store::open` is called, released when the `Store` is dropped. Prevents two processes from writing to the same store simultaneously.

Together these give FerrumDB a credible single-node ACID story: serializable isolation (one writer at a time), committed-only reads (the `BTreeMap` only contains committed state), and durable commits (WAL + fsync).

When indexes are added later, they will ride inside the same transaction boundary with no redesign needed.

### Step 2 — SSTable layer

- A size threshold on the memtable triggers a flush to an immutable sorted file on disk.
- Reads check the memtable first, then walk SSTables from newest to oldest.
- The SSTable file format is a binary sorted sequence of key-value records, written once and never modified.

### Step 3 — Compaction

- When too many SSTables accumulate, reads degrade (more files to check per lookup).
- Compaction merges SSTables into a single file, discarding deleted keys and old versions.
- A simple size-triggered strategy (merge all files when there are more than N) is the starting point.

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

---

## Development Approach

The ideas, architecture, and design decisions behind FerrumDB are my own. I used AI (Claude) to speed up the code-writing process and catch implementation issues early. All design choices, supervision of the implementation, and review of correctness are mine.

---

## License

FerrumDB is licensed under the Apache License, Version 2.0. See `LICENSE` for the full license text and `NOTICE` for the required attribution notice.
