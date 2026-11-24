//! Encrypted binlog cache database compatible with tdesktop's `lib_storage`.
//!
//! Port of `Telegram/lib_storage/storage/cache/storage_cache_types.h`, `.cpp`,
//! `storage_cache_binlog_reader.cpp`, and `storage_cache_database_object.cpp`.
//!
//! On-disk layout under `<base>`:
//! - `<base>/version`: 4-byte little-endian `i32` version directory number (e.g. `0`)
//! - `<base>/<version>/binlog`: `TDEF` encrypted append-only log of 16-byte-aligned records
//!   (`BasicHeader`, `Store` / `StoreWithTime`, `MultiStore`, `MultiRemove`, `MultiAccess`)
//! - `<base>/<version>/<XX>/<XXXXXXXXXXXX>`: `TDEF` encrypted entry payload files, verified
//!   by `XXH32(payload, seed=0)` recorded in the binlog.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tg_crypto::{StorageEncryptionKey, random_bytes};
use xxhash_rust::xxh32::xxh32;

use crate::encrypted_file::{EncryptedFile, OpenMode};
use crate::error::{Result, StorageError};

/// Maximum entry payload size supported by 24-bit `EntrySize` (`(1 << 24) - 1` = ~16 MiB).
pub const CACHE_DATA_SIZE_LIMIT: usize = (1 << 24) - 1;

/// `BasicHeader::kTrackEstimatedTime` bit flag.
pub const FLAG_TRACK_ESTIMATED_TIME: u32 = 0x01;

const RECORD_TYPE_STORE: u8 = 0x01;
const RECORD_TYPE_MULTI_STORE: u8 = 0x02;
const RECORD_TYPE_MULTI_REMOVE: u8 = 0x03;
const RECORD_TYPE_MULTI_ACCESS: u8 = 0x04;

/// 128-bit cache entry key (`Storage::Cache::Key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct CacheKey {
    /// Upper 64 bits.
    pub high: u64,
    /// Lower 64 bits.
    pub low: u64,
}

impl CacheKey {
    /// Construct a `CacheKey`.
    #[must_use]
    pub const fn new(high: u64, low: u64) -> Self {
        Self { high, low }
    }

    /// Whether the key is non-zero.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.high != 0 || self.low != 0
    }

    fn encode(self, dst: &mut [u8]) {
        dst[0..8].copy_from_slice(&self.high.to_le_bytes());
        dst[8..16].copy_from_slice(&self.low.to_le_bytes());
    }

    fn decode(src: &[u8]) -> Self {
        Self {
            high: u64::from_le_bytes([
                src[0], src[1], src[2], src[3], src[4], src[5], src[6], src[7],
            ]),
            low: u64::from_le_bytes([
                src[8], src[9], src[10], src[11], src[12], src[13], src[14], src[15],
            ]),
        }
    }
}

/// 7-byte random place identifier mapping an entry to `<XX>/<XXXXXXXXXXXX>` (`details::PlaceId`).
pub type PlaceId = [u8; 7];

/// Format a 7-byte `PlaceId` into its relative path `"XX/XXXXXXXXXXXX"` (`PlaceFromId`).
#[must_use]
pub fn place_from_id(place: PlaceId) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(15);
    for (i, &byte) in place.iter().enumerate() {
        out.push(HEX[(byte & 0x0F) as usize] as char);
        out.push(HEX[(byte >> 4) as usize] as char);
        if i == 0 {
            out.push('/');
        }
    }
    out
}

/// Compute `XXH32(data, seed = 0)` matching `CountChecksum` in `storage_cache_database_object.cpp`.
#[must_use]
pub fn count_checksum(data: &[u8]) -> u32 {
    xxh32(data, 0)
}

/// Monotonic + wall-clock timestamp recorded in binlog records (`EstimatedTimePoint`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EstimatedTimePoint {
    /// Logical monotonic relative timestamp in seconds.
    pub relative: u64,
    /// System unix timestamp in seconds.
    pub system: u32,
}

