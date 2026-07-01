use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::ops::Bound;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use crate::errors::AppError;
use crate::proto::value_message::Kind;
use crate::proto::{Operation, ValueMessage};
use crate::sstable::{Entry, SsTable};
use crate::wal::Wal;

/// Default memtable budget: once the live data in the memtable reaches this many
/// bytes, it is flushed to a new SSTable. Sized as a budget rather than an entry
/// count so the threshold tracks actual memory use. Tunable per table via
/// [`Store::set_memtable_budget`] to match the target device.
const DEFAULT_MEMTABLE_MAX_BYTES: usize = 4 << 20; // 4 MiB

/// Once the number of SSTables exceeds this, they are compacted into one. A higher
/// value means fewer compactions (less write amplification) but more files to
/// consult per lookup; the per-SSTable key range keeps that cost low.
const MAX_SSTABLES: usize = 8;

/// An interactive, atomic unit of work against a single table.
///
/// Writes are buffered in memory until `commit()`. Reads through the transaction
/// see its own uncommitted writes first (read-your-writes), then fall through to
/// the committed state. On commit, every buffered write goes to the WAL followed
/// by a single COMMIT + fsync. Dropping the transaction — or calling `rollback()`
/// — discards the buffer; nothing reaches disk.
pub struct Transaction<'a> {
    store: &'a mut Store,
    // Buffered writes: `Some(value)` is a put, `None` is a delete. A BTreeMap so a
    // later write to a key replaces an earlier one within the transaction.
    pending: BTreeMap<Vec<u8>, Option<Value>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Integer(i32),
    Float(f64),
    Text(String),
    Boolean(bool),
    Bytes(Vec<u8>),
}

impl Value {
    /// Encodes this value into its protobuf form. This is the single place a
    /// `Value` becomes a `ValueMessage` — the WAL and the SSTable both go
    /// through here.
    pub(crate) fn to_proto(&self) -> ValueMessage {
        let kind = match self {
            Value::Integer(n) => Kind::Integer(*n),
            Value::Float(n) => Kind::Float(*n),
            Value::Text(s) => Kind::Text(s.clone()),
            Value::Boolean(b) => Kind::Boolean(*b),
            Value::Bytes(b) => Kind::Data(b.clone()),
        };
        ValueMessage { kind: Some(kind) }
    }

    /// Decodes a value from its protobuf form. The inverse of [`Value::to_proto`].
    pub(crate) fn from_proto(msg: &ValueMessage) -> Result<Value, AppError> {
        match msg.kind.as_ref() {
            Some(Kind::Integer(n)) => Ok(Value::Integer(*n)),
            Some(Kind::Float(n)) => Ok(Value::Float(*n)),
            Some(Kind::Text(s)) => Ok(Value::Text(s.clone())),
            Some(Kind::Boolean(b)) => Ok(Value::Boolean(*b)),
            Some(Kind::Data(b)) => Ok(Value::Bytes(b.clone())),
            None => Err(AppError::DecodeError(
                "value message has no kind set".to_string(),
            )),
        }
    }
}

/// A single table. All of its files live under one directory:
/// `wal.log`, `LOCK`, and `sstable_<id>.sst` files.
#[derive(Debug)]
pub struct Store {
    // The memtable. Holds committed values and tombstones (deletion markers).
    // A tombstone shadows any older value for the same key in the SSTables below.
    // Keys are arbitrary bytes, sorted lexicographically.
    data: BTreeMap<Vec<u8>, Entry>,
    // Running estimate of the memtable's live data size in bytes (keys + values).
    // Drives the flush decision; kept in sync by `memtable_insert`.
    memtable_bytes: usize,
    memtable_max_bytes: usize,
    wal: Wal,
    sequence: u64,
    dir: PathBuf,
    // Immutable on-disk tables, ordered newest first (index 0 is the newest).
    // Reads consult the memtable, then these in order.
    sstables: Vec<SsTable>,
    next_sstable_id: u64,
    // Held for its Drop — releasing it unlocks the table for other processes.
    _lock: Option<File>,
}

