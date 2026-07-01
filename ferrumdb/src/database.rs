use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ferrumdb_core::store::Store;

use crate::error::Error;
use crate::table::Table;

/// An embedded FerrumDB database rooted at a directory.
///
/// A database manages named tables, each an independent key-value store in its own
/// subdirectory. A table is opened once and its handle cached, because each table
/// holds an exclusive lock for its lifetime.
pub struct Database {
    root: PathBuf,
    open: HashMap<String, Store>,
}

impl Database {
    /// Opens a database rooted at `dir`, creating the directory if needed.
    pub fn open(dir: impl AsRef<Path>) -> Result<Database, Error> {
        let root = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&root).map_err(|e| Error::Io(e.to_string()))?;
        Ok(Database { root, open: HashMap::new() })
    }

    /// Creates a new table. Errors with [`Error::TableExists`] if one already exists.
    pub fn create_table(&mut self, name: &str) -> Result<(), Error> {
        validate_name(name)?;
        if self.root.join(name).exists() {
            return Err(Error::TableExists(name.to_string()));
        }
        self.open_store(name)?;
        Ok(())
    }

    /// Returns a handle to a table, creating it if it does not already exist.
    pub fn table(&mut self, name: &str) -> Result<Table<'_>, Error> {
        validate_name(name)?;
        if !self.open.contains_key(name) {
            self.open_store(name)?;
        }
        let store = self.open.get_mut(name).expect("just inserted");
        Ok(Table { store })
    }

    /// Deletes a table and all of its data. Errors with [`Error::TableNotFound`]
    /// if the table does not exist.
    pub fn delete_table(&mut self, name: &str) -> Result<(), Error> {
        validate_name(name)?;
        let dir = self.root.join(name);
        if !dir.exists() {
            return Err(Error::TableNotFound(name.to_string()));
        }
        self.open.remove(name); // drop the Store, releasing its lock, before removing files
        std::fs::remove_dir_all(&dir).map_err(|e| Error::Io(e.to_string()))?;
        Ok(())
    }

    /// Lists the names of all tables in the database, sorted.
    pub fn list_tables(&self) -> Result<Vec<String>, Error> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&self.root).map_err(|e| Error::Io(e.to_string()))? {
            let entry = entry.map_err(|e| Error::Io(e.to_string()))?;
            if entry.path().is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                names.push(name.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    fn open_store(&mut self, name: &str) -> Result<(), Error> {
        let dir = self.root.join(name);
        let store = Store::open_with_dir(&dir.to_string_lossy())?;
        self.open.insert(name.to_string(), store);
        Ok(())
    }
}

/// Table names must be non-empty and safe to use as a directory name — no path
/// separators or `.`/`..`, so a table can never escape the database root.
fn validate_name(name: &str) -> Result<(), Error> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name == "."
        || name == ".."
    {
        return Err(Error::InvalidTableName(name.to_string()));
    }
    Ok(())
}
