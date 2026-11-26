//! TGB-3019: the media cache index written by the previous release (rusqlite 0.32 with bundled
//! SQLite 3.46, `tests/fixtures/media_cache_rusqlite_0_32`) opens and reads unchanged with the
//! upgraded rusqlite, and stays writable and readable by the new code afterwards.
//!
//! The fixture was written by `MediaCacheIndex::open_with_key` of tg-storage at master 43b63657
//! with the 64-byte key `0, 1, ..., 63`: blobs `photo:1001` (19 bytes, Photo) and `sticker:2002`
//! (4096 x 7, Sticker) and one file location.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tempfile::TempDir;
use tg_storage::StorageEncryptionKey;
use tg_storage::cache::media_cache::{LocalFileRecord, MediaCacheIndex, MediaCacheTag};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media_cache_rusqlite_0_32")
}

fn fixture_key() -> StorageEncryptionKey {
    StorageEncryptionKey::new((0u8..64).collect()).expect("64-byte key")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create dir");
    for entry in fs::read_dir(from).expect("read fixture dir") {
        let entry = entry.expect("fixture entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

fn open_copy() -> (TempDir, MediaCacheIndex) {
    let dir = TempDir::new().expect("tempdir");
    copy_dir(&fixture_dir(), dir.path());
    let cache = MediaCacheIndex::open_with_key(dir.path(), fixture_key()).expect("open old index");
    (dir, cache)
}

#[test]
fn old_sqlite_file_passes_integrity_check_under_new_sqlite() {
    let dir = TempDir::new().expect("tempdir");
    copy_dir(&fixture_dir(), dir.path());
    let conn = Connection::open(dir.path().join("index.sqlite3")).expect("open");
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("integrity_check");
    assert_eq!(integrity, "ok");
    let user_version: i32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("user_version");
    assert_eq!(user_version, 2, "fixture is a schema v2 encrypted cache");
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM media_cache", [], |row| row.get(0))
        .expect("count");
    assert_eq!(rows, 2);
}

#[test]
fn media_cache_written_by_rusqlite_0_32_reads_back() {
    let (_dir, cache) = open_copy();

    // A key mismatch or a failed migration would purge the cache: the entries must survive.
    let stats = cache.stats().expect("stats");
    assert_eq!(stats.entry_count, 2);
    assert_eq!(stats.total_bytes, 19 + 4096);

    assert_eq!(
        cache.get_blob("photo:1001").expect("get photo").as_deref(),
        Some(&b"fixture photo bytes"[..])
    );
    assert_eq!(
        cache.get_blob("sticker:2002").expect("get sticker"),
        Some(vec![7u8; 4096])
    );

    let location = cache
        .get_file_location(0x0123_4567_89ab_cdef, 42)
        .expect("get location")
        .expect("location kept");
    assert_eq!(location.path, PathBuf::from("downloads/fixture.bin"));
    assert_eq!(location.size, 123);
    assert_eq!(location.modified_ms, 1_700_000_000_000);
}

#[test]
fn old_media_cache_accepts_new_writes_and_reopens() {
    let (dir, cache) = open_copy();
    cache
        .put_blob("voice:3003", MediaCacheTag::Voice, b"new entry")
        .expect("put");
    cache
        .upsert_file_location(&LocalFileRecord {
            key_high: 1,
            key_low: 2,
            path: PathBuf::from("downloads/new.bin"),
            size: 9,
            modified_ms: 1,
        })
        .expect("upsert");
    assert!(cache.remove_blob("photo:1001").expect("remove"));
    drop(cache);

    let reopened = MediaCacheIndex::open_with_key(dir.path(), fixture_key()).expect("reopen");
    assert_eq!(reopened.stats().expect("stats").entry_count, 2);
    assert_eq!(
        reopened.get_blob("voice:3003").expect("get").as_deref(),
        Some(&b"new entry"[..])
    );
    assert_eq!(
        reopened.get_blob("sticker:2002").expect("get"),
        Some(vec![7u8; 4096])
    );
    assert!(reopened.get_blob("photo:1001").expect("get").is_none());
    assert!(
        reopened
            .get_file_location(0x0123_4567_89ab_cdef, 42)
            .expect("get")
            .is_some()
    );
}
