use ferrumdb::{Database, Error};
use std::fs;

fn setup(name: &str) -> String {
    let root = format!("./data/api_{}", name);
    let _ = fs::remove_dir_all(&root);
    root
}

fn teardown(root: &str) {
    let _ = fs::remove_dir_all(root);
}

#[test]
fn test_put_get_delete() {
    let root = setup("put_get_delete");
    let mut db = Database::open(&root).unwrap();
    let mut t = db.table("kv").unwrap();

    t.put(b"a", b"apple").unwrap();
    assert_eq!(t.get(b"a").unwrap(), Some(b"apple".to_vec()));
    assert_eq!(t.get(b"missing").unwrap(), None);

    t.delete(b"a").unwrap();
    assert_eq!(t.get(b"a").unwrap(), None);
    // Delete is idempotent.
    t.delete(b"a").unwrap();

    teardown(&root);
}

#[test]
fn test_overwrite() {
    let root = setup("overwrite");
    let mut db = Database::open(&root).unwrap();
    let mut t = db.table("kv").unwrap();

    t.put(b"k", b"one").unwrap();
    t.put(b"k", b"two").unwrap();
    assert_eq!(t.get(b"k").unwrap(), Some(b"two".to_vec()));

    teardown(&root);
}

#[test]
fn test_arbitrary_byte_values() {
    let root = setup("byte_values");
    let mut db = Database::open(&root).unwrap();
    let mut t = db.table("kv").unwrap();

    let binary: &[u8] = &[0u8, 255, 1, 254, 0, 42];
    t.put(b"blob", binary).unwrap();
    assert_eq!(t.get(b"blob").unwrap(), Some(binary.to_vec()));

    // Empty value round-trips too.
    t.put(b"empty", b"").unwrap();
    assert_eq!(t.get(b"empty").unwrap(), Some(Vec::new()));

    teardown(&root);
}

#[test]
fn test_contains() {
    let root = setup("contains");
    let mut db = Database::open(&root).unwrap();
    let mut t = db.table("kv").unwrap();

    t.put(b"here", b"1").unwrap();
    assert!(t.contains(b"here").unwrap());
    assert!(!t.contains(b"nope").unwrap());

    teardown(&root);
}

#[test]
fn test_put_batch_is_atomic_and_get_batch() {
    let root = setup("batch");
    let mut db = Database::open(&root).unwrap();
    let mut t = db.table("kv").unwrap();

    t.put_batch(&[
        (b"k1", b"v1"),
        (b"k2", b"v2"),
        (b"k3", b"v3"),
    ]).unwrap();

    let got = t.get_batch(&[b"k1", b"missing", b"k3"]).unwrap();
    assert_eq!(got, vec![Some(b"v1".to_vec()), None, Some(b"v3".to_vec())]);

    teardown(&root);
}

#[test]
fn test_arbitrary_byte_keys() {
    let root = setup("byte_keys");
    let mut db = Database::open(&root).unwrap();
    let mut t = db.table("kv").unwrap();

    // Keys are arbitrary bytes, including non-UTF-8 and embedded nulls.
    let binary_key: &[u8] = &[0xff, 0x00, 0xfe, 42];
    t.put(binary_key, b"value").unwrap();
    assert_eq!(t.get(binary_key).unwrap(), Some(b"value".to_vec()));
    assert!(t.contains(binary_key).unwrap());

    teardown(&root);
}

#[test]
fn test_table_management() {
    let root = setup("tables");
    let mut db = Database::open(&root).unwrap();

    db.create_table("users").unwrap();
    db.create_table("orders").unwrap();

    // create_table on an existing table is an error.
    assert!(matches!(db.create_table("users"), Err(Error::TableExists(_))));

    let mut tables = db.list_tables().unwrap();
    tables.sort();
    assert_eq!(tables, vec!["orders".to_string(), "users".to_string()]);

    // delete_table removes it; a missing table errors.
    db.delete_table("orders").unwrap();
    assert_eq!(db.list_tables().unwrap(), vec!["users".to_string()]);
    assert!(matches!(db.delete_table("orders"), Err(Error::TableNotFound(_))));

    teardown(&root);
}

