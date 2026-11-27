//! Rustgram-side encrypted SQLite media cache index and downloaded file location registry.
//!
//! Unlike `tdata` files (which stay strictly byte-compatible with tdesktop), Rustgram's
//! own media cache index uses bundled SQLite (`rusqlite`) for O(1) lookups, atomic LRU
//! eviction, and tracking both cached media blobs and user-downloaded files on disk.
//!
//! Security model (aligned with tdesktop's `lib_storage/storage/cache/` and `storage_account.cpp`):
//! - Every cached blob on disk is encrypted using tdesktop's `TDEF` AES-256-CTR [`EncryptedFile`]
//!   format with a per-account [`StorageEncryptionKey`] derived from the `tdata` local key, and
//!   authenticated with an inner HMAC-SHA-256 under a key derived from the cache key. The MAC
//!   also covers the entry the blob belongs to, so a blob cannot be altered or swapped for
//!   another entry's blob by someone who can write the cache folder but does not have the key.
//! - Cache lookup keys in SQLite are keyed SHA-256 digests, and all relative/absolute paths stored
//!   in `media_cache` and `file_locations` are encrypted with random per-row salts.
//! - Legacy unencrypted caches (`PRAGMA user_version < 2` or non-`TDEF` blobs) are purged on open.
//! - Before an account directory is attached, [`MediaCacheIndex::open_memory_only`] keeps all data
//!   strictly in RAM without touching `%TEMP%`.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use tg_crypto::{
    AES_BLOCK_SIZE, AuthKey, STORAGE_CTR_SALT_SIZE, StorageEncryptionKey, const_time_eq,
    hmac_sha256_parts, random_bytes, sha256,
};

use crate::encrypted_file::{EncryptedFile, OpenMode, TDEF_MAGIC};
use crate::error::{Result, StorageError};

/// SQLite `PRAGMA user_version` for the encrypted media cache schema (v2).
const MEDIA_CACHE_SCHEMA_VERSION: i32 = 2;

/// Prefix for encrypted string columns stored in SQLite (`enc1:<salt>:<mac>:<ciphertext>`).
const ENC_STRING_PREFIX: &str = "enc1:";

/// Header length inside each `TDEF` blob payload: `u64 LE plaintext_len` + `32-byte HMAC-SHA-256`.
const BLOB_FRAME_HEADER_LEN: usize = 8 + 32;

/// File-name prefix of streamed media files inside `<cache root>/streamed`. The rest of the name
/// is a keyed hash ([`keyed_stream_file_stem`]), so names do not reveal document ids.
pub const STREAM_FILE_PREFIX: &str = "s_";

/// Legacy plaintext sidecar manifest filename purged during migration.
const LEGACY_MANIFEST_FILENAME: &str = "cache_meta.json";

static CACHE_KEY_REGISTRY: OnceLock<Mutex<HashMap<PathBuf, StorageEncryptionKey>>> =
    OnceLock::new();

fn key_registry() -> &'static Mutex<HashMap<PathBuf, StorageEncryptionKey>> {
    CACHE_KEY_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn normalize_registry_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Derives the tdesktop-compatible [`StorageEncryptionKey`] directly from the 256-byte `local_key`
/// (`Storage::Account::cacheKey` in `storage/storage_account.cpp:1879-1887`).
#[must_use]
pub fn cache_key_from_local_key(local_key: &AuthKey) -> StorageEncryptionKey {
    match StorageEncryptionKey::new(local_key.data().to_vec()) {
        Ok(key) => key,
        Err(_) => unreachable!("256-byte AuthKey is always a valid v1 StorageEncryptionKey"),
    }
}

/// Derives a per-account 64-byte (`StorageEncryptionKey::SIZE_V2`) media cache encryption key
/// from the domain's 256-byte `local_key` and the account's `data_name` (e.g., `"data"`,
/// `"data#2"`), domain-separated via SHA-256 so each account in a multi-account `tdata` gets an
/// independent AES-256-CTR cache key while keeping existing `tdata` files untouched.
#[must_use]
pub fn derive_media_cache_key(
    local_key: &AuthKey,
    account_data_name: &str,
) -> StorageEncryptionKey {
    let left = sha256(&[
        &local_key.data()[..128],
        b"rustgram.media_cache.key.v1\0",
        account_data_name.as_bytes(),
    ]);
    let right = sha256(&[
        &local_key.data()[128..],
        b"rustgram.media_cache.key.v1\0",
        account_data_name.as_bytes(),
    ]);
    let mut material = Vec::with_capacity(StorageEncryptionKey::SIZE_V2);
    material.extend_from_slice(&left);
    material.extend_from_slice(&right);
    match StorageEncryptionKey::new(material) {
        Ok(key) => key,
        Err(_) => unreachable!("64-byte derived key is always a valid v2 StorageEncryptionKey"),
    }
}

/// Generates a fresh random 256-byte ephemeral [`StorageEncryptionKey`] (never persisted to disk).
#[must_use]
pub fn random_ephemeral_cache_key() -> StorageEncryptionKey {
    let mut bytes = vec![0u8; StorageEncryptionKey::SIZE_V1];
    random_bytes(&mut bytes);
    match StorageEncryptionKey::new(bytes) {
        Ok(key) => key,
        Err(_) => unreachable!("256-byte random key is always a valid v1 StorageEncryptionKey"),
    }
}

/// Returns the random [`StorageEncryptionKey`] of this process (generated on first use, never
/// written to disk). Cache writers use it for directories that have no registered account key, so
/// nothing is stored in plaintext and files written by one process are unreadable by the next.
#[must_use]
pub fn process_ephemeral_cache_key() -> StorageEncryptionKey {
    static KEY: OnceLock<StorageEncryptionKey> = OnceLock::new();
    KEY.get_or_init(random_ephemeral_cache_key).clone()
}

/// Registers an active cache root directory's encryption key in memory so companion cache writers
/// (such as [`crate::StreamedFileDownloader`] storing `.part` files inside `<cache_root>/streamed`)
/// automatically encrypt files under `root_dir` with the same account key.
pub fn register_cache_key(root_dir: &Path, key: StorageEncryptionKey) {
    let norm = normalize_registry_path(root_dir);
    if let Ok(mut guard) = key_registry().lock() {
        // A directory registered before it existed is stored under the path as given; once it
        // exists it is stored under its canonical path. Drop the first form so a lookup cannot
        // return a key that was replaced.
        if norm != root_dir {
            guard.remove(root_dir);
        }
        guard.insert(norm, key);
    }
}

/// Unregisters any in-memory cache encryption key associated with `root_dir` (e.g. on logout).
pub fn unregister_cache_key(root_dir: &Path) {
    let norm = normalize_registry_path(root_dir);
    if let Ok(mut guard) = key_registry().lock() {
        guard.remove(&norm);
        guard.remove(root_dir);
    }
}

/// Looks up the active [`StorageEncryptionKey`] registered for `path` or any of its ancestor
/// directories, if present.
#[must_use]
pub fn lookup_cache_key_for_path(path: &Path) -> Option<StorageEncryptionKey> {
    let guard = key_registry().lock().ok()?;
    for anc in path.ancestors() {
        if let Some(key) = guard.get(anc) {
            return Some(key.clone());
        }
        if let Ok(canon) = fs::canonicalize(anc)
            && let Some(key) = guard.get(&canon)
        {
            return Some(key.clone());
        }
    }
    None
}

fn resolve_or_create_ephemeral_key(root_dir: &Path) -> StorageEncryptionKey {
    if let Some(existing) = lookup_cache_key_for_path(root_dir) {
        return existing;
    }
    let key = random_ephemeral_cache_key();
    register_cache_key(root_dir, key.clone());
    key
}

/// Category tags for Rustgram media cache entries.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaCacheTag {
    /// Uncategorized blob.
    Other = 0,
    /// Photos and image thumbnails.
    Photo = 1,
    /// Videos and GIFs.
    Video = 2,
    /// Voice notes and round videos.
    Voice = 3,
    /// Audio tracks / music.
    Audio = 4,
    /// Stickers and custom emoji.
    Sticker = 5,
    /// Generic documents.
    Document = 6,
}

impl MediaCacheTag {
    /// Convert a raw tag byte to `MediaCacheTag`.
    #[must_use]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Photo,
            2 => Self::Video,
            3 => Self::Voice,
            4 => Self::Audio,
            5 => Self::Sticker,
            6 => Self::Document,
            _ => Self::Other,
        }
    }
}

