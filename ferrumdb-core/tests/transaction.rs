use ferrumdb_core::store::{Store, Value};
use std::fs;

fn setup(name: &str) -> (String, String) {
    let dir = format!("./data/tx_{}", name);
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

// All ops in a committed transaction are visible immediately after commit.
#[test]
fn test_transaction_all_ops_visible_after_commit() {
    let (snap, wal) = setup("commit_visible");
    let mut store = Store::open_with_paths(&snap, &wal).unwrap();

    let mut tx = store.begin_transaction();
    tx.set_value("a".to_string(), Value::Integer(1));
    tx.set_value("b".to_string(), Value::Integer(2));
    tx.set_value("c".to_string(), Value::Integer(3));
    tx.commit().unwrap();

    assert_eq!(store.get_value("a").unwrap(), Some(Value::Integer(1)));
    assert_eq!(store.get_value("b").unwrap(), Some(Value::Integer(2)));
    assert_eq!(store.get_value("c").unwrap(), Some(Value::Integer(3)));

    teardown(&snap, &wal);
}

// Dropping a transaction without committing leaves the store unchanged.
#[test]
fn test_transaction_rollback_on_drop() {
    let (snap, wal) = setup("rollback");
    let mut store = Store::open_with_paths(&snap, &wal).unwrap();
    store.set_value("existing".to_string(), Value::Integer(99)).unwrap();

    {
        let mut tx = store.begin_transaction();
        tx.set_value("new_key".to_string(), Value::Integer(1));
        // Drop without commit — rollback.
    }

    assert_eq!(store.get_value("existing").unwrap(), Some(Value::Integer(99)));
    assert_eq!(store.get_value("new_key").unwrap(), None);

    teardown(&snap, &wal);
}

// A committed transaction survives a crash (reopen).
#[test]
fn test_transaction_survives_recovery() {
    let (snap, wal) = setup("tx_recovery");

    {
        let mut store = Store::open_with_paths(&snap, &wal).unwrap();
        let mut tx = store.begin_transaction();
        tx.set_value("x".to_string(), Value::Integer(10));
        tx.set_value("y".to_string(), Value::Integer(20));
        tx.set_value("z".to_string(), Value::Integer(30));
        tx.commit().unwrap();
        // Simulated crash — no checkpoint.
    }

    let store = Store::open_with_paths(&snap, &wal).unwrap();
    assert_eq!(store.get_value("x").unwrap(), Some(Value::Integer(10)));
    assert_eq!(store.get_value("y").unwrap(), Some(Value::Integer(20)));
    assert_eq!(store.get_value("z").unwrap(), Some(Value::Integer(30)));

    teardown(&snap, &wal);
}

// Uncommitted WAL entries (no COMMIT marker) are discarded on recovery.
#[test]
fn test_uncommitted_entries_discarded_on_recovery() {
    let (snap, wal) = setup("uncommitted");

    {
        let mut store = Store::open_with_paths(&snap, &wal).unwrap();
        // Write one committed transaction first.
        let mut tx = store.begin_transaction();
        tx.set_value("committed".to_string(), Value::Integer(1));
        tx.commit().unwrap();

        // Write directly to the WAL without a COMMIT — simulates a crash mid-write.
        use ferrumdb_core::wal::Wal;
        let raw_wal = Wal::with_path(&wal);
        let entry = Wal::create_put_entry("uncommitted".to_string(), &Value::Integer(2), 99);
        raw_wal.append(&entry).unwrap();
        // No write_commit — process "crashes" here.
    }

    let store = Store::open_with_paths(&snap, &wal).unwrap();
    assert_eq!(store.get_value("committed").unwrap(), Some(Value::Integer(1)));
    assert_eq!(store.get_value("uncommitted").unwrap(), None, "uncommitted entry must not survive recovery");

    teardown(&snap, &wal);
}

// A transaction with a mix of puts and deletes commits atomically.
#[test]
fn test_transaction_mixed_ops() {
    let (snap, wal) = setup("mixed_ops");
    let mut store = Store::open_with_paths(&snap, &wal).unwrap();
    store.set_value("to_delete".to_string(), Value::Text("bye".to_string())).unwrap();
    store.set_value("to_keep".to_string(), Value::Text("hi".to_string())).unwrap();

    let mut tx = store.begin_transaction();
    tx.set_value("new".to_string(), Value::Integer(42));
    tx.delete_value("to_delete".to_string());
    tx.commit().unwrap();

    assert_eq!(store.get_value("new").unwrap(), Some(Value::Integer(42)));
    assert_eq!(store.get_value("to_delete").unwrap(), None);
    assert_eq!(store.get_value("to_keep").unwrap(), Some(Value::Text("hi".to_string())));

    teardown(&snap, &wal);
}

// Multiple sequential transactions each commit independently.
#[test]
fn test_sequential_transactions() {
    let (snap, wal) = setup("sequential");

    {
        let mut store = Store::open_with_paths(&snap, &wal).unwrap();
        for i in 0..5i32 {
            let mut tx = store.begin_transaction();
            tx.set_value(format!("key_{}", i), Value::Integer(i));
            tx.commit().unwrap();
        }
    } // lock released here

    let store2 = Store::open_with_paths(&snap, &wal).unwrap();
    for i in 0..5i32 {
        assert_eq!(store2.get_value(&format!("key_{}", i)).unwrap(), Some(Value::Integer(i)));
    }

    teardown(&snap, &wal);
}
