use crate::errors::AppError;
use crate::proto::value_message::Kind;
use crate::proto::{StoreSnapshot, ValueMessage};
use crate::store::{Store, Value};
use prost::Message;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

impl Store {
    fn value_to_proto(value: &Value) -> ValueMessage {
        match value {
            Value::Integer(n) => ValueMessage {
                kind: Some(Kind::Integer(*n)),
            },
            Value::Float(n) => ValueMessage {
                kind: Some(Kind::Float(*n)),
            },
            Value::Text(s) => ValueMessage {
                kind: Some(Kind::Text(s.clone())),
            },
            Value::Boolean(b) => ValueMessage {
                kind: Some(Kind::Boolean(*b)),
            },
        }
    }

    fn proto_to_value(proto: &ValueMessage) -> Result<Value, AppError> {
        match proto.kind.as_ref() {
            Some(Kind::Integer(n)) => Ok(Value::Integer(*n)),
            Some(Kind::Float(n)) => Ok(Value::Float(*n)),
            Some(Kind::Text(s)) => Ok(Value::Text(s.clone())),
            Some(Kind::Boolean(b)) => Ok(Value::Boolean(*b)),
            None => Err(AppError::IoError("Type of value not covered".to_string())),
        }
    }

    pub(crate) fn store_to_proto_store(&self) -> StoreSnapshot {
        let data = self
            .get_data()
            .iter()
            .map(|(key, value)| (key.clone(), Store::value_to_proto(value)))
            .collect();
        StoreSnapshot { data }
    }

    pub(crate) fn proto_to_store(proto_store: StoreSnapshot) -> Result<Store, AppError> {
        let data: Result<BTreeMap<String, Value>, AppError> = proto_store
            .data
            .into_iter()
            .map(|(key, value_message)| {
                Store::proto_to_value(&value_message).map(|value| (key, value))
            })
            .collect();

        Ok(Store::from_data(data?))
    }

    pub fn save_to_file(&self) -> Result<String, String> {
        let proto_store = self.store_to_proto_store();
        let bytes = proto_store.encode_to_vec();
        if let Some(parent) = Path::new(&self.snapshot_path).parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut file = fs::File::create(&self.snapshot_path).map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        Ok("Data persisted to file".to_string())
    }

    pub fn load_from_file(&self) -> Result<Store, AppError> {
        let bytes = fs::read(&self.snapshot_path)
            .map_err(|error| AppError::IoError(error.to_string()))?;
        let proto_store = StoreSnapshot::decode(bytes.as_slice())
            .map_err(|error| AppError::DecodeError(error.to_string()))?;
        Store::proto_to_store(proto_store)
    }
}