/// Metadata row for a cached media object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaCacheEntry {
    /// Stable cache key (e.g., `"doc:<dc>:<id>"`, `"photo:<dc>:<id>:<size>"`).
    pub key: String,
    /// Relative path inside the cache root directory.
    pub rel_path: String,
    /// File size in bytes (plaintext payload size).
    pub size: u64,
    /// Media category tag.
    pub tag: MediaCacheTag,
    /// Last access timestamp in Unix seconds.
    pub last_accessed: i64,
    /// Creation timestamp in Unix seconds.
    pub created_at: i64,
}

/// Registered local filesystem location for a downloaded file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFileRecord {
    /// Media key `(first, second)`.
    pub key_high: u64,
    /// Media key low 64 bits.
    pub key_low: u64,
    /// Absolute path on disk.
    pub path: PathBuf,
    /// File size in bytes when saved.
    pub size: u64,
    /// File modification time in Unix milliseconds.
    pub modified_ms: i64,
}

/// Summary statistics for [`MediaCacheIndex`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MediaCacheStats {
    /// Total number of cached blobs.
    pub entry_count: u64,
    /// Sum of blob sizes in bytes.
    pub total_bytes: u64,
}

/// Per-category byte totals aggregated directly from SQLite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MediaCacheTagStats {
    /// Cached photo and thumbnail bytes.
    pub photos_bytes: u64,
    /// Cached video and GIF bytes.
    pub videos_bytes: u64,
    /// Cached sticker and custom emoji bytes.
    pub stickers_bytes: u64,
    /// Cached documents, voice, audio, and other bytes.
    pub files_bytes: u64,
}

enum CacheBackend {
    Disk {
        root_dir: PathBuf,
    },
    MemoryOnly {
        blobs: Mutex<HashMap<String, Vec<u8>>>,
    },
}

/// Encrypted SQLite-backed media cache and local file location index for Rustgram.
pub struct MediaCacheIndex {
    backend: CacheBackend,
    key: StorageEncryptionKey,
    conn: Connection,
}

impl MediaCacheIndex {
    /// Open or initialize an encrypted Rustgram media cache index in `root_dir`
    /// (`index.sqlite3` + encrypted `TDEF` files under `blobs/`) using `key`.
    ///
    /// Any legacy unencrypted cache (`PRAGMA user_version < 2`, plaintext blobs, or plaintext
    /// `cache_meta.json`) is deleted automatically on open.
    pub fn open_with_key(root_dir: impl Into<PathBuf>, key: StorageEncryptionKey) -> Result<Self> {
        let root_dir = root_dir.into();
        fs::create_dir_all(root_dir.join("blobs"))?;
        register_cache_key(&root_dir, key.clone());
        let conn = Connection::open(root_dir.join("index.sqlite3"))?;
        Self::init_and_migrate(&root_dir, &conn, &key)?;
        Ok(Self {
            backend: CacheBackend::Disk { root_dir },
            key,
            conn,
        })
    }

    /// Open or initialize an encrypted Rustgram media cache index in `root_dir`.
    ///
    /// Uses any [`StorageEncryptionKey`] already registered for `root_dir` (or an ancestor `tdata`
    /// directory), otherwise generates a random per-process ephemeral key (never persisted to disk).
    pub fn open(root_dir: impl Into<PathBuf>) -> Result<Self> {
        let root_dir = root_dir.into();
        fs::create_dir_all(root_dir.join("blobs"))?;
        let key = resolve_or_create_ephemeral_key(&root_dir);
        Self::open_with_key(root_dir, key)
    }

    /// Open an in-memory SQLite index storing encrypted `TDEF` blobs under `root_dir`
    /// (handy for unit tests).
    pub fn open_in_memory(root_dir: impl Into<PathBuf>) -> Result<Self> {
        let root_dir = root_dir.into();
        fs::create_dir_all(root_dir.join("blobs"))?;
        let key = resolve_or_create_ephemeral_key(&root_dir);
        let conn = Connection::open_in_memory()?;
        Self::init_schema(&conn)?;
        Ok(Self {
            backend: CacheBackend::Disk { root_dir },
            key,
            conn,
        })
    }