impl EstimatedTimePoint {
    fn encode(self, dst: &mut [u8]) {
        let rel1 = (self.relative & 0xFFFF_FFFF) as u32;
        let rel2 = ((self.relative >> 32) & 0xFFFF_FFFF) as u32;
        dst[0..4].copy_from_slice(&rel1.to_le_bytes());
        dst[4..8].copy_from_slice(&rel2.to_le_bytes());
        dst[8..12].copy_from_slice(&self.system.to_le_bytes());
    }

    fn decode(src: &[u8]) -> Self {
        let rel1 = u32::from_le_bytes([src[0], src[1], src[2], src[3]]);
        let rel2 = u32::from_le_bytes([src[4], src[5], src[6], src[7]]);
        let system = u32::from_le_bytes([src[8], src[9], src[10], src[11]]);
        Self {
            relative: u64::from(rel1) | (u64::from(rel2) << 32),
            system,
        }
    }
}

/// In-memory index metadata for a stored cache entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheEntryMeta {
    /// On-disk encrypted file place ID.
    pub place: PlaceId,
    /// User-defined category tag (e.g., photo, video, audio, sticker).
    pub tag: u8,
    /// `XXH32` checksum of the plaintext entry bytes.
    pub checksum: u32,
    /// Plaintext payload size in bytes.
    pub size: usize,
    /// Estimated last-access relative timestamp.
    pub use_time: u64,
}

/// Aggregate cache statistics (`Storage::Cache::Stats`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BinlogStats {
    /// Total number of live entries.
    pub count: usize,
    /// Total plaintext byte size of live entries.
    pub total_size: u64,
    /// Per-tag `(count, total_size)` breakdown.
    pub by_tag: HashMap<u8, (usize, u64)>,
}

/// Encrypted binlog cache database compatible with tdesktop `Storage::Cache::Database`.
pub struct BinlogCache {
    base_path: PathBuf,
    version_dir: PathBuf,
    key: StorageEncryptionKey,
    track_estimated_time: bool,
    time: EstimatedTimePoint,
    binlog: EncryptedFile,
    entries: HashMap<CacheKey, CacheEntryMeta>,
}

impl BinlogCache {
    /// Open or create a binlog cache database at `base_path` encrypted with `key`.
    pub fn open(
        base_path: impl Into<PathBuf>,
        key: StorageEncryptionKey,
        track_estimated_time: bool,
    ) -> Result<Self> {
        let base_path = base_path.into();
        fs::create_dir_all(&base_path)?;

        let version = read_version_value(&base_path).unwrap_or(0);
        if read_version_value(&base_path).is_none() {
            write_version_value(&base_path, version)?;
        }

        let version_dir = base_path.join(version.to_string());
        fs::create_dir_all(&version_dir)?;

        let binlog_path = version_dir.join("binlog");
        let ready_path = version_dir.join("binlog-ready");
        if ready_path.exists() {
            let _ = fs::remove_file(&binlog_path);
            fs::rename(&ready_path, &binlog_path)?;
        }

        let mut binlog = EncryptedFile::open(&binlog_path, OpenMode::ReadAppend, &key)?;
        let time = if binlog.size() == 0 {
            let now = unix_now_secs();
            let mut header = [0u8; 16];
            let flags = if track_estimated_time {
                FLAG_TRACK_ESTIMATED_TIME
            } else {
                0
            };
            let format_and_flags = flags << 8; // format = 0
            header[0..4].copy_from_slice(&format_and_flags.to_le_bytes());
            header[4..8].copy_from_slice(&now.to_le_bytes());
            binlog.write_blocks(&header)?;
            binlog.flush()?;
            EstimatedTimePoint {
                relative: u64::from(now),
                system: now,
            }
        } else {
            let mut header = [0u8; 16];
            if binlog.read_blocks(&mut header)? != 16 {
                return Err(StorageError::UnexpectedEof);
            }
            let format_and_flags = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
            let format = format_and_flags & 0xFF;
            let flags = format_and_flags >> 8;
            if format != 0 {
                return Err(StorageError::UnsupportedVersion(format as i32));
            }
            let file_tracks_time = (flags & FLAG_TRACK_ESTIMATED_TIME) != 0;
            if file_tracks_time != track_estimated_time {
                return Err(StorageError::CorruptData(
                    "binlog trackEstimatedTime mismatch".into(),
                ));
            }
            let sys_time = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
            EstimatedTimePoint {
                relative: u64::from(sys_time),
                system: sys_time,
            }
        };

        let mut cache = Self {
            base_path,
            version_dir,
            key,
            track_estimated_time,
            time,
            binlog,
            entries: HashMap::new(),
        };
        cache.replay_binlog()?;
        Ok(cache)
    }

