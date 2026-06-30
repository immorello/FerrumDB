# SSTable Module — Design Spec

This document specifies the SSTable (Sorted String Table) format and read/write paths. It began as a contract to implement against; the on-disk format and its reader/writer now exist in `ferrumdb-core/src/sstable.rs`.

**Implementation status:**
- ✅ **Format + reader/writer** (`SsTable::flush` / `open` / `get`) — implemented, with per-block CRC32. Format version 2 also persists each table's key range.
- ✅ **Tombstones in the memtable** — `Store` holds `Entry` (value or tombstone); deletes write tombstones.
- ✅ **Flush wiring** — `Store::flush` writes the memtable to a new SSTable and clears the WAL; an automatic flush fires when the memtable exceeds a threshold. Reads consult the memtable, then SSTables newest→oldest. The snapshot mechanism has been replaced by flush.
- ✅ **Buffer manager** — the flush threshold is a **byte budget** (memtable live size), tunable per table via `Store::set_memtable_budget`, not an entry count. Each SSTable stores its min/max key so a point lookup **skips** any table whose range cannot contain the key.
- ✅ **Compaction** — `Store::compact` merges all SSTables into one (newest value per key, tombstones dropped); runs automatically once the SSTable count exceeds a threshold. The merged table is fsynced before the old files are removed.
- ✅ **Bloom filter** (format v3) — a per-table membership filter persisted in the file and loaded into RAM on open. A lookup for a key the table does not contain returns without reading any block.
- ⬜ **Block cache** — read-path optimization, a later step.

The format is fixed deliberately and up front, because a file format is a forever decision: once FerrumDB writes `.sst` files to a device, the reader must be able to load them for the life of that data.

---

## Purpose

An SSTable is an **immutable, sorted, on-disk file** holding key-value pairs. It is what the in-memory memtable (the `BTreeMap`) becomes when it grows too large to keep in RAM.

The SSTable solves the one problem the current design cannot:

> The memtable grows without bound. On a device with limited RAM, it must eventually be moved to disk.

When the memtable crosses a size threshold, it is **flushed**: written out as one new SSTable, then cleared. Because the `BTreeMap` is already sorted, the flush is a single in-order walk with no extra sort step.

SSTable flush is the normal mechanism for bounding both memtable size and WAL growth; it replaced the earlier snapshot mechanism. After a flush, the entries now safely on disk are removed from the WAL.

---

## Where It Fits

```
write
  │
  ├─ WAL (durability)
  └─ BTreeMap memtable (RAM, sorted)
        │
        │  when memtable exceeds threshold
        ▼
     flush ──► SSTable_3.sst   ← newest
               SSTable_2.sst
               SSTable_1.sst   ← oldest
                    │
                    ▼
               compaction (future — merges SSTables)
```

Reads check the memtable first, then SSTables from **newest to oldest**. The first source that has the key wins. This newest-wins ordering is what makes updates and deletes correct without ever modifying an existing file.

---

## File Format Overview

An SSTable file has three regions, written in this order:

```
┌──────────────────────────────────────────────┐
│ DATA REGION                                    │
│   data block 0   (~4 KB of sorted records)     │
│   data block 1                                 │
│   ...                                          │
│   data block N                                 │
├──────────────────────────────────────────────┤
│ INDEX REGION                                   │
│   sparse index: one entry per data block       │
│   key range: min_key, max_key  (v2)            │
│   bloom filter: k, bit-length, bits  (v3)      │
├──────────────────────────────────────────────┤
│ FOOTER  (fixed 28 bytes)                        │
│   index_offset · index_len · entry_count       │
│   magic · version                              │
└──────────────────────────────────────────────┘
```

The file is read **back-to-front**: the reader seeks to the last 28 bytes (the footer), learns where the index lives, loads the index into RAM, and from then on can find any key with one seek and one block read. The data region is never scanned linearly.

All multi-byte integers are **big-endian**, matching the WAL.

---

## Data Block Format

A data block is a sequence of records followed by a checksum:

```
┌──────────────────────────────────────────┐
│ record 0                                   │
│ record 1                                   │
│ ...                                        │
│ CRC32 of all preceding record bytes (u32)  │
└──────────────────────────────────────────┘
```

A new block is started when adding the next record would push the current block's record bytes past `BLOCK_SIZE` (default **4096**). Blocks are therefore *approximately* 4 KB, not exactly — a single record larger than 4 KB gets its own oversized block. There is no padding.

### Record format

```
┌────────────┬───────────┬────────┬────────────┬──────────────┐
│ key_len    │ key       │ type   │ val_len    │ value        │
│ u32        │ key_len B │ u8     │ u32        │ val_len B    │
└────────────┴───────────┴────────┴────────────┴──────────────┘
```