    /// Open a strictly memory-only media cache with zero filesystem writes (used before an account
    /// `tdata` directory is attached so nothing is ever written to `%TEMP%`).
    pub fn open_memory_only() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init_schema(&conn)?;
        Ok(Self {
            backend: CacheBackend::MemoryOnly {
                blobs: Mutex::new(HashMap::new()),
            },
            key: random_ephemeral_cache_key(),
            conn,
        })
    }

    fn init_and_migrate(
        root_dir: &Path,
        conn: &Connection,
        key: &StorageEncryptionKey,
    ) -> Result<()> {
        let current_ver: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if current_ver < MEDIA_CACHE_SCHEMA_VERSION {
            // Purge legacy unencrypted blobs, plaintext manifest, and unencrypted SQLite rows.
            purge_legacy_plaintext_cache(root_dir, conn)?;
            Self::init_schema(conn)?;
            write_key_tag(conn, key)?;
        } else {
            // Always delete any leftover plaintext manifest or unencrypted blobs.
            let _ = fs::remove_file(root_dir.join(LEGACY_MANIFEST_FILENAME));
            purge_non_tdef_blobs(&root_dir.join("blobs"));
            purge_legacy_stream_files(&root_dir.join("streamed"));
            Self::init_schema(conn)?;
            if !cache_matches_key(conn, key)? {
                purge_cache_contents(root_dir, conn, key)?;
            } else {
                write_key_tag(conn, key)?;
            }
        }
        Ok(())
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(&format!(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS cache_meta (
                 name  TEXT PRIMARY KEY NOT NULL,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS media_cache (
                 key           TEXT PRIMARY KEY NOT NULL,
                 rel_path      TEXT NOT NULL,
                 size          INTEGER NOT NULL,
                 tag           INTEGER NOT NULL,
                 last_accessed INTEGER NOT NULL,
                 created_at    INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_media_cache_lru
                 ON media_cache (last_accessed ASC);
             CREATE INDEX IF NOT EXISTS idx_media_cache_tag
                 ON media_cache (tag);
             CREATE TABLE IF NOT EXISTS file_locations (
                 key_high    INTEGER NOT NULL,
                 key_low     INTEGER NOT NULL,
                 path        TEXT NOT NULL,
                 size        INTEGER NOT NULL,
                 modified_ms INTEGER NOT NULL,
                 PRIMARY KEY (key_high, key_low)
             );
             PRAGMA user_version = {MEDIA_CACHE_SCHEMA_VERSION};"
        ))?;
        Ok(())
    }

    /// Root cache directory (empty path when opened in strictly memory-only mode).
    #[must_use]
    pub fn root_dir(&self) -> &Path {
        match &self.backend {
            CacheBackend::Disk { root_dir } => root_dir,
            CacheBackend::MemoryOnly { .. } => Path::new(""),
        }
    }

    /// Returns `true` when this index stores blobs on disk rather than strictly in RAM.
    #[must_use]
    pub const fn is_disk_backed(&self) -> bool {
        matches!(self.backend, CacheBackend::Disk { .. })
    }

    /// Returns the active [`StorageEncryptionKey`] protecting this cache.
    #[must_use]
    pub const fn encryption_key(&self) -> &StorageEncryptionKey {
        &self.key
    }

    /// Store `bytes` under `key` with category `tag`, overwriting any previous entry.
    pub fn put_blob(&self, key: &str, tag: MediaCacheTag, bytes: &[u8]) -> Result<MediaCacheEntry> {
        let entry_id = keyed_entry_id(&self.key, key);
        let db_key = encode_hex(&entry_id);
        let shard = format!("{:02x}", entry_id[0]);
        let hex_name = encode_hex(&entry_id[..16]);
        let rel_path = format!("blobs/{shard}/{hex_name}");

        match &self.backend {
            CacheBackend::Disk { root_dir } => {
                let full_path = root_dir.join(&rel_path);
                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                write_encrypted_blob_bound(&full_path, &self.key, &entry_id, bytes)?;
            }
            CacheBackend::MemoryOnly { blobs } => {
                if let Ok(mut guard) = blobs.lock() {
                    guard.insert(rel_path.clone(), bytes.to_vec());
                }
            }
        }

        let enc_rel_path = encrypt_index_string(&self.key, &rel_path)?;
        let now = now_unix_secs();
        let size = bytes.len() as u64;
        self.conn.execute(
            "INSERT INTO media_cache (key, rel_path, size, tag, last_accessed, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(key) DO UPDATE SET
                 rel_path = excluded.rel_path,
                 size = excluded.size,
                 tag = excluded.tag,
                 last_accessed = excluded.last_accessed",
            params![
                db_key,
                enc_rel_path,
                size as i64,
                tag as u8 as i64,
                now,
                now
            ],
        )?;

        Ok(MediaCacheEntry {
            key: key.to_string(),
            rel_path,
            size,
            tag,
            last_accessed: now,
            created_at: now,
        })
    }

    /// Read and decrypt a cached blob by `key`, updating its `last_accessed` timestamp.
    pub fn get_blob(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.read_blob(key, true)
    }

    /// Like [`Self::get_blob`] without the `last_accessed` write: the caller records the access
    /// later with [`Self::touch_keys`], which batches many reads into one transaction.
    pub fn get_blob_deferred_touch(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.read_blob(key, false)
    }

    /// Sets `last_accessed` to now for every key in `keys` (cache keys as given to
    /// [`Self::put_blob`]) in one transaction; returns the number of rows updated.
    pub fn touch_keys<'a>(&self, keys: impl IntoIterator<Item = &'a str>) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let now = now_unix_secs();
        let mut updated = 0usize;
        {
            let mut stmt =
                tx.prepare_cached("UPDATE media_cache SET last_accessed = ?1 WHERE key = ?2")?;
            for key in keys {
                let db_key = encode_hex(&keyed_entry_id(&self.key, key));
                updated += stmt.execute(params![now, db_key])?;
            }
        }
        tx.commit()?;
        Ok(updated)
    }

    fn read_blob(&self, key: &str, touch: bool) -> Result<Option<Vec<u8>>> {
        let entry_id = keyed_entry_id(&self.key, key);
        let db_key = encode_hex(&entry_id);
        let enc_rel_path: Option<String> = self
            .conn
            .query_row(
                "SELECT rel_path FROM media_cache WHERE key = ?1",
                params![db_key],
                |row| row.get(0),
            )
            .optional()?;

        let Some(enc_rel) = enc_rel_path else {
            return Ok(None);
        };
        let Ok(rel) = decrypt_index_string(&self.key, &enc_rel) else {
            // Wrong key or corrupt index row: treat as cache miss.
            return Ok(None);
        };

        match &self.backend {
            CacheBackend::Disk { root_dir } => {
                let full_path = root_dir.join(&rel);
                match read_encrypted_blob_bound(&full_path, &self.key, &entry_id) {
                    Ok(bytes) => {
                        if touch {
                            let _ = self.conn.execute(
                                "UPDATE media_cache SET last_accessed = ?1 WHERE key = ?2",
                                params![now_unix_secs(), db_key],
                            );
                        }
                        Ok(Some(bytes))
                    }
                    Err(StorageError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                        let _ = self
                            .conn
                            .execute("DELETE FROM media_cache WHERE key = ?1", params![db_key]);
                        Ok(None)
                    }
                    Err(StorageError::DecryptionFailed)
                    | Err(StorageError::InvalidMagic)
                    | Err(StorageError::ChecksumMismatch)
                    | Err(StorageError::CorruptData(_)) => {
                        // Purge unreadable/corrupt/plaintext blob, and any blob that fails
                        // authentication (tampered, or written by an older build), and treat
                        // it as a cache miss.
                        let _ = fs::remove_file(&full_path);
                        let _ = self
                            .conn
                            .execute("DELETE FROM media_cache WHERE key = ?1", params![db_key]);
                        Ok(None)
                    }
                    Err(e) => Err(e),
                }
            }
            CacheBackend::MemoryOnly { blobs } => {
                let maybe = blobs.lock().ok().and_then(|g| g.get(&rel).cloned());
                if touch && maybe.is_some() {
                    let _ = self.conn.execute(
                        "UPDATE media_cache SET last_accessed = ?1 WHERE key = ?2",
                        params![now_unix_secs(), db_key],
                    );
                }
                Ok(maybe)
            }
        }
    }

    /// Delete a cached blob by `key`.
    pub fn remove_blob(&self, key: &str) -> Result<bool> {
        let db_key = encode_hex(&keyed_entry_id(&self.key, key));
        let enc_rel_path: Option<String> = self
            .conn
            .query_row(
                "SELECT rel_path FROM media_cache WHERE key = ?1",
                params![db_key],
                |row| row.get(0),
            )
            .optional()?;
        let Some(enc_rel) = enc_rel_path else {
            return Ok(false);
        };
        if let Ok(rel) = decrypt_index_string(&self.key, &enc_rel) {
            self.delete_backing_blob(&rel);
        }
        self.conn
            .execute("DELETE FROM media_cache WHERE key = ?1", params![db_key])?;
        Ok(true)
    }

    fn delete_backing_blob(&self, rel: &str) {
        match &self.backend {
            CacheBackend::Disk { root_dir } => {
                let _ = fs::remove_file(root_dir.join(rel));
            }
            CacheBackend::MemoryOnly { blobs } => {
                if let Ok(mut guard) = blobs.lock() {
                    guard.remove(rel);
                }
            }
        }
    }

    /// Return aggregate entry count and byte size.
    pub fn stats(&self) -> Result<MediaCacheStats> {
        let (count, total): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM media_cache",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(MediaCacheStats {
            entry_count: count.max(0) as u64,
            total_bytes: total.max(0) as u64,
        })
    }

    /// Return per-category cached byte totals directly from SQLite.
    pub fn stats_by_tag(&self) -> Result<MediaCacheTagStats> {
        let mut stmt = self
            .conn
            .prepare("SELECT tag, COALESCE(SUM(size), 0) FROM media_cache GROUP BY tag")?;
        let mut out = MediaCacheTagStats::default();
        let rows = stmt.query_map([], |row| {
            let tag_i64: i64 = row.get(0)?;
            let sum_i64: i64 = row.get(1)?;
            Ok((tag_i64, sum_i64))
        })?;
        for row in rows {
            let (tag_i64, sum_i64) = row?;
            let bytes = sum_i64.max(0) as u64;
            match MediaCacheTag::from_u8(tag_i64.clamp(0, 255) as u8) {
                MediaCacheTag::Photo => out.photos_bytes = out.photos_bytes.saturating_add(bytes),
                MediaCacheTag::Video => out.videos_bytes = out.videos_bytes.saturating_add(bytes),
                MediaCacheTag::Sticker => {
                    out.stickers_bytes = out.stickers_bytes.saturating_add(bytes);
                }
                MediaCacheTag::Document
                | MediaCacheTag::Voice
                | MediaCacheTag::Audio
                | MediaCacheTag::Other => {
                    out.files_bytes = out.files_bytes.saturating_add(bytes);
                }
            }
        }
        Ok(out)
    }

    /// Evict entries older than `max_age_secs` (if `Some`) and least-recently-used entries
    /// until `total_bytes <= max_total_bytes` (if `Some`). Returns number of evicted entries.
    pub fn prune(&self, max_total_bytes: Option<u64>, max_age_secs: Option<i64>) -> Result<usize> {
        let mut evicted = 0usize;

        if let Some(max_age) = max_age_secs {
            let cutoff = now_unix_secs().saturating_sub(max_age);
            let mut stmt = self
                .conn
                .prepare("SELECT key, rel_path FROM media_cache WHERE last_accessed < ?1")?;
            let expired: Vec<(String, String)> = stmt
                .query_map(params![cutoff], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for (k, enc_rel) in expired {
                if let Ok(rel) = decrypt_index_string(&self.key, &enc_rel) {
                    self.delete_backing_blob(&rel);
                }
                self.conn
                    .execute("DELETE FROM media_cache WHERE key = ?1", params![k])?;
                evicted += 1;
            }
        }

        if let Some(limit) = max_total_bytes {
            let mut current = self.stats()?.total_bytes;
            if current > limit {
                let mut stmt = self.conn.prepare(
                    "SELECT key, rel_path, size FROM media_cache ORDER BY last_accessed ASC, created_at ASC",
                )?;
                let rows: Vec<(String, String, i64)> = stmt
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for (k, enc_rel, sz) in rows {
                    if current <= limit {
                        break;
                    }
                    if let Ok(rel) = decrypt_index_string(&self.key, &enc_rel) {
                        self.delete_backing_blob(&rel);
                    }
                    self.conn
                        .execute("DELETE FROM media_cache WHERE key = ?1", params![k])?;
                    current = current.saturating_sub(sz.max(0) as u64);
                    evicted += 1;
                }
            }
        }

        Ok(evicted)
    }

    /// Register or update a local downloaded file location (path is encrypted at rest in SQLite).
    pub fn upsert_file_location(&self, record: &LocalFileRecord) -> Result<()> {
        let path_str = record.path.to_string_lossy();
        let enc_path = encrypt_index_string(&self.key, path_str.as_ref())?;
        self.conn.execute(
            "INSERT INTO file_locations (key_high, key_low, path, size, modified_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(key_high, key_low) DO UPDATE SET
                 path = excluded.path,
                 size = excluded.size,
                 modified_ms = excluded.modified_ms",
            params![
                record.key_high as i64,
                record.key_low as i64,
                enc_path,
                record.size as i64,
                record.modified_ms,
            ],
        )?;
        Ok(())
    }

    /// Look up and decrypt a local downloaded file location by `(key_high, key_low)`.
    pub fn get_file_location(
        &self,
        key_high: u64,
        key_low: u64,
    ) -> Result<Option<LocalFileRecord>> {
        let row_opt: Option<(i64, i64, String, i64, i64)> = self
            .conn
            .query_row(
                "SELECT key_high, key_low, path, size, modified_ms
                 FROM file_locations WHERE key_high = ?1 AND key_low = ?2",
                params![key_high as i64, key_low as i64],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((kh, kl, enc_path, sz, m)) = row_opt else {
            return Ok(None);
        };
        let Ok(decrypted_path) = decrypt_index_string(&self.key, &enc_path) else {
            return Ok(None);
        };
        Ok(Some(LocalFileRecord {
            key_high: kh as u64,
            key_low: kl as u64,
            path: PathBuf::from(decrypted_path),
            size: sz.max(0) as u64,
            modified_ms: m,
        }))
    }

    /// Remove a registered local file location.
    pub fn remove_file_location(&self, key_high: u64, key_low: u64) -> Result<bool> {
        let rows = self.conn.execute(
            "DELETE FROM file_locations WHERE key_high = ?1 AND key_low = ?2",
            params![key_high as i64, key_low as i64],
        )?;
        Ok(rows > 0)
    }

    /// Purges all cached media blobs, streamed partial downloads, and index rows while keeping
    /// the database initialized under the current encryption key.
    pub fn purge(&self) -> Result<()> {
        match &self.backend {
            CacheBackend::Disk { root_dir } => {
                purge_cache_contents(root_dir, &self.conn, &self.key)?;
            }
            CacheBackend::MemoryOnly { blobs } => {
                if let Ok(mut guard) = blobs.lock() {
                    guard.clear();
                }
                self.conn
                    .execute_batch("DELETE FROM media_cache; DELETE FROM file_locations;")?;
                write_key_tag(&self.conn, &self.key)?;
            }
        }
        Ok(())
    }

    /// Rotates the media cache encryption key from `self.key` to `new_key`:
    /// - Re-encrypts readable `file_locations` rows under `new_key` (returning the count preserved).
    /// - Purges `media_cache` rows and on-disk `blobs/` & `streamed/` files (whose lookup keys are
    ///   one-way keyed hashes of the old key) so no unreadable entries remain on disk.
    /// - Updates the in-memory cache key registry and on-disk `cache_meta` key tag.
    pub fn rekey(&mut self, new_key: StorageEncryptionKey) -> Result<usize> {
        if self.key.as_bytes() == new_key.as_bytes() {
            if let CacheBackend::Disk { root_dir } = &self.backend {
                register_cache_key(root_dir, new_key);
            }
            write_key_tag(&self.conn, &self.key)?;
            return Ok(0);
        }

        let mut reencrypted_locations: Vec<(i64, i64, String, i64, i64)> = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT key_high, key_low, path, size, modified_ms FROM file_locations")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?;
            for row in rows {
                let (kh, kl, enc_path, sz, modified_ms) = row?;
                if let Ok(plain_path) = decrypt_index_string(&self.key, &enc_path) {
                    let new_enc_path = encrypt_index_string(&new_key, &plain_path)?;
                    reencrypted_locations.push((kh, kl, new_enc_path, sz, modified_ms));
                }
            }
        }

        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM media_cache", [])?;
        tx.execute("DELETE FROM file_locations", [])?;
        for (kh, kl, enc_path, sz, modified_ms) in &reencrypted_locations {
            tx.execute(
                "INSERT INTO file_locations (key_high, key_low, path, size, modified_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![kh, kl, enc_path, sz, modified_ms],
            )?;
        }
        write_key_tag(&tx, &new_key)?;
        tx.commit()?;

        match &self.backend {
            CacheBackend::Disk { root_dir } => {
                purge_disk_blob_dirs(root_dir)?;
                register_cache_key(root_dir, new_key.clone());
            }
            CacheBackend::MemoryOnly { blobs } => {
                if let Ok(mut guard) = blobs.lock() {
                    guard.clear();
                }
            }
        }
        self.key = new_key;
        Ok(reencrypted_locations.len())
    }
}

