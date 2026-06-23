# WAL Module — Current State

This document describes the Write-Ahead Log as it exists today: its file format, its Rust API, how it integrates with the Store, and what it does and does not guarantee. It is meant as a reference for reasoning about the next changes (COMMIT markers, write lock).

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

- **operation** — `PUT` or `DELETE`. `PUT` requires a `value`. `DELETE` leaves `value` empty.
- **key** — the store key as a UTF-8 string.
- **sequence** — a monotonically increasing counter assigned by `Store`, not by `Wal`. The WAL itself does not validate or assign sequence numbers. After recovery, `Store` sets its counter to the highest sequence seen so new writes never reuse old numbers.
- **timestamp** — exists in the schema but is always written as `0`. Currently unused.

---

## Rust API

### `Wal` struct

```rust
pub struct Wal {
    file_path: String,
}
```

Holds only the path to the WAL file. It is stateless between calls — the file is opened and closed on every `append`. There is no held file descriptor.

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
pub fn append(&self, entry: &WalEntry) -> Result<(), AppError>
```

Opens the file in append mode, writes the length prefix followed by the protobuf payload, calls `flush` and then `sync_all`. The file is closed after every call.

The double call (`flush` then `sync_all`) is intentional: `flush` drains the userspace buffer, `sync_all` issues an `fsync` to push data from the OS page cache to the physical medium.

**The file is reopened on every call.** This is correct but expensive for high write rates. It is a known limitation.

### `read_all`

```rust
pub fn read_all(&self) -> Result<Vec<WalEntry>, AppError>
```

Reads the entire file into memory, then parses records sequentially. Returns all successfully decoded entries in write order.

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

Every mutation goes through the WAL before touching the in-memory `BTreeMap`:

```
set_value("price", 19.99)
  │
  ├─ increment sequence  (sequence = N)
  ├─ build WalEntry      (PUT, "price", 19.99, N)
  ├─ wal.append(entry)   ← fsync happens here
  └─ data.insert(...)    ← only after WAL succeeds
```

If `wal.append` fails, the `BTreeMap` is not touched. The write either lands in both or in neither.

```
delete_value("price")
  │
  ├─ check key exists    ← returns KeyNotFound if missing
  ├─ increment sequence  (sequence = N)
  ├─ build WalEntry      (DELETE, "price", N)
  ├─ wal.append(entry)   ← fsync happens here
  └─ data.remove(...)    ← only after WAL succeeds
```

### Recovery path (`Store::open`)

```
open_with_paths(snapshot_path, wal_path)
  │
  ├─ if snapshot exists → load_from_file()
  │     reads protobuf snapshot into BTreeMap
  │
  ├─ wal.read_all()
  │     returns all entries in write order
  │
  ├─ for each entry:
  │     PUT    → data.insert(key, value)
  │     DELETE → data.remove(key)
  │     update sequence to max(sequence, entry.sequence)
  │
  └─ return Store with recovered state
```

All WAL entries are replayed unconditionally on top of the snapshot. This is safe because WAL entries written before the last checkpoint are already baked into the snapshot — the checkpoint always clears the WAL immediately after writing the snapshot.

### Checkpoint path

```
checkpoint()
  │
  ├─ save_to_file()   ← write + fsync snapshot to disk
  └─ wal.clear()      ← truncate + fsync WAL file
```

The snapshot is always written before the WAL is cleared. If the process crashes between these two steps, the next `open` loads the good snapshot and finds an empty or partial WAL — both are safe.

---

## Examples

### Write two keys and read them back after a simulated restart

```rust
use ferrumdb_core::store::{Store, Value};

// First run
{
    let mut store = Store::open_with_paths("./data/snap.pb", "./data/wal.log").unwrap();
    store.set_value("city".to_string(), Value::Text("Rome".to_string())).unwrap();
    store.set_value("population".to_string(), Value::Integer(2_873_000)).unwrap();
    // Process exits here — no checkpoint called
}

// Second run — WAL replay restores both keys
{
    let store = Store::open_with_paths("./data/snap.pb", "./data/wal.log").unwrap();
    assert_eq!(store.get_value("city"), Some(&Value::Text("Rome".to_string())));
    assert_eq!(store.get_value("population"), Some(&Value::Integer(2_873_000)));
}
```

### Checkpoint then write more

```rust
let mut store = Store::open_with_paths("./data/snap.pb", "./data/wal.log").unwrap();

store.set_value("a".to_string(), Value::Integer(1)).unwrap();
store.set_value("b".to_string(), Value::Integer(2)).unwrap();

// Snapshot written, WAL cleared
store.checkpoint().unwrap();

// These land only in the WAL
store.set_value("c".to_string(), Value::Integer(3)).unwrap();

// After restart: a and b come from snapshot, c comes from WAL
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

### No multi-operation atomicity

Every `set_value` and `delete_value` is immediately fsynced as its own WAL entry. There is no way to group multiple operations so that either all of them land or none of them do. Example:

```rust
store.set_value("balance_a".to_string(), Value::Integer(900)).unwrap();
// crash here
store.set_value("balance_b".to_string(), Value::Integer(100)).unwrap();
```

After recovery, only `balance_a` is updated. The two writes are not atomic.

To fix this, the WAL needs a `COMMIT` marker. The write path would become: buffer entries in memory → write all entries to WAL → write COMMIT entry → fsync once → apply to BTreeMap. The replay path would discard any entries not followed by a COMMIT.

### No write lock

Nothing prevents two processes from opening the same store directory simultaneously. Both would write to the same WAL file, interleaving entries with potentially conflicting sequence numbers, and produce a corrupt store.

The fix is a lockfile (`./data/LOCK`) acquired exclusively when `Store::open` is called and released when the `Store` is dropped.

### No checksum per entry

Bit-flip corruption inside a valid-length record is not detected. A CRC32 written as part of the frame header (before or after the length prefix) would catch this.

### File reopened on every append

The WAL file is opened, written, and closed on each `append` call. For high write rates this is expensive. The fix is to hold the file open for the lifetime of the `Wal` instance and use `&mut self` on `append`.