impl Store {
    /// Returns the in-memory portion of the table (the memtable). Data that has
    /// been flushed to SSTables is not included.
    pub fn get_data(&self) -> &BTreeMap<Vec<u8>, Entry> {
        &self.data
    }

    /// Sets the memtable byte budget that triggers an automatic flush. Intended
    /// to be called right after opening, to match the target device's memory.
    pub fn set_memtable_budget(&mut self, bytes: usize) {
        self.memtable_max_bytes = bytes;
    }

    /// Inserts into the memtable, keeping the byte estimate in sync. Replacing an
    /// existing key subtracts the old footprint before adding the new one.
    fn memtable_insert(&mut self, key: Vec<u8>, entry: Entry) {
        self.memtable_bytes += entry_footprint(&key, &entry);
        if let Some(old) = self.data.get(&key) {
            self.memtable_bytes -= entry_footprint(&key, old);
        }
        self.data.insert(key, entry);
    }

    /// Opens a named table, storing all its files under `./data/<name>/`.
    pub fn open_table(name: &str) -> Result<Store, AppError> {
        Self::open_with_dir(&format!("./data/{}", name))
    }

    /// Opens a table rooted at `dir`, acquiring an exclusive lock, loading any
    /// existing SSTables, and replaying the WAL on top. Use a unique `dir` per
    /// table; tests use this directly for isolation.
    pub fn open_with_dir(dir: &str) -> Result<Store, AppError> {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).map_err(|e| AppError::IoError(e.to_string()))?;