/// Synchronizes an on-disk media cache directory `root_dir` to `new_key` after a local key update:
/// - Always registers `new_key` in the in-memory cache key registry.
/// - If `old_key` matches `new_key`, leaves existing encrypted entries intact.
/// - If `old_key` differs from `new_key` and matches the on-disk cache, re-encrypts
///   `file_locations` under `new_key` and purges unreadable blobs & streamed files.
/// - If `old_key` is unknown or does not match the on-disk cache, purges any unreadable entries
///   while leaving a cache that already matches `new_key` untouched.
pub fn rekey_or_purge_cache_dir(
    root_dir: &Path,
    old_key: Option<&StorageEncryptionKey>,
    new_key: StorageEncryptionKey,
) -> Result<()> {
    register_cache_key(root_dir, new_key.clone());
    if old_key.is_some_and(|old| old.as_bytes() == new_key.as_bytes()) {
        return Ok(());
    }

    let db_path = root_dir.join("index.sqlite3");
    let has_blobs = root_dir.join("blobs").exists();
    let has_streamed = root_dir.join("streamed").exists();
    if !db_path.exists() && !has_blobs && !has_streamed {
        return Ok(());
    }

    if db_path.exists() {
        if let Some(old) = old_key {
            let conn = Connection::open(&db_path)?;
            let current_ver: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if current_ver >= MEDIA_CACHE_SCHEMA_VERSION {
                MediaCacheIndex::init_schema(&conn)?;
                if cache_matches_key(&conn, old)? {
                    let mut cache = MediaCacheIndex {
                        backend: CacheBackend::Disk {
                            root_dir: root_dir.to_path_buf(),
                        },
                        key: old.clone(),
                        conn,
                    };
                    cache.rekey(new_key)?;
                    return Ok(());
                }
            }
        }
        // Either no old key was provided or the on-disk cache didn't match old_key:
        // opening with new_key preserves an already-matching cache and purges any mismatched one.
        let _cache = MediaCacheIndex::open_with_key(root_dir, new_key)?;
        return Ok(());
    }

    // Stray blob/streamed directories without an index cannot be verified; purge them on rotation.
    if old_key.is_some() {
        purge_disk_blob_dirs(root_dir)?;
    }
    Ok(())
}

fn keyed_entry_id(key: &StorageEncryptionKey, cache_key: &str) -> [u8; 32] {
    sha256(&[
        key.as_bytes(),
        b"rustgram.media_cache.entry\0",
        cache_key.as_bytes(),
    ])
}

/// Returns the file-name stem for the streamed media file of object `(key_high, key_low)`:
/// [`STREAM_FILE_PREFIX`] followed by a keyed hash. Without the cache key the name does not
/// reveal which document it belongs to, and it changes when the key changes.
#[must_use]
pub fn keyed_stream_file_stem(key: &StorageEncryptionKey, key_high: u64, key_low: u64) -> String {
    let id = sha256(&[
        key.as_bytes(),
        b"rustgram.media_cache.stream\0",
        &key_high.to_le_bytes(),
        &key_low.to_le_bytes(),
    ]);
    format!("{STREAM_FILE_PREFIX}{}", encode_hex(&id[..16]))
}

/// Derives the HMAC key that authenticates cache content from the cache encryption key.
fn blob_mac_key(key: &StorageEncryptionKey) -> [u8; 32] {
    sha256(&[key.as_bytes(), b"rustgram.media_cache.blob_mac.v1\0"])
}

