//! SSTable (Sorted String Table) module for FerrumDB
//!
//! An SSTable is an immutable, sorted, on-disk file of key-value pairs.
//! It is what the in-memory memtable becomes when flushed to disk.
//!
//! File layout (see docs/sstable.md for the full spec):
//!
//! ```text
//! [ data block 0 ][ data block 1 ] ... [ data block N ]  ← DATA REGION
//! [ sparse index: one entry per block ]                  ← INDEX REGION
//! [ footer: 28 fixed bytes ]                             ← FOOTER
//! ```
//!
//! The file is read back-to-front: the footer (last 28 bytes) points to the
//! index, the index is loaded into RAM, and from then on any key is found with
//! one binary search plus a single block read.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use prost::Message;

use crate::errors::AppError;
use crate::proto::ValueMessage;
use crate::store::Value;

/// Target size of a data block. A block ends once its record bytes reach this
/// threshold, so blocks are approximately — not exactly — this size.
const BLOCK_SIZE: usize = 4096;

/// Identifies a file as a FerrumDB SSTable. Written into the footer.
const MAGIC: &[u8; 4] = b"FSST";

/// Current SSTable format version.
/// v2 appends the table's min and max key after the sparse index, so a point
/// lookup can skip a table whose key range cannot contain the key.
/// v3 appends a bloom filter after the key range, so a lookup can skip a table
/// that is in range but definitely does not contain the key.
const VERSION: u32 = 3;

/// Fixed footer size: index_offset(8) + index_len(8) + entry_count(4) + magic(4) + version(4).
const FOOTER_LEN: usize = 28;

/// Record type tag inside a data block.
const TYPE_VALUE: u8 = 0;
const TYPE_TOMBSTONE: u8 = 1;

/// What a key resolves to: a present value, or a deletion marker.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Value(Value),
    Tombstone,
}

/// One entry in the sparse index — describes a single data block.
#[derive(Debug, Clone)]
struct IndexEntry {
    first_key: String,
    block_offset: u64,
    block_len: u32,
}

/// A single immutable on-disk SSTable, with its sparse index held in RAM.
#[derive(Debug)]
pub struct SsTable {
    path: PathBuf,
    index: Vec<IndexEntry>,
    // Smallest and largest key in the table. `None` only for an empty table.
    // Used to skip the table entirely when a lookup key falls outside its range.
    min_key: Option<String>,
    max_key: Option<String>,
    // Membership filter over every key in the table. `None` only for an empty
    // table. Lets a lookup skip a table that is in range but lacks the key.
    bloom: Option<BloomFilter>,
}

/// A bloom filter: a bit array probed by `k` hash functions. A negative answer is
/// definitive (the key is absent); a positive answer is probabilistic (it may be a
/// false positive). Built from all of a table's keys at flush time.
#[derive(Debug)]
struct BloomFilter {
    bits: Vec<u8>,
    k: u32,
}

impl BloomFilter {
    /// Sizes the filter for `num_keys` at ~10 bits/key with 7 hash functions,
    /// which targets a ~1% false-positive rate.
    fn new(num_keys: usize) -> Self {
        const BITS_PER_KEY: usize = 10;
        let m_bits = (num_keys * BITS_PER_KEY).max(64);
        BloomFilter { bits: vec![0u8; m_bits.div_ceil(8)], k: 7 }
    }

    /// Bit positions for a key, via double hashing (Kirsch–Mitzenmacher): one
    /// 64-bit hash split into two, combined as h1 + i*h2.
    fn positions(&self, key: &str) -> impl Iterator<Item = usize> + '_ {
        let m = (self.bits.len() * 8) as u64;
        let h = hash64(key.as_bytes());
        let h1 = h & 0xFFFF_FFFF;
        let h2 = (h >> 32) | 1; // force odd so it never collapses to one position
        (0..self.k as u64).map(move |i| (h1.wrapping_add(i.wrapping_mul(h2)) % m) as usize)
    }

    fn add(&mut self, key: &str) {
        for bit in self.positions(key).collect::<Vec<_>>() {
            self.bits[bit / 8] |= 1 << (bit % 8);
        }
    }

    fn maybe_contains(&self, key: &str) -> bool {
        self.positions(key).all(|bit| self.bits[bit / 8] & (1 << (bit % 8)) != 0)
    }
}

/// FNV-1a 64-bit hash. Inlined to avoid a crate dependency, like the block CRC.
fn hash64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

