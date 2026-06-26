use ferrumdb_core::sstable::{Entry, SsTable};
use ferrumdb_core::store::Value;
use std::collections::BTreeMap;
use std::fs;

fn setup(name: &str) -> String {
    let dir = format!("./data/sst_{}", name);
    fs::create_dir_all(&dir).ok();
    let path = format!("{}/test.sst", dir);
    let _ = fs::remove_file(&path);
    path
}

fn teardown(path: &str) {
    let _ = fs::remove_file(path);
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = fs::remove_dir(parent);
    }
}

// Write a handful of typed values, read them back through a fresh open().
#[test]
fn test_flush_and_read_roundtrip() {
    let path = setup("roundtrip");

    let mut mem: BTreeMap<String, Entry> = BTreeMap::new();
    mem.insert("a".to_string(), Entry::Value(Value::Integer(1)));
    mem.insert("b".to_string(), Entry::Value(Value::Text("hello".to_string())));
    mem.insert("c".to_string(), Entry::Value(Value::Float(2.5)));
    mem.insert("d".to_string(), Entry::Value(Value::Boolean(true)));

    SsTable::flush(&path, mem.iter()).unwrap();

    // Reopen from disk — index is loaded from the footer, not from memory.
    let sst = SsTable::open(&path).unwrap();
    assert_eq!(sst.get("a").unwrap(), Some(Entry::Value(Value::Integer(1))));
    assert_eq!(sst.get("b").unwrap(), Some(Entry::Value(Value::Text("hello".to_string()))));
    assert_eq!(sst.get("c").unwrap(), Some(Entry::Value(Value::Float(2.5))));
    assert_eq!(sst.get("d").unwrap(), Some(Entry::Value(Value::Boolean(true))));

    teardown(&path);
}

// A key that was never written returns None.
#[test]
fn test_missing_key_returns_none() {
    let path = setup("missing");

    let mut mem: BTreeMap<String, Entry> = BTreeMap::new();
    mem.insert("apple".to_string(), Entry::Value(Value::Integer(1)));
    mem.insert("cherry".to_string(), Entry::Value(Value::Integer(3)));
    SsTable::flush(&path, mem.iter()).unwrap();

    let sst = SsTable::open(&path).unwrap();
    assert_eq!(sst.get("banana").unwrap(), None); // between existing keys
    assert_eq!(sst.get("aaa").unwrap(), None);    // before the first key
    assert_eq!(sst.get("zzz").unwrap(), None);    // after the last key

    teardown(&path);
}

// Enough records to span multiple blocks; every key must still be findable.
#[test]
fn test_lookup_across_multiple_blocks() {
    let path = setup("multiblock");

    let mut mem: BTreeMap<String, Entry> = BTreeMap::new();
    for i in 0..1000 {
        mem.insert(format!("key_{:06}", i), Entry::Value(Value::Integer(i)));
    }
    SsTable::flush(&path, mem.iter()).unwrap();

    let sst = SsTable::open(&path).unwrap();
    assert!(sst.block_count() > 1, "1000 records should span multiple blocks");

    // Probe the first, last, and several interior keys.
    assert_eq!(sst.get("key_000000").unwrap(), Some(Entry::Value(Value::Integer(0))));
    assert_eq!(sst.get("key_000999").unwrap(), Some(Entry::Value(Value::Integer(999))));
    assert_eq!(sst.get("key_000500").unwrap(), Some(Entry::Value(Value::Integer(500))));
    assert_eq!(sst.get("key_000250").unwrap(), Some(Entry::Value(Value::Integer(250))));
    assert_eq!(sst.get("key_000750").unwrap(), Some(Entry::Value(Value::Integer(750))));

    teardown(&path);
}

// A tombstone is preserved and returned as Entry::Tombstone, not as a value.
#[test]
fn test_tombstone_roundtrip() {
    let path = setup("tombstone");

    let mut mem: BTreeMap<String, Entry> = BTreeMap::new();
    mem.insert("alive".to_string(), Entry::Value(Value::Integer(1)));
    mem.insert("dead".to_string(), Entry::Tombstone);
    SsTable::flush(&path, mem.iter()).unwrap();

    let sst = SsTable::open(&path).unwrap();
    assert_eq!(sst.get("alive").unwrap(), Some(Entry::Value(Value::Integer(1))));
    assert_eq!(sst.get("dead").unwrap(), Some(Entry::Tombstone));

    teardown(&path);
}

// A flipped byte in the data region is caught by the block CRC on read.
#[test]
fn test_crc_detects_corruption() {
    let path = setup("crc");

    let mut mem: BTreeMap<String, Entry> = BTreeMap::new();
    mem.insert("key".to_string(), Entry::Value(Value::Text("important".to_string())));
    SsTable::flush(&path, mem.iter()).unwrap();

    // Corrupt the very first byte of the file (inside the first data block).
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 0xFF;
    fs::write(&path, &bytes).unwrap();

    let sst = SsTable::open(&path).unwrap();
    let result = sst.get("key");
    assert!(result.is_err(), "corrupted block must be rejected by CRC, got {:?}", result);

    teardown(&path);
}

// An empty memtable still produces a valid, openable file with no blocks.
#[test]
fn test_empty_sstable() {
    let path = setup("empty");

    let mem: BTreeMap<String, Entry> = BTreeMap::new();
    SsTable::flush(&path, mem.iter()).unwrap();

    let sst = SsTable::open(&path).unwrap();
    assert_eq!(sst.block_count(), 0);
    assert_eq!(sst.get("anything").unwrap(), None);

    teardown(&path);
}