fn blob_mac(key: &StorageEncryptionKey, context: &[u8], plaintext: &[u8]) -> [u8; 32] {
    hmac_sha256_parts(
        &blob_mac_key(key),
        &[
            b"rustgram.media_cache.blob\0",
            &(context.len() as u64).to_le_bytes(),
            context,
            &(plaintext.len() as u64).to_le_bytes(),
            plaintext,
        ],
    )
}

/// Encrypts `plaintext` into a `TDEF` [`EncryptedFile`] at `path` with an inner length prefix and
/// an HMAC-SHA-256 over the content under a key derived from `key`.
pub fn write_encrypted_blob(
    path: &Path,
    key: &StorageEncryptionKey,
    plaintext: &[u8],
) -> Result<()> {
    write_encrypted_blob_bound(path, key, &[], plaintext)
}

/// Reads and authenticates a `TDEF` [`EncryptedFile`] blob written by [`write_encrypted_blob`].
pub fn read_encrypted_blob(path: &Path, key: &StorageEncryptionKey) -> Result<Vec<u8>> {
    read_encrypted_blob_bound(path, key, &[])
}

/// Like [`write_encrypted_blob`], with `context` (for example the entry id or the owner of the
/// file) covered by the MAC: [`read_encrypted_blob_bound`] accepts the file only with the same
/// `context`, so one valid blob cannot be copied over another.
pub fn write_encrypted_blob_bound(
    path: &Path,
    key: &StorageEncryptionKey,
    context: &[u8],
    plaintext: &[u8],
) -> Result<()> {
    let mut framed = Vec::with_capacity(BLOB_FRAME_HEADER_LEN + plaintext.len());
    framed.extend_from_slice(&(plaintext.len() as u64).to_le_bytes());
    framed.extend_from_slice(&blob_mac(key, context, plaintext));
    framed.extend_from_slice(plaintext);

    let mut file = EncryptedFile::open(path, OpenMode::Write, key)?;
    file.write_with_padding(&framed)?;
    file.flush()?;
    Ok(())
}

/// Reads a `TDEF` [`EncryptedFile`] blob written by [`write_encrypted_blob_bound`] and checks its
/// HMAC for `context`. A blob that was altered, belongs to another context, or was written by an
/// older build (unkeyed SHA-256 digest) fails with [`StorageError::ChecksumMismatch`].
pub fn read_encrypted_blob_bound(
    path: &Path,
    key: &StorageEncryptionKey,
    context: &[u8],
) -> Result<Vec<u8>> {
    let mut file = EncryptedFile::open(path, OpenMode::Read, key)?;
    let total = usize::try_from(file.size())
        .map_err(|_| StorageError::CorruptData("encrypted blob exceeds addressable size".into()))?;
    if total < BLOB_FRAME_HEADER_LEN {
        return Err(StorageError::CorruptData(
            "encrypted blob payload shorter than frame header".into(),
        ));
    }
    let mut buf = vec![0u8; total];
    let read = file.read_blocks(&mut buf)?;
    if read < BLOB_FRAME_HEADER_LEN {
        return Err(StorageError::CorruptData(
            "truncated encrypted blob payload".into(),
        ));
    }
    let plain_len = u64::from_le_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ]);
    let plain_len_usize = usize::try_from(plain_len)
        .map_err(|_| StorageError::CorruptData("encrypted blob length overflow".into()))?;
    let end = BLOB_FRAME_HEADER_LEN
        .checked_add(plain_len_usize)
        .ok_or_else(|| StorageError::CorruptData("encrypted blob length overflow".into()))?;
    if end > read {
        return Err(StorageError::CorruptData(
            "encrypted blob length exceeds stored bytes".into(),
        ));
    }
    let stored_mac = &buf[8..BLOB_FRAME_HEADER_LEN];
    let plaintext = &buf[BLOB_FRAME_HEADER_LEN..end];
    let computed = blob_mac(key, context, plaintext);
    if !const_time_eq(stored_mac, &computed) {
        return Err(StorageError::ChecksumMismatch);
    }
    Ok(plaintext.to_vec())
}

fn encrypt_index_string(key: &StorageEncryptionKey, plaintext: &str) -> Result<String> {
    let mut salt = [0u8; STORAGE_CTR_SALT_SIZE];
    random_bytes(&mut salt);
    let raw = plaintext.as_bytes();
    let mac = sha256(&[key.as_bytes(), &salt, b"rustgram.media_cache.str.v1\0", raw]);

    let state = key.prepare_ctr_state(&salt);
    let padded_len = raw.len().div_ceil(AES_BLOCK_SIZE) * AES_BLOCK_SIZE;
    let mut buf = vec![0u8; padded_len];
    buf[..raw.len()].copy_from_slice(raw);
    if padded_len > 0 {
        state
            .encrypt(&mut buf, 0)
            .map_err(|e| StorageError::CorruptData(e.to_string()))?;
    }
    let ct = &buf[..raw.len()];
    Ok(format!(
        "{ENC_STRING_PREFIX}{}:{}:{}",
        encode_hex(&salt),
        encode_hex(&mac),
        encode_hex(ct)
    ))
}

fn decrypt_index_string(key: &StorageEncryptionKey, encoded: &str) -> Result<String> {
    let rest = encoded
        .strip_prefix(ENC_STRING_PREFIX)
        .ok_or(StorageError::DecryptionFailed)?;
    let mut parts = rest.splitn(3, ':');
    let salt_hex = parts.next().ok_or(StorageError::DecryptionFailed)?;
    let mac_hex = parts.next().ok_or(StorageError::DecryptionFailed)?;
    let ct_hex = parts.next().ok_or(StorageError::DecryptionFailed)?;

    let salt_vec = decode_hex(salt_hex).ok_or(StorageError::DecryptionFailed)?;
    let mac_vec = decode_hex(mac_hex).ok_or(StorageError::DecryptionFailed)?;
    let ct = decode_hex(ct_hex).ok_or(StorageError::DecryptionFailed)?;
    if salt_vec.len() != STORAGE_CTR_SALT_SIZE || mac_vec.len() != 32 {
        return Err(StorageError::DecryptionFailed);
    }
    let mut salt = [0u8; STORAGE_CTR_SALT_SIZE];
    salt.copy_from_slice(&salt_vec);

    let state = key.prepare_ctr_state(&salt);
    let padded_len = ct.len().div_ceil(AES_BLOCK_SIZE) * AES_BLOCK_SIZE;
    let mut buf = vec![0u8; padded_len];
    buf[..ct.len()].copy_from_slice(&ct);
    if padded_len > 0 {
        state
            .decrypt(&mut buf, 0)
            .map_err(|e| StorageError::CorruptData(e.to_string()))?;
    }
    let plain = &buf[..ct.len()];
    let expected_mac = sha256(&[
        key.as_bytes(),
        &salt,
        b"rustgram.media_cache.str.v1\0",
        plain,
    ]);
    if !const_time_eq(&mac_vec, &expected_mac) {
        return Err(StorageError::DecryptionFailed);
    }
    String::from_utf8(plain.to_vec()).map_err(|_| StorageError::DecryptionFailed)
}

fn key_tag_for(key: &StorageEncryptionKey) -> String {
    encode_hex(&sha256(&[
        key.as_bytes(),
        b"rustgram.media_cache.key_tag.v1\0",
    ]))
}

fn write_key_tag(conn: &Connection, key: &StorageEncryptionKey) -> Result<()> {
    conn.execute(
        "INSERT INTO cache_meta (name, value) VALUES ('key_tag', ?1)
         ON CONFLICT(name) DO UPDATE SET value = excluded.value",
        params![key_tag_for(key)],
    )?;
    Ok(())
}

