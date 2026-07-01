use ferrumdb_core::store::{Store, Value};
use std::fs;
use std::path::Path;

fn setup(name: &str) -> String {
    let dir = format!("./data/flush_{}", name);
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn teardown(dir: &str) {
    let _ = fs::remove_dir_all(dir);
}

fn count_sstables(dir: &str) -> usize {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with("sstable_") && n.ends_with(".sst"))
                .unwrap_or(false)
        })
        .count()
}

// An explicit flush writes one SSTable and empties the memtable.
#[test]
fn test_flush_creates_sstable_and_empties_memtable() {
    let dir = setup("creates_sstable");
    let mut store = Store::open_with_dir(&dir).unwrap();

    store.set_value(b"a".to_vec(), Value::Integer(1)).unwrap();
    store.set_value(b"b".to_vec(), Value::Integer(2)).unwrap();
    store.flush().unwrap();

    assert_eq!(count_sstables(&dir), 1, "flush should write exactly one SSTable");
    assert!(store.get_data().is_empty(), "memtable should be empty after flush");

    // Data is still readable — now served from the SSTable.
    assert_eq!(store.get_value(b"a").unwrap(), Some(Value::Integer(1)));
    assert_eq!(store.get_value(b"b").unwrap(), Some(Value::Integer(2)));

    teardown(&dir);
}

// Flushing an empty memtable is a no-op (no stray SSTable files).
#[test]
fn test_flush_empty_is_noop() {
    let dir = setup("empty_noop");
    let mut store = Store::open_with_dir(&dir).unwrap();

    store.flush().unwrap();
    assert_eq!(count_sstables(&dir), 0, "flushing nothing must not create a file");

    teardown(&dir);
}

// A value in the memtable shadows an older value for the same key in an SSTable.
#[test]
fn test_memtable_shadows_sstable() {
    let dir = setup("memtable_shadows");
    let mut store = Store::open_with_dir(&dir).unwrap();

    store.set_value(b"k".to_vec(), Value::Integer(1)).unwrap();
    store.flush().unwrap(); // SSTable: k = 1
    store.set_value(b"k".to_vec(), Value::Integer(2)).unwrap(); // memtable: k = 2

    assert_eq!(store.get_value(b"k").unwrap(), Some(Value::Integer(2)), "memtable must win over SSTable");

    teardown(&dir);
}

// When a key exists in two SSTables, the newer SSTable wins.
#[test]
fn test_newest_sstable_wins() {
    let dir = setup("newest_wins");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        store.set_value(b"k".to_vec(), Value::Integer(1)).unwrap();
        store.flush().unwrap(); // sstable_1: k = 1
        store.set_value(b"k".to_vec(), Value::Integer(2)).unwrap();
        store.flush().unwrap(); // sstable_2: k = 2
        assert_eq!(count_sstables(&dir), 2);
    }

    // Survives reopen too: ordering is reconstructed from file ids.
    let store = Store::open_with_dir(&dir).unwrap();
    assert_eq!(store.get_value(b"k").unwrap(), Some(Value::Integer(2)), "newest SSTable must win");

    teardown(&dir);
}

// A read walks memtable then SSTables newest→oldest, finding keys spread across layers.
#[test]
fn test_layered_read_across_many_sstables() {
    let dir = setup("layered_read");
    let mut store = Store::open_with_dir(&dir).unwrap();

    store.set_value(b"a".to_vec(), Value::Integer(1)).unwrap();
    store.flush().unwrap(); // sstable_1: a
    store.set_value(b"b".to_vec(), Value::Integer(2)).unwrap();
    store.flush().unwrap(); // sstable_2: b
    store.set_value(b"c".to_vec(), Value::Integer(3)).unwrap(); // memtable: c

    assert_eq!(store.get_value(b"a").unwrap(), Some(Value::Integer(1)));
    assert_eq!(store.get_value(b"b").unwrap(), Some(Value::Integer(2)));
    assert_eq!(store.get_value(b"c").unwrap(), Some(Value::Integer(3)));
    assert_eq!(store.get_value(b"missing").unwrap(), None);

    teardown(&dir);
}

// Exceeding the memtable byte budget triggers automatic flushes, so the memtable
// stays bounded no matter how much is written. A tiny budget is set so the test
// is fast and deterministic.
#[test]
fn test_auto_flush_bounds_memtable() {
    let dir = setup("auto_flush");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        store.set_memtable_budget(64); // bytes — tiny, forces frequent flushes
        for i in 0..50 {
            store.set_value(format!("key_{:03}", i).into_bytes(), Value::Integer(i)).unwrap();
        }

        assert!(count_sstables(&dir) >= 1, "small budget must trigger auto-flush");
        assert!(
            store.get_data().len() < 50,
            "the memtable must hold far fewer than all entries — memory is bounded"
        );
        assert!(Path::new(&dir).join("wal.log").exists());
    }

    // Everything is still readable after reopen, served from the SSTables.
    let store = Store::open_with_dir(&dir).unwrap();
    for i in 0..50 {
        assert_eq!(
            store.get_value(format!("key_{:03}", i).as_bytes()).unwrap(),
            Some(Value::Integer(i)),
            "key_{:03} must survive auto-flush and recovery",
            i
        );
    }

    teardown(&dir);
}
