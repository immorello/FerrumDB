//! A hands-on tour of the FerrumDB public API.
//!
//! Run it with:
//!     cargo run -p ferrumdb --example demo
//!
//! Then poke at the files it leaves on disk:
//!     ls -la data/demo/users        # wal.log, LOCK (and sstable_*.sst once flushed)
//!
//! Edit this file and re-run to experiment.

use ferrumdb::Database;

/// Helper to print a byte value as text (values are raw bytes).
fn show(v: &Option<Vec<u8>>) -> String {
    match v {
        Some(bytes) => format!("{:?}", String::from_utf8_lossy(bytes)),
        None => "None".to_string(),
    }
}

fn main() -> Result<(), ferrumdb::Error> {
    // Start from a clean directory so the demo is repeatable.
    let root = "./data/demo";
    let _ = std::fs::remove_dir_all(root);

    let mut db = Database::open(root)?;
    println!("opened database at {root}");

    // --- Tables ---
    db.create_table("users")?;
    db.create_table("config")?;
    println!("tables: {:?}\n", db.list_tables()?);

    // --- Put / Get ---
    let mut users = db.table("users")?;
    users.put(b"user:42", b"alice")?;
    users.put(b"user:7", b"bob")?;
    println!("get user:42 -> {}", show(&users.get(b"user:42")?));
    println!("get user:99 -> {}  (absent)\n", show(&users.get(b"user:99")?));

    // --- Overwrite ---
    users.put(b"user:42", b"alice smith")?;
    println!("after overwrite, user:42 -> {}\n", show(&users.get(b"user:42")?));

    // --- Atomic batch write + batch read ---
    users.put_batch(&[
        (b"user:1", b"carol"),
        (b"user:2", b"dave"),
        (b"user:3", b"erin"),
    ])?;
    let batch = users.get_batch(&[b"user:1", b"user:2", b"nope"])?;
    println!("get_batch [user:1, user:2, nope] -> {:?}\n",
        batch.iter().map(show).collect::<Vec<_>>());

    // --- contains / delete ---
    println!("contains user:7 -> {}", users.contains(b"user:7")?);
    users.delete(b"user:7")?;
    println!("after delete, contains user:7 -> {}\n", users.contains(b"user:7")?);

    // --- Arbitrary byte values (not just text) ---
    let blob: &[u8] = &[0, 255, 1, 254, 42];
    users.put(b"raw", blob)?;
    println!("raw bytes round-trip -> {:?}\n", users.get(b"raw")?);

    // --- Persistence: drop the database, reopen, read back ---
    drop(db);
    let mut db = Database::open(root)?;
    let users = db.table("users")?;
    println!("after reopen, user:42 -> {}", show(&users.get(b"user:42")?));
    println!("tables still there: {:?}", db.list_tables()?);

    println!("\ndone. inspect the files with:  ls -la {root}/users");
    Ok(())
}
