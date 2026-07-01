//! FerrumDB — an embedded key-value store.
//!
//! FerrumDB stores byte values under UTF-8 keys, organized into named tables, with
//! durable writes and crash recovery — no query language, no server. It is meant to
//! be linked directly into an application, the way SQLite is.
//!
//! # Quick start
//!
//! ```no_run
//! use ferrumdb::Database;
//!
//! let mut db = Database::open("./data/app")?;
//! db.create_table("users")?;
//!
//! let mut users = db.table("users")?;
//! users.put(b"user:42", b"alice")?;
//! assert_eq!(users.get(b"user:42")?, Some(b"alice".to_vec()));
//!
//! users.put_batch(&[(b"user:1", b"bob"), (b"user:2", b"carol")])?;
//! users.delete(b"user:42")?;
//! # Ok::<(), ferrumdb::Error>(())
//! ```
//!
//! Keys are UTF-8 byte slices; values are arbitrary bytes. Batched writes are
//! atomic and durable with a single fsync.

mod database;
mod error;
mod table;

pub use database::Database;
pub use error::Error;
pub use table::Table;
