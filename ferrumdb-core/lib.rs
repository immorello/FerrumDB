pub mod errors;
pub mod persistence;
pub mod store;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/ferrumdb.rs"));
}
