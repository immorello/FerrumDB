use std::collections::BTreeMap;
use std::path::Path;

use crate::errors::AppError;
use crate::proto::value_message::Kind;
use crate::proto::{Operation, ValueMessage};
use crate::wal::{Wal, WAL_PATH};

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
    data: BTreeMap<String, Value>,
    wal: Wal,
    sequence: u64,
    pub(crate) snapshot_path: String,
}

impl Store {
    pub fn new() -> Store {
        Store {
            data: BTreeMap::new(),
            wal: Wal::new(),
            sequence: 0,
            snapshot_path: STORAGE_PATH.to_string(),
        }
    }

    pub fn get_data(&self) -> &BTreeMap<String, Value> {
        &self.data
    }

    pub fn from_data(data: BTreeMap<String, Value>) -> Store {
        Store {
            data,
            wal: Wal::new(),
            sequence: 0,
            snapshot_path: STORAGE_PATH.to_string(),
        }
    }

    /// Opens a Store at the default paths, loading the last snapshot and replaying WAL entries.
    /// This is the correct production entry point — use `new()` only for tests that want a blank slate.
    pub fn open() -> Result<Store, AppError> {
        Self::open_with_paths(STORAGE_PATH, WAL_PATH)
    }

    /// Opens a Store at custom paths. Use in tests to avoid file-level conflicts between test cases.
    pub fn open_with_paths(snapshot_path: &str, wal_path: &str) -> Result<Store, AppError> {
        let mut store = Store {
            data: BTreeMap::new(),
            wal: Wal::with_path(wal_path),
            sequence: 0,
            snapshot_path: snapshot_path.to_string(),
        };

        if Path::new(snapshot_path).exists() {
            let snapshot = store.load_from_file()?;
            store.data = snapshot.data;
        }

        let entries = store.wal.read_all()?;
        for entry in &entries {
            match entry.operation() {
                Operation::Put => {
                    if let Some(value_msg) = &entry.value {
                        let value = Store::value_from_proto(value_msg)?;
                        store.data.insert(entry.key.clone(), value);
                    }
                }
                Operation::Delete => {
                    store.data.remove(&entry.key);
                }
            }
            store.sequence = store.sequence.max(entry.sequence);
        }

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
        self.data.insert(new_key.clone(), new_value);
        Ok(format!("Inserted value with key {}", new_key))
    }

    pub fn get_value(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    pub fn delete_value(&mut self, key: &str) -> Result<String, AppError> {
        if !self.data.contains_key(key) {
            return Err(AppError::KeyNotFound(format!(
                "Could not delete value with key {}",
                key
            )));
        }
        self.sequence += 1;
        let entry = Wal::create_delete_entry(key.to_string(), self.sequence);
        self.wal.append(&entry)?;
        self.data.remove(key);
        Ok(format!("Deleted value with key {}", key))
    }

    pub fn list_values(&self) -> Result<String, AppError> {
        if self.data.is_empty() {
            return Err(AppError::InternalError("Store is empty".to_string()));
        }

        let mut result = "Here's the complete list of items in the store:\n".to_string();
        for (key, value) in &self.data {
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

    fn value_from_proto(msg: &ValueMessage) -> Result<Value, AppError> {
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
