use ferrumdb_core::errors::AppError;
use std::fmt;

/// Errors returned by the FerrumDB public API.
#[derive(Debug)]
pub enum Error {
    /// An I/O error from the underlying storage.
    Io(String),
    /// A table name was empty or contained a path separator.
    InvalidTableName(String),
    /// `create_table` was called for a table that already exists.
    TableExists(String),
    /// The requested table does not exist.
    TableNotFound(String),
    /// A stored value was not written through the bytes API (should not happen
    /// for databases only ever accessed through this API).
    UnexpectedValue,
    /// Any other internal error.
    Internal(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(m) => write!(f, "io error: {m}"),
            Error::InvalidTableName(n) => write!(f, "invalid table name: {n}"),
            Error::TableExists(n) => write!(f, "table already exists: {n}"),
            Error::TableNotFound(n) => write!(f, "table not found: {n}"),
            Error::UnexpectedValue => write!(f, "stored value was not written as bytes"),
            Error::Internal(m) => write!(f, "internal error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<AppError> for Error {
    fn from(e: AppError) -> Self {
        match e {
            AppError::IoError(m) => Error::Io(m),
            AppError::DecodeError(m) | AppError::InternalError(m) | AppError::KeyNotFound(m) => {
                Error::Internal(m)
            }
        }
    }
}
