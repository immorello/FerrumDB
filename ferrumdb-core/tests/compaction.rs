use ferrumdb_core::store::{Store, Value};
use std::fs;

fn setup(name: &str) -> String {
    let dir = format!("./data/compact_{}", name);
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

// Compaction merges many SSTables into one, preserving all live data.
#[test]
fn test_compact_merges_into_one() {
    let dir = setup("merges_into_one");
    let mut store = Store::open_with_dir(&dir).unwrap();

    for i in 0..4 {
        store.set_value(format!("key_{}", i).into_bytes(), Value::Integer(i)).unwrap();
        store.flush().unwrap();
    }
    assert_eq!(count_sstables(&dir), 4);

    store.compact().unwrap();

    assert_eq!(count_sstables(&dir), 1, "compaction should leave a single SSTable");
    for i in 0..4 {
        assert_eq!(store.get_value(format!("key_{}", i).as_bytes()).unwrap(), Some(Value::Integer(i)));
    }

    teardown(&dir);
}

// When a key appears in several SSTables, compaction keeps the newest value.
#[test]
fn test_compact_keeps_newest_value() {
    let dir = setup("keeps_newest");
    let mut store = Store::open_with_dir(&dir).unwrap();

    store.set_value(b"k".to_vec(), Value::Integer(1)).unwrap();
    store.flush().unwrap();
    store.set_value(b"k".to_vec(), Value::Integer(2)).unwrap();
    store.flush().unwrap();
    store.set_value(b"k".to_vec(), Value::Integer(3)).unwrap();
    store.flush().unwrap();

    store.compact().unwrap();

    assert_eq!(count_sstables(&dir), 1);
    assert_eq!(store.get_value(b"k").unwrap(), Some(Value::Integer(3)), "newest value must win");

    teardown(&dir);
}

// A key deleted before compaction is dropped entirely (tombstone reclaimed).
#[test]
fn test_compact_drops_deleted_keys() {
    let dir = setup("drops_deleted");
    let mut store = Store::open_with_dir(&dir).unwrap();

    store.set_value(b"a".to_vec(), Value::Integer(1)).unwrap();
    store.set_value(b"b".to_vec(), Value::Integer(2)).unwrap();
    store.flush().unwrap();
    store.delete_value(b"a").unwrap();
    store.flush().unwrap();

    store.compact().unwrap();

    assert_eq!(count_sstables(&dir), 1);
    assert_eq!(store.get_value(b"a").unwrap(), None, "deleted key must not survive compaction");
    assert_eq!(store.get_value(b"b").unwrap(), Some(Value::Integer(2)));

    teardown(&dir);
}

// Compacting a table whose keys are all deleted leaves no SSTable at all.
#[test]
fn test_compact_all_deleted_leaves_nothing() {
    let dir = setup("all_deleted");
    let mut store = Store::open_with_dir(&dir).unwrap();

    store.set_value(b"x".to_vec(), Value::Integer(1)).unwrap();
    store.flush().unwrap();
    store.delete_value(b"x").unwrap();
    store.flush().unwrap();

    store.compact().unwrap();

    assert_eq!(count_sstables(&dir), 0, "a fully-deleted table compacts to nothing");
    assert_eq!(store.get_value(b"x").unwrap(), None);

    teardown(&dir);
}

// Compacted state is correctly reloaded after reopening the table.
#[test]
fn test_compaction_survives_recovery() {
    let dir = setup("survives_recovery");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        for i in 0..5 {
            store.set_value(format!("key_{}", i).into_bytes(), Value::Integer(i)).unwrap();
            store.flush().unwrap();
        }
        store.compact().unwrap();
        assert_eq!(count_sstables(&dir), 1);
    }

    let store = Store::open_with_dir(&dir).unwrap();
    for i in 0..5 {
        assert_eq!(store.get_value(format!("key_{}", i).as_bytes()).unwrap(), Some(Value::Integer(i)));
    }

    teardown(&dir);
}

// Automatic compaction keeps the number of SSTables bounded under many flushes.
#[test]
fn test_auto_compaction_bounds_sstable_count() {
    let dir = setup("auto_bounds");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        // 20 explicit flushes would be 20 SSTables without compaction.
        for i in 0..20 {
            store.set_value(format!("key_{:02}", i).into_bytes(), Value::Integer(i)).unwrap();
            store.flush().unwrap();
        }
        // MAX_SSTABLES is 8; auto-compaction must keep us at or below it.
        assert!(
            count_sstables(&dir) <= 8,
            "auto-compaction should bound the SSTable count, found {}",
            count_sstables(&dir)
        );
    }

    // All 20 keys remain readable across the compacted/merged tables.
    let store = Store::open_with_dir(&dir).unwrap();
    for i in 0..20 {
        assert_eq!(
            store.get_value(format!("key_{:02}", i).as_bytes()).unwrap(),
            Some(Value::Integer(i)),
            "key_{:02} must survive auto-compaction",
            i
        );
    }

    teardown(&dir);
}
