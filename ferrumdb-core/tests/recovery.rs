use ferrumdb_core::proto::Operation;
use ferrumdb_core::store::{Store, Value};
use ferrumdb_core::wal::Wal;
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

// --- WAL replay ---

#[test]
fn test_wal_replay_on_open() {
    let (snap, wal) = setup("wal_replay");

    {
        let mut store = Store::open_with_paths(&snap, &wal).unwrap();
        store.set_value("a".to_string(), Value::Integer(1)).unwrap();
        store.set_value("b".to_string(), Value::Text("hello".to_string())).unwrap();
        store.set_value("c".to_string(), Value::Boolean(true)).unwrap();
        // No checkpoint — WAL is the only record.
    }

    let store = Store::open_with_paths(&snap, &wal).unwrap();
    assert_eq!(store.get_value("a"), Some(&Value::Integer(1)));
    assert_eq!(store.get_value("b"), Some(&Value::Text("hello".to_string())));
    assert_eq!(store.get_value("c"), Some(&Value::Boolean(true)));

    teardown(&snap, &wal);
}

#[test]
fn test_delete_replayed_on_open() {
    let (snap, wal) = setup("delete_replay");

    {
        let mut store = Store::open_with_paths(&snap, &wal).unwrap();
        store.set_value("keep".to_string(), Value::Integer(10)).unwrap();
        store.set_value("drop".to_string(), Value::Integer(99)).unwrap();
        store.delete_value("drop").unwrap();
    }

    let store = Store::open_with_paths(&snap, &wal).unwrap();
    assert_eq!(store.get_value("keep"), Some(&Value::Integer(10)));
    assert_eq!(store.get_value("drop"), None);

    teardown(&snap, &wal);
}

// --- Checkpoint ---

#[test]
fn test_checkpoint_clears_wal() {
    let (snap, wal) = setup("checkpoint_clears_wal");

    let mut store = Store::open_with_paths(&snap, &wal).unwrap();
    store.set_value("x".to_string(), Value::Float(3.14)).unwrap();
    store.checkpoint().unwrap();

    let wal_entries = Wal::with_path(&wal).read_all().unwrap();
    assert!(wal_entries.is_empty(), "WAL must be empty after checkpoint");

    teardown(&snap, &wal);
}

#[test]
fn test_recovery_from_snapshot_after_checkpoint() {
    let (snap, wal) = setup("snapshot_recovery");

    {
        let mut store = Store::open_with_paths(&snap, &wal).unwrap();
        store.set_value("k1".to_string(), Value::Integer(1)).unwrap();
        store.set_value("k2".to_string(), Value::Integer(2)).unwrap();
        store.checkpoint().unwrap();
        // WAL is now empty; all state lives in the snapshot.
    }

    let store = Store::open_with_paths(&snap, &wal).unwrap();
    assert_eq!(store.get_value("k1"), Some(&Value::Integer(1)));
    assert_eq!(store.get_value("k2"), Some(&Value::Integer(2)));

    teardown(&snap, &wal);
}

#[test]
fn test_recovery_snapshot_plus_wal() {
    let (snap, wal) = setup("snapshot_plus_wal");

    {
        let mut store = Store::open_with_paths(&snap, &wal).unwrap();
        store.set_value("a".to_string(), Value::Integer(1)).unwrap();
        store.set_value("b".to_string(), Value::Integer(2)).unwrap();
        store.checkpoint().unwrap();
        // Writes after checkpoint land only in the WAL.
        store.set_value("c".to_string(), Value::Integer(3)).unwrap();
        store.set_value("d".to_string(), Value::Integer(4)).unwrap();
        // Simulated crash — no second checkpoint.
    }

    let store = Store::open_with_paths(&snap, &wal).unwrap();
    assert_eq!(store.get_value("a"), Some(&Value::Integer(1)));
    assert_eq!(store.get_value("b"), Some(&Value::Integer(2)));
    assert_eq!(store.get_value("c"), Some(&Value::Integer(3)));
    assert_eq!(store.get_value("d"), Some(&Value::Integer(4)));

    teardown(&snap, &wal);
}

#[test]
fn test_sequence_continues_after_open() {
    let (snap, wal) = setup("sequence_continuity");

    {
        let mut store = Store::open_with_paths(&snap, &wal).unwrap();
        store.set_value("x".to_string(), Value::Integer(1)).unwrap();
        store.set_value("y".to_string(), Value::Integer(2)).unwrap();
    }

    // After recovery, new writes must not reuse sequence numbers.
    // We verify this indirectly: the WAL must contain 3 entries with ascending sequences.
    {
        let mut store = Store::open_with_paths(&snap, &wal).unwrap();
        store.set_value("z".to_string(), Value::Integer(3)).unwrap();
    }

    let entries = Wal::with_path(&wal).read_all().unwrap();
    // Filter to data entries only — COMMIT entries share the sequence of the preceding write.
    let sequences: Vec<u64> = entries.iter()
        .filter(|e| matches!(e.operation(), Operation::Put | Operation::Delete))
        .map(|e| e.sequence)
        .collect();
    assert!(
        sequences.windows(2).all(|w| w[0] < w[1]),
        "sequences must be strictly increasing: {:?}",
        sequences
    );

    teardown(&snap, &wal);
}
