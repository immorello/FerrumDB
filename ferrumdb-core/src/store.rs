use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use crate::errors::AppError;
use crate::proto::value_message::Kind;
use crate::proto::{Operation, ValueMessage};
use crate::sstable::Entry;
use crate::wal::{Wal, WAL_PATH};

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

pub const STORAGE_PATH: &str = "./data/storage.pb";

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Integer(i32),
    Float(f64),
    Text(String),
    Boolean(bool),
}

#[derive(Debug)]
pub struct Store {
    // The memtable. Holds committed values and tombstones (deletion markers).
    // A tombstone shadows any older value for the same key once SSTables exist.
    data: BTreeMap<String, Entry>,
    wal: Wal,
    sequence: u64,
    pub(crate) snapshot_path: String,
    // Held for its Drop — releasing it unlocks the table for other processes.
    _lock: Option<File>,
}

impl Store {
    pub fn new() -> Store {
        Store {
            data: BTreeMap::new(),
            wal: Wal::new(),
            sequence: 0,
            snapshot_path: STORAGE_PATH.to_string(),
            _lock: None,
        }
    }

    pub fn get_data(&self) -> &BTreeMap<String, Entry> {
        &self.data
    }

    pub fn from_data(data: BTreeMap<String, Value>) -> Store {
        let data = data.into_iter().map(|(k, v)| (k, Entry::Value(v))).collect();
        Store {
            data,
            wal: Wal::new(),
            sequence: 0,
            snapshot_path: STORAGE_PATH.to_string(),
            _lock: None,
        }
    }

    /// Opens a Store at the default paths, loading the last snapshot and replaying WAL entries.
    /// This is the correct production entry point — use `new()` only for tests that want a blank slate.
    pub fn open() -> Result<Store, AppError> {
        Self::open_with_paths(STORAGE_PATH, WAL_PATH)
    }

    /// Opens a named table, storing all its files under ./data/<name>/.
    pub fn open_table(name: &str) -> Result<Store, AppError> {
        let snapshot = format!("./data/{}/snapshot.pb", name);
        let wal      = format!("./data/{}/wal.log", name);
        Self::open_with_paths(&snapshot, &wal)
    }

