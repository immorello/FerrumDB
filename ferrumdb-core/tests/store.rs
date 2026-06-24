use ferrumdb_core::errors::AppError;
use ferrumdb_core::store::{Store, Value};
use std::fs;

fn setup(name: &str) -> (String, String) {
    let dir = format!("./data/test_{}", name);
    fs::create_dir_all(&dir).ok();
    let snapshot = format!("{}/snapshot.pb", dir);
    let wal      = format!("{}/wal.log", dir);
    let _ = fs::remove_file(&snapshot);
    let _ = fs::remove_file(&wal);
    let _ = fs::remove_file(format!("{}/LOCK", dir));
    (snapshot, wal)
}

fn teardown(snapshot: &str, wal: &str) {
    let _ = fs::remove_file(snapshot);
    let _ = fs::remove_file(wal);
    if let Some(parent) = std::path::Path::new(snapshot).parent() {
        let _ = fs::remove_file(parent.join("LOCK"));
        let _ = fs::remove_dir(parent);
    }
}

#[test]
fn test_keys_iterated_in_sorted_order() {
    let (snap, wal) = setup("sorted_iteration");

    let mut store = Store::open_with_paths(&snap, &wal).unwrap();
    store.set_value("zebra".to_string(), Value::Integer(3)).unwrap();
    store.set_value("apple".to_string(), Value::Integer(1)).unwrap();
    store.set_value("mango".to_string(), Value::Integer(2)).unwrap();

    let keys: Vec<&String> = store.get_data().keys().collect();
    assert_eq!(keys, vec!["apple", "mango", "zebra"]);

    teardown(&snap, &wal);
}

#[test]
fn test_sorted_order_survives_recovery() {
    let (snap, wal) = setup("sorted_recovery");

    {
        let mut store = Store::open_with_paths(&snap, &wal).unwrap();
        store.set_value("c".to_string(), Value::Integer(3)).unwrap();
        store.set_value("a".to_string(), Value::Integer(1)).unwrap();
        store.set_value("b".to_string(), Value::Integer(2)).unwrap();
    }

    let store = Store::open_with_paths(&snap, &wal).unwrap();
    let keys: Vec<&String> = store.get_data().keys().collect();
    assert_eq!(keys, vec!["a", "b", "c"]);

    teardown(&snap, &wal);
}

#[test]
fn test_sorted_order_survives_checkpoint_and_recovery() {
    let (snap, wal) = setup("sorted_checkpoint");

    {
        let mut store = Store::open_with_paths(&snap, &wal).unwrap();
        store.set_value("z".to_string(), Value::Integer(26)).unwrap();
        store.set_value("a".to_string(), Value::Integer(1)).unwrap();
        store.set_value("m".to_string(), Value::Integer(13)).unwrap();
        store.checkpoint().unwrap();
        store.set_value("f".to_string(), Value::Integer(6)).unwrap();
    }

    let store = Store::open_with_paths(&snap, &wal).unwrap();
    let keys: Vec<&String> = store.get_data().keys().collect();
    assert_eq!(keys, vec!["a", "f", "m", "z"]);

    teardown(&snap, &wal);
}

#[test]
fn test_set_get_delete() {
    let (snap, wal) = setup("set_get_delete");

    let mut store = Store::open_with_paths(&snap, &wal).unwrap();

    store.set_value("key".to_string(), Value::Text("value".to_string())).unwrap();
    assert_eq!(store.get_value("key"), Some(&Value::Text("value".to_string())));

    store.delete_value("key").unwrap();
    assert_eq!(store.get_value("key"), None);

    assert!(store.delete_value("key").is_err());

    teardown(&snap, &wal);
}

#[test]
fn test_overwrite_keeps_sorted_order() {
    let (snap, wal) = setup("overwrite_sorted");

    let mut store = Store::open_with_paths(&snap, &wal).unwrap();
    store.set_value("b".to_string(), Value::Integer(1)).unwrap();
    store.set_value("a".to_string(), Value::Integer(2)).unwrap();
    store.set_value("b".to_string(), Value::Integer(99)).unwrap();

    let keys: Vec<&String> = store.get_data().keys().collect();
    assert_eq!(keys, vec!["a", "b"]);
    assert_eq!(store.get_value("b"), Some(&Value::Integer(99)));

    teardown(&snap, &wal);
}

#[test]
fn test_double_open_same_table_fails() {
    let (snap, wal) = setup("double_open");

    let _store1 = Store::open_with_paths(&snap, &wal).unwrap();
    let result  = Store::open_with_paths(&snap, &wal);

    assert!(
        matches!(result, Err(AppError::IoError(_))),
        "opening the same table twice must fail while the first handle is held"
    );

    teardown(&snap, &wal);
}
