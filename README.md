# FerrumDB

FerrumDB is an open-source database engine written in Rust, built around typed items, durable writes, and scalable key-based data access.

## Goal

The long-term goal is to build a standalone storage engine with durable writes, immutable sorted storage files, and fast key-based access — implemented from the ground up in Rust.

This repository is the next step after a Rust learning path. The purpose is to move from small educational projects into a more realistic database engine with clear storage layers and strong testing discipline.

## What Has Been Built

### Write-Ahead Log (WAL)

The first major component is a durable Write-Ahead Log backed by Protocol Buffers.

Every write (`PUT`) and delete (`DELETE`) is appended to the WAL before touching in-memory state. Each entry carries a key, a typed value, and a monotonically increasing sequence number. The format is length-prefixed binary (8-byte big-endian length followed by the protobuf payload), which makes truncated-tail detection straightforward after a crash.

### Store with WAL Integration

The in-memory store (`Store`) is fully wired to the WAL:

- `set_value` and `delete_value` write to the WAL first, then update the in-memory state. There is no write-through snapshot on every operation.
- The in-memory structure is a `BTreeMap`, so keys are always held in sorted lexicographic order. This is a prerequisite for the SSTable layer.
- `save_to_file` is now a checkpoint operation — it writes a protobuf snapshot to disk and fsyncs it.

### Recovery

`Store::open()` is the production entry point. On startup it:

1. Loads the last snapshot from disk (if one exists).
2. Reads all WAL entries and replays them on top of the snapshot.
3. Sets the sequence counter to the highest sequence seen, so new writes never reuse old numbers.

`Store::checkpoint()` writes the current state as a snapshot and then clears and fsyncs the WAL. After a checkpoint, WAL replay on the next open starts from an empty log.

### Testing

The project has integration tests across three suites:

- `tests/wal.rs` — append, read-back, persistence across instances, and clear
- `tests/recovery.rs` — WAL replay on restart, delete replay, checkpoint clears WAL, snapshot-only recovery, snapshot + WAL combined recovery, sequence continuity across restarts
- `tests/store.rs` — sorted iteration, sorted order after WAL replay, sorted order after checkpoint and recovery, set/get/delete correctness, overwrite stability

## Next Steps

1. SSTable-style immutable on-disk sorted files
2. Memtable flush: when the `BTreeMap` exceeds a size threshold, iterate it in order and write a sorted SSTable file
3. SSTable reads and multi-file merging
4. Compaction

## Design Notes

The storage architecture follows the LSM-tree (Log-Structured Merge-Tree) model:

- Writes go to the WAL first (durability), then to the in-memory `BTreeMap` (the memtable).
- Periodically the memtable is flushed to an immutable sorted SSTable file on disk.
- On read, the memtable is checked first, then SSTables from newest to oldest.
- Compaction merges SSTables to bound read amplification and reclaim space from deleted keys.

## Development Approach

The ideas, architecture, and design decisions behind FerrumDB are my own. I used AI (Claude) to speed up the code-writing process and catch implementation issues early. All design choices, supervision of the implementation, and review of correctness are mine.

## Repository Intent

FerrumDB is not a Rust exercise. It is the start of a standalone database engine built with a realistic storage-engine focus.

## License

FerrumDB is licensed under the Apache License, Version 2.0. See `LICENSE` for the full license text and `NOTICE` for the required attribution notice.