impl SsTable {
    /// Writes a sorted sequence of entries to a new SSTable file and returns the
    /// opened table. The iterator must yield keys in ascending order (a BTreeMap
    /// iterator satisfies this).
    pub fn flush<'a, I>(path: impl AsRef<Path>, entries: I) -> Result<SsTable, AppError>
    where
        I: Iterator<Item = (&'a String, &'a Entry)>,
    {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| AppError::IoError(e.to_string()))?;
        }

        let mut file: Vec<u8> = Vec::new();
        let mut index: Vec<IndexEntry> = Vec::new();

        let mut block: Vec<u8> = Vec::new();
        let mut block_first_key: Option<String> = None;
        let mut block_offset: u64 = 0;

        // Track the table-wide key range. Keys arrive in ascending order, so the
        // first key seen is the min and the last is the max.
        let mut min_key: Option<String> = None;
        let mut max_key: Option<String> = None;
        // Every key, to build the bloom filter once the count is known. Holds
        // borrows into the caller's map; bounded by the memtable size.
        let mut keys: Vec<&str> = Vec::new();

        for (key, entry) in entries {
            if min_key.is_none() {
                min_key = Some(key.clone());
            }
            max_key = Some(key.clone());
            keys.push(key);

            if block.is_empty() {
                block_first_key = Some(key.clone());
                block_offset = file.len() as u64;
            }
            encode_record(&mut block, key, entry);

            if block.len() >= BLOCK_SIZE {
                finalize_block(&mut file, &mut block, &mut index, block_first_key.take(), block_offset);
            }
        }
        // Flush the trailing partial block.
        if !block.is_empty() {
            finalize_block(&mut file, &mut block, &mut index, block_first_key.take(), block_offset);
        }

        // Index region: the sparse index entries, then the table key range.
        let index_offset = file.len() as u64;
        for e in &index {
            write_u32(&mut file, e.first_key.len() as u32);
            file.extend_from_slice(e.first_key.as_bytes());
            file.extend_from_slice(&e.block_offset.to_be_bytes());
            write_u32(&mut file, e.block_len);
        }
        // Key range, only present for a non-empty table (min and max are Some together).
        if let (Some(min), Some(max)) = (&min_key, &max_key) {
            write_u32(&mut file, min.len() as u32);
            file.extend_from_slice(min.as_bytes());
            write_u32(&mut file, max.len() as u32);
            file.extend_from_slice(max.as_bytes());
        }
        // Bloom filter, also only for a non-empty table. Written after the key
        // range as: k (u32), bit-length (u32), bit bytes.
        let bloom = if keys.is_empty() {
            None
        } else {
            let mut b = BloomFilter::new(keys.len());
            for k in &keys {
                b.add(k);
            }
            write_u32(&mut file, b.k);
            write_u32(&mut file, b.bits.len() as u32);
            file.extend_from_slice(&b.bits);
            Some(b)
        };
        let index_len = file.len() as u64 - index_offset;

        // Footer.
        file.extend_from_slice(&index_offset.to_be_bytes());
        file.extend_from_slice(&index_len.to_be_bytes());
        write_u32(&mut file, index.len() as u32);
        file.extend_from_slice(MAGIC);
        file.extend_from_slice(&VERSION.to_be_bytes());

        // Write once, then fsync for durability.
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .map_err(|e| AppError::IoError(e.to_string()))?;
        f.write_all(&file).map_err(|e| AppError::IoError(e.to_string()))?;
        f.sync_all().map_err(|e| AppError::IoError(e.to_string()))?;

        Ok(SsTable { path: path.to_path_buf(), index, min_key, max_key, bloom })
    }

    /// Opens an existing SSTable: reads the footer, validates it, and loads the
    /// sparse index into RAM. The data blocks stay on disk.
    pub fn open(path: impl AsRef<Path>) -> Result<SsTable, AppError> {
        let path = path.as_ref();
        let mut f = File::open(path).map_err(|e| AppError::IoError(e.to_string()))?;
        let file_len = f.metadata().map_err(|e| AppError::IoError(e.to_string()))?.len();

        if file_len < FOOTER_LEN as u64 {
            return Err(AppError::DecodeError("SSTable smaller than footer".to_string()));
        }

        // Read the footer (last 28 bytes).
        f.seek(SeekFrom::Start(file_len - FOOTER_LEN as u64))
            .map_err(|e| AppError::IoError(e.to_string()))?;
        let mut footer = [0u8; FOOTER_LEN];
        f.read_exact(&mut footer).map_err(|e| AppError::IoError(e.to_string()))?;

        let index_offset = u64::from_be_bytes(footer[0..8].try_into().unwrap());
        let index_len = u64::from_be_bytes(footer[8..16].try_into().unwrap());
        let entry_count = u32::from_be_bytes(footer[16..20].try_into().unwrap()) as usize;
        let magic = &footer[20..24];
        let version = u32::from_be_bytes(footer[24..28].try_into().unwrap());

        if magic != MAGIC {
            return Err(AppError::DecodeError("not a FerrumDB SSTable (bad magic)".to_string()));
        }
        if version != VERSION {
            return Err(AppError::DecodeError(format!("unsupported SSTable version {}", version)));
        }

        // Read and parse the index region.
        f.seek(SeekFrom::Start(index_offset)).map_err(|e| AppError::IoError(e.to_string()))?;
        let mut buf = vec![0u8; index_len as usize];
        f.read_exact(&mut buf).map_err(|e| AppError::IoError(e.to_string()))?;

        let mut index = Vec::with_capacity(entry_count);
        let mut c = 0usize;
        for _ in 0..entry_count {
            let key_len = read_u32(&buf, &mut c)? as usize;
            let first_key = read_string(&buf, &mut c, key_len)?;
            let block_offset = read_u64(&buf, &mut c)?;
            let block_len = read_u32(&buf, &mut c)?;
            index.push(IndexEntry { first_key, block_offset, block_len });
        }

        // Key range and bloom filter follow the index entries (both absent for an
        // empty table; both present together otherwise).
        let (min_key, max_key, bloom) = if c < buf.len() {
            let min_len = read_u32(&buf, &mut c)? as usize;
            let min = read_string(&buf, &mut c, min_len)?;
            let max_len = read_u32(&buf, &mut c)? as usize;
            let max = read_string(&buf, &mut c, max_len)?;

            let k = read_u32(&buf, &mut c)?;
            let bits_len = read_u32(&buf, &mut c)? as usize;
            let bits = read_bytes(&buf, &mut c, bits_len)?.to_vec();

            (Some(min), Some(max), Some(BloomFilter { bits, k }))
        } else {
            (None, None, None)
        };

        Ok(SsTable { path: path.to_path_buf(), index, min_key, max_key, bloom })
    }

    /// Looks up a key. Returns `None` if the key is not present in this table.
    /// A tombstone is returned as `Some(Entry::Tombstone)` — the caller decides
    /// whether that shadows older tables.
    pub fn get(&self, key: &str) -> Result<Option<Entry>, AppError> {
        if self.index.is_empty() {
            return Ok(None);
        }

        // Skip the whole table if the key falls outside its range — no disk read.
        if let (Some(min), Some(max)) = (&self.min_key, &self.max_key)
            && (key < min.as_str() || key > max.as_str())
        {
            return Ok(None);
        }

        // Skip if the bloom filter says the key is definitely absent — no disk read.
        if let Some(bloom) = &self.bloom
            && !bloom.maybe_contains(key)
        {
            return Ok(None);
        }

        // Find the block whose first_key is the largest <= key.
        let pos = match self.index.binary_search_by(|e| e.first_key.as_str().cmp(key)) {
            Ok(i) => i,
            Err(0) => return Ok(None), // key precedes the first block
            Err(i) => i - 1,
        };
        let index_entry = &self.index[pos];

        // Read that one block and scan it for an exact key match. Records are
        // sorted, so we can stop once a record key passes the target.
        let mut f = File::open(&self.path).map_err(|e| AppError::IoError(e.to_string()))?;
        let records = self.read_block(&mut f, index_entry)?;
        let mut c = 0usize;
        while c < records.len() {
            let (rec_key, rec_entry) = decode_record(&records, &mut c)?;
            if rec_key == key {
                return Ok(Some(rec_entry));
            }
            if rec_key.as_str() > key {
                break;
            }
        }

        Ok(None)
    }

    /// Reads every entry in the table, in ascending key order. Used by compaction
    /// to merge tables. Loads one block at a time, but returns all entries in
    /// memory — callers should expect O(table size) allocation.
    pub fn read_all_entries(&self) -> Result<Vec<(String, Entry)>, AppError> {
        let mut out = Vec::new();
        if self.index.is_empty() {
            return Ok(out);
        }

        let mut f = File::open(&self.path).map_err(|e| AppError::IoError(e.to_string()))?;
        for index_entry in &self.index {
            let records = self.read_block(&mut f, index_entry)?;
            let mut c = 0usize;
            while c < records.len() {
                out.push(decode_record(&records, &mut c)?);
            }
        }
        Ok(out)
    }

    /// Reads one data block, verifies its CRC, and returns just the record bytes
    /// (the trailing 4-byte CRC stripped off).
    fn read_block(&self, f: &mut File, index_entry: &IndexEntry) -> Result<Vec<u8>, AppError> {
        f.seek(SeekFrom::Start(index_entry.block_offset))
            .map_err(|e| AppError::IoError(e.to_string()))?;
        let mut block = vec![0u8; index_entry.block_len as usize];
        f.read_exact(&mut block).map_err(|e| AppError::IoError(e.to_string()))?;

        let split = block.len() - 4;
        let stored_crc = u32::from_be_bytes(block[split..].try_into().unwrap());
        if crc32(&block[..split]) != stored_crc {
            return Err(AppError::DecodeError(format!(
                "SSTable block CRC mismatch at offset {}",
                index_entry.block_offset
            )));
        }
        block.truncate(split);
        Ok(block)
    }

    /// Number of data blocks (= sparse index entries). Useful for tests.
    pub fn block_count(&self) -> usize {
        self.index.len()
    }

    /// The table's `(min_key, max_key)` range, or `None` if the table is empty.
    pub fn key_range(&self) -> Option<(&str, &str)> {
        match (&self.min_key, &self.max_key) {
            (Some(min), Some(max)) => Some((min.as_str(), max.as_str())),
            _ => None,
        }
    }

    /// Bloom-filter membership test: `false` means the key is definitely not in
    /// this table; `true` means it is probably present (with a small false-positive
    /// rate). An empty table returns `false` for every key.
    pub fn might_contain(&self, key: &str) -> bool {
        match &self.bloom {
            Some(bloom) => bloom.maybe_contains(key),
            None => false,
        }
    }

    /// Path to the underlying file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// --- record / block encoding helpers ---