        // Acquire an exclusive lock on <dir>/LOCK, held for the lifetime of the
        // Store and released automatically on drop.
        let lock_path = dir.join("LOCK");
        let lock_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false) // the lock file is a marker; never truncate it
            .open(&lock_path)
            .map_err(|e| AppError::IoError(e.to_string()))?;

        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn flock(fd: i32, operation: i32) -> i32;
            }
            const LOCK_EX: i32 = 2;
            const LOCK_NB: i32 = 4;
            let ret = unsafe { flock(lock_file.as_raw_fd(), LOCK_EX | LOCK_NB) };
            if ret != 0 {
                return Err(AppError::IoError(format!(
                    "table is already open: {}",
                    lock_path.display()
                )));
            }
        }

        let wal = Wal::with_path(dir.join("wal.log").to_string_lossy().into_owned());
        let mut store = Store {
            data: BTreeMap::new(),
            memtable_bytes: 0,
            memtable_max_bytes: DEFAULT_MEMTABLE_MAX_BYTES,
            wal,
            sequence: 0,
            dir,
            sstables: Vec::new(),
            next_sstable_id: 1,
            _lock: Some(lock_file),
        };

        store.load_sstables()?;
        store.replay_wal()?;

        Ok(store)
    }

    /// Discovers `sstable_<id>.sst` files in the table directory, opens each
    /// (loading its sparse index into RAM), and orders them newest first.
    fn load_sstables(&mut self) -> Result<(), AppError> {
        let read_dir = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(_) => return Ok(()), // nothing to load
        };

        let mut found: Vec<(u64, SsTable)> = Vec::new();
        for entry in read_dir {
            let path = entry.map_err(|e| AppError::IoError(e.to_string()))?.path();
            if let Some(id) = sstable_id_from_path(&path) {
                found.push((id, SsTable::open(&path)?));
            }
        }

        // Highest id is newest; the next id to assign is one past the max.
        self.next_sstable_id = found.iter().map(|(id, _)| *id + 1).max().unwrap_or(1);
        found.sort_by_key(|(id, _)| std::cmp::Reverse(*id)); // newest first
        self.sstables = found.into_iter().map(|(_, sst)| sst).collect();
        Ok(())
    }

    /// Replays committed WAL entries on top of the current memtable. The
    /// uncommitted tail (entries after the last COMMIT) is truncated from the
    /// WAL by `read_for_recovery`, so a later COMMIT cannot adopt them.
    fn replay_wal(&mut self) -> Result<(), AppError> {
        let entries = self.wal.read_for_recovery()?;
        let mut pending: Vec<crate::proto::WalEntry> = Vec::new();

        for entry in entries {
            match entry.operation() {
                Operation::Put | Operation::Delete => pending.push(entry),
                Operation::Commit => {
                    for e in &pending {
                        match e.operation() {
                            Operation::Put => {
                                if let Some(value_msg) = &e.value {
                                    let value = Value::from_proto(value_msg)?;
                                    self.memtable_insert(e.key.clone(), Entry::Value(value));
                                }
                            }
                            Operation::Delete => {
                                self.memtable_insert(e.key.clone(), Entry::Tombstone);
                            }
                            _ => {}
                        }
                        self.sequence = self.sequence.max(e.sequence);
                    }
                    self.sequence = self.sequence.max(entry.sequence);
                    pending.clear();
                }
            }
        }
        Ok(())
    }

    /// Writes the whole memtable out as a new immutable SSTable, then clears the
    /// memtable and truncates the WAL. The SSTable is fsynced before the WAL is
    /// cleared: a crash in between leaves the SSTable complete and the WAL still
    /// holding the same entries, which replay harmlessly on top.
    pub fn flush(&mut self) -> Result<(), AppError> {
        if self.data.is_empty() {
            return Ok(());
        }

        let id = self.next_sstable_id;
        let path = self.dir.join(format!("sstable_{}.sst", id));
        let sst = SsTable::flush(&path, self.data.iter())?;
        self.next_sstable_id += 1;

        self.sstables.insert(0, sst); // newest at the front
        self.data.clear();
        self.memtable_bytes = 0;
        self.wal.clear()?;

        self.maybe_compact()?;
        Ok(())
    }

    /// Flushes if the memtable's byte estimate has reached the budget.
    fn maybe_flush(&mut self) -> Result<(), AppError> {
        if self.memtable_bytes >= self.memtable_max_bytes {
            self.flush()?;
        }
        Ok(())
    }

    /// Compacts if too many SSTables have accumulated.
    fn maybe_compact(&mut self) -> Result<(), AppError> {
        if self.sstables.len() > MAX_SSTABLES {
            self.compact()?;
        }
        Ok(())
    }

    /// Merges all SSTables into a single new one, keeping only the newest value
    /// per key and dropping tombstones (a full merge leaves nothing older for a
    /// tombstone to shadow). The merged table is fsynced before the old files are
    /// removed, so a crash in between is safe: the stale tables simply replay
    /// behind the newer merged table and lose to it on every read.
    pub fn compact(&mut self) -> Result<(), AppError> {
        if self.sstables.len() < 2 {
            return Ok(()); // nothing worth merging
        }

        // Merge oldest -> newest so a newer entry overwrites an older one.
        let mut merged: BTreeMap<Vec<u8>, Entry> = BTreeMap::new();
        for sst in self.sstables.iter().rev() {
            for (key, entry) in sst.read_all_entries()? {
                merged.insert(key, entry);
            }
        }
        // Full merge: a tombstone has nothing older left to shadow, so drop it.
        merged.retain(|_, entry| matches!(entry, Entry::Value(_)));

        let old_paths: Vec<PathBuf> = self.sstables.iter().map(|s| s.path().to_path_buf()).collect();

        if merged.is_empty() {
            // Everything was deleted — no new table needed.
            self.sstables.clear();
        } else {
            let id = self.next_sstable_id;
            let path = self.dir.join(format!("sstable_{}.sst", id));
            let new_sst = SsTable::flush(&path, merged.iter())?;
            self.next_sstable_id += 1;
            self.sstables = vec![new_sst];
        }

        // Best-effort cleanup of the now-obsolete files (safe to leave if it fails).
        for path in old_paths {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }

    pub fn set_value(&mut self, new_key: Vec<u8>, new_value: Value) -> Result<String, AppError> {
        self.sequence += 1;
        let entry = Wal::create_put_entry(new_key.clone(), &new_value, self.sequence);
        self.wal.append(&entry)?;
        self.wal.write_commit(self.sequence)?;
        self.memtable_insert(new_key, Entry::Value(new_value));
        self.maybe_flush()?;
        Ok("Inserted value".to_string())
    }

    /// Looks up a key. Checks the memtable first, then SSTables from newest to
    /// oldest; the first source that has the key wins. A tombstone resolves to
    /// `None`, the same as a key that was never written.
    pub fn get_value(&self, key: &[u8]) -> Result<Option<Value>, AppError> {
        match self.data.get(key) {
            Some(Entry::Value(v)) => return Ok(Some(v.clone())),
            Some(Entry::Tombstone) => return Ok(None),
            None => {}
        }

        for sst in &self.sstables {
            match sst.get(key)? {
                Some(Entry::Value(v)) => return Ok(Some(v)),
                Some(Entry::Tombstone) => return Ok(None),
                None => {}
            }
        }

        Ok(None)
    }

    /// Returns all key/value pairs whose key falls within `(lo, hi)`, in ascending
    /// key order. Merges the memtable with every SSTable — newest value per key
    /// wins, tombstoned keys are excluded. SSTables whose key range does not
    /// overlap the bounds are skipped without a read.
    ///
    /// This materializes the matching range in memory; a streaming, block-ranged
    /// iterator is a future optimization.
    pub fn scan_range(
        &self,
        lo: Bound<&[u8]>,
        hi: Bound<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Value)>, AppError> {
        let mut merged: BTreeMap<Vec<u8>, Entry> = BTreeMap::new();

        // Oldest -> newest so a newer entry overwrites an older one: SSTables from
        // oldest to newest, then the memtable (newest of all).
        for sst in self.sstables.iter().rev() {
            if let Some((min, max)) = sst.key_range()
                && !range_overlaps(min, max, lo, hi)
            {
                continue;
            }
            for (key, entry) in sst.read_all_entries()? {
                if in_bounds(&key, lo, hi) {
                    merged.insert(key, entry);
                }
            }
        }
        for (key, entry) in self.data.range::<[u8], _>((lo, hi)) {
            merged.insert(key.clone(), entry.clone());
        }

        Ok(merged
            .into_iter()
            .filter_map(|(key, entry)| match entry {
                Entry::Value(v) => Some((key, v)),
                Entry::Tombstone => None,
            })
            .collect())
    }

    /// Deletes a key by writing a tombstone. Idempotent: deleting a key that does
    /// not exist is not an error, because the key may live in an SSTable whose
    /// membership cannot be checked without a disk read.
    pub fn delete_value(&mut self, key: &[u8]) -> Result<String, AppError> {
        self.sequence += 1;
        let entry = Wal::create_delete_entry(key.to_vec(), self.sequence);
        self.wal.append(&entry)?;
        self.wal.write_commit(self.sequence)?;
        self.memtable_insert(key.to_vec(), Entry::Tombstone);
        self.maybe_flush()?;
        Ok("Deleted value".to_string())
    }

    /// Begins an interactive transaction.
    ///
    /// Writes are buffered and applied atomically on `commit()`; reads through the
    /// transaction see its own uncommitted writes. Dropping it (or `rollback()`)
    /// discards everything.
    pub fn begin_transaction(&mut self) -> Transaction<'_> {
        Transaction { store: self, pending: BTreeMap::new() }
    }
}

