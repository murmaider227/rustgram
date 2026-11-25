//! Security review #3 (SEC3-D) regression tests for account slots and local storage.

use super::passcode_and_settings_tests::{synthetic_user, test_creds};
use super::*;
use tempfile::TempDir;
use tg_storage::cache::media_cache::{MediaCacheIndex, MediaCacheTag};

fn user_with_id(id: i64) -> tl_types::User {
    let mut user = synthetic_user();
    user.id = id;
    user
}

/// D1: a cleanup queued for a slot must never delete data of a newer account in that slot.
#[test]
fn sec3_d1_pending_cleanup_never_deletes_new_account_in_reused_slot() {
    let tmp = TempDir::new().expect("tempdir");
    let opts = WorkingDirOpenOptions::new(tmp.path(), test_creds());
    let mut mgr = AccountManager::open_and_unlock(opts, b"").expect("open");
    let tdata = tmp.path().join("tdata");
    let file_part = mgr.account_key_file_part(0);
    mgr.complete_login(0, &user_with_id(777_001))
        .expect("first login");

    // The first account's logout could not delete two folders: one queued by this build
    // (tagged live path) and one queued by an older build (untagged live path).
    let webview = tdata.join("webview_profiles").join(&file_part);
    record_pending_cleanup(&tdata, &webview);
    let mut queue = read_pending_queue(&tdata);
    queue.push(file_part.clone());
    write_pending_queue(&tdata, &queue);

    // A different account signs in to the same slot and creates its own data there.
    mgr.complete_login(0, &user_with_id(777_002))
        .expect("second login");
    fs::create_dir_all(&webview).expect("mkdir webview");
    fs::write(webview.join("cookies"), b"new-account").expect("seed webview");
    let map_dir = tdata.join(&file_part);
    assert!(map_dir.is_dir(), "the new account's map folder exists");

    process_pending_cleanups(&tdata);

    assert!(
        webview.join("cookies").is_file(),
        "a queued retry must not delete the new account's webview profile"
    );
    assert!(
        map_dir.is_dir(),
        "an untagged entry from an older build must not delete the new account's folder"
    );
    assert!(
        !tdata.join(PENDING_CLEANUP_FILENAME).exists(),
        "cancelled and untagged entries leave the queue"
    );
}

/// D1: tombstones are deleted on the next retry, and tagged live paths of a slot nobody signed
/// in to since the logout are still removed.
#[test]
fn sec3_d1_queue_retries_tombstones_and_unclaimed_live_paths() {
    let tmp = TempDir::new().expect("tempdir");
    let tdata = tmp.path().join("tdata");
    let tombstone = tdata
        .join("media_cache")
        .join("ABCDEF.deleted-0123456789ab");
    let live = tdata.join("webview_profiles").join("0011223344556677");
    for dir in [&tombstone, &live] {
        fs::create_dir_all(dir).expect("mkdir");
        fs::write(dir.join("f"), b"old").expect("seed");
    }
    record_pending_cleanup(&tdata, &tombstone);
    record_pending_cleanup(&tdata, &live);
    assert_eq!(
        read_pending_queue(&tdata),
        vec![
            "media_cache/ABCDEF.deleted-0123456789ab".to_owned(),
            "live:webview_profiles/0011223344556677".to_owned(),
        ]
    );

    assert_eq!(process_pending_cleanups(&tdata), 2);
    assert!(!tombstone.exists());
    assert!(!live.exists());
    assert!(!tdata.join(PENDING_CLEANUP_FILENAME).exists());

    // A tombstone that never made it into the queue is swept as well.
    let orphan = tdata.join("0011223344556677.deleted-aabbccddeeff");
    fs::create_dir_all(&orphan).expect("mkdir orphan");
    assert_eq!(process_pending_cleanups(&tdata), 1);
    assert!(!orphan.exists());
}

/// D1: the media cache folder and key are bound to a per-login generation, so an account that
/// takes over a slot whose old cache could not be deleted neither opens nor decrypts it.
#[test]
fn sec3_d1_new_login_in_reused_slot_gets_fresh_cache_dir_and_key() {
    let tmp = TempDir::new().expect("tempdir");
    let opts = WorkingDirOpenOptions::new(tmp.path(), test_creds());
    let mut mgr = AccountManager::open_and_unlock(opts, b"").expect("open");
    let tdata = tmp.path().join("tdata");

    mgr.complete_login(0, &user_with_id(777_001))
        .expect("first login");
    let dir_a = mgr.account_media_cache_dir(0);
    let key_a = mgr.account_cache_key(0).expect("key a");
    assert_ne!(
        dir_a,
        tdata.join("media_cache").join(mgr.account_key_file_part(0)),
        "a login made by this build uses a generation-bound folder"
    );

    // Logout wipes the slot, but the cache folder is held open and survives.
    mgr.wipe_account_files_for(0, Some(UserId(777_001)), true);
    {
        let cache = MediaCacheIndex::open_with_key(&dir_a, key_a.clone()).expect("open a");
        cache
            .put_blob("photo:1:1:m", MediaCacheTag::Photo, b"first-account-photo")
            .expect("put");
    }
    assert!(dir_a.is_dir());

    mgr.complete_login(0, &user_with_id(777_002))
        .expect("second login");
    let dir_b = mgr.account_media_cache_dir(0);
    let key_b = mgr.account_cache_key(0).expect("key b");
    assert_ne!(
        dir_b, dir_a,
        "new login must not reuse the old cache folder"
    );
    assert_ne!(
        key_b.as_bytes(),
        key_a.as_bytes(),
        "new login must not reuse the old cache key"
    );
    assert!(
        !dir_a.exists(),
        "the unlocked leftover cache is removed at the new login"
    );

    // The host resolves the same folder and key through the free functions.
    let data_name = mgr.domain.storage().data_name().to_owned();
    assert_eq!(
        account_media_cache_dir_for_tdata(&tdata, &data_name, 0),
        dir_b
    );
    assert_eq!(
        resolve_account_cache_key_for_tdata(&tdata, &data_name, 0).map(|k| k.as_bytes().to_vec()),
        Some(key_b.as_bytes().to_vec())
    );
}
