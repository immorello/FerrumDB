use ferrumdb_core::store::{Store, Value};

use crate::error::Error;

/// A handle to one table, borrowed from a [`crate::Database`].
///
/// Keys are UTF-8 byte slices; values are arbitrary bytes. The handle borrows the
/// database, so only one table handle is live at a time (the engine is
/// single-writer by design).
pub struct Table<'a> {
    pub(crate) store: &'a mut Store,
}

impl Table<'_> {
    /// Inserts a value, overwriting any existing value for the key.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), Error> {
        let key = key_str(key)?;
        self.store.set_value(key, Value::Bytes(value.to_vec()))?;
        Ok(())
    }

    /// Returns the value for a key, or `None` if it is absent.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        let key = key_str(key)?;
        match self.store.get_value(&key)? {
            Some(Value::Bytes(bytes)) => Ok(Some(bytes)),
            Some(_) => Err(Error::UnexpectedValue),
            None => Ok(None),
        }
    }

    /// Returns whether a key is present.
    pub fn contains(&self, key: &[u8]) -> Result<bool, Error> {
        Ok(self.get(key)?.is_some())
    }

    /// Deletes a key. Idempotent — deleting a key that is not present is not an error.
    pub fn delete(&mut self, key: &[u8]) -> Result<(), Error> {
        let key = key_str(key)?;
        self.store.delete_value(&key)?;
        Ok(())
    }

    /// Atomically writes many key/value pairs: either all of them land, or none do
    /// (one fsync for the whole batch).
    pub fn put_batch(&mut self, entries: &[(&[u8], &[u8])]) -> Result<(), Error> {
        let mut tx = self.store.begin_transaction();
        for &(key, value) in entries {
            // An invalid key returns here, dropping the transaction — a full rollback
            // with nothing written.
            let key = key_str(key)?;
            tx.set_value(key, Value::Bytes(value.to_vec()));
        }
        tx.commit()?;
        Ok(())
    }

    /// Looks up many keys, returning a value (or `None`) for each, in the same order.
    pub fn get_batch(&self, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, Error> {
        keys.iter().map(|&key| self.get(key)).collect()
    }
}

/// Keys must be UTF-8 in this release.
fn key_str(key: &[u8]) -> Result<String, Error> {
    std::str::from_utf8(key).map(str::to_string).map_err(|_| Error::InvalidKey)
}
