use ferrumdb_core::proto::Operation;
use ferrumdb_core::store::{Store, Value};
use ferrumdb_core::wal::Wal;
use std::fs;

fn setup(name: &str) -> String {
    let dir = format!("./data/test_{}", name);
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn teardown(dir: &str) {
    let _ = fs::remove_dir_all(dir);
}

fn wal_path(dir: &str) -> String {
    format!("{}/wal.log", dir)
}

// --- WAL replay ---

#[test]
fn test_wal_replay_on_open() {
    let dir = setup("wal_replay");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        store.set_value(b"a".to_vec(), Value::Integer(1)).unwrap();
        store.set_value(b"b".to_vec(), Value::Text("hello".to_string())).unwrap();
        store.set_value(b"c".to_vec(), Value::Boolean(true)).unwrap();
        // No flush — the WAL is the only record.
    }

    let store = Store::open_with_dir(&dir).unwrap();
    assert_eq!(store.get_value(b"a").unwrap(), Some(Value::Integer(1)));
    assert_eq!(store.get_value(b"b").unwrap(), Some(Value::Text("hello".to_string())));
    assert_eq!(store.get_value(b"c").unwrap(), Some(Value::Boolean(true)));

    teardown(&dir);
}

#[test]
fn test_delete_replayed_on_open() {
    let dir = setup("delete_replay");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        store.set_value(b"keep".to_vec(), Value::Integer(10)).unwrap();
        store.set_value(b"drop".to_vec(), Value::Integer(99)).unwrap();
        store.delete_value(b"drop").unwrap();
    }

    let store = Store::open_with_dir(&dir).unwrap();
    assert_eq!(store.get_value(b"keep").unwrap(), Some(Value::Integer(10)));
    assert_eq!(store.get_value(b"drop").unwrap(), None);

    teardown(&dir);
}

// --- Flush ---

#[test]
fn test_flush_clears_wal() {
    let dir = setup("flush_clears_wal");

    let mut store = Store::open_with_dir(&dir).unwrap();
    store.set_value(b"x".to_vec(), Value::Float(2.5)).unwrap();
    store.flush().unwrap();

    let wal_entries = Wal::with_path(wal_path(&dir)).read_all().unwrap();
    assert!(wal_entries.is_empty(), "WAL must be empty after a flush");

    teardown(&dir);
}

#[test]
fn test_recovery_after_flush() {
    let dir = setup("recovery_after_flush");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        store.set_value(b"k1".to_vec(), Value::Integer(1)).unwrap();
        store.set_value(b"k2".to_vec(), Value::Integer(2)).unwrap();
        store.flush().unwrap();
        // WAL is now empty; all state lives in an SSTable.
    }

    let store = Store::open_with_dir(&dir).unwrap();
    assert_eq!(store.get_value(b"k1").unwrap(), Some(Value::Integer(1)));
    assert_eq!(store.get_value(b"k2").unwrap(), Some(Value::Integer(2)));

    teardown(&dir);
}

#[test]
fn test_recovery_sstable_plus_wal() {
    let dir = setup("sstable_plus_wal");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        store.set_value(b"a".to_vec(), Value::Integer(1)).unwrap();
        store.set_value(b"b".to_vec(), Value::Integer(2)).unwrap();
        store.flush().unwrap();
        // Writes after the flush land only in the WAL.
        store.set_value(b"c".to_vec(), Value::Integer(3)).unwrap();
        store.set_value(b"d".to_vec(), Value::Integer(4)).unwrap();
        // Simulated crash — no flush.
    }

    let store = Store::open_with_dir(&dir).unwrap();
    assert_eq!(store.get_value(b"a").unwrap(), Some(Value::Integer(1)));
    assert_eq!(store.get_value(b"b").unwrap(), Some(Value::Integer(2)));
    assert_eq!(store.get_value(b"c").unwrap(), Some(Value::Integer(3)));
    assert_eq!(store.get_value(b"d").unwrap(), Some(Value::Integer(4)));

    teardown(&dir);
}

