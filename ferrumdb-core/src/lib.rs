pub mod errors;
pub mod persistence;
pub mod store;
pub mod wal;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/ferrumdb.rs"));
}
