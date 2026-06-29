use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
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

enum TxOp {
    Put(String, Value),
    Delete(String),
}

/// A buffered, atomic unit of work against a single table.
///
/// All operations are held in memory until `commit()` is called. On commit,
/// every entry is written to the WAL followed by a single COMMIT + fsync.
/// Dropping a Transaction without committing is a silent rollback — nothing
/// reaches disk.
pub struct Transaction<'a> {
    store: &'a mut Store,
    ops: Vec<TxOp>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Integer(i32),
    Float(f64),
    Text(String),
    Boolean(bool),
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
    data: BTreeMap<String, Entry>,
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
    pub fn get_data(&self) -> &BTreeMap<String, Entry> {
        &self.data
    }

    /// Sets the memtable byte budget that triggers an automatic flush. Intended
    /// to be called right after opening, to match the target device's memory.
    pub fn set_memtable_budget(&mut self, bytes: usize) {
        self.memtable_max_bytes = bytes;
    }

    /// Inserts into the memtable, keeping the byte estimate in sync. Replacing an
    /// existing key subtracts the old footprint before adding the new one.
    fn memtable_insert(&mut self, key: String, entry: Entry) {
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

    /// Replays committed WAL entries on top of the current memtable. Entries
    /// after the last COMMIT are discarded (uncommitted when the process died).
    fn replay_wal(&mut self) -> Result<(), AppError> {
        let entries = self.wal.read_all()?;
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
        let mut merged: BTreeMap<String, Entry> = BTreeMap::new();
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

    pub fn set_value(&mut self, new_key: String, new_value: Value) -> Result<String, AppError> {
        self.sequence += 1;
        let entry = Wal::create_put_entry(new_key.clone(), &new_value, self.sequence);
        self.wal.append(&entry)?;
        self.wal.write_commit(self.sequence)?;
        self.memtable_insert(new_key.clone(), Entry::Value(new_value));
        self.maybe_flush()?;
        Ok(format!("Inserted value with key {}", new_key))
    }

    /// Looks up a key. Checks the memtable first, then SSTables from newest to
    /// oldest; the first source that has the key wins. A tombstone resolves to
    /// `None`, the same as a key that was never written.
    pub fn get_value(&self, key: &str) -> Result<Option<Value>, AppError> {
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

    /// Deletes a key by writing a tombstone. Idempotent: deleting a key that does
    /// not exist is not an error, because the key may live in an SSTable whose
    /// membership cannot be checked without a disk read.
    pub fn delete_value(&mut self, key: &str) -> Result<String, AppError> {
        self.sequence += 1;
        let entry = Wal::create_delete_entry(key.to_string(), self.sequence);
        self.wal.append(&entry)?;
        self.wal.write_commit(self.sequence)?;
        self.memtable_insert(key.to_string(), Entry::Tombstone);
        self.maybe_flush()?;
        Ok(format!("Deleted value with key {}", key))
    }

    /// Begins an explicit multi-write transaction.
    ///
    /// Operations are buffered in memory and written atomically on `commit()`.
    /// Dropping the transaction without committing discards all buffered ops.
    pub fn begin_transaction(&mut self) -> Transaction<'_> {
        Transaction { store: self, ops: Vec::new() }
    }
}

/// Parses the SSTable id out of a `.../sstable_<id>.sst` path, or `None` if the
/// file name does not match that pattern.
fn sstable_id_from_path(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    name.strip_prefix("sstable_")?.strip_suffix(".sst")?.parse::<u64>().ok()
}

/// Approximate in-memory footprint of one memtable entry: the key bytes plus the
/// value bytes. Used only to drive the flush budget, so an estimate is fine.
fn entry_footprint(key: &str, entry: &Entry) -> usize {
    let value_bytes = match entry {
        Entry::Value(Value::Integer(_)) => 4,
        Entry::Value(Value::Float(_)) => 8,
        Entry::Value(Value::Boolean(_)) => 1,
        Entry::Value(Value::Text(s)) => s.len(),
        Entry::Tombstone => 0,
    };
    key.len() + value_bytes
}

impl<'a> Transaction<'a> {
    pub fn set_value(&mut self, key: String, value: Value) {
        self.ops.push(TxOp::Put(key, value));
    }

    /// Buffers a delete. Idempotent — deleting a key that does not exist is not
    /// an error, matching `Store::delete_value`. The delete is applied as a
    /// tombstone when the transaction commits.
    pub fn delete_value(&mut self, key: String) {
        self.ops.push(TxOp::Delete(key));
    }

    /// Writes all buffered operations to the WAL, writes a COMMIT marker, fsyncs,
    /// then applies every operation to the in-memory memtable.
    /// A single fsync covers the entire batch.
    pub fn commit(self) -> Result<(), AppError> {
        for op in &self.ops {
            self.store.sequence += 1;
            let entry = match op {
                TxOp::Put(k, v) => Wal::create_put_entry(k.clone(), v, self.store.sequence),
                TxOp::Delete(k) => Wal::create_delete_entry(k.clone(), self.store.sequence),
            };
            self.store.wal.append(&entry)?;
        }
        self.store.wal.write_commit(self.store.sequence)?;

        for op in self.ops {
            match op {
                TxOp::Put(k, v) => self.store.memtable_insert(k, Entry::Value(v)),
                TxOp::Delete(k) => self.store.memtable_insert(k, Entry::Tombstone),
            }
        }
        self.store.maybe_flush()?;
        Ok(())
    }
}