/// Parses the SSTable id out of a `.../sstable_<id>.sst` path, or `None` if the
/// file name does not match that pattern.
fn sstable_id_from_path(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    name.strip_prefix("sstable_")?.strip_suffix(".sst")?.parse::<u64>().ok()
}

/// Whether `key` lies within the `(lo, hi)` bounds.
fn in_bounds(key: &[u8], lo: Bound<&[u8]>, hi: Bound<&[u8]>) -> bool {
    let above_lo = match lo {
        Bound::Unbounded => true,
        Bound::Included(b) => key >= b,
        Bound::Excluded(b) => key > b,
    };
    let below_hi = match hi {
        Bound::Unbounded => true,
        Bound::Included(b) => key <= b,
        Bound::Excluded(b) => key < b,
    };
    above_lo && below_hi
}

/// Whether an SSTable spanning `[min, max]` can contain any key within `(lo, hi)`.
fn range_overlaps(min: &[u8], max: &[u8], lo: Bound<&[u8]>, hi: Bound<&[u8]>) -> bool {
    let max_below_lo = match lo {
        Bound::Unbounded => false,
        Bound::Included(b) => max < b,
        Bound::Excluded(b) => max <= b,
    };
    let min_above_hi = match hi {
        Bound::Unbounded => false,
        Bound::Included(b) => min > b,
        Bound::Excluded(b) => min >= b,
    };
    !max_below_lo && !min_above_hi
}

