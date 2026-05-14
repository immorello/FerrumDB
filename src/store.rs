use std::collections::HashMap;

use crate::errors::AppError;

pub const STORAGE_PATH: &str = "./data/storage.pb";

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Integer(i32),
    Float(f64),
    Text(String),
    Boolean(bool),
}

#[derive(Debug, Default)]
pub struct Store {
    data: HashMap<String, Value>,
}

impl Store {
    pub fn new() -> Store {
        Store {
            data: HashMap::new(),
        }
    }

    pub fn get_data(&self) -> &HashMap<String, Value> {
        &self.data
    }

    pub fn from_data(data: HashMap<String, Value>) -> Store {
        Store { data }
    }

    pub fn set_value(&mut self, new_key: String, new_value: Value) -> Result<String, AppError> {
        self.data.insert(new_key.clone(), new_value);
        self.save_to_file()
            .map_err(AppError::IoError)
            .map(|_| format!("Inserted value with key {}", new_key))
    }

    pub fn get_value(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    pub fn delete_value(&mut self, key: &str) -> Result<String, AppError> {
        match self.data.remove(key) {
            Some(_) => self
                .save_to_file()
                .map_err(AppError::InternalError)
                .map(|_| format!("Deleted value with key {}", key)),
            None => Err(AppError::KeyNotFound(format!(
                "Could not delete value with key {}",
                key
            ))),
        }
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
}