    /// Base directory of this binlog cache.
    #[must_use]
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// Look up entry metadata without reading its payload from disk.
    #[must_use]
    pub fn meta(&self, key: CacheKey) -> Option<&CacheEntryMeta> {
        self.entries.get(&key)
    }

    /// Return aggregate statistics across all live entries.
    #[must_use]
    pub fn stats(&self) -> BinlogStats {
        let mut stats = BinlogStats::default();
        for entry in self.entries.values() {
            stats.count += 1;
            stats.total_size += entry.size as u64;
            if entry.tag != 0 {
                let slot = stats.by_tag.entry(entry.tag).or_insert((0, 0));
                slot.0 += 1;
                slot.1 += entry.size as u64;
            }
        }
        stats
    }

    /// Store `data` under `key` with category `tag`, writing an encrypted entry file and
    /// appending a `Store` / `StoreWithTime` record to the binlog.
    pub fn put(&mut self, key: CacheKey, data: &[u8], tag: u8) -> Result<()> {
        if !key.is_valid() || data.is_empty() || data.len() > CACHE_DATA_SIZE_LIMIT {
            return Err(StorageError::CorruptData(
                "invalid cache key or payload size".into(),
            ));
        }

        let mut place = [0u8; 7];
        random_bytes(&mut place);
        let rel_path = place_from_id(place);
        let full_path = self.version_dir.join(&rel_path);

        {
            let mut entry_file = EncryptedFile::open(&full_path, OpenMode::Write, &self.key)?;
            entry_file.write_with_padding(data)?;
            entry_file.flush()?;
        }

        let checksum = count_checksum(data);
        let tp = self.current_time_point();

        if self.track_estimated_time {
            let mut record = [0u8; 48];
            encode_store_record(&mut record[..32], tag, data.len(), place, checksum, key);
            tp.encode(&mut record[32..44]);
            self.binlog.write_blocks(&record)?;
        } else {
            let mut record = [0u8; 32];
            encode_store_record(&mut record, tag, data.len(), place, checksum, key);
            self.binlog.write_blocks(&record)?;
        }
        self.binlog.flush()?;

        // Remove old file if overwriting
        if let Some(old) = self.entries.insert(
            key,
            CacheEntryMeta {
                place,
                tag,
                checksum,
                size: data.len(),
                use_time: tp.relative,
            },
        ) {
            let _ = fs::remove_file(self.version_dir.join(place_from_id(old.place)));
        }
        self.time = tp;
        Ok(())
    }

