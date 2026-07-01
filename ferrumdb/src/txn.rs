use ferrumdb_core::store::{Transaction, Value};

use crate::error::Error;

/// An interactive transaction over a [`crate::Table`].
///
/// Writes are buffered until [`commit`](Txn::commit); reads see the transaction's
/// own uncommitted writes first (read-your-writes), then the committed table.
/// Dropping the transaction, or calling [`rollback`](Txn::rollback), discards
/// everything. A committed transaction is atomic and durable with a single fsync.
pub struct Txn<'a> {
    pub(crate) inner: Transaction<'a>,
}

impl Txn<'_> {
    /// Buffers a put.
    pub fn put(&mut self, key: &[u8], value: &[u8]) {
        self.inner.set_value(key.to_vec(), Value::Bytes(value.to_vec()));
    }

    /// Buffers a delete.
    pub fn delete(&mut self, key: &[u8]) {
        self.inner.delete_value(key.to_vec());
    }

    /// Reads within the transaction, seeing its own buffered writes first.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        match self.inner.get_value(key)? {
            Some(Value::Bytes(bytes)) => Ok(Some(bytes)),
            Some(_) => Err(Error::UnexpectedValue),
            None => Ok(None),
        }
    }

    /// Applies all buffered writes atomically and durably (one fsync).
    pub fn commit(self) -> Result<(), Error> {
        self.inner.commit()?;
        Ok(())
    }

    /// Discards all buffered writes.
    pub fn rollback(self) {
        self.inner.rollback();
    }
}
