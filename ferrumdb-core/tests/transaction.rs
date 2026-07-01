use ferrumdb_core::store::{Store, Value};
use std::fs;

fn setup(name: &str) -> String {
    let dir = format!("./data/tx_{}", name);
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn teardown(dir: &str) {
    let _ = fs::remove_dir_all(dir);
}

fn wal_path(dir: &str) -> String {
    format!("{}/wal.log", dir)
}

// All ops in a committed transaction are visible immediately after commit.
#[test]
fn test_transaction_all_ops_visible_after_commit() {
    let dir = setup("commit_visible");
    let mut store = Store::open_with_dir(&dir).unwrap();

    let mut tx = store.begin_transaction();
    tx.set_value(b"a".to_vec(), Value::Integer(1));
    tx.set_value(b"b".to_vec(), Value::Integer(2));
    tx.set_value(b"c".to_vec(), Value::Integer(3));
    tx.commit().unwrap();

    assert_eq!(store.get_value(b"a").unwrap(), Some(Value::Integer(1)));
    assert_eq!(store.get_value(b"b").unwrap(), Some(Value::Integer(2)));
    assert_eq!(store.get_value(b"c").unwrap(), Some(Value::Integer(3)));

    teardown(&dir);
}

// Dropping a transaction without committing leaves the store unchanged.
#[test]
fn test_transaction_rollback_on_drop() {
    let dir = setup("rollback");
    let mut store = Store::open_with_dir(&dir).unwrap();
    store.set_value(b"existing".to_vec(), Value::Integer(99)).unwrap();

    {
        let mut tx = store.begin_transaction();
        tx.set_value(b"new_key".to_vec(), Value::Integer(1));
        // Drop without commit — rollback.
    }

    assert_eq!(store.get_value(b"existing").unwrap(), Some(Value::Integer(99)));
    assert_eq!(store.get_value(b"new_key").unwrap(), None);

    teardown(&dir);
}

// A committed transaction survives a crash (reopen).
#[test]
fn test_transaction_survives_recovery() {
    let dir = setup("tx_recovery");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        let mut tx = store.begin_transaction();
        tx.set_value(b"x".to_vec(), Value::Integer(10));
        tx.set_value(b"y".to_vec(), Value::Integer(20));
        tx.set_value(b"z".to_vec(), Value::Integer(30));
        tx.commit().unwrap();
        // Simulated crash — no flush.
    }

    let store = Store::open_with_dir(&dir).unwrap();
    assert_eq!(store.get_value(b"x").unwrap(), Some(Value::Integer(10)));
    assert_eq!(store.get_value(b"y").unwrap(), Some(Value::Integer(20)));
    assert_eq!(store.get_value(b"z").unwrap(), Some(Value::Integer(30)));

    teardown(&dir);
}

// Uncommitted WAL entries (no COMMIT marker) are discarded on recovery.
#[test]
fn test_uncommitted_entries_discarded_on_recovery() {
    let dir = setup("uncommitted");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        // Write one committed transaction first.
        let mut tx = store.begin_transaction();
        tx.set_value(b"committed".to_vec(), Value::Integer(1));
        tx.commit().unwrap();

        // Write directly to the WAL without a COMMIT — simulates a crash mid-write.
        use ferrumdb_core::wal::Wal;
        let mut raw_wal = Wal::with_path(wal_path(&dir));
        let entry = Wal::create_put_entry(b"uncommitted".to_vec(), &Value::Integer(2), 99);
        raw_wal.append(&entry).unwrap();
        // No write_commit — process "crashes" here.
    }

    let store = Store::open_with_dir(&dir).unwrap();
    assert_eq!(store.get_value(b"committed").unwrap(), Some(Value::Integer(1)));
    assert_eq!(store.get_value(b"uncommitted").unwrap(), None, "uncommitted entry must not survive recovery");

    teardown(&dir);
}

// A transaction with a mix of puts and deletes commits atomically.
#[test]
fn test_transaction_mixed_ops() {
    let dir = setup("mixed_ops");
    let mut store = Store::open_with_dir(&dir).unwrap();
    store.set_value(b"to_delete".to_vec(), Value::Text("bye".to_string())).unwrap();
    store.set_value(b"to_keep".to_vec(), Value::Text("hi".to_string())).unwrap();

    let mut tx = store.begin_transaction();
    tx.set_value(b"new".to_vec(), Value::Integer(42));
    tx.delete_value(b"to_delete".to_vec());
    tx.commit().unwrap();

    assert_eq!(store.get_value(b"new").unwrap(), Some(Value::Integer(42)));
    assert_eq!(store.get_value(b"to_delete").unwrap(), None);
    assert_eq!(store.get_value(b"to_keep").unwrap(), Some(Value::Text("hi".to_string())));

    teardown(&dir);
}

// A transaction reads its own uncommitted writes, and committed state for others.
#[test]
fn test_read_your_own_writes() {
    let dir = setup("read_your_writes");
    let mut store = Store::open_with_dir(&dir).unwrap();
    store.set_value(b"a".to_vec(), Value::Integer(1)).unwrap();

    let mut tx = store.begin_transaction();
    tx.set_value(b"b".to_vec(), Value::Integer(2));

    // Sees its own uncommitted write, and the committed value for an untouched key.
    assert_eq!(tx.get_value(b"b").unwrap(), Some(Value::Integer(2)));
    assert_eq!(tx.get_value(b"a").unwrap(), Some(Value::Integer(1)));

    // A delete in the transaction reads as absent within it.
    tx.delete_value(b"a".to_vec());
    assert_eq!(tx.get_value(b"a").unwrap(), None);

    tx.commit().unwrap();
    assert_eq!(store.get_value(b"a").unwrap(), None);
    assert_eq!(store.get_value(b"b").unwrap(), Some(Value::Integer(2)));

    teardown(&dir);
}

// An explicit rollback discards all buffered writes.
#[test]
fn test_explicit_rollback_discards() {
    let dir = setup("rollback_explicit");
    let mut store = Store::open_with_dir(&dir).unwrap();
    store.set_value(b"a".to_vec(), Value::Integer(1)).unwrap();

    let mut tx = store.begin_transaction();
    tx.set_value(b"a".to_vec(), Value::Integer(99));
    tx.set_value(b"new".to_vec(), Value::Integer(1));
    tx.rollback();

    assert_eq!(store.get_value(b"a").unwrap(), Some(Value::Integer(1)));
    assert_eq!(store.get_value(b"new").unwrap(), None);

    teardown(&dir);
}

// Multiple sequential transactions each commit independently.
#[test]
fn test_sequential_transactions() {
    let dir = setup("sequential");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        for i in 0..5i32 {
            let mut tx = store.begin_transaction();
            tx.set_value(format!("key_{}", i).into_bytes(), Value::Integer(i));
            tx.commit().unwrap();
        }
    } // lock released here

    let store2 = Store::open_with_dir(&dir).unwrap();
    for i in 0..5i32 {
        assert_eq!(store2.get_value(format!("key_{}", i).as_bytes()).unwrap(), Some(Value::Integer(i)));
    }

    teardown(&dir);
}