fn encode_record(out: &mut Vec<u8>, key: &str, entry: &Entry) {
    write_u32(out, key.len() as u32);
    out.extend_from_slice(key.as_bytes());
    match entry {
        Entry::Value(v) => {
            out.push(TYPE_VALUE);
            let bytes = v.to_proto().encode_to_vec();
            write_u32(out, bytes.len() as u32);
            out.extend_from_slice(&bytes);
        }
        Entry::Tombstone => {
            out.push(TYPE_TOMBSTONE);
            write_u32(out, 0);
        }
    }
}

/// Decodes one record at `*c`, advancing the cursor past it.
fn decode_record(buf: &[u8], c: &mut usize) -> Result<(String, Entry), AppError> {
    let key_len = read_u32(buf, c)? as usize;
    let key = read_string(buf, c, key_len)?;
    let rec_type = read_u8(buf, c)?;
    let val_len = read_u32(buf, c)? as usize;

    let entry = if rec_type == TYPE_VALUE {
        let val_bytes = read_bytes(buf, c, val_len)?;
        let msg = ValueMessage::decode(val_bytes).map_err(|e| AppError::DecodeError(e.to_string()))?;
        Entry::Value(Value::from_proto(&msg)?)
    } else {
        Entry::Tombstone
    };
    Ok((key, entry))
}

