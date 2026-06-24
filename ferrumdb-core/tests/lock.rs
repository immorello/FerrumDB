use ferrumdb_core::errors::AppError;
use ferrumdb_core::store::{Store, Value};
use std::fs;
use std::path::Path;

fn setup(name: &str) -> (String, String) {
    let dir = format!("./data/lock_{}", name);
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
    if let Some(parent) = Path::new(snapshot).parent() {
        let _ = fs::remove_file(parent.join("LOCK"));
        let _ = fs::remove_dir(parent);
    }
}

// Opening the same table twice must fail while the first handle is alive.
#[test]
fn test_double_open_fails() {
    let (snap, wal) = setup("double_open");

    let _first = Store::open_with_paths(&snap, &wal).unwrap();
    let result = Store::open_with_paths(&snap, &wal);

    assert!(
        matches!(result, Err(AppError::IoError(_))),
        "second open must fail while first handle is held"
    );

    teardown(&snap, &wal);
}

// Dropping the Store releases the lock — the same path can be reopened.
#[test]
fn test_lock_released_on_drop() {
    let (snap, wal) = setup("lock_released");

    {
        let mut store = Store::open_with_paths(&snap, &wal).unwrap();
        store.set_value("k".to_string(), Value::Integer(1)).unwrap();
        // store dropped here — lock released
    }

    // Must succeed: lock is no longer held
    let store = Store::open_with_paths(&snap, &wal).unwrap();
    assert_eq!(store.get_value("k"), Some(&Value::Integer(1)));

    teardown(&snap, &wal);
}

// Two different tables must be openable at the same time — locks are per table.
#[test]
fn test_different_tables_open_simultaneously() {
    let (snap_a, wal_a) = setup("table_a");
    let (snap_b, wal_b) = setup("table_b");

    let mut store_a = Store::open_with_paths(&snap_a, &wal_a).unwrap();
    let mut store_b = Store::open_with_paths(&snap_b, &wal_b).unwrap();

    store_a.set_value("key".to_string(), Value::Text("from_a".to_string())).unwrap();
    store_b.set_value("key".to_string(), Value::Text("from_b".to_string())).unwrap();

    assert_eq!(store_a.get_value("key"), Some(&Value::Text("from_a".to_string())));
    assert_eq!(store_b.get_value("key"), Some(&Value::Text("from_b".to_string())));

    teardown(&snap_a, &wal_a);
    teardown(&snap_b, &wal_b);
}

// The LOCK file is created on open.
#[test]
fn test_lock_file_created_on_open() {
    let (snap, wal) = setup("lock_file_exists");

    {
        let _store = Store::open_with_paths(&snap, &wal).unwrap();
        let lock_path = Path::new(&snap).parent().unwrap().join("LOCK");
        assert!(lock_path.exists(), "LOCK file must exist while store is open");
    }

    teardown(&snap, &wal);
}

// Acquire, release, acquire again — lock can be reused across sessions.
#[test]
fn test_lock_reacquired_after_multiple_cycles() {
    let (snap, wal) = setup("lock_cycles");

    for i in 0..3 {
        let mut store = Store::open_with_paths(&snap, &wal)
            .unwrap_or_else(|e| panic!("cycle {} failed to open: {e:?}", i));
        store.set_value(format!("key_{}", i), Value::Integer(i as i32)).unwrap();
    }

    // All three writes must be recoverable after three open/close cycles.
    let store = Store::open_with_paths(&snap, &wal).unwrap();
    assert_eq!(store.get_value("key_0"), Some(&Value::Integer(0)));
    assert_eq!(store.get_value("key_1"), Some(&Value::Integer(1)));
    assert_eq!(store.get_value("key_2"), Some(&Value::Integer(2)));

    teardown(&snap, &wal);
}