- **key_len** — byte length of the key.
- **key** — the store key, UTF-8.
- **type** — `0` = value present, `1` = tombstone (a deletion marker; see below).
- **val_len** — byte length of the value. Always `0` when type is tombstone.
- **value** — the typed value, encoded as a protobuf `ValueMessage` (the same encoding the WAL and snapshot already use). Absent for tombstones.

Records within a block are sorted by key in ascending lexicographic order. So are the blocks themselves, and so the whole file is globally sorted.

### Why reuse `ValueMessage`?

FerrumDB already encodes typed values (`Integer`, `Float`, `Text`, `Boolean`) as `ValueMessage` in both the WAL and the snapshot. Reusing it for SSTable values means one encoder and one decoder for typed values across the entire system — no third representation to keep in sync.

### Why a CRC per block?

Embedded targets run on flash storage and lose power unexpectedly. A bit-flip on an SD card would otherwise be returned silently as wrong data. The block CRC is verified on every block read; a mismatch is surfaced as a corruption error rather than bad data. This is the one guarantee the WAL format deliberately lacks, and it belongs here from version 1 — adding it later would be a format-breaking change to the block layout.

---

## Sparse Index Format

The index holds **one entry per data block** — hence "sparse." It does not list every key, only the first key of each block. This keeps the index small enough to live entirely in RAM while still letting the reader jump straight to the right block.

Each index entry:

```
┌────────────┬─────────────┬──────────────┬───────────┐
│ key_len    │ first_key   │ block_offset │ block_len │
│ u32        │ key_len B   │ u64          │ u32       │
└────────────┴─────────────┴──────────────┴───────────┘
```

- **first_key** — the smallest key in that block.
- **block_offset** — byte offset of the block from the start of the file.
- **block_len** — total bytes of the block, including its CRC. With offset + length the reader fetches exactly one block in a single read.

Index entries appear in ascending key order, so a lookup is a binary search.

### Key range (format v2)

Immediately after the index entries, a non-empty table stores its overall key range:

```
┌────────────┬───────────┬────────────┬───────────┐
│ min_key_len│ min_key   │ max_key_len│ max_key   │
│ u32        │ len B     │ u32        │ len B     │
└────────────┴───────────┴────────────┴───────────┘
```

This lives inside the index region (it is covered by `index_len`), so the reader loads it in the same read as the index. With it, a point lookup whose key is `< min_key` or `> max_key` returns immediately without touching a data block — the table is skipped. An empty table writes no key range.

### Bloom filter (format v3)

After the key range, a non-empty table stores a bloom filter over **every** key it holds (values and tombstones alike):

```
┌────────────┬────────────┬───────────────┐
│ k          │ bit_length │ bits          │
│ u32        │ u32        │ bit_length B  │
└────────────┴────────────┴───────────────┘
```

A bloom filter is a bit array probed by `k` hash functions. A lookup hashes the key the same `k` ways: if any of those bits is 0 the key is **definitely absent** and the table is skipped with no block read; if all are 1 the key is *probably* present and the lookup proceeds. It is sized at ~10 bits/key with `k = 7` (≈1% false-positive rate), and the hashing uses double hashing over a single in-crate FNV-1a 64-bit hash — no new dependency, matching the block CRC.

The filter must include tombstone keys: a lookup for a deleted key has to find the tombstone (and stop), not be told the key is absent and fall through to an older table that still holds the old value.

The bloom filter and the key range are written together — both present for a non-empty table, both absent for an empty one — so the reader parses them as a pair when there are bytes after the index entries.

---

## Footer Format

The footer is a fixed **28 bytes** at the very end of the file:

```
┌──────────────┬────────────┬──────────────┬─────────┬─────────┐
│ index_offset │ index_len  │ entry_count  │ magic   │ version │
│ u64          │ u64        │ u32          │ u32     │ u32     │
└──────────────┴────────────┴──────────────┴─────────┴─────────┘
```

- **index_offset** — byte offset where the index region begins.
- **index_len** — byte length of the index region.
- **entry_count** — number of index entries (= number of data blocks).
- **magic** — the ASCII bytes `FSST` (`0x46 0x53 0x53 0x54`). Confirms the file really is a FerrumDB SSTable before any other byte is trusted.
- **version** — format version, currently `3` (v2 added the key range, v3 the bloom filter, both after the index). A reader rejects a version it does not recognize.

Because the footer is fixed-size and last, the reader's first action is `seek(file_size - 28)`, read 28 bytes, check magic and version, then use `index_offset`/`index_len` to load the index.

---

## Tombstones (Deletes)

Deletes cannot remove data from an SSTable, because SSTables are immutable. Instead a delete writes a **tombstone** — a record with `type = 1` — that shadows any older value for that key.

This changes the current delete semantics. Today `delete_value` does `data.remove(key)`. Once SSTables exist, a delete must insert a tombstone into the memtable, because the key may still live in an older SSTable on disk. The tombstone then flushes into a new SSTable like any other record.