#[test]
fn test_invalid_table_name_is_rejected() {
    let root = setup("bad_table");
    let mut db = Database::open(&root).unwrap();

    assert!(matches!(db.table(""), Err(Error::InvalidTableName(_))));
    assert!(matches!(db.table("a/b"), Err(Error::InvalidTableName(_))));
    assert!(matches!(db.table(".."), Err(Error::InvalidTableName(_))));

    teardown(&root);
}

#[test]
fn test_scan_range_and_prefix() {
    let root = setup("scan");
    let mut db = Database::open(&root).unwrap();
    let mut t = db.table("kv").unwrap();

    t.put_batch(&[
        (b"user:1", b"alice"),
        (b"user:2", b"bob"),
        (b"user:3", b"carol"),
        (b"post:1", b"hello"),
    ]).unwrap();

    // Full scan is sorted across all keys.
    let all: Vec<Vec<u8>> = t.scan().unwrap().into_iter().map(|(k, _)| k).collect();
    assert_eq!(all, vec![
        b"post:1".to_vec(),
        b"user:1".to_vec(),
        b"user:2".to_vec(),
        b"user:3".to_vec(),
    ]);

    // Prefix scan returns only the "user:" keys, with values.
    let users = t.scan_prefix(b"user:").unwrap();
    assert_eq!(users, vec![
        (b"user:1".to_vec(), b"alice".to_vec()),
        (b"user:2".to_vec(), b"bob".to_vec()),
        (b"user:3".to_vec(), b"carol".to_vec()),
    ]);

    // Half-open range.
    let mid: Vec<Vec<u8>> = t.range(b"user:1", b"user:3").unwrap().into_iter().map(|(k, _)| k).collect();
    assert_eq!(mid, vec![b"user:1".to_vec(), b"user:2".to_vec()]);

    teardown(&root);
}

#[test]
fn test_transaction_read_your_writes_then_commit() {
    let root = setup("txn_commit");
    let mut db = Database::open(&root).unwrap();
    let mut t = db.table("kv").unwrap();
    t.put(b"a", b"1").unwrap();

    let mut tx = t.transaction();
    tx.put(b"b", b"2");
    assert_eq!(tx.get(b"b").unwrap(), Some(b"2".to_vec())); // read-your-writes
    assert_eq!(tx.get(b"a").unwrap(), Some(b"1".to_vec())); // committed state
    tx.delete(b"a");
    assert_eq!(tx.get(b"a").unwrap(), None);
    tx.commit().unwrap();

    assert_eq!(t.get(b"a").unwrap(), None);
    assert_eq!(t.get(b"b").unwrap(), Some(b"2".to_vec()));

    teardown(&root);
}

#[test]
fn test_transaction_rollback() {
    let root = setup("txn_rollback");
    let mut db = Database::open(&root).unwrap();
    let mut t = db.table("kv").unwrap();
    t.put(b"a", b"1").unwrap();

    let mut tx = t.transaction();
    tx.put(b"a", b"changed");
    tx.put(b"new", b"x");
    tx.rollback();

    assert_eq!(t.get(b"a").unwrap(), Some(b"1".to_vec()));
    assert_eq!(t.get(b"new").unwrap(), None);

    teardown(&root);
}

#[test]
fn test_data_persists_across_reopen() {
    let root = setup("persist");

    {
        let mut db = Database::open(&root).unwrap();
        let mut t = db.table("kv").unwrap();
        t.put(b"durable", b"yes").unwrap();
        // db dropped here — table lock released, no explicit flush
    }

    let mut db = Database::open(&root).unwrap();
    let t = db.table("kv").unwrap();
    assert_eq!(t.get(b"durable").unwrap(), Some(b"yes".to_vec()));

    teardown(&root);
}

#[test]
fn test_separate_tables_are_independent() {
    let root = setup("independent");
    let mut db = Database::open(&root).unwrap();

    db.table("a").unwrap().put(b"k", b"from_a").unwrap();
    db.table("b").unwrap().put(b"k", b"from_b").unwrap();

    assert_eq!(db.table("a").unwrap().get(b"k").unwrap(), Some(b"from_a".to_vec()));
    assert_eq!(db.table("b").unwrap().get(b"k").unwrap(), Some(b"from_b".to_vec()));

    teardown(&root);
}