    /// Opens a Store at custom paths. Use in tests to avoid file-level conflicts between test cases.
    pub fn open_with_paths(snapshot_path: &str, wal_path: &str) -> Result<Store, AppError> {
        // Ensure the table directory exists before acquiring the lock.
        if let Some(parent) = Path::new(snapshot_path).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::IoError(e.to_string()))?;
        }

        // Acquire an exclusive lock on ./data/<table>/LOCK.
        // The lock is held for the lifetime of the Store and released automatically on drop.
        let lock_path = Path::new(snapshot_path)
            .parent()
            .map(|p| p.join("LOCK"))
            .unwrap_or_else(|| "LOCK".into());

        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
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

        let mut store = Store {
            data: BTreeMap::new(),
            wal: Wal::with_path(wal_path),
            sequence: 0,
            snapshot_path: snapshot_path.to_string(),
            _lock: Some(lock_file),
        };

        if Path::new(snapshot_path).exists() {
            let snapshot = store.load_from_file()?;
            store.data = snapshot.data;
        }

        let entries = store.wal.read_all()?;
        let mut pending: Vec<crate::proto::WalEntry> = Vec::new();
        for entry in entries {
            match entry.operation() {
                Operation::Put | Operation::Delete => {
                    pending.push(entry);
                }
                Operation::Commit => {
                    for e in &pending {
                        match e.operation() {
                            Operation::Put => {
                                if let Some(value_msg) = &e.value {
                                    let value = Store::value_from_proto(value_msg)?;
                                    store.data.insert(e.key.clone(), Entry::Value(value));
                                }
                            }
                            Operation::Delete => {
                                store.data.insert(e.key.clone(), Entry::Tombstone);
                            }
                            _ => {}
                        }
                        store.sequence = store.sequence.max(e.sequence);
                    }
                    store.sequence = store.sequence.max(entry.sequence);
                    pending.clear();
                }
            }
        }
        // Entries in `pending` have no following COMMIT — discarded (uncommitted at crash time).

        Ok(store)
    }

    /// Writes the current state as a snapshot and clears the WAL.
    /// After a checkpoint, WAL replay on the next open starts from an empty log.
    pub fn checkpoint(&mut self) -> Result<(), AppError> {
        self.save_to_file().map_err(AppError::IoError)?;
        self.wal.clear()
    }

    pub fn set_value(&mut self, new_key: String, new_value: Value) -> Result<String, AppError> {
        self.sequence += 1;
        let entry = Wal::create_put_entry(new_key.clone(), &new_value, self.sequence);
        self.wal.append(&entry)?;
        self.wal.write_commit(self.sequence)?;
        self.data.insert(new_key.clone(), Entry::Value(new_value));
        Ok(format!("Inserted value with key {}", new_key))
    }

    /// Looks up a key. A tombstone (deleted key) resolves to `None`, the same as
    /// a key that was never written. Returns an owned value because, once the
    /// SSTable layer lands, the value may be read fresh from disk.
    pub fn get_value(&self, key: &str) -> Result<Option<Value>, AppError> {
        match self.data.get(key) {
            Some(Entry::Value(v)) => Ok(Some(v.clone())),
            Some(Entry::Tombstone) => Ok(None),
            None => Ok(None),
        }
    }

    /// Deletes a key by writing a tombstone. Idempotent: deleting a key that does
    /// not exist is not an error, because once SSTables exist a key's membership
    /// cannot be checked without a disk read.
    pub fn delete_value(&mut self, key: &str) -> Result<String, AppError> {
        self.sequence += 1;
        let entry = Wal::create_delete_entry(key.to_string(), self.sequence);
        self.wal.append(&entry)?;
        self.wal.write_commit(self.sequence)?;
        self.data.insert(key.to_string(), Entry::Tombstone);
        Ok(format!("Deleted value with key {}", key))
    }

    /// Begins an explicit multi-write transaction.
    ///
    /// Operations are buffered in memory and written atomically on `commit()`.
    /// Dropping the transaction without committing discards all buffered ops.
    pub fn begin_transaction(&mut self) -> Transaction<'_> {
        Transaction { store: self, ops: Vec::new() }
    }

    pub fn list_values(&self) -> Result<String, AppError> {
        if self.data.is_empty() {
            return Err(AppError::InternalError("Store is empty".to_string()));
        }

        let mut result = "Here's the complete list of items in the store:\n".to_string();
        for (key, entry) in &self.data {
            let value = match entry {
                Entry::Value(v) => v,
                Entry::Tombstone => continue, // deleted key — not listed
            };
            let line = match value {
                Value::Integer(num) => format!("Value for item with key {}: {}\n", key, num),
                Value::Float(num) => format!("Value for item with key {}: {}\n", key, num),
                Value::Text(txt) => format!("Value for item with key {}: {}\n", key, txt),
                Value::Boolean(boolean) => {
                    format!("Value for item with key {}: {}\n", key, boolean)
                }
            };
            result.push_str(&line);
        }

        Ok(result)
    }

    pub(crate) fn value_from_proto(msg: &ValueMessage) -> Result<Value, AppError> {
        match msg.kind.as_ref() {
            Some(Kind::Integer(n)) => Ok(Value::Integer(*n)),
            Some(Kind::Float(n)) => Ok(Value::Float(*n)),
            Some(Kind::Text(s)) => Ok(Value::Text(s.clone())),
            Some(Kind::Boolean(b)) => Ok(Value::Boolean(*b)),
            None => Err(AppError::DecodeError(
                "WAL entry has unknown value type".to_string(),
            )),
        }
    }
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
    /// then applies every operation to the in-memory BTreeMap.
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
                TxOp::Put(k, v) => { self.store.data.insert(k, Entry::Value(v)); }
                TxOp::Delete(k) => { self.store.data.insert(k, Entry::Tombstone); }
            }
        }
        Ok(())
    }
}
