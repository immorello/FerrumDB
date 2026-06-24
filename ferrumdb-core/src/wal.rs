//! Write-Ahead Log (WAL) module for FerrumDB
//!
//! Provides durable, append-only logging of database operations for crash recovery.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use crate::errors::AppError;
use crate::proto::value_message::Kind;
use crate::proto::{Operation, ValueMessage, WalEntry};
use prost::Message;

/// Default path for the WAL log file
pub const WAL_PATH: &str = "./data/wal.log";

/// Write-Ahead Log structure
#[derive(Debug, Default)]
pub struct Wal {
    file_path: String,
}

impl Wal {
    /// Creates a new WAL instance with the default path
    pub fn new() -> Self {
        Self::with_path(WAL_PATH)
    }

    /// Creates a new WAL instance with a custom path
    pub fn with_path(path: impl Into<String>) -> Self {
        let file_path = path.into();
        if let Some(parent) = Path::new(&file_path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        Wal { file_path }
    }

    /// Appends a WAL entry to the log file without fsyncing.
    ///
    /// Not durable on its own — always follow a batch of appends with
    /// `write_commit()` to make them durable and atomically visible on recovery.
    pub fn append(&self, entry: &WalEntry) -> Result<(), AppError> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)
            .map_err(|e| AppError::IoError(e.to_string()))?;

        let bytes = entry.encode_to_vec();
        let len = bytes.len() as u64;

        file.write_all(&len.to_be_bytes())
            .map_err(|e| AppError::IoError(e.to_string()))?;
        file.write_all(&bytes)
            .map_err(|e| AppError::IoError(e.to_string()))?;
        file.flush()
            .map_err(|e| AppError::IoError(e.to_string()))?;

        Ok(())
    }

    /// Writes a COMMIT marker and fsyncs — makes all preceding entries durable.
    ///
    /// On recovery, only entries that precede a COMMIT are replayed.
    /// Any entries after the last COMMIT are discarded (they were uncommitted at crash time).
    pub fn write_commit(&self, sequence: u64) -> Result<(), AppError> {
        let commit_entry = WalEntry {
            operation: Operation::Commit.into(),
            key: String::new(),
            value: None,
            sequence,
            timestamp: 0,
        };

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)
            .map_err(|e| AppError::IoError(e.to_string()))?;

        let bytes = commit_entry.encode_to_vec();
        let len = bytes.len() as u64;

        file.write_all(&len.to_be_bytes())
            .map_err(|e| AppError::IoError(e.to_string()))?;
        file.write_all(&bytes)
            .map_err(|e| AppError::IoError(e.to_string()))?;
        file.flush()
            .map_err(|e| AppError::IoError(e.to_string()))?;
        file.sync_all()
            .map_err(|e| AppError::IoError(e.to_string()))?;

        Ok(())
    }

    /// Reads all entries from the WAL file
    /// 
    /// Returns a vector of WalEntry in the order they were written.
    /// Skips any corrupted entries (returns what it can read).
    pub fn read_all(&self) -> Result<Vec<WalEntry>, AppError> {
        if !Path::new(&self.file_path).exists() {
            return Ok(Vec::new());
        }

        let mut file = File::open(&self.file_path)
            .map_err(|e| AppError::IoError(e.to_string()))?;

        let mut entries = Vec::new();
        let mut buffer = Vec::new();

        // Read the entire file
        file
            .read_to_end(&mut buffer)
            .map_err(|e| AppError::IoError(e.to_string()))?;

        // Parse entries from the buffer
        let mut offset = 0;
        while offset < buffer.len() {
            // Check if we have enough bytes for the length prefix
            if offset + 8 > buffer.len() {
                // Incomplete length prefix - corrupted tail
                break;
            }

            // Read the length prefix (8 bytes, big-endian)
            let len = u64::from_be_bytes([
                buffer[offset],
                buffer[offset + 1],
                buffer[offset + 2],
                buffer[offset + 3],
                buffer[offset + 4],
                buffer[offset + 5],
                buffer[offset + 6],
                buffer[offset + 7],
            ]) as usize;

            offset += 8;

            // Check if we have enough bytes for the entry
            if offset + len > buffer.len() {
                // Incomplete entry - corrupted tail
                break;
            }

            // Decode the entry
            match WalEntry::decode(&buffer[offset..offset + len]) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    // Skip corrupted entry and continue
                    eprintln!("WAL: Failed to decode entry at offset {}: {}", offset, e);
                }
            }

            offset += len;
        }

        Ok(entries)
    }

    /// Clears the WAL file (called after a checkpoint).
    /// Truncates the file and fsyncs so the empty state is durable before writes resume.
    pub fn clear(&self) -> Result<(), AppError> {
        let file = File::create(&self.file_path)
            .map_err(|e| AppError::IoError(e.to_string()))?;
        file.sync_all()
            .map_err(|e| AppError::IoError(e.to_string()))?;
        Ok(())
    }

    /// Returns the path of the WAL file
    pub fn path(&self) -> &str {
        &self.file_path
    }
}

/// Helper functions for creating WAL entries from store operations
impl Wal {
    /// Creates a PUT entry for storing a value
    pub fn create_put_entry(key: String, value: &crate::store::Value, sequence: u64) -> WalEntry {
        let value_msg = match value {
            crate::store::Value::Integer(n) => ValueMessage {
                kind: Some(Kind::Integer(*n)),
            },
            crate::store::Value::Float(n) => ValueMessage {
                kind: Some(Kind::Float(*n)),
            },
            crate::store::Value::Text(s) => ValueMessage {
                kind: Some(Kind::Text(s.clone())),
            },
            crate::store::Value::Boolean(b) => ValueMessage {
                kind: Some(Kind::Boolean(*b)),
            },
        };

        WalEntry {
            operation: Operation::Put.into(),
            key,
            value: Some(value_msg),
            sequence,
            timestamp: 0, // Can be set by caller if needed
        }
    }

    /// Creates a DELETE entry for removing a value
    pub fn create_delete_entry(key: String, sequence: u64) -> WalEntry {
        WalEntry {
            operation: Operation::Delete.into(),
            key,
            value: None,
            sequence,
            timestamp: 0,
        }
    }
}