fn finalize_block(
    file: &mut Vec<u8>,
    block: &mut Vec<u8>,
    index: &mut Vec<IndexEntry>,
    first_key: Option<String>,
    offset: u64,
) {
    let crc = crc32(block);
    let start = file.len();
    file.extend_from_slice(block);
    file.extend_from_slice(&crc.to_be_bytes());
    let block_len = (file.len() - start) as u32;
    index.push(IndexEntry {
        first_key: first_key.unwrap_or_default(),
        block_offset: offset,
        block_len,
    });
    block.clear();
}

// --- primitive readers/writers (big-endian, matching the WAL) ---

fn write_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn read_u8(buf: &[u8], c: &mut usize) -> Result<u8, AppError> {
    if *c + 1 > buf.len() {
        return Err(AppError::DecodeError("unexpected end of SSTable record".to_string()));
    }
    let v = buf[*c];
    *c += 1;
    Ok(v)
}

fn read_u32(buf: &[u8], c: &mut usize) -> Result<u32, AppError> {
    if *c + 4 > buf.len() {
        return Err(AppError::DecodeError("unexpected end of SSTable record".to_string()));
    }
    let v = u32::from_be_bytes(buf[*c..*c + 4].try_into().unwrap());
    *c += 4;
    Ok(v)
}

fn read_u64(buf: &[u8], c: &mut usize) -> Result<u64, AppError> {
    if *c + 8 > buf.len() {
        return Err(AppError::DecodeError("unexpected end of SSTable record".to_string()));
    }
    let v = u64::from_be_bytes(buf[*c..*c + 8].try_into().unwrap());
    *c += 8;
    Ok(v)
}

fn read_bytes<'a>(buf: &'a [u8], c: &mut usize, len: usize) -> Result<&'a [u8], AppError> {
    if *c + len > buf.len() {
        return Err(AppError::DecodeError("unexpected end of SSTable record".to_string()));
    }
    let s = &buf[*c..*c + len];
    *c += len;
    Ok(s)
}

fn read_string(buf: &[u8], c: &mut usize, len: usize) -> Result<String, AppError> {
    let bytes = read_bytes(buf, c, len)?;
    String::from_utf8(bytes.to_vec()).map_err(|e| AppError::DecodeError(e.to_string()))
}

// --- CRC32 (IEEE 802.3, polynomial 0xEDB88320) ---
// Inlined to avoid a crate dependency, per the project's minimal-deps discipline.

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let lsb = crc & 1;
            crc >>= 1;
            if lsb == 1 {
                crc ^= 0xEDB8_8320;
            }
        }
    }
    !crc
}
