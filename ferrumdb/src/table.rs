use std::ops::Bound;

use ferrumdb_core::store::{Store, Value};

use crate::error::Error;
use crate::txn::Txn;

/// A list of key/value pairs, as returned by scans.
pub type Pairs = Vec<(Vec<u8>, Vec<u8>)>;

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
        self.store.set_value(key.to_vec(), Value::Bytes(value.to_vec()))?;
        Ok(())
    }

    /// Returns the value for a key, or `None` if it is absent.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        match self.store.get_value(key)? {
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
        self.store.delete_value(key)?;
        Ok(())
    }

    /// Atomically writes many key/value pairs: either all of them land, or none do
    /// (one fsync for the whole batch).
    pub fn put_batch(&mut self, entries: &[(&[u8], &[u8])]) -> Result<(), Error> {
        let mut tx = self.store.begin_transaction();
        for &(key, value) in entries {
            tx.set_value(key.to_vec(), Value::Bytes(value.to_vec()));
        }
        tx.commit()?;
        Ok(())
    }

    /// Looks up many keys, returning a value (or `None`) for each, in the same order.
    pub fn get_batch(&self, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, Error> {
        keys.iter().map(|&key| self.get(key)).collect()
    }

    /// Begins an interactive transaction: buffer writes, read your own writes, then
    /// `commit` (atomic, one fsync) or `rollback`.
    pub fn transaction(&mut self) -> Txn<'_> {
        Txn { inner: self.store.begin_transaction() }
    }

    /// Returns every key/value pair in the table, in ascending key order.
    pub fn scan(&self) -> Result<Pairs, Error> {
        self.range_bounds(Bound::Unbounded, Bound::Unbounded)
    }

    /// Returns key/value pairs in the half-open key range `[start, end)`, sorted.
    pub fn range(&self, start: &[u8], end: &[u8]) -> Result<Pairs, Error> {
        self.range_bounds(Bound::Included(start), Bound::Excluded(end))
    }

    /// Returns every key/value pair whose key begins with `prefix`, sorted.
    pub fn scan_prefix(&self, prefix: &[u8]) -> Result<Pairs, Error> {
        let upper = prefix_upper_bound(prefix);
        let hi = match &upper {
            Some(u) => Bound::Excluded(u.as_slice()),
            None => Bound::Unbounded, // empty or all-0xFF prefix has no upper bound
        };
        self.range_bounds(Bound::Included(prefix), hi)
    }

    fn range_bounds(&self, lo: Bound<&[u8]>, hi: Bound<&[u8]>) -> Result<Pairs, Error> {
        let mut out = Vec::new();
        for (key, value) in self.store.scan_range(lo, hi)? {
            match value {
                Value::Bytes(bytes) => out.push((key, bytes)),
                _ => return Err(Error::UnexpectedValue),
            }
        }
        Ok(out)
    }
}

/// The smallest key that is greater than every key beginning with `prefix`, or
/// `None` if there is none (an empty prefix, or one that is entirely `0xFF`).
fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.last_mut() {
        if *last < 0xFF {
            *last += 1;
            return Some(end);
        }
        end.pop();
    }
    None
}