fn read_key_tag(conn: &Connection) -> Result<Option<String>> {
    let tag: Option<String> = conn
        .query_row(
            "SELECT value FROM cache_meta WHERE name = 'key_tag'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(tag)
}

fn cache_matches_key(conn: &Connection, key: &StorageEncryptionKey) -> Result<bool> {
    if let Some(stored) = read_key_tag(conn)? {
        return Ok(stored == key_tag_for(key));
    }
    if let Some(enc_rel) = conn
        .query_row("SELECT rel_path FROM media_cache LIMIT 1", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()?
    {
        return Ok(decrypt_index_string(key, &enc_rel).is_ok());
    }
    if let Some(enc_path) = conn
        .query_row("SELECT path FROM file_locations LIMIT 1", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()?
    {
        return Ok(decrypt_index_string(key, &enc_path).is_ok());
    }
    Ok(true)
}

fn purge_disk_blob_dirs(root_dir: &Path) -> Result<()> {
    let _ = fs::remove_file(root_dir.join(LEGACY_MANIFEST_FILENAME));
    let blobs_dir = root_dir.join("blobs");
    if blobs_dir.exists() {
        let _ = fs::remove_dir_all(&blobs_dir);
    }
    fs::create_dir_all(&blobs_dir)?;
    let streamed_dir = root_dir.join("streamed");
    if streamed_dir.exists() {
        let _ = fs::remove_dir_all(&streamed_dir);
    }
    Ok(())
}

fn purge_cache_contents(
    root_dir: &Path,
    conn: &Connection,
    key: &StorageEncryptionKey,
) -> Result<()> {
    purge_disk_blob_dirs(root_dir)?;
    conn.execute_batch(
        "DELETE FROM media_cache;
         DELETE FROM file_locations;",
    )?;
    write_key_tag(conn, key)?;
    Ok(())
}

fn purge_legacy_plaintext_cache(root_dir: &Path, conn: &Connection) -> Result<()> {
    purge_disk_blob_dirs(root_dir)?;
    conn.execute_batch(
        "DROP TABLE IF EXISTS cache_meta;
         DROP TABLE IF EXISTS media_cache;
         DROP TABLE IF EXISTS file_locations;
         VACUUM;",
    )?;
    Ok(())
}

/// File-name prefix older builds used in `<cache root>/streamed`
/// (`stream_<document id>_<dc>.bin` plus its `.part` / `.part.meta`).
const LEGACY_STREAM_FILE_PREFIX: &str = "stream_";

/// Deletes the files older builds left in `<cache root>/streamed`: their names carry the
/// document id, and current builds never open those names again.
fn purge_legacy_stream_files(streamed_dir: &Path) {
    let Ok(entries) = fs::read_dir(streamed_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|n| n.starts_with(LEGACY_STREAM_FILE_PREFIX))
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn purge_non_tdef_blobs(blobs_dir: &Path) {
    let Ok(shards) = fs::read_dir(blobs_dir) else {
        return;
    };
    for shard_entry in shards.flatten() {
        let shard_path = shard_entry.path();
        if !shard_path.is_dir() {
            let _ = fs::remove_file(&shard_path);
            continue;
        }
        let Ok(files) = fs::read_dir(&shard_path) else {
            continue;
        };
        for file_entry in files.flatten() {
            let path = file_entry.path();
            if !path.is_file() {
                continue;
            }
            let is_tdef = fs::File::open(&path)
                .ok()
                .and_then(|mut f| {
                    let mut magic = [0u8; 4];
                    f.read_exact(&mut magic).ok().map(|()| magic == TDEF_MAGIC)
                })
                .unwrap_or(false);
            if !is_tdef {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.as_chunks::<2>().0 {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_local_key(seed: u8) -> AuthKey {
        let mut data = [0u8; 256];
        for (i, b) in data.iter_mut().enumerate() {
            *b = seed.wrapping_add(i as u8);
        }
        AuthKey::new(data)
    }

    #[test]
    fn sqlite_media_cache_and_file_locations() {
        let dir = TempDir::new().expect("tempdir");
        let cache = MediaCacheIndex::open(dir.path()).expect("open");

        cache
            .put_blob("photo:2:100:m", MediaCacheTag::Photo, &[1u8; 500])
            .expect("put 1");
        cache
            .put_blob("sticker:2:200", MediaCacheTag::Sticker, &[2u8; 800])
            .expect("put 2");
        assert_eq!(cache.stats().expect("stats").total_bytes, 1300);
        assert_eq!(
            cache.get_blob("photo:2:100:m").expect("get").as_deref(),
            Some(&[1u8; 500][..])
        );

        // Prune down to 900 bytes -> evicts one entry
        let evicted = cache.prune(Some(900), None).expect("prune");
        assert_eq!(evicted, 1);
        assert!(cache.stats().expect("stats after").total_bytes <= 900);

        // File locations
        let rec = LocalFileRecord {
            key_high: 0x1111,
            key_low: 0x2222,
            path: dir.path().join("downloaded.pdf"),
            size: 12_345,
            modified_ms: 1_700_000_000_000,
        };
        cache.upsert_file_location(&rec).expect("upsert");
        assert_eq!(
            cache.get_file_location(0x1111, 0x2222).expect("get loc"),
            Some(rec)
        );
        assert!(cache.remove_file_location(0x1111, 0x2222).expect("del loc"));
        assert_eq!(cache.get_file_location(0x1111, 0x2222).expect("gone"), None);
    }

    #[test]
    fn d1_media_cache_blobs_and_index_paths_are_encrypted_on_disk() {
        let dir = TempDir::new().expect("tempdir");
        let local_key = test_local_key(0x42);
        let key_acc0 = derive_media_cache_key(&local_key, "data");
        let key_acc1 = derive_media_cache_key(&local_key, "data#2");
        assert_ne!(
            key_acc0.as_bytes(),
            key_acc1.as_bytes(),
            "different accounts must derive distinct cache encryption keys"
        );

        let secret_blob = b"TOP_SECRET_PRIVATE_CHAT_PHOTO_BYTES_1234567890";
        let secret_key = "photo:2:99887766:secret_size";
        let secret_download_path = dir.path().join("confidential_salary_report_2026.pdf");

        let entry = {
            let cache = MediaCacheIndex::open_with_key(dir.path(), key_acc0.clone())
                .expect("open encrypted cache");
            let e = cache
                .put_blob(secret_key, MediaCacheTag::Photo, secret_blob)
                .expect("put encrypted blob");
            cache
                .upsert_file_location(&LocalFileRecord {
                    key_high: 0xAA,
                    key_low: 0xBB,
                    path: secret_download_path.clone(),
                    size: 4096,
                    modified_ms: 1_700_000_000_000,
                })
                .expect("upsert encrypted file location");
            // Force WAL checkpoint so all SQLite bytes are visible in `index.sqlite3`.
            let _ = cache.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
            e
        };

        // 1. Blob file on disk MUST be TDEF-encrypted and MUST NOT contain plaintext bytes.
        let blob_path = dir.path().join(&entry.rel_path);
        let on_disk_blob = fs::read(&blob_path).expect("read raw blob on disk");
        assert!(
            on_disk_blob.starts_with(&TDEF_MAGIC),
            "cached blob on disk must use TDEF encrypted file format"
        );
        assert!(
            !on_disk_blob
                .windows(secret_blob.len())
                .any(|w| w == secret_blob),
            "cached blob on disk must not contain plaintext bytes"
        );

        // 2. SQLite index on disk MUST NOT contain plaintext media key or file path.
        let sqlite_bytes = fs::read(dir.path().join("index.sqlite3")).expect("read index.sqlite3");
        let needle_key = b"99887766";
        let needle_path = b"confidential_salary_report_2026";
        let needle_rel = b"blobs/";
        assert!(
            !sqlite_bytes
                .windows(needle_key.len())
                .any(|w| w == needle_key),
            "index.sqlite3 must not expose plaintext cache keys"
        );
        assert!(
            !sqlite_bytes
                .windows(needle_path.len())
                .any(|w| w == needle_path),
            "index.sqlite3 must not expose plaintext file_locations paths"
        );
        assert!(
            !sqlite_bytes
                .windows(needle_rel.len())
                .any(|w| w == needle_rel),
            "index.sqlite3 must not expose plaintext rel_path strings"
        );

        // 3. Reopening with the SAME derived key decrypts both blob and file location.
        let reopened = MediaCacheIndex::open_with_key(dir.path(), key_acc0).expect("reopen");
        assert_eq!(
            reopened.get_blob(secret_key).expect("get blob").as_deref(),
            Some(&secret_blob[..])
        );
        assert_eq!(
            reopened
                .get_file_location(0xAA, 0xBB)
                .expect("get loc")
                .map(|r| r.path),
            Some(secret_download_path)
        );
        drop(reopened);

        // 4. Reopening with a DIFFERENT account's key cannot read either the blob or file location.
        let wrong = MediaCacheIndex::open_with_key(dir.path(), key_acc1).expect("open wrong key");
        assert_eq!(wrong.get_blob(secret_key).expect("wrong key blob"), None);
        assert_eq!(
            wrong.get_file_location(0xAA, 0xBB).expect("wrong key loc"),
            None
        );
    }

    #[test]
    fn d1_legacy_plaintext_cache_is_deleted_on_open() {
        let dir = TempDir::new().expect("tempdir");
        let legacy_blob = dir.path().join("blobs").join("aa").join("legacy_plain");
        fs::create_dir_all(legacy_blob.parent().unwrap()).expect("mkdir");
        fs::write(&legacy_blob, b"PLAINTEXT_LEGACY_PHOTO").expect("write legacy blob");
        let legacy_manifest = dir.path().join(LEGACY_MANIFEST_FILENAME);
        fs::write(&legacy_manifest, br#"{"entries":{"photo:1:1:x":{}}}"#).expect("write manifest");

        // Simulate a v0 plaintext index.sqlite3.
        {
            let conn = Connection::open(dir.path().join("index.sqlite3")).expect("open legacy db");
            conn.execute_batch(
                "CREATE TABLE media_cache (
                    key TEXT PRIMARY KEY NOT NULL,
                    rel_path TEXT NOT NULL,
                    size INTEGER NOT NULL,
                    tag INTEGER NOT NULL,
                    last_accessed INTEGER NOT NULL,
                    created_at INTEGER NOT NULL
                 );
                 INSERT INTO media_cache VALUES ('photo:1:1:x', 'blobs/aa/legacy_plain', 22, 1, 1, 1);
                 PRAGMA user_version = 0;",
            )
            .expect("seed legacy db");
        }

        let key = derive_media_cache_key(&test_local_key(0x77), "data");
        let cache = MediaCacheIndex::open_with_key(dir.path(), key).expect("migrate open");

        assert!(
            !legacy_blob.exists(),
            "legacy plaintext blob must be deleted"
        );
        assert!(
            !legacy_manifest.exists(),
            "legacy plaintext cache_meta.json must be deleted"
        );
        assert_eq!(
            cache.stats().expect("stats").entry_count,
            0,
            "legacy unencrypted rows must be purged on migration"
        );
    }

    #[test]
    fn d1_memory_only_cache_creates_no_files_on_disk() {
        let cache = MediaCacheIndex::open_memory_only().expect("open memory only");
        assert!(!cache.is_disk_backed());
        assert_eq!(cache.root_dir(), Path::new(""));
        cache
            .put_blob("photo:2:7:m", MediaCacheTag::Photo, b"in-ram-only")
            .expect("put");
        assert_eq!(
            cache.get_blob("photo:2:7:m").expect("get").as_deref(),
            Some(&b"in-ram-only"[..])
        );
        assert_eq!(cache.stats_by_tag().expect("tag stats").photos_bytes, 11);
    }

    #[test]
    fn rekey_and_key_mismatch_purge_leave_no_unreadable_entries() {
        let dir = TempDir::new().expect("tempdir");
        let key_v1 = derive_media_cache_key(&test_local_key(0x11), "data");
        let key_v2 = derive_media_cache_key(&test_local_key(0x22), "data");
        let key_v3 = derive_media_cache_key(&test_local_key(0x33), "data");
        let key_v4 = derive_media_cache_key(&test_local_key(0x44), "data");

        let dl_path = dir.path().join("saved_report.pdf");
        let old_blob_path;
        {
            let mut cache = MediaCacheIndex::open_with_key(dir.path(), key_v1.clone()).expect("v1");
            let entry = cache
                .put_blob("photo:2:500:m", MediaCacheTag::Photo, b"payload-v1")
                .expect("put v1");
            old_blob_path = dir.path().join(&entry.rel_path);
            assert!(old_blob_path.is_file());
            let streamed = dir.path().join("streamed");
            fs::create_dir_all(&streamed).expect("mkdir streamed");
            fs::write(streamed.join("doc_500.part"), b"encrypted-part").expect("write part");
            cache
                .upsert_file_location(&LocalFileRecord {
                    key_high: 1,
                    key_low: 2,
                    path: dl_path.clone(),
                    size: 128,
                    modified_ms: 1_700_000_000_000,
                })
                .expect("upsert loc");

            // Rekey from v1 -> v2 preserves file_locations, purges old blobs/streamed, updates registry.
            let preserved = cache.rekey(key_v2.clone()).expect("rekey v1->v2");
            assert_eq!(preserved, 1);
            assert_eq!(cache.stats().expect("stats").entry_count, 0);
            assert_eq!(cache.stats().expect("stats").total_bytes, 0);
            assert!(
                !old_blob_path.exists(),
                "old-key blob must be deleted on rekey"
            );
            assert!(
                !streamed.exists(),
                "old-key streamed partials must be deleted on rekey"
            );
            assert_eq!(
                cache.get_file_location(1, 2).expect("loc").map(|r| r.path),
                Some(dl_path.clone()),
                "file_locations must be re-encrypted under new key"
            );
            assert_eq!(
                lookup_cache_key_for_path(dir.path())
                    .as_ref()
                    .map(|k| k.as_bytes()),
                Some(key_v2.as_bytes())
            );
            cache
                .put_blob("photo:2:501:m", MediaCacheTag::Photo, b"payload-v2")
                .expect("put v2");
        }

        // Opening with a mismatched key v3 without rekey purges unreadable entries instead of
        // leaving phantom stats or orphaned blobs on disk.
        let v2_blob_path;
        {
            let cache_v2 = MediaCacheIndex::open_with_key(dir.path(), key_v2.clone()).expect("v2");
            assert_eq!(
                cache_v2
                    .get_blob("photo:2:501:m")
                    .expect("get v2")
                    .as_deref(),
                Some(&b"payload-v2"[..])
            );
            let entry = cache_v2
                .put_blob("photo:2:502:m", MediaCacheTag::Photo, b"payload-v2b")
                .expect("put v2b");
            v2_blob_path = dir.path().join(&entry.rel_path);
            assert!(v2_blob_path.is_file());
        }
        {
            let cache_v3 = MediaCacheIndex::open_with_key(dir.path(), key_v3.clone()).expect("v3");
            assert_eq!(
                cache_v3.stats().expect("stats v3").entry_count,
                0,
                "opening with a rotated key must purge unreadable index rows"
            );
            assert_eq!(cache_v3.stats().expect("stats v3").total_bytes, 0);
            assert!(
                !v2_blob_path.exists(),
                "opening with a rotated key must delete unreadable blob files"
            );
            assert_eq!(cache_v3.get_file_location(1, 2).expect("loc v3"), None);
            cache_v3
                .put_blob("photo:2:503:m", MediaCacheTag::Photo, b"payload-v3")
                .expect("put v3");
            cache_v3
                .upsert_file_location(&LocalFileRecord {
                    key_high: 3,
                    key_low: 4,
                    path: dl_path.clone(),
                    size: 256,
                    modified_ms: 1_700_000_001_000,
                })
                .expect("upsert loc v3");
        }

        // `rekey_or_purge_cache_dir` with old_key=v3 -> new_key=v4 preserves file_locations, purges blobs.
        rekey_or_purge_cache_dir(dir.path(), Some(&key_v3), key_v4.clone()).expect("rekey dir");
        assert_eq!(
            lookup_cache_key_for_path(dir.path())
                .as_ref()
                .map(|k| k.as_bytes()),
            Some(key_v4.as_bytes())
        );
        let cache_v4 = MediaCacheIndex::open_with_key(dir.path(), key_v4).expect("v4");
        assert_eq!(cache_v4.stats().expect("stats v4").entry_count, 0);
        assert_eq!(
            cache_v4
                .get_file_location(3, 4)
                .expect("loc v4")
                .map(|r| r.path),
            Some(dl_path)
        );
    }

    #[test]
    fn sec3_b2_re_registering_a_directory_replaces_its_key() {
        let root = TempDir::new().expect("tempdir");
        let dir = root.path().join("bot_storage").join("user_1");
        let first = derive_media_cache_key(&test_local_key(0x71), "data");
        let second = derive_media_cache_key(&test_local_key(0x72), "data");

        // Registered before the directory exists, then again once it exists.
        register_cache_key(&dir, first);
        fs::create_dir_all(&dir).expect("mkdir");
        register_cache_key(&dir, second.clone());
        assert_eq!(
            lookup_cache_key_for_path(&dir.join("bot_storage_1.tdef"))
                .as_ref()
                .map(|k| k.as_bytes()),
            Some(second.as_bytes()),
            "the replaced key must not be returned"
        );
        unregister_cache_key(&dir);
        assert!(lookup_cache_key_for_path(&dir).is_none());
    }

    fn xor_at(bytes: &mut [u8], at: usize, delta: &[u8]) {
        for (b, d) in bytes[at..at + delta.len()].iter_mut().zip(delta) {
            *b ^= *d;
        }
    }

    fn xor_pair(a: &[u8], b: &[u8]) -> Vec<u8> {
        a.iter().zip(b).map(|(x, y)| x ^ y).collect()
    }

    #[test]
    fn sec3_b3_cache_blob_rewritten_without_the_key_is_dropped() {
        let dir = TempDir::new().expect("tempdir");
        let key = derive_media_cache_key(&test_local_key(0x51), "data");
        let cache = MediaCacheIndex::open_with_key(dir.path(), key).expect("open");

        // The attacker knows the cached plaintext (a document they sent) and can write the cache
        // folder, but does not have the key. AES-CTR is malleable: XOR-ing the ciphertext with
        // `old ^ new` turns the content into `new`, and the same trick rewrites an unkeyed
        // digest stored next to it. That was enough for the old SHA-256 format.
        let known: &[u8] = b"INVOICE_ORIGINAL_CONTENT_0001.pdf";
        let forged: &[u8] = b"INVOICE_REPLACED_BY_ATTACKER.exe!";
        assert_eq!(known.len(), forged.len());
        let entry = cache
            .put_blob("doc:2:4242", MediaCacheTag::Document, known)
            .expect("put");
        let blob_path = dir.path().join(&entry.rel_path);

        let mut raw = fs::read(&blob_path).expect("read blob");
        let frame_at = crate::encrypted_file::TDEF_DATA_START_OFFSET as usize;
        xor_at(
            &mut raw,
            frame_at + 8,
            &xor_pair(&sha256(&[known]), &sha256(&[forged])),
        );
        xor_at(
            &mut raw,
            frame_at + BLOB_FRAME_HEADER_LEN,
            &xor_pair(known, forged),
        );
        fs::write(&blob_path, &raw).expect("write forged blob");

        assert_eq!(
            cache.get_blob("doc:2:4242").expect("get forged"),
            None,
            "a blob rewritten without the key must not be returned"
        );
        assert!(!blob_path.exists(), "the forged blob must be deleted");
        assert_eq!(cache.stats().expect("stats").entry_count, 0);

        // A plain bit flip in the content is rejected too.
        let entry = cache
            .put_blob("doc:2:4242", MediaCacheTag::Document, known)
            .expect("put again");
        let blob_path = dir.path().join(&entry.rel_path);
        let mut raw = fs::read(&blob_path).expect("read blob");
        raw[frame_at + BLOB_FRAME_HEADER_LEN + 3] ^= 0x01;
        fs::write(&blob_path, &raw).expect("write flipped blob");
        assert_eq!(cache.get_blob("doc:2:4242").expect("get flipped"), None);
    }

    #[test]
    fn sec3_b3_cache_blob_cannot_be_swapped_between_entries() {
        let dir = TempDir::new().expect("tempdir");
        let key = derive_media_cache_key(&test_local_key(0x52), "data");
        let cache = MediaCacheIndex::open_with_key(dir.path(), key.clone()).expect("open");

        let wanted = cache
            .put_blob(
                "doc:2:1",
                MediaCacheTag::Document,
                b"the document the user asked for",
            )
            .expect("put wanted");
        let other = cache
            .put_blob(
                "doc:2:2",
                MediaCacheTag::Document,
                b"another valid cached document",
            )
            .expect("put other");
        // Both files are valid blobs under the same key; copy one over the other.
        fs::copy(
            dir.path().join(&other.rel_path),
            dir.path().join(&wanted.rel_path),
        )
        .expect("swap blobs");

        assert_eq!(
            cache.get_blob("doc:2:1").expect("get swapped"),
            None,
            "a blob of another entry must not be returned"
        );
        assert_eq!(
            cache.get_blob("doc:2:2").expect("get other").as_deref(),
            Some(&b"another valid cached document"[..])
        );

        // The standalone helpers bind to their context the same way.
        let path = dir.path().join("sidecar.tdef");
        write_encrypted_blob_bound(&path, &key, b"owner-1", b"payload").expect("write bound");
        assert_eq!(
            read_encrypted_blob_bound(&path, &key, b"owner-1").expect("read bound"),
            b"payload"
        );
        assert!(matches!(
            read_encrypted_blob_bound(&path, &key, b"owner-2"),
            Err(StorageError::ChecksumMismatch)
        ));
        assert!(matches!(
            read_encrypted_blob(&path, &key),
            Err(StorageError::ChecksumMismatch)
        ));
    }

    #[test]
    fn sec3_b3_stream_file_names_are_keyed_and_old_names_are_purged() {
        let dir = TempDir::new().expect("tempdir");
        let key_a = derive_media_cache_key(&test_local_key(0x53), "data");
        let key_b = derive_media_cache_key(&test_local_key(0x54), "data");
        let doc_id = 0x0000_0000_000d_bf89_u64;

        let stem = keyed_stream_file_stem(&key_a, doc_id, 2);
        assert!(stem.starts_with(STREAM_FILE_PREFIX));
        assert_eq!(stem, keyed_stream_file_stem(&key_a, doc_id, 2));
        assert_ne!(stem, keyed_stream_file_stem(&key_b, doc_id, 2));
        assert_ne!(stem, keyed_stream_file_stem(&key_a, doc_id, 4));
        assert!(
            !stem.contains("dbf89"),
            "the file name must not carry the document id"
        );

        // Files that older builds named after the document id are removed when the cache opens.
        drop(MediaCacheIndex::open_with_key(dir.path(), key_a.clone()).expect("create"));
        let streamed = dir.path().join("streamed");
        fs::create_dir_all(&streamed).expect("mkdir streamed");
        let old_final = streamed.join("stream_00000000000dbf89_0000000000000002.bin");
        let old_part = streamed.join("stream_00000000000dbf89_0000000000000002.bin.part");
        let old_meta = streamed.join("stream_00000000000dbf89_0000000000000002.bin.part.meta");
        let current = streamed.join(format!("{stem}.bin"));
        for path in [&old_final, &old_part, &old_meta, &current] {
            fs::write(path, b"x").expect("write");
        }
        drop(MediaCacheIndex::open_with_key(dir.path(), key_a).expect("reopen"));
        assert!(!old_final.exists());
        assert!(!old_part.exists());
        assert!(!old_meta.exists());
        assert!(current.exists(), "current-format names are kept");
    }
}

#[cfg(test)]
mod tests_deferred_touch {
    use super::*;
    use tempfile::TempDir;

    fn last_accessed(cache: &MediaCacheIndex, key: &str) -> i64 {
        let db_key = encode_hex(&keyed_entry_id(cache.encryption_key(), key));
        cache
            .conn
            .query_row(
                "SELECT last_accessed FROM media_cache WHERE key = ?1",
                params![db_key],
                |row| row.get(0),
            )
            .expect("row")
    }

    #[test]
    fn deferred_reads_keep_lru_order_until_the_batched_touch() {
        let dir = TempDir::new().expect("tempdir");
        let cache = MediaCacheIndex::open(dir.path()).expect("open");
        cache
            .put_blob("photo:2:1:x", MediaCacheTag::Photo, &[1u8; 400])
            .expect("put a");
        cache
            .put_blob("photo:2:2:x", MediaCacheTag::Photo, &[2u8; 400])
            .expect("put b");
        cache
            .conn
            .execute("UPDATE media_cache SET last_accessed = 1000", [])
            .expect("age rows");

        // The read itself writes nothing.
        assert_eq!(
            cache
                .get_blob_deferred_touch("photo:2:1:x")
                .expect("read")
                .as_deref(),
            Some(&[1u8; 400][..])
        );
        assert_eq!(last_accessed(&cache, "photo:2:1:x"), 1000);

        // One batched touch records it; an unknown key updates nothing.
        assert_eq!(
            cache
                .touch_keys(["photo:2:1:x", "photo:2:404:x"])
                .expect("touch"),
            1
        );
        assert!(last_accessed(&cache, "photo:2:1:x") > 1000);

        // LRU pruning now evicts the untouched entry, exactly like after a touching get_blob.
        assert_eq!(cache.prune(Some(400), None).expect("prune"), 1);
        assert!(
            cache
                .get_blob_deferred_touch("photo:2:1:x")
                .expect("a")
                .is_some()
        );
        assert!(
            cache
                .get_blob_deferred_touch("photo:2:2:x")
                .expect("b")
                .is_none()
        );
    }
}
