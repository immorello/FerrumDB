pub mod errors;
pub mod sstable;
pub mod store;
pub mod wal;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/ferrumdb.rs"));
}