#[test]
fn test_sequence_continues_after_open() {
    let dir = setup("sequence_continuity");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        store.set_value(b"x".to_vec(), Value::Integer(1)).unwrap();
        store.set_value(b"y".to_vec(), Value::Integer(2)).unwrap();
    }

    // After recovery, new writes must not reuse sequence numbers. We verify this
    // indirectly: the WAL must contain entries with strictly ascending sequences.
    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        store.set_value(b"z".to_vec(), Value::Integer(3)).unwrap();
    }

    let entries = Wal::with_path(wal_path(&dir)).read_all().unwrap();
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

    teardown(&dir);
}

// --- Tombstone correctness across SSTables ---

// A value flushed to one SSTable, then deleted and flushed to a newer SSTable,
// must read as deleted: the newer tombstone shadows the older value.
#[test]
fn test_tombstone_shadows_across_sstables() {
    let dir = setup("tombstone_shadows_sstables");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        store.set_value(b"x".to_vec(), Value::Integer(5)).unwrap();
        store.flush().unwrap(); // sstable_1: x = 5
        store.delete_value(b"x").unwrap();
        store.flush().unwrap(); // sstable_2: x = tombstone
    }

    let store = Store::open_with_dir(&dir).unwrap();
    assert_eq!(
        store.get_value(b"x").unwrap(),
        None,
        "a newer tombstone SSTable must shadow an older value SSTable"
    );

    teardown(&dir);
}

// A value flushed to an SSTable, then deleted with the delete living only in the
// WAL (no flush), must read as deleted after recovery: the replayed tombstone in
// the memtable shadows the SSTable value.
#[test]
fn test_wal_delete_shadows_sstable_value() {
    let dir = setup("wal_delete_shadows_sstable");

    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        store.set_value(b"x".to_vec(), Value::Integer(5)).unwrap();
        store.flush().unwrap(); // sstable_1: x = 5
        store.delete_value(b"x").unwrap(); // WAL DELETE only, no flush
        // Simulated crash.
    }

    let store = Store::open_with_dir(&dir).unwrap();
    assert_eq!(
        store.get_value(b"x").unwrap(),
        None,
        "a committed WAL delete must shadow the SSTable value on recovery"
    );

    teardown(&dir);
}

// An uncommitted WAL tail (a crash between append and COMMIT) must be truncated
// on recovery so a *later* COMMIT cannot silently adopt it.
#[test]
fn test_uncommitted_tail_not_adopted_by_later_commit() {
    let dir = setup("uncommitted_tail");

    // A normal committed write.
    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        store.set_value(b"committed".to_vec(), Value::Integer(1)).unwrap();
    }

    // Simulate a crash that left an uncommitted PUT in the WAL (no COMMIT after).
    {
        let mut raw = Wal::with_path(wal_path(&dir));
        let ghost = Wal::create_put_entry(b"ghost".to_vec(), &Value::Integer(99), 50);
        raw.append(&ghost).unwrap();
    }

    // Reopen (truncates the uncommitted tail), then commit a brand-new write.
    {
        let mut store = Store::open_with_dir(&dir).unwrap();
        assert_eq!(store.get_value(b"ghost").unwrap(), None, "uncommitted entry must not survive reopen");
        store.set_value(b"after".to_vec(), Value::Integer(2)).unwrap();
    }

    // The ghost must still be absent — session 2's COMMIT must not have adopted it.
    {
        let store = Store::open_with_dir(&dir).unwrap();
        assert_eq!(store.get_value(b"ghost").unwrap(), None, "uncommitted entry must never be resurrected");
        assert_eq!(store.get_value(b"committed").unwrap(), Some(Value::Integer(1)));
        assert_eq!(store.get_value(b"after").unwrap(), Some(Value::Integer(2)));
    }

    teardown(&dir);
}
