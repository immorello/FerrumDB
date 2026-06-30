//! Write-Ahead Log (WAL) module for FerrumDB
//!
//! Provides durable, append-only logging of database operations for crash recovery.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use crate::errors::AppError;
use crate::proto::{Operation, WalEntry};
use prost::Message;

/// Default path for the WAL log file
pub const WAL_PATH: &str = "./data/wal.log";

/// Write-Ahead Log structure.
///
/// Holds the path and a cached append handle. The handle is opened lazily on the
/// first write and reused for the WAL's lifetime, so appends do not pay a file
/// open/close each — the dominant cost of batched writes before this.
#[derive(Debug, Default)]
pub struct Wal {
    file_path: String,
    file: Option<File>,
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
        Wal { file_path, file: None }
    }

    /// Returns the cached append handle, opening it on first use.
    fn writer(&mut self) -> Result<&mut File, AppError> {
        if self.file.is_none() {
            let f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.file_path)
                .map_err(|e| AppError::IoError(e.to_string()))?;
            self.file = Some(f);
        }
        Ok(self.file.as_mut().unwrap())
    }

    /// Appends a WAL entry to the log file without fsyncing.
    ///
    /// Not durable on its own — always follow a batch of appends with
    /// `write_commit()` to make them durable and atomically visible on recovery.
    pub fn append(&mut self, entry: &WalEntry) -> Result<(), AppError> {
        let framed = frame(entry);
        self.writer()?
            .write_all(&framed)
            .map_err(|e| AppError::IoError(e.to_string()))
    }

    /// Writes a COMMIT marker and fsyncs — makes all preceding entries durable.
    ///
    /// On recovery, only entries that precede a COMMIT are replayed; any entries
    /// after the last COMMIT are truncated away (they were uncommitted at crash time).
    pub fn write_commit(&mut self, sequence: u64) -> Result<(), AppError> {
        let commit_entry = WalEntry {
            operation: Operation::Commit.into(),
            key: String::new(),
            value: None,
            sequence,
            timestamp: 0,
        };
        let framed = frame(&commit_entry);

        let file = self.writer()?;
        file.write_all(&framed).map_err(|e| AppError::IoError(e.to_string()))?;
        file.sync_all().map_err(|e| AppError::IoError(e.to_string()))?;
        Ok(())
    }

    /// Reads all entries from the WAL file, in write order. Stops at a truncated
    /// tail. Used by tests to inspect the raw log; recovery uses
    /// [`read_for_recovery`](Self::read_for_recovery) instead.
    pub fn read_all(&self) -> Result<Vec<WalEntry>, AppError> {
        let buffer = match self.read_file()? {
            Some(b) => b,
            None => return Ok(Vec::new()),
        };
        Ok(parse_buffer(&buffer).0)
    }

    /// Reads the committed entries for recovery and removes any uncommitted tail
    /// (bytes after the last COMMIT) from the file. Truncating the tail prevents a
    /// later COMMIT from silently adopting writes from a crashed session.
    pub fn read_for_recovery(&mut self) -> Result<Vec<WalEntry>, AppError> {
        let buffer = match self.read_file()? {
            Some(b) => b,
            None => return Ok(Vec::new()),
        };

        let (mut entries, committed_count, committed_end) = parse_buffer(&buffer);

        if committed_end < buffer.len() {
            // Drop the uncommitted tail from the file, then forget any cached
            // handle so the next append reopens at the truncated end.
            let f = OpenOptions::new()
                .write(true)
                .open(&self.file_path)
                .map_err(|e| AppError::IoError(e.to_string()))?;
            f.set_len(committed_end as u64).map_err(|e| AppError::IoError(e.to_string()))?;
            f.sync_all().map_err(|e| AppError::IoError(e.to_string()))?;
            self.file = None;
        }

        entries.truncate(committed_count);
        Ok(entries)
    }

    /// Reads the whole WAL file into memory, or `None` if it does not exist.
    fn read_file(&self) -> Result<Option<Vec<u8>>, AppError> {
        if !Path::new(&self.file_path).exists() {
            return Ok(None);
        }
        let mut file = File::open(&self.file_path).map_err(|e| AppError::IoError(e.to_string()))?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer).map_err(|e| AppError::IoError(e.to_string()))?;
        Ok(Some(buffer))
    }

    /// Clears the WAL file (called after a flush). Truncates the file and fsyncs
    /// so the empty state is durable before writes resume.
    pub fn clear(&mut self) -> Result<(), AppError> {
        self.file = None; // drop the cached append handle before truncating
        let file = File::create(&self.file_path).map_err(|e| AppError::IoError(e.to_string()))?;
        file.sync_all().map_err(|e| AppError::IoError(e.to_string()))?;
        Ok(())
    }

    /// Returns the path of the WAL file
    pub fn path(&self) -> &str {
        &self.file_path
    }
}

/// Frames one entry as an 8-byte big-endian length prefix followed by its
/// protobuf bytes, in a single buffer so it can be written with one `write_all`.
fn frame(entry: &WalEntry) -> Vec<u8> {
    let bytes = entry.encode_to_vec();
    let mut framed = Vec::with_capacity(8 + bytes.len());
    framed.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    framed.extend_from_slice(&bytes);
    framed
}

/// Parses length-prefixed records from `buffer`. Returns all decoded entries, the
/// number of entries up to and including the last COMMIT, and the byte offset just
/// past that last COMMIT (0 if there is none). Stops at a truncated tail.
fn parse_buffer(buffer: &[u8]) -> (Vec<WalEntry>, usize, usize) {
    let mut entries = Vec::new();
    let mut committed_count = 0;
    let mut committed_end = 0;

    let mut offset = 0;
    while offset < buffer.len() {
        if offset + 8 > buffer.len() {
            break; // incomplete length prefix — truncated tail
        }
        let len = u64::from_be_bytes(buffer[offset..offset + 8].try_into().unwrap()) as usize;
        offset += 8;

        if offset + len > buffer.len() {
            break; // incomplete entry — truncated tail
        }

        match WalEntry::decode(&buffer[offset..offset + len]) {
            Ok(entry) => {
                let is_commit = entry.operation() == Operation::Commit;
                entries.push(entry);
                offset += len;
                if is_commit {
                    committed_count = entries.len();
                    committed_end = offset;
                }
            }
            Err(e) => {
                eprintln!("WAL: Failed to decode entry at offset {}: {}", offset, e);
                offset += len;
            }
        }
    }

    (entries, committed_count, committed_end)
}

/// Helper functions for creating WAL entries from store operations
impl Wal {
    /// Creates a PUT entry for storing a value
    pub fn create_put_entry(key: String, value: &crate::store::Value, sequence: u64) -> WalEntry {
        WalEntry {
            operation: Operation::Put.into(),
            key,
            value: Some(value.to_proto()),
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
