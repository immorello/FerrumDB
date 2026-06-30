# WAL Module — Current State

This document describes the Write-Ahead Log as it exists today: its file format, its Rust API, how it integrates with the Store, and what it does and does not guarantee. COMMIT markers and the write lock, once future work, are now implemented and described here.

---

## Purpose

The WAL is an append-only log of every write operation before that operation touches in-memory state. Its job is durability: if the process crashes after a write lands in the WAL but before anything else happens, the write can be replayed on the next startup and nothing is lost.

The contract is simple:

> A write is durable if and only if it has been fsynced to the WAL file.

---

## File Format

The WAL file is a flat sequence of length-prefixed binary records. Each record is:

```
┌─────────────────────────┬───────────────────────────┐
│  length prefix (8 bytes)│  protobuf payload (N bytes)│
│  big-endian u64         │  encoded WalEntry          │
└─────────────────────────┴───────────────────────────┘
```

The length prefix encodes the exact byte count of the following protobuf payload. There is no checksum, no alignment padding, no record separator.

### Why length-prefix and not a delimiter?

Protobuf encoding is not self-delimiting — you cannot tell where one message ends and the next begins without an external framing mechanism. A length prefix is the standard solution. It also makes truncated-tail detection straightforward: if the file ends before the full payload described by the last length prefix, that last record is incomplete and discarded.

### Crash safety of the format

The only corruption the format defends against is a truncated tail — the last write was partial because the process died mid-write. In that case `read_all` stops at the first incomplete record and returns everything before it.

It does not defend against bit-flip corruption inside a valid-length record. A CRC per record would be needed for that.

---

## Protobuf Schema

Defined in `ferrumdb-core/src/proto/wal.proto`:

```proto
enum Operation {
  PUT    = 0;
  DELETE = 1;
  COMMIT = 2;
}

message WalEntry {
  Operation    operation = 1;
  string       key       = 2;
  ValueMessage value     = 3;  // only populated for PUT
  uint64       sequence  = 4;
  uint64       timestamp = 5;  // currently always 0
}
```

`ValueMessage` is imported from `store.proto` and carries a typed value:

```proto
message ValueMessage {
  oneof kind {
    int32  integer = 1;
    double float   = 2;
    string text    = 3;
    bool   boolean = 4;
  }
}
```

### Field notes

- **operation** — `PUT`, `DELETE`, or `COMMIT`. `PUT` requires a `value`. `DELETE` and `COMMIT` leave `value` empty. A `COMMIT` entry marks the end of a transaction; entries not followed by a `COMMIT` are discarded on replay.
- **key** — the store key as a UTF-8 string.
- **sequence** — a monotonically increasing counter assigned by `Store`, not by `Wal`. The WAL itself does not validate or assign sequence numbers. After recovery, `Store` sets its counter to the highest sequence seen so new writes never reuse old numbers.
- **timestamp** — exists in the schema but is always written as `0`. Currently unused.

---

## Rust API

### `Wal` struct

```rust
pub struct Wal {
    file_path: String,
    file: Option<File>,
}
```

Holds the path and a cached append handle. The handle is opened lazily on the first write and reused for the WAL's lifetime, so appends do not pay a file open/close each — the dominant cost of batched writes before this. Writes therefore take `&mut self`.

### Constructors

```rust
// Uses the default path: "./data/wal.log"
let wal = Wal::new();

// Custom path — used in tests for isolation
let wal = Wal::with_path("./data/my.log");
```

Both constructors call `create_dir_all` on the parent directory so the first `append` never fails due to a missing directory.

### `append`

```rust
pub fn append(&mut self, entry: &WalEntry) -> Result<(), AppError>
```

Frames the entry (8-byte length prefix + protobuf payload) and writes it to the cached append handle in a single `write_all`. **It does not fsync.** A bare `append` is therefore not durable on its own — it must be followed by `write_commit`, which fsyncs the whole batch at once.

This split is what makes transactions cheap: N operations are appended, then a single `write_commit` pays one fsync for all of them.

### `write_commit`

```rust
pub fn write_commit(&mut self, sequence: u64) -> Result<(), AppError>
```

Writes a `COMMIT` entry to the cached handle and then calls `sync_all`. The `sync_all` issues the `fsync` that makes every preceding `append` since the last commit durable. On recovery, any entries written after the last `COMMIT` are truncated from the file (see `read_for_recovery`).

### `read_all` and `read_for_recovery`

```rust
pub fn read_all(&self) -> Result<Vec<WalEntry>, AppError>
pub fn read_for_recovery(&mut self) -> Result<Vec<WalEntry>, AppError>
```

`read_all` reads the entire file into memory, parses records sequentially, and returns all successfully decoded entries in write order — used by tests to inspect the raw log.

`read_for_recovery` is what the `Store` uses on open. It returns only the entries up to and including the last `COMMIT`, and **truncates the file** to drop any uncommitted tail. This closes a subtle hole: without it, an uncommitted entry left by a crashed session could be silently adopted by the *next* session's `COMMIT`.

Handles two failure cases:

- **Incomplete length prefix** (fewer than 8 bytes remain at the current offset): stops and returns what was collected so far. This is the normal crash-at-write-start case.
- **Incomplete payload** (the length prefix says N bytes follow but fewer than N bytes remain): same — stops and returns. This is the normal crash-mid-write case.

If a record's protobuf decodes to an error despite having the right length, it prints to stderr and skips that record, continuing to the next. This case should not happen in practice because partial writes are caught by the length check above.

### `clear`

```rust
pub fn clear(&self) -> Result<(), AppError>
```

Truncates the file to zero bytes by opening it with `File::create` (which truncates), then calls `sync_all` to make the empty state durable. Called by `Store::checkpoint` after the snapshot has been written.

