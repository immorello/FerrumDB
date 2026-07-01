use ferrumdb_core::store::Value;
use ferrumdb_core::wal::Wal;
use ferrumdb_core::proto::Operation;
use std::fs;

#[test]
fn test_append_and_read_entries() {
    let test_path = "./data/test_wal_integration.log";
    
    // Ensure data directory exists
    fs::create_dir_all("data").ok();
    
    // Clean up any existing test file
    let _ = fs::remove_file(test_path);

    let mut wal = Wal::with_path(test_path);

    // Test 1: Append PUT entries
    let entry1 = Wal::create_put_entry(
        b"user_1".to_vec(),
        &Value::Integer(42),
        1
    );
    wal.append(&entry1).expect("Failed to append entry 1");

    let entry2 = Wal::create_put_entry(
        b"name".to_vec(),
        &Value::Text("FerrumDB".to_string()),
        2
    );
    wal.append(&entry2).expect("Failed to append entry 2");

    // Test DELETE entry
    let entry3 = Wal::create_delete_entry(b"user_1".to_vec(), 3);
    wal.append(&entry3).expect("Failed to append entry 3");

    // Test Float
    let entry4 = Wal::create_put_entry(
        b"price".to_vec(),
        &Value::Float(19.99),
        4
    );
    wal.append(&entry4).expect("Failed to append entry 4");

    // Test Boolean
    let entry5 = Wal::create_put_entry(
        b"active".to_vec(),
        &Value::Boolean(true),
        5
    );
    wal.append(&entry5).expect("Failed to append entry 5");

    // Test 2: Read all entries back
    let entries = wal.read_all().expect("Failed to read entries");
    
    assert_eq!(entries.len(), 5, "Should have 5 entries");
    
    // Verify first entry
    assert_eq!(entries[0].key, b"user_1");
    assert_eq!(entries[0].operation(), Operation::Put);
    assert_eq!(entries[0].sequence, 1);
    
    // Verify second entry
    assert_eq!(entries[1].key, b"name");
    assert_eq!(entries[1].operation(), Operation::Put);
    assert_eq!(entries[1].sequence, 2);
    
    // Verify third entry (DELETE)
    assert_eq!(entries[2].key, b"user_1");
    assert_eq!(entries[2].operation(), Operation::Delete);
    assert_eq!(entries[2].sequence, 3);
    
    // Verify fourth entry
    assert_eq!(entries[3].key, b"price");
    assert_eq!(entries[3].operation(), Operation::Put);
    assert_eq!(entries[3].sequence, 4);
    
    // Verify fifth entry
    assert_eq!(entries[4].key, b"active");
    assert_eq!(entries[4].operation(), Operation::Put);
    assert_eq!(entries[4].sequence, 5);
    
    // Clean up
    fs::remove_file(test_path).ok();
}

#[test]
fn test_wal_persistence() {
    let test_path = "./data/test_wal_persistence.log";
    
    // Ensure data directory exists
    fs::create_dir_all("data").ok();
    
    // Clean up any existing test file
    let _ = fs::remove_file(test_path);
    
    // Create WAL and write entries
    {
        let mut wal = Wal::with_path(test_path);
        let entry = Wal::create_put_entry(
            b"test_key".to_vec(),
            &Value::Integer(123),
            1
        );
        wal.append(&entry).expect("Failed to append");
    }
    
    // Open a new WAL instance and read back
    let wal = Wal::with_path(test_path);
    let entries = wal.read_all().expect("Failed to read");
    
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, b"test_key");
    assert_eq!(entries[0].sequence, 1);
    
    // Clean up
    fs::remove_file(test_path).ok();
}

#[test]
fn test_wal_clear() {
    let test_path = "./data/test_wal_clear.log";
    
    // Ensure data directory exists
    fs::create_dir_all("data").ok();
    
    // Clean up any existing test file
    let _ = fs::remove_file(test_path);

    let mut wal = Wal::with_path(test_path);

    // Add an entry
    let entry = Wal::create_put_entry(
        b"key1".to_vec(),
        &Value::Text("value1".to_string()),
        1
    );
    wal.append(&entry).expect("Failed to append");
    
    // Verify it exists
    assert!(wal.read_all().expect("Failed to read").len() == 1);
    
    // Clear the WAL
    wal.clear().expect("Failed to clear");
    
    // Verify it's empty
    assert!(wal.read_all().expect("Failed to read").is_empty());
    
    // Clean up
    fs::remove_file(test_path).ok();
}
