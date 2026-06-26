use crate::errors::AppError;
use crate::proto::StoreSnapshot;
use crate::sstable::Entry;
use crate::store::{Store, Value};
use prost::Message;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

impl Store {
    pub(crate) fn store_to_proto_store(&self) -> StoreSnapshot {
        // Tombstones are dropped from the snapshot: it represents the full state
        // at checkpoint time, so a deleted key simply has nothing to record.
        let data = self
            .get_data()
            .iter()
            .filter_map(|(key, entry)| match entry {
                Entry::Value(value) => Some((key.clone(), value.to_proto())),
                Entry::Tombstone => None,
            })
            .collect();
        StoreSnapshot { data }
    }

    /// Writes the current state as a protobuf snapshot, fsynced to disk.
    pub fn save_to_file(&self) -> Result<(), AppError> {
        let bytes = self.store_to_proto_store().encode_to_vec();
        if let Some(parent) = Path::new(&self.snapshot_path).parent() {
            fs::create_dir_all(parent).map_err(|e| AppError::IoError(e.to_string()))?;
        }
        let mut file = fs::File::create(&self.snapshot_path)
            .map_err(|e| AppError::IoError(e.to_string()))?;
        file.write_all(&bytes).map_err(|e| AppError::IoError(e.to_string()))?;
        file.sync_all().map_err(|e| AppError::IoError(e.to_string()))?;
        Ok(())
    }

    /// Loads a snapshot from disk into a memtable. Snapshots only ever hold
    /// values, so every entry is loaded as `Entry::Value`.
    pub fn load_from_file(&self) -> Result<BTreeMap<String, Entry>, AppError> {
        let bytes = fs::read(&self.snapshot_path)
            .map_err(|e| AppError::IoError(e.to_string()))?;
        let proto_store = StoreSnapshot::decode(bytes.as_slice())
            .map_err(|e| AppError::DecodeError(e.to_string()))?;

        let mut data = BTreeMap::new();
        for (key, msg) in proto_store.data {
            data.insert(key, Entry::Value(Value::from_proto(&msg)?));
        }
        Ok(data)
    }
}