### `path`

```rust
pub fn path(&self) -> &str
```

Returns the file path. Used by tests to inspect the file directly.

### Entry builders

```rust
// Creates a PUT entry
Wal::create_put_entry(key: String, value: &Value, sequence: u64) -> WalEntry

// Creates a DELETE entry
Wal::create_delete_entry(key: String, sequence: u64) -> WalEntry
```

These are associated functions on `Wal` — they do not use `self`. They convert from `store::Value` to the protobuf `ValueMessage`. They exist on `Wal` for historical reasons; conceptually they belong on `WalEntry`.

---

## Integration with Store

The `Store` struct owns a `Wal` instance and a `sequence: u64` counter.

### Write path

Every mutation goes through the WAL before touching the in-memory `BTreeMap`. A single `set_value`/`delete_value` is implicitly its own one-operation transaction: append, then commit.

```
set_value("price", 19.99)
  │
  ├─ increment sequence    (sequence = N)
  ├─ build WalEntry        (PUT, "price", 19.99, N)
  ├─ wal.append(entry)     ← buffered, NOT yet durable
  ├─ wal.write_commit(N)   ← COMMIT entry + fsync (durable here)
  └─ data.insert(...)      ← Entry::Value, only after WAL succeeds
```

A delete is the same shape, but inserts a tombstone instead of a value, and is idempotent (deleting a missing key is not an error):

```
delete_value("price")
  │
  ├─ increment sequence    (sequence = N)
  ├─ build WalEntry        (DELETE, "price", N)
  ├─ wal.append(entry)     ← buffered
  ├─ wal.write_commit(N)   ← COMMIT + fsync
  └─ data.insert(Tombstone) ← shadows any older value
```

An explicit multi-operation `Transaction` appends every buffered op, then calls `write_commit` once — one fsync for the whole batch. If the `BTreeMap` update is reached, the WAL write already succeeded; the write lands in both or in neither.

### Recovery path (`Store::open_with_dir`)

```
open_with_dir(dir)
  │
  ├─ discover sstable_<id>.sst files, load each index, order newest → oldest
  │
  ├─ wal.read_all()
  │     returns all entries in write order
  │
  ├─ buffer PUT/DELETE entries until a COMMIT is seen, then apply the batch
  │  to the memtable:
  │     PUT    → memtable.insert(key, Entry::Value(value))
  │     DELETE → memtable.insert(key, Entry::Tombstone)
  │     update sequence to max(sequence, entry.sequence)
  │
  ├─ entries after the last COMMIT are discarded (uncommitted at crash time)
  │
  └─ return Store with recovered state
```

Committed WAL entries are replayed into the memtable on top of the SSTables. This is safe because entries written before the last flush are already in an SSTable — a flush clears the WAL immediately after the SSTable is fsynced.

### Flush path

```
flush()
  │
  ├─ write the memtable to a new sstable_<id>.sst  ← fsynced
  └─ wal.clear()                                   ← truncate + fsync WAL file
```

The SSTable is always fsynced before the WAL is cleared. If the process crashes between these two steps, the next open finds the complete SSTable and a WAL that still holds the same entries — they replay harmlessly on top of the SSTable.

---

## Examples

### Write two keys and read them back after a simulated restart

```rust
use ferrumdb_core::store::{Store, Value};

// First run
{
    let mut store = Store::open_with_dir("./data/cities").unwrap();
    store.set_value("city".to_string(), Value::Text("Rome".to_string())).unwrap();
    store.set_value("population".to_string(), Value::Integer(2_873_000)).unwrap();
    // Process exits here — no flush called
}

// Second run — WAL replay restores both keys
{
    let store = Store::open_with_dir("./data/cities").unwrap();
    assert_eq!(store.get_value("city").unwrap(), Some(Value::Text("Rome".to_string())));
    assert_eq!(store.get_value("population").unwrap(), Some(Value::Integer(2_873_000)));
}
```

### Flush then write more

```rust
let mut store = Store::open_with_dir("./data/cities").unwrap();

store.set_value("a".to_string(), Value::Integer(1)).unwrap();
store.set_value("b".to_string(), Value::Integer(2)).unwrap();

// Memtable written to an SSTable, WAL cleared
store.flush().unwrap();

// These land only in the WAL
store.set_value("c".to_string(), Value::Integer(3)).unwrap();

// After restart: a and b come from the SSTable, c comes from the WAL
```

### Inspect the WAL directly

```rust
use ferrumdb_core::wal::Wal;

let wal = Wal::with_path("./data/wal.log");
let entries = wal.read_all().unwrap();

for entry in &entries {
    println!("seq={} op={:?} key={}", entry.sequence, entry.operation(), entry.key);
}
```

---

## What the WAL Does Not Currently Do

### No checksum per entry

Bit-flip corruption inside a valid-length record is not detected. A CRC32 written as part of the frame header (before or after the length prefix) would catch this. The SSTable format already does this per block (see `docs/sstable.md`); the WAL does not yet.

---

## What the WAL Now Does (previously listed as missing)

- **Multi-operation atomicity** — via the `COMMIT` marker. `append` no longer fsyncs; `write_commit` writes a COMMIT entry and fsyncs once for the whole batch. On recovery, entries not followed by a COMMIT are truncated. See the write and recovery paths above.
- **Write lock** — `Store::open_with_dir` acquires an exclusive `flock` on `./data/<table>/LOCK`, held for the lifetime of the `Store` and released on drop. A second open of the same table fails immediately.
- **Cached append handle** — the WAL holds its file open for its lifetime instead of reopening on every append. This lifted batched-write throughput ~6× and also sped up single writes. The handle is dropped and reopened only on `clear` (after a flush) and when recovery truncates an uncommitted tail.