    /// Read and checksum-verify the cached bytes for `key`.
    pub fn get(&self, key: CacheKey) -> Result<Option<Vec<u8>>> {
        let Some(meta) = self.entries.get(&key) else {
            return Ok(None);
        };
        let path = self.version_dir.join(place_from_id(meta.place));
        let mut entry_file = match EncryptedFile::open(&path, OpenMode::Read, &self.key) {
            Ok(f) => f,
            Err(StorageError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let mut buf = vec![0u8; meta.size];
        let n = entry_file.read_with_padding(&mut buf)?;
        if n != meta.size {
            return Err(StorageError::UnexpectedEof);
        }
        if count_checksum(&buf) != meta.checksum {
            return Err(StorageError::ChecksumMismatch);
        }
        Ok(Some(buf))
    }

    /// Remove `key` by appending a `MultiRemove(1)` record and deleting the entry file.
    pub fn remove(&mut self, key: CacheKey) -> Result<bool> {
        let Some(old) = self.entries.remove(&key) else {
            return Ok(false);
        };
        let mut buf = [0u8; 32];
        buf[0] = RECORD_TYPE_MULTI_REMOVE;
        encode_u24_le(1, &mut buf[1..4]);
        key.encode(&mut buf[16..32]);
        self.binlog.write_blocks(&buf)?;
        self.binlog.flush()?;
        let _ = fs::remove_file(self.version_dir.join(place_from_id(old.place)));
        Ok(true)
    }

    fn current_time_point(&self) -> EstimatedTimePoint {
        let now = unix_now_secs();
        let delta = i64::from(now)
            .saturating_sub(i64::from(self.time.system))
            .max(0) as u64;
        EstimatedTimePoint {
            relative: self.time.relative.saturating_add(delta),
            system: now,
        }
    }

    fn replay_binlog(&mut self) -> Result<()> {
        let remaining = self.binlog.size().saturating_sub(self.binlog.offset()) as usize;
        if remaining == 0 {
            return Ok(());
        }
        let mut data = vec![0u8; remaining];
        let n = self.binlog.read_blocks(&mut data)?;
        data.truncate(n);

        let mut pos = 0usize;
        let store_size = if self.track_estimated_time { 48 } else { 32 };

        while pos < data.len() {
            let rec_type = data[pos];
            match rec_type {
                RECORD_TYPE_STORE => {
                    if pos + store_size > data.len() {
                        break;
                    }
                    self.apply_store_slice(&data[pos..pos + store_size]);
                    pos += store_size;
                }
                RECORD_TYPE_MULTI_STORE => {
                    if pos + 16 > data.len() {
                        break;
                    }
                    let count = decode_u24_le(&data[pos + 1..pos + 4]);
                    let needed = 16 + count * store_size;
                    if count == 0 || pos + needed > data.len() {
                        break;
                    }
                    for i in 0..count {
                        let start = pos + 16 + i * store_size;
                        self.apply_store_slice(&data[start..start + store_size]);
                    }
                    pos += needed;
                }
                RECORD_TYPE_MULTI_REMOVE => {
                    if pos + 16 > data.len() {
                        break;
                    }
                    let count = decode_u24_le(&data[pos + 1..pos + 4]);
                    let needed = 16 + count * 16;
                    if count == 0 || pos + needed > data.len() {
                        break;
                    }
                    for i in 0..count {
                        let start = pos + 16 + i * 16;
                        let key = CacheKey::decode(&data[start..start + 16]);
                        self.entries.remove(&key);
                    }
                    pos += needed;
                }
                RECORD_TYPE_MULTI_ACCESS if self.track_estimated_time => {
                    if pos + 16 > data.len() {
                        break;
                    }
                    let count = decode_u24_le(&data[pos + 1..pos + 4]);
                    let tp = EstimatedTimePoint::decode(&data[pos + 4..pos + 16]);
                    let needed = 16 + count * 16;
                    if pos + needed > data.len() {
                        break;
                    }
                    if tp.relative > self.time.relative {
                        self.time = tp;
                    }
                    for i in 0..count {
                        let start = pos + 16 + i * 16;
                        let key = CacheKey::decode(&data[start..start + 16]);
                        if let Some(entry) = self.entries.get_mut(&key) {
                            entry.use_time = tp.relative;
                        }
                    }
                    pos += needed;
                }
                _ => break,
            }
        }
        Ok(())
    }

    fn apply_store_slice(&mut self, slice: &[u8]) {
        let tag = slice[1];
        let size = decode_u24_le(&slice[2..5]);
        let mut place = [0u8; 7];
        place.copy_from_slice(&slice[5..12]);
        let checksum = u32::from_le_bytes([slice[12], slice[13], slice[14], slice[15]]);
        let key = CacheKey::decode(&slice[16..32]);
        let use_time = if self.track_estimated_time {
            let tp = EstimatedTimePoint::decode(&slice[32..44]);
            if tp.relative > self.time.relative {
                self.time = tp;
            }
            tp.relative
        } else {
            self.time.relative
        };
        if size > 0 && size <= CACHE_DATA_SIZE_LIMIT {
            self.entries.insert(
                key,
                CacheEntryMeta {
                    place,
                    tag,
                    checksum,
                    size,
                    use_time,
                },
            );
        }
    }
}

fn encode_store_record(
    dst: &mut [u8],
    tag: u8,
    size: usize,
    place: PlaceId,
    checksum: u32,
    key: CacheKey,
) {
    dst[0] = RECORD_TYPE_STORE;
    dst[1] = tag;
    encode_u24_le(size, &mut dst[2..5]);
    dst[5..12].copy_from_slice(&place);
    dst[12..16].copy_from_slice(&checksum.to_le_bytes());
    key.encode(&mut dst[16..32]);
}

fn encode_u24_le(val: usize, dst: &mut [u8]) {
    dst[0] = (val & 0xFF) as u8;
    dst[1] = ((val >> 8) & 0xFF) as u8;
    dst[2] = ((val >> 16) & 0xFF) as u8;
}

fn decode_u24_le(src: &[u8]) -> usize {
    usize::from(src[0]) | (usize::from(src[1]) << 8) | (usize::from(src[2]) << 16)
}

fn read_version_value(base: &Path) -> Option<i32> {
    let bytes = fs::read(base.join("version")).ok()?;
    if bytes.len() < 4 {
        return None;
    }
    Some(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn write_version_value(base: &Path, version: i32) -> Result<()> {
    fs::write(base.join("version"), version.to_le_bytes())?;
    Ok(())
}

fn unix_now_secs() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().max(1) as u32)
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn place_from_id_matches_tdesktop_nibble_order() {
        let place: PlaceId = [0xA1, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6, 0x07];
        assert_eq!(place_from_id(place), "1A/2B3C4D5E6F70");
    }

    #[test]
    fn binlog_cache_put_get_reopen_remove() {
        let dir = TempDir::new().expect("tempdir");
        let key = StorageEncryptionKey::new(vec![0x33u8; 256]).expect("key");
        let k1 = CacheKey::new(0x0100, 111_222);
        let k2 = CacheKey::new(0x0200, 333_444);

        {
            let mut db = BinlogCache::open(dir.path(), key.clone(), true).expect("open");
            db.put(k1, b"first cached thumbnail", 1).expect("put k1");
            db.put(k2, b"second cached sticker data!", 2)
                .expect("put k2");
            assert_eq!(db.stats().count, 2);
            assert_eq!(
                db.get(k1).expect("get k1").as_deref(),
                Some(&b"first cached thumbnail"[..])
            );
        }

        // Reopen from disk and replay binlog
        {
            let mut db = BinlogCache::open(dir.path(), key.clone(), true).expect("reopen");
            assert_eq!(db.stats().count, 2);
            assert_eq!(
                db.get(k1).expect("get k1").as_deref(),
                Some(&b"first cached thumbnail"[..])
            );
            assert_eq!(
                db.get(k2).expect("get k2").as_deref(),
                Some(&b"second cached sticker data!"[..])
            );
            assert!(db.remove(k1).expect("remove k1"));
            assert_eq!(db.get(k1).expect("get removed"), None);
            assert_eq!(db.stats().count, 1);
        }

        // Reopen again after removal
        let db = BinlogCache::open(dir.path(), key, true).expect("reopen 2");
        assert_eq!(db.stats().count, 1);
        assert_eq!(db.get(k1).expect("still gone"), None);
        assert!(db.get(k2).expect("still there").is_some());
    }
}
