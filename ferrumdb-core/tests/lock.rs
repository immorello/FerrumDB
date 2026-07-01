use ferrumdb_core::errors::AppError;
use ferrumdb_core::store::{Store, Value};
use std::fs;
use std::path::Path;

fn setup(name: &str) -> String {
    let dir = format!("./data/lock_{}", name);
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn teardown(dir: &str) {
    let _ = fs::remove_dir_all(dir);
}

// Opening the same table twice must fail while the first handle is alive.
#[test]
fn test_double_open_fails() {
    let dir = setup("double_open");

    let _first = Store::open_with_dir(&dir).unwrap();
    let result = Store::open_with_dir(&dir);

    assert!(
        matches!(result, Err(AppError::IoError(_))),
        "second open must fail while first handle is held"
    );

    teardown(&dir);
}

// Dropping the Store releases the lock — the same table can be reopened.
#[test]
fn test_lock_released_on_drop() {
    let dir = setup("lock_released");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        store.set_value(b"k".to_vec(), Value::Integer(1)).unwrap();
        // store dropped here — lock released
    }

    // Must succeed: lock is no longer held
    let store = Store::open_with_dir(&dir).unwrap();
    assert_eq!(store.get_value(b"k").unwrap(), Some(Value::Integer(1)));

    teardown(&dir);
}

// Two different tables must be openable at the same time — locks are per table.
#[test]
fn test_different_tables_open_simultaneously() {
    let dir_a = setup("table_a");
    let dir_b = setup("table_b");

    let mut store_a = Store::open_with_dir(&dir_a).unwrap();
    let mut store_b = Store::open_with_dir(&dir_b).unwrap();

    store_a.set_value(b"key".to_vec(), Value::Text("from_a".to_string())).unwrap();
    store_b.set_value(b"key".to_vec(), Value::Text("from_b".to_string())).unwrap();

    assert_eq!(store_a.get_value(b"key").unwrap(), Some(Value::Text("from_a".to_string())));
    assert_eq!(store_b.get_value(b"key").unwrap(), Some(Value::Text("from_b".to_string())));

    teardown(&dir_a);
    teardown(&dir_b);
}

// The LOCK file is created on open.
#[test]
fn test_lock_file_created_on_open() {
    let dir = setup("lock_file_exists");

    {
        let _store = Store::open_with_dir(&dir).unwrap();
        let lock_path = Path::new(&dir).join("LOCK");
        assert!(lock_path.exists(), "LOCK file must exist while store is open");
    }

    teardown(&dir);
}

// Acquire, release, acquire again — the table can be reused across sessions.
#[test]
fn test_lock_reacquired_after_multiple_cycles() {
    let dir = setup("lock_cycles");

    for i in 0..3 {
        let mut store = Store::open_with_dir(&dir)
            .unwrap_or_else(|e| panic!("cycle {} failed to open: {e:?}", i));
        store.set_value(format!("key_{}", i).into_bytes(), Value::Integer(i)).unwrap();
    }

    // All three writes must be recoverable after three open/close cycles.
    let store = Store::open_with_dir(&dir).unwrap();
    assert_eq!(store.get_value(b"key_0").unwrap(), Some(Value::Integer(0)));
    assert_eq!(store.get_value(b"key_1").unwrap(), Some(Value::Integer(1)));
    assert_eq!(store.get_value(b"key_2").unwrap(), Some(Value::Integer(2)));

    teardown(&dir);
}