On read, newest-wins resolves it: a tombstone found in a newer source means "deleted," and the search stops without consulting older SSTables. Compaction (future) is what finally drops the tombstone and the shadowed value together, reclaiming the space.

---

## The Read Path

```
get(key)
  │
  ├─ 1. memtable (BTreeMap)
  │      hit value     → return it
  │      hit tombstone → return None (deleted)
  │      miss          → continue
  │
  └─ 2. for each SSTable, newest → oldest:
         ├─ skip if key is outside [min_key, max_key]      (no disk read)
         ├─ skip if the bloom filter says key is absent     (no disk read)
         ├─ binary-search the in-RAM sparse index
         │     → the block whose first_key is the largest ≤ key
         ├─ seek + read that one block (offset, len from index)
         ├─ verify block CRC
         ├─ scan the block's records for an exact key match
         │     hit value     → return it
         │     hit tombstone → return None (deleted), stop
         │     miss          → try next SSTable
         │
         └─ exhausted all SSTables → return None
```

The cost of a point lookup is: per SSTable, two in-RAM checks (key range, then bloom filter) that often avoid touching the disk at all; only if both pass does it do one in-memory binary search and at most one 4 KB block read. Absent-key lookups against on-disk SSTables run at roughly in-memory speed because the bloom filter rejects them before any block read.

---

## The Write / Flush Path

```
flush()
  │
  ├─ open a new file:  sstable_<id>.sst   (id = next monotonic counter)
  ├─ walk the memtable in sorted order, emitting records:
  │     accumulate into the current block
  │     when block ≥ BLOCK_SIZE → finalize block (append CRC), record an
  │                               index entry (first_key, offset, len),
  │                               start a new block
  ├─ finalize the last block
  ├─ write the index region; remember its offset and length
  ├─ write the 28-byte footer
  ├─ fsync the file
  ├─ clear the memtable
  └─ truncate the WAL  (its entries are now durable in the SSTable)
```

The new file is fsynced **before** the WAL is truncated. If the process crashes between those two steps, the next open finds a complete SSTable and a WAL that still contains the same entries — they get replayed harmlessly on top of data already in the SSTable, and newest-wins keeps the result correct.

---

## File Naming and the SSTable Set

SSTables live alongside the other table files:

```
./data/users/
  ├── wal.log
  ├── LOCK
  ├── sstable_1.sst    ← oldest
  ├── sstable_2.sst
  └── sstable_3.sst    ← newest
```

The numeric id is a monotonic counter. **Higher id = newer**, which directly gives the newest-to-oldest read order. On `Store::open_with_dir`, every `.sst` file is discovered, its footer and index loaded into RAM, and the set is ordered by id descending.

---

## Compaction

As SSTables accumulate, a point lookup has more tables to consult and deleted keys are never reclaimed. `Store::compact` performs a **full merge**: it reads every entry from every SSTable (oldest → newest so newer values overwrite older), drops tombstones (a full merge leaves nothing older for a tombstone to shadow), and writes the result as a single new SSTable. It runs automatically once the SSTable count exceeds `MAX_SSTABLES`.

```
compact()
  ├─ merge all SSTables, newest value per key wins
  ├─ drop tombstones (and the values they shadowed)
  ├─ write one new SSTable, fsync it
  └─ delete the old SSTable files
```

The new table is fsynced **before** the old files are removed. A crash in between is safe: on reopen both the merged table and the stale ones load, the merged table has the highest id (newest), and it wins every read; the next compaction cleans up the leftovers. The current implementation buffers the merged data in memory, so peak memory during compaction is roughly the combined live size of the tables being merged — fine while that stays within a few memtable budgets, and a candidate for a streaming merge later.

---

## Scope — What This Spec Does NOT Cover

These are deliberately left for later steps so the first SSTable implementation stays tractable:

- **Block cache** — keeping hot SSTable blocks in memory so reads that miss the memtable do not always hit disk.
- **Configurable block size** — `BLOCK_SIZE` is a hardcoded `const` (4096) for now. It becomes a `Config` field only when a second knob justifies a config struct.
- **Secondary indexes** — a separate parallel set of SSTables mapping value → key. Not meaningful until values become structured rather than scalar.
- **Compression** — block compression (e.g. LZ4) trades CPU for disk space. Easy to add later as a per-block flag.

---

## Resolved Decisions

1. **Flush trigger** — *resolved:* both. An explicit `Store::flush()` plus an automatic flush when the memtable's live size passes a **byte budget** (`DEFAULT_MEMTABLE_MAX_BYTES`, 1 MiB), tunable per table via `Store::set_memtable_budget`.
2. **Snapshot vs SSTable** — *resolved:* fully replaced. `snapshot.pb` / `persistence.rs` are gone; SSTable flush is the single path to disk.
3. **CRC scope** — *resolved:* per-block CRC32 is implemented (format v1+).