/// Approximate in-memory footprint of one memtable entry: the key bytes plus the
/// value bytes. Used only to drive the flush budget, so an estimate is fine.
fn entry_footprint(key: &[u8], entry: &Entry) -> usize {
    let value_bytes = match entry {
        Entry::Value(Value::Integer(_)) => 4,
        Entry::Value(Value::Float(_)) => 8,
        Entry::Value(Value::Boolean(_)) => 1,
        Entry::Value(Value::Text(s)) => s.len(),
        Entry::Value(Value::Bytes(b)) => b.len(),
        Entry::Tombstone => 0,
    };
    key.len() + value_bytes
}

impl<'a> Transaction<'a> {
    /// Buffers a write, replacing any earlier buffered write for the same key.
    pub fn set_value(&mut self, key: Vec<u8>, value: Value) {
        self.pending.insert(key, Some(value));
    }

    /// Buffers a delete. Idempotent — deleting a key that does not exist is not
    /// an error, matching `Store::delete_value`. Applied as a tombstone on commit.
    pub fn delete_value(&mut self, key: Vec<u8>) {
        self.pending.insert(key, None);
    }

    /// Reads within the transaction. A key written or deleted in this transaction
    /// resolves to its buffered state (read-your-writes); otherwise the read falls
    /// through to the committed store.
    pub fn get_value(&self, key: &[u8]) -> Result<Option<Value>, AppError> {
        match self.pending.get(key) {
            Some(Some(value)) => Ok(Some(value.clone())),
            Some(None) => Ok(None), // deleted in this transaction
            None => self.store.get_value(key),
        }
    }

    /// Writes all buffered operations to the WAL, writes one COMMIT marker, fsyncs,
    /// then applies them to the in-memory memtable. A single fsync covers the batch.
    pub fn commit(self) -> Result<(), AppError> {
        if self.pending.is_empty() {
            return Ok(());
        }

        for (key, val) in &self.pending {
            self.store.sequence += 1;
            let entry = match val {
                Some(v) => Wal::create_put_entry(key.clone(), v, self.store.sequence),
                None => Wal::create_delete_entry(key.clone(), self.store.sequence),
            };
            self.store.wal.append(&entry)?;
        }
        self.store.wal.write_commit(self.store.sequence)?;

        for (key, val) in self.pending {
            match val {
                Some(v) => self.store.memtable_insert(key, Entry::Value(v)),
                None => self.store.memtable_insert(key, Entry::Tombstone),
            }
        }
        self.store.maybe_flush()?;
        Ok(())
    }

    /// Discards all buffered writes without touching disk. Equivalent to dropping
    /// the transaction; provided so intent reads clearly at the call site.
    pub fn rollback(self) {}
}
