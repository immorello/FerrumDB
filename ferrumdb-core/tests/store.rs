use ferrumdb_core::errors::AppError;
use ferrumdb_core::store::{Store, Value};
use std::fs;

fn setup(name: &str) -> String {
    let dir = format!("./data/test_{}", name);
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn teardown(dir: &str) {
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn test_keys_iterated_in_sorted_order() {
    let dir = setup("sorted_iteration");

    let mut store = Store::open_with_dir(&dir).unwrap();
    store.set_value("zebra".to_string(), Value::Integer(3)).unwrap();
    store.set_value("apple".to_string(), Value::Integer(1)).unwrap();
    store.set_value("mango".to_string(), Value::Integer(2)).unwrap();

    let keys: Vec<&String> = store.get_data().keys().collect();
    assert_eq!(keys, vec!["apple", "mango", "zebra"]);

    teardown(&dir);
}

#[test]
fn test_sorted_order_survives_recovery() {
    let dir = setup("sorted_recovery");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        store.set_value("c".to_string(), Value::Integer(3)).unwrap();
        store.set_value("a".to_string(), Value::Integer(1)).unwrap();
        store.set_value("b".to_string(), Value::Integer(2)).unwrap();
    }

    let store = Store::open_with_dir(&dir).unwrap();
    let keys: Vec<&String> = store.get_data().keys().collect();
    assert_eq!(keys, vec!["a", "b", "c"]);

    teardown(&dir);
}

// After an explicit flush, data lives in an SSTable; a later write lives in the
// memtable. Both must be readable after reopening the table.
#[test]
fn test_data_survives_flush_and_recovery() {
    let dir = setup("flush_recovery");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        store.set_value("z".to_string(), Value::Integer(26)).unwrap();
        store.set_value("a".to_string(), Value::Integer(1)).unwrap();
        store.set_value("m".to_string(), Value::Integer(13)).unwrap();
        store.flush().unwrap(); // z, a, m -> SSTable; memtable cleared
        store.set_value("f".to_string(), Value::Integer(6)).unwrap(); // memtable + WAL
    }

    let store = Store::open_with_dir(&dir).unwrap();
    assert_eq!(store.get_value("a").unwrap(), Some(Value::Integer(1)));
    assert_eq!(store.get_value("f").unwrap(), Some(Value::Integer(6)));
    assert_eq!(store.get_value("m").unwrap(), Some(Value::Integer(13)));
    assert_eq!(store.get_value("z").unwrap(), Some(Value::Integer(26)));

    teardown(&dir);
}

#[test]
fn test_set_get_delete() {
    let dir = setup("set_get_delete");

    let mut store = Store::open_with_dir(&dir).unwrap();

    store.set_value("key".to_string(), Value::Text("value".to_string())).unwrap();
    assert_eq!(store.get_value("key").unwrap(), Some(Value::Text("value".to_string())));

    store.delete_value("key").unwrap();
    assert_eq!(store.get_value("key").unwrap(), None);

    // Delete is idempotent: deleting an already-deleted key is not an error.
    store.delete_value("key").unwrap();
    assert_eq!(store.get_value("key").unwrap(), None);

    teardown(&dir);
}

#[test]
fn test_overwrite_keeps_sorted_order() {
    let dir = setup("overwrite_sorted");

    let mut store = Store::open_with_dir(&dir).unwrap();
    store.set_value("b".to_string(), Value::Integer(1)).unwrap();
    store.set_value("a".to_string(), Value::Integer(2)).unwrap();
    store.set_value("b".to_string(), Value::Integer(99)).unwrap();

    let keys: Vec<&String> = store.get_data().keys().collect();
    assert_eq!(keys, vec!["a", "b"]);
    assert_eq!(store.get_value("b").unwrap(), Some(Value::Integer(99)));

    teardown(&dir);
}

#[test]
fn test_double_open_same_table_fails() {
    let dir = setup("double_open");

    let _store1 = Store::open_with_dir(&dir).unwrap();
    let result = Store::open_with_dir(&dir);

    assert!(
        matches!(result, Err(AppError::IoError(_))),
        "opening the same table twice must fail while the first handle is held"
    );

    teardown(&dir);
}
