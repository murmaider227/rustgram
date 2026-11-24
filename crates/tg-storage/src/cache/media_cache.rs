//! Rustgram-side SQLite media cache index and downloaded file location registry.
//!
//! Unlike `tdata` files (which stay strictly byte-compatible with tdesktop), Rustgram's
//! own media cache index uses bundled SQLite (`rusqlite`) for O(1) lookups, atomic LRU
//! eviction, and tracking both cached media blobs and user-downloaded files on disk.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Result;

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
    /// File size in bytes.
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

/// SQLite-backed media cache and local file location index for Rustgram.
pub struct MediaCacheIndex {
    root_dir: PathBuf,
    conn: Connection,
}

impl MediaCacheIndex {
    /// Open or initialize a Rustgram media cache index in `root_dir` (`index.sqlite3` + `blobs/`).
    pub fn open(root_dir: impl Into<PathBuf>) -> Result<Self> {
        let root_dir = root_dir.into();
        fs::create_dir_all(root_dir.join("blobs"))?;
        let conn = Connection::open(root_dir.join("index.sqlite3"))?;
        Self::init_schema(&conn)?;
        Ok(Self { root_dir, conn })
    }

    /// Open an in-memory index rooted at `root_dir` (handy for unit tests).
    pub fn open_in_memory(root_dir: impl Into<PathBuf>) -> Result<Self> {
        let root_dir = root_dir.into();
        fs::create_dir_all(root_dir.join("blobs"))?;
        let conn = Connection::open_in_memory()?;
        Self::init_schema(&conn)?;
        Ok(Self { root_dir, conn })
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
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
             );",
        )?;
        Ok(())
    }

    /// Root cache directory.
    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    /// Store `bytes` under `key` with category `tag`, overwriting any previous entry.
    pub fn put_blob(&self, key: &str, tag: MediaCacheTag, bytes: &[u8]) -> Result<MediaCacheEntry> {
        let hash = tg_crypto::sha256(&[key.as_bytes()]);
        let shard = format!("{:02x}", hash[0]);
        let hex_name: String = hash[..16].iter().map(|b| format!("{b:02x}")).collect();
        let rel_path = format!("blobs/{shard}/{hex_name}");
        let full_path = self.root_dir.join(&rel_path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&full_path, bytes)?;

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
            params![key, rel_path, size as i64, tag as u8 as i64, now, now],
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

    /// Read a cached blob by `key`, updating its `last_accessed` timestamp.
    pub fn get_blob(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let rel_path: Option<String> = self
            .conn
            .query_row(
                "SELECT rel_path FROM media_cache WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;

        let Some(rel) = rel_path else {
            return Ok(None);
        };
        let full_path = self.root_dir.join(&rel);
        match fs::read(&full_path) {
            Ok(bytes) => {
                let _ = self.conn.execute(
                    "UPDATE media_cache SET last_accessed = ?1 WHERE key = ?2",
                    params![now_unix_secs(), key],
                );
                Ok(Some(bytes))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let _ = self
                    .conn
                    .execute("DELETE FROM media_cache WHERE key = ?1", params![key]);
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a cached blob by `key`.
    pub fn remove_blob(&self, key: &str) -> Result<bool> {
        let rel_path: Option<String> = self
            .conn
            .query_row(
                "SELECT rel_path FROM media_cache WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        let Some(rel) = rel_path else {
            return Ok(false);
        };
        let _ = fs::remove_file(self.root_dir.join(rel));
        self.conn
            .execute("DELETE FROM media_cache WHERE key = ?1", params![key])?;
        Ok(true)
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
            for (k, rel) in expired {
                let _ = fs::remove_file(self.root_dir.join(rel));
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
                for (k, rel, sz) in rows {
                    if current <= limit {
                        break;
                    }
                    let _ = fs::remove_file(self.root_dir.join(rel));
                    self.conn
                        .execute("DELETE FROM media_cache WHERE key = ?1", params![k])?;
                    current = current.saturating_sub(sz.max(0) as u64);
                    evicted += 1;
                }
            }
        }

        Ok(evicted)
    }

    /// Register or update a local downloaded file location.
    pub fn upsert_file_location(&self, record: &LocalFileRecord) -> Result<()> {
        let path_str = record.path.to_string_lossy();
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
                path_str.as_ref(),
                record.size as i64,
                record.modified_ms,
            ],
        )?;
        Ok(())
    }

    /// Look up a local downloaded file location by `(key_high, key_low)`.
    pub fn get_file_location(
        &self,
        key_high: u64,
        key_low: u64,
    ) -> Result<Option<LocalFileRecord>> {
        self.conn
            .query_row(
                "SELECT key_high, key_low, path, size, modified_ms
                 FROM file_locations WHERE key_high = ?1 AND key_low = ?2",
                params![key_high as i64, key_low as i64],
                |row| {
                    let kh: i64 = row.get(0)?;
                    let kl: i64 = row.get(1)?;
                    let p: String = row.get(2)?;
                    let sz: i64 = row.get(3)?;
                    let m: i64 = row.get(4)?;
                    Ok(LocalFileRecord {
                        key_high: kh as u64,
                        key_low: kl as u64,
                        path: PathBuf::from(p),
                        size: sz.max(0) as u64,
                        modified_ms: m,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Remove a registered local file location.
    pub fn remove_file_location(&self, key_high: u64, key_low: u64) -> Result<bool> {
        let rows = self.conn.execute(
            "DELETE FROM file_locations WHERE key_high = ?1 AND key_low = ?2",
            params![key_high as i64, key_low as i64],
        )?;
        Ok(rows > 0)
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
}
