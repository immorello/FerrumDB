use ferrumdb_core::store::{Store, Value};
use std::fs;
use std::ops::Bound;

fn setup(name: &str) -> String {
    let dir = format!("./data/scan_{}", name);
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn teardown(dir: &str) {
    let _ = fs::remove_dir_all(dir);
}

fn all(store: &Store) -> Vec<(Vec<u8>, Value)> {
    store.scan_range(Bound::Unbounded, Bound::Unbounded).unwrap()
}

// A full scan returns every key in ascending order.
#[test]
fn test_scan_all_sorted() {
    let dir = setup("all_sorted");
    let mut store = Store::open_with_dir(&dir).unwrap();
    store.set_value(b"c".to_vec(), Value::Integer(3)).unwrap();
    store.set_value(b"a".to_vec(), Value::Integer(1)).unwrap();
    store.set_value(b"b".to_vec(), Value::Integer(2)).unwrap();

    let keys: Vec<Vec<u8>> = all(&store).into_iter().map(|(k, _)| k).collect();
    assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);

    teardown(&dir);
}

// A scan merges keys that live in SSTables with keys still in the memtable.
#[test]
fn test_scan_merges_memtable_and_sstables() {
    let dir = setup("merge");
    let mut store = Store::open_with_dir(&dir).unwrap();

    store.set_value(b"a".to_vec(), Value::Integer(1)).unwrap();
    store.set_value(b"c".to_vec(), Value::Integer(3)).unwrap();
    store.flush().unwrap(); // a, c -> SSTable
    store.set_value(b"b".to_vec(), Value::Integer(2)).unwrap(); // memtable

    let got = all(&store);
    assert_eq!(
        got,
        vec![
            (b"a".to_vec(), Value::Integer(1)),
            (b"b".to_vec(), Value::Integer(2)),
            (b"c".to_vec(), Value::Integer(3)),
        ]
    );

    teardown(&dir);
}

// A bounded scan is half-open: [start, end).
#[test]
fn test_range_is_half_open() {
    let dir = setup("half_open");
    let mut store = Store::open_with_dir(&dir).unwrap();
    for k in [b"a", b"b", b"c", b"d"] {
        store.set_value(k.to_vec(), Value::Integer(0)).unwrap();
    }

    let keys: Vec<Vec<u8>> = store
        .scan_range(Bound::Included(b"b"), Bound::Excluded(b"d"))
        .unwrap()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(keys, vec![b"b".to_vec(), b"c".to_vec()]);

    teardown(&dir);
}

// When a key exists in an SSTable and the memtable, the scan sees the newer value.
#[test]
fn test_scan_newest_value_wins() {
    let dir = setup("newest_wins");
    let mut store = Store::open_with_dir(&dir).unwrap();

    store.set_value(b"k".to_vec(), Value::Integer(1)).unwrap();
    store.flush().unwrap(); // SSTable: k = 1
    store.set_value(b"k".to_vec(), Value::Integer(2)).unwrap(); // memtable: k = 2

    assert_eq!(all(&store), vec![(b"k".to_vec(), Value::Integer(2))]);

    teardown(&dir);
}

// Deleted keys are excluded from scans, including across SSTables.
#[test]
fn test_scan_excludes_deleted_keys() {
    let dir = setup("excludes_deleted");
    let mut store = Store::open_with_dir(&dir).unwrap();

    store.set_value(b"a".to_vec(), Value::Integer(1)).unwrap();
    store.set_value(b"b".to_vec(), Value::Integer(2)).unwrap();
    store.flush().unwrap(); // a, b -> SSTable
    store.delete_value(b"a").unwrap(); // tombstone in memtable

    let keys: Vec<Vec<u8>> = all(&store).into_iter().map(|(k, _)| k).collect();
    assert_eq!(keys, vec![b"b".to_vec()], "deleted key must not appear in a scan");

    teardown(&dir);
}
