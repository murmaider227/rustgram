//! Known-answer and async integration tests for `cache` and `transfer` modules of `tg-media`.

use std::collections::BTreeSet;
use std::time::Duration;
use tg_media::MediaError;
use tg_media::cache::{
    ANIMATION_CACHE_TAG, CacheSettings, IMAGE_CACHE_TAG, LocalFileCache, MediaLocationType,
    STICKER_CACHE_TAG, StorageKey, StorageLocationKeyInput, StorageLocationType,
    audio_album_thumb_cache_key, big_file_base_cache_key, decode_partial_payload,
    document_cache_key, document_thumb_cache_key, encode_partial_payload, geo_point_cache_key,
    media_key, sha1_in_memory_key, storage_file_cache_key, url_cache_key, web_document_cache_key,
};
use tg_media::transfer::download::{
    BAD_REQUEST_DURATION_MS, CancelToken, DcBalanceState, DownloadQueue, DownloadRequest,
    DownloadScheduler, MAX_SESSIONS_COUNT, MAX_WAITED_IN_SESSION, PartAssembler,
    REMOVE_SESSION_AFTER_TIMEOUTS, RETRY_ADD_SESSION_SUCCESSES, RETRY_ADD_SESSION_TIMEOUT_MS,
    START_WAITED_IN_SESSION, download_file,
};
use tg_media::transfer::streamed::{
    LoadedPart, MAX_STREAMED_REQUEST_PARTS, STREAMING_PART_SIZE, SpeedEstimator,
    StreamedFileDownloaderState, StreamingPriorityQueue,
};
use tg_media::transfer::upload::{
    BIG_FILE_THRESHOLD, FAST_REQUEST_THRESHOLD_MS, MAX_FILE_SIZE_DEFAULT, MAX_FILE_SIZE_PREMIUM,
    MAX_UPLOAD_PARTS_DEFAULT, MAX_UPLOAD_PARTS_PREMIUM, PHOTO_UPLOAD_PART_SIZE,
    SLOW_REQUEST_THRESHOLD_MS, TDESKTOP_USE_BIG_FILES_FROM, UPLOAD_PART_SIZE_32K,
    UPLOAD_PART_SIZE_64K, UPLOAD_PART_SIZE_128K, UPLOAD_PART_SIZE_256K, UPLOAD_PART_SIZE_512K,
    UploadConfig, UploadPlan, UploadSessionBalancer, UploadedInputFile, choose_upload_part_size,
    compute_md5_hex, upload_bytes,
};
use tg_media::transfer::{
    CDN_HASH_CHUNK_SIZE, CdnFileHash, CdnHashVerifier, CheckCdnHashResult, DOWNLOAD_PART_SIZE,
    FakeCdnFile, FakeMediaInvoker, TransferLocation, align_download_offset, apply_cdn_aes_ctr,
    cdn_ctr_iv, choose_download_part_size, validate_get_cdn_file_request,
    validate_get_file_request,
};

// ============================================================================
// Cache keys & LocalFileCache (LRU, size limits, expiration, persistence)
// ============================================================================

#[test]
fn storage_cache_keys_match_tdesktop_formulas() {
    // DocumentCacheKey(dc=2, id=0x123456789ABCDEF0) -> { 0x102, 0x123456789ABCDEF0 }
    let doc_key = document_cache_key(2, 0x1234_5678_9ABC_DEF0);
    assert_eq!(doc_key, StorageKey::new(0x0102, 0x1234_5678_9ABC_DEF0));
    assert_eq!(StorageKey::from_hex(&doc_key.to_hex()), Some(doc_key));

    // DocumentThumbCacheKey(dc=4, id=42) -> { 0x204, 42 }
    assert_eq!(document_thumb_cache_key(4, 42), StorageKey::new(0x0204, 42));

    // AudioAlbumThumbCacheKey(99) -> { 0x300, 99 }
    assert_eq!(audio_album_thumb_cache_key(99), StorageKey::new(0x0300, 99));

    // StorageFileLocation::cacheKey for Document with size_letter = 0 delegates to DocumentCacheKey.
    let loc_doc = StorageLocationKeyInput {
        kind: StorageLocationType::Document,
        dc_id: 2,
        id: 0x1234_5678_9ABC_DEF0,
        volume_id: 0,
        local_id: 0,
        size_letter: 0,
    };
    assert_eq!(storage_file_cache_key(loc_doc), doc_key);

    // StorageFileLocation::cacheKey for Document thumbnail ('m'): shifted = (2+3)<<8 = 0x500, letter<<16 = 'm'<<16
    let loc_doc_thumb = StorageLocationKeyInput {
        size_letter: b'm',
        ..loc_doc
    };
    assert_eq!(
        storage_file_cache_key(loc_doc_thumb),
        StorageKey::new(0x0502 | (u64::from(b'm') << 16), 0x1234_5678_9ABC_DEF0)
    );

    // StorageFileLocation::cacheKey for Photo ('y'): shifted = (5+3)<<8 = 0x800
    let loc_photo = StorageLocationKeyInput {
        kind: StorageLocationType::Photo,
        dc_id: 5,
        id: 777,
        volume_id: 0,
        local_id: 0,
        size_letter: b'y',
    };
    assert_eq!(
        storage_file_cache_key(loc_photo),
        StorageKey::new(0x0805 | (u64::from(b'y') << 16), 777)
    );

    // StorageFileLocation::cacheKey for Legacy (volume_id + local_id): shifted = (0+3)<<8 = 0x300
    let loc_legacy = StorageLocationKeyInput {
        kind: StorageLocationType::Legacy,
        dc_id: 1,
        id: 0,
        volume_id: 9999,
        local_id: 123,
        size_letter: 0,
    };
    assert_eq!(
        storage_file_cache_key(loc_legacy),
        StorageKey::new(0x0301 | (123u64 << 16), 9999)
    );

    // bigFileBaseCacheKey leaves lower 16 bits of `low` zero for Document/Photo.
    let base_doc = big_file_base_cache_key(loc_doc).expect("doc base key");
    assert_eq!(base_doc.low & 0xFFFF, 0);
    assert_eq!(base_doc.high & 0xFFFF_0000, 0x0001_0000);
    assert_eq!(base_doc.with_part_offset(3).low, base_doc.low + 3);

    // WebDocument, URL, GeoPoint, MediaKey, SHA-1 in-memory key.
    let web_k = web_document_cache_key(b"https://example.com/a.jpg");
    assert_eq!(web_k.high >> 32, 0x0204); // tag 0x200 << 32 | dc 4 << 32 -> (high >> 32) == 0x204
    let url_k = url_cache_key("https://example.com/a.jpg");
    assert_eq!(url_k.high & 0x0000_0300_0000_0000, 0x0000_0300_0000_0000);
    assert_eq!(url_k.high >> 48, 0);
    let geo_k = geo_point_cache_key(37.7749, -122.4194, 300, 200, 15, 2);
    assert_eq!(
        geo_k.high,
        0x0000_0400_0000_0000 | (0x0F02u64 << 32) | ((300u64 << 16) | 200u64)
    );
    assert_eq!(
        media_key(MediaLocationType::Document, 2, 55),
        StorageKey::new((0x4e45_abe9u64 << 32) | 2, 55)
    );
    assert!(sha1_in_memory_key(b"hello").is_valid());

    // Partial payload encoding/decoding.
    let partial = encode_partial_payload(b"abc");
    assert_eq!(decode_partial_payload(&partial), Some(b"abc".as_slice()));
    assert_eq!(decode_partial_payload(b"normal"), None);
}

#[test]
fn local_file_cache_lru_eviction_tags_and_persistence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut settings = CacheSettings::new(dir.path());
    settings.total_size_limit = 100;
    settings.total_time_limit = Some(Duration::from_secs(60));

    let k1 = document_cache_key(1, 1);
    let k2 = document_cache_key(1, 2);
    let k3 = document_cache_key(1, 3);
    let k4 = document_cache_key(1, 4);

    let mut cache = LocalFileCache::open(settings.clone()).expect("open");
    cache
        .put_at(k1, IMAGE_CACHE_TAG, &[1u8; 40], 1_000)
        .expect("put k1");
    cache
        .put_at(k2, STICKER_CACHE_TAG, &[2u8; 40], 2_000)
        .expect("put k2");
    assert_eq!(cache.stats().full.count, 2);
    assert_eq!(cache.stats().full.total_size, 80);

    // Touch k1 so k2 becomes least-recently-used.
    assert_eq!(
        cache.get_at(k1, 3_000).expect("get k1"),
        Some(vec![1u8; 40])
    );

    // Insert k3 (40 bytes) -> total would be 120 > 100, so k2 must be evicted!
    cache
        .put_at(k3, IMAGE_CACHE_TAG, &[3u8; 40], 4_000)
        .expect("put k3");
    assert!(!cache.contains(k2));
    assert!(cache.contains(k1));
    assert!(cache.contains(k3));
    assert_eq!(cache.stats().full.total_size, 80);
    assert_eq!(cache.stats().tagged[&IMAGE_CACHE_TAG].count, 2);

    // copy_if_empty & put_if_empty.
    assert!(
        !cache
            .put_if_empty(k1, IMAGE_CACHE_TAG, &[9u8; 10])
            .expect("put_if_empty")
    );
    cache
        .put_at(k4, ANIMATION_CACHE_TAG, &[4u8; 10], 5_000)
        .expect("put k4");
    assert_eq!(
        cache.clear_by_tag(ANIMATION_CACHE_TAG).expect("clear tag"),
        1
    );
    assert!(!cache.contains(k4));

    // Expiration after 60 seconds.
    assert_eq!(cache.evict_expired(100_000).expect("evict expired"), 2);
    assert_eq!(cache.stats().full.count, 0);

    // Persistence across reopen.
    cache
        .put(k1, IMAGE_CACHE_TAG, b"persisted")
        .expect("put persisted");
    drop(cache);

    let mut reopened = LocalFileCache::open(settings).expect("reopen");
    assert_eq!(reopened.get(k1).expect("read"), Some(b"persisted".to_vec()));
    assert_eq!(reopened.stats().tagged[&IMAGE_CACHE_TAG].count, 1);
}

// ============================================================================
// Alignment rules, CDN crypto/hash verification, DownloadManager state machine
// ============================================================================

#[test]
fn download_alignment_and_cdn_crypto_rules() {
    // Valid non-precise upload.getFile calls (128KB, 256KB, 512KB, 1MB).
    assert!(validate_get_file_request(0, 128 * 1024, false).is_ok());
    assert!(validate_get_file_request(128 * 1024, 128 * 1024, false).is_ok());
    assert!(validate_get_file_request(512 * 1024, 512 * 1024, false).is_ok());
    assert!(validate_get_file_request(1024 * 1024, 1024 * 1024, false).is_ok());

    // Invalid non-precise: unaligned offset or non-divisor limit (e.g. 768KB).
    assert!(validate_get_file_request(4096, 128 * 1024, false).is_err());
    assert!(validate_get_file_request(0, 768 * 1024, false).is_err());
    assert!(validate_get_file_request(0, 2 * 1024 * 1024, false).is_err());

    // Precise mode: 4KB aligned offset allowed as long as it doesn't cross 1MB boundary.
    assert!(validate_get_file_request(4096, 128 * 1024, true).is_ok());
    assert!(validate_get_file_request(1024 * 1024 - 4096, 8192, true).is_err());
    assert!(validate_get_file_request(100, 4096, true).is_err());

    // CDN rules.
    assert!(validate_get_cdn_file_request(0, CDN_HASH_CHUNK_SIZE).is_ok());
    assert!(validate_get_cdn_file_request(1024 * 1024 - 4096, 128 * 1024).is_err());

    // Part size selection.
    assert_eq!(
        choose_download_part_size(500 * 1024 * 1024, true),
        DOWNLOAD_PART_SIZE
    );
    assert_eq!(choose_download_part_size(1024 * 1024, false), 128 * 1024);
    assert_eq!(
        choose_download_part_size(8 * 1024 * 1024, false),
        256 * 1024
    );
    assert_eq!(
        choose_download_part_size(32 * 1024 * 1024, false),
        512 * 1024
    );
    assert_eq!(
        choose_download_part_size(200 * 1024 * 1024, false),
        1024 * 1024
    );
    assert_eq!(align_download_offset(300_000, 128 * 1024), 262_144);

    // CDN AES-256-CTR IV derivation: offset 128 KiB = 0x20000 -> offset >> 4 = 0x2000 -> [0, 0, 0x20, 0x00].
    let base_iv = [1u8; 16];
    let iv = cdn_ctr_iv(&base_iv, 128 * 1024).expect("iv");
    assert_eq!(&iv[0..12], &[1u8; 12]);
    assert_eq!(&iv[12..16], &[0x00, 0x00, 0x20, 0x00]);
    assert!(cdn_ctr_iv(&base_iv, 7).is_err());

    // Encrypt/decrypt roundtrip + CdnHashVerifier.
    let key = [0x42u8; 32];
    let orig = vec![0xABu8; 128 * 1024];
    let mut buf = orig.clone();
    apply_cdn_aes_ctr(&mut buf, &key, &base_iv, 128 * 1024).expect("enc");
    assert_ne!(buf, orig);
    apply_cdn_aes_ctr(&mut buf, &key, &base_iv, 128 * 1024).expect("dec");
    assert_eq!(buf, orig);

    let mut verifier = CdnHashVerifier::new();
    assert_eq!(verifier.check(0, &orig), CheckCdnHashResult::NoHash);
    use sha2::{Digest as _, Sha256};
    verifier.add_hashes([CdnFileHash {
        offset: 0,
        limit: orig.len() as u32,
        hash: Sha256::digest(&orig).to_vec(),
    }]);
    assert_eq!(verifier.check(0, &orig), CheckCdnHashResult::Good);
    buf[0] ^= 1;
    assert_eq!(verifier.check(0, &buf), CheckCdnHashResult::Invalid);
}

#[test]
fn download_queue_dc_balance_and_part_assembler() {
    // Priority queue ordering & generation reset.
    let mut q = DownloadQueue::new();
    q.enqueue(1, 0);
    q.enqueue(2, 0);
    q.enqueue(3, 5);
    assert_eq!(q.ordered_ids(), vec![(3, 5), (2, 0), (1, 0)]);
    assert_eq!(q.next_task(true, |id| id != 3), None); // onlyHighestPriority blocks lower priorities
    assert_eq!(q.next_task(false, |id| id != 3), Some(2));
    q.reset_generation();
    assert_eq!(q.ordered_ids(), vec![(3, 5), (2, -1), (1, -1)]);

    // Per-DC session balancer: scales window up to MAX_WAITED_IN_SESSION and sessions up to 8.
    let mut dc = DcBalanceState::default();
    assert_eq!(dc.sessions.len(), 1);
    assert_eq!(dc.sessions[0].max_waited_amount, START_WAITED_IN_SESSION);

    // 3 fast successes at full window increase max_waited_amount and add session #2.
    let mut now = 10_000u64;
    for i in 0..RETRY_ADD_SESSION_SUCCESSES {
        let max_w = dc.sessions[0].max_waited_amount;
        let added =
            dc.record_request_succeeded(0, max_w, now - 100, now, DOWNLOAD_PART_SIZE as usize);
        now += 500;
        if i + 1 == RETRY_ADD_SESSION_SUCCESSES {
            assert!(added);
        }
    }
    assert_eq!(dc.sessions.len(), 2);
    assert!(dc.sessions[0].max_waited_amount > START_WAITED_IN_SESSION);
    assert!(dc.sessions[0].max_waited_amount <= MAX_WAITED_IN_SESSION);

    // 4 timeouts shed session #2 back down to 1.
    let mut removed = None;
    for _ in 0..REMOVE_SESSION_AFTER_TIMEOUTS {
        now += 1_000;
        removed = dc.record_timeout(1, now);
    }
    assert_eq!(removed, Some(1));
    assert_eq!(dc.sessions.len(), 1);
    // Slow request >= BAD_REQUEST_DURATION_MS does not accumulate timeout debt at 1 session
    // (`DownloadManagerMtproto::sessionTimedOut`, `download_manager_mtproto.cpp:325-326`),
    // and counts as a timeout once >1 session is active.
    dc.record_request_succeeded(
        0,
        DOWNLOAD_PART_SIZE as usize,
        now + 100,
        now + 100 + BAD_REQUEST_DURATION_MS,
        DOWNLOAD_PART_SIZE as usize,
    );
    assert_eq!(dc.timeouts, 0);
    dc.sessions.push(Default::default());
    dc.record_request_succeeded(
        1,
        DOWNLOAD_PART_SIZE as usize,
        now + 200,
        now + 200 + BAD_REQUEST_DURATION_MS,
        DOWNLOAD_PART_SIZE as usize,
    );
    assert_eq!(dc.timeouts, 1);
    const {
        assert!(MAX_SESSIONS_COUNT == 8);
        assert!(RETRY_ADD_SESSION_TIMEOUT_MS == 8000);
    }

    // Scheduler integration.
    let mut sched = DownloadScheduler::new();
    sched.enqueue(2, 100, 1);
    assert_eq!(
        sched.poll_next(2, DOWNLOAD_PART_SIZE as usize, |_| true),
        Some((100, 0))
    );

    // Out-of-order part assembly with hole tracking.
    let mut asm = PartAssembler::with_capacity(30);
    asm.write_part(20, b"0123456789").expect("part 2");
    assert_eq!(asm.downloaded_bytes(), 10);
    assert!(asm.clone().finalize().is_err()); // hole at 0..20
    asm.write_part(0, b"abcdefghij").expect("part 0");
    assert_eq!(asm.downloaded_bytes(), 20);
    asm.write_part(10, b"klmnopqrst").expect("part 1");
    assert_eq!(asm.downloaded_bytes(), 30);
    assert_eq!(asm.read_back(10, 5), Some(b"klmno".as_slice()));
    assert_eq!(
        asm.finalize().expect("complete"),
        b"abcdefghijklmnopqrst0123456789"
    );
}

#[tokio::test]
async fn async_download_normal_cdn_reupload_ref_refresh_cache_and_cancel() {
    let invoker = FakeMediaInvoker::new();

    // 1. Multi-chunk regular download + cache write + cache hit.
    let payload: Vec<u8> = (0..(300 * 1024)).map(|i| (i % 251) as u8).collect();
    invoker.insert_file(10, payload.clone());
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cache = LocalFileCache::open(CacheSettings::new(dir.path())).expect("cache");
    let key = document_cache_key(2, 10);
    let cancel = CancelToken::new();

    let mut req = DownloadRequest::new(
        2,
        TransferLocation::Document {
            id: 10,
            access_hash: 99,
            file_reference: vec![1, 2, 3],
            thumb_size: String::new(),
        },
        payload.len() as u64,
    );
    req.cache_target = Some((key, IMAGE_CACHE_TAG));

    let mut progress_events = Vec::new();
    let got = download_file(&invoker, req.clone(), Some(&mut cache), &cancel, |p| {
        progress_events.push(p);
    })
    .await
    .expect("normal download");
    assert_eq!(got, payload);
    assert!(!progress_events.is_empty());

    // Second download should hit local cache (0 extra RPCs).
    let before = invoker.call_counts().0;
    let got_cached = download_file(&invoker, req, Some(&mut cache), &cancel, |_| {})
        .await
        .expect("cached download");
    assert_eq!(got_cached, payload);
    assert_eq!(invoker.call_counts().0, before);

    // 2. FILE_REFERENCE_EXPIRED refresh & automatic retry.
    invoker.insert_file(11, b"refreshed-ok".to_vec());
    invoker.set_file_reference_refresh(11, vec![0x01], vec![0x02]);
    let ref_req = DownloadRequest::new(
        2,
        TransferLocation::Document {
            id: 11,
            access_hash: 11,
            file_reference: vec![0x01],
            thumb_size: String::new(),
        },
        12,
    );
    let got_ref = download_file(&invoker, ref_req, None, &cancel, |_| {})
        .await
        .expect("refreshed download");
    assert_eq!(got_ref, b"refreshed-ok");

    // 3a. CDN redirect + reupload_cdn_file.
    let cdn_payload: Vec<u8> = (0..(300 * 1024)).map(|i| ((i * 7) % 256) as u8).collect();
    invoker.insert_cdn_file(
        20,
        FakeCdnFile {
            cdn_dc_id: 203,
            file_token: b"tok-20".to_vec(),
            key: [0x11; 32],
            iv: [0x22; 16],
            plaintext: cdn_payload.clone(),
            initial_hash_count: Some(1),
            require_reupload_once: true,
            corrupt_offset: None,
        },
    );
    let cdn_req = DownloadRequest::new(
        2,
        TransferLocation::Document {
            id: 20,
            access_hash: 20,
            file_reference: vec![9],
            thumb_size: String::new(),
        },
        cdn_payload.len() as u64,
    );
    let got_cdn = download_file(&invoker, cdn_req, None, &cancel, |_| {})
        .await
        .expect("cdn download");
    assert_eq!(got_cdn, cdn_payload);

    // 3b. CDN redirect with partial initial hashes (triggers upload.getCdnFileHashes).
    invoker.insert_cdn_file(
        23,
        FakeCdnFile {
            cdn_dc_id: 203,
            file_token: b"tok-23".to_vec(),
            key: [0x77; 32],
            iv: [0x88; 16],
            plaintext: cdn_payload.clone(),
            initial_hash_count: Some(1),
            require_reupload_once: false,
            corrupt_offset: None,
        },
    );
    let lazy_cdn_req = DownloadRequest::new(
        2,
        TransferLocation::Document {
            id: 23,
            access_hash: 23,
            file_reference: vec![9],
            thumb_size: String::new(),
        },
        cdn_payload.len() as u64,
    );
    let got_lazy = download_file(&invoker, lazy_cdn_req, None, &cancel, |_| {})
        .await
        .expect("lazy cdn hashes download");
    assert_eq!(got_lazy, cdn_payload);

    let (_, cdn_calls, hash_calls, reupload_calls) = invoker.call_counts();
    assert!(cdn_calls >= 6);
    assert!(hash_calls >= 1);
    assert_eq!(reupload_calls, 1);

    // 4. Corrupted CDN chunk triggers CdnHashMismatch.
    invoker.insert_cdn_file(
        21,
        FakeCdnFile {
            cdn_dc_id: 203,
            file_token: b"tok-21".to_vec(),
            key: [0x33; 32],
            iv: [0x44; 16],
            plaintext: vec![0x55; 128 * 1024],
            initial_hash_count: None,
            require_reupload_once: false,
            corrupt_offset: Some(0),
        },
    );
    let bad_req = DownloadRequest::new(
        2,
        TransferLocation::Document {
            id: 21,
            access_hash: 21,
            file_reference: vec![9],
            thumb_size: String::new(),
        },
        128 * 1024,
    );
    let err = download_file(&invoker, bad_req, None, &cancel, |_| {})
        .await
        .expect_err("corrupt cdn must fail");
    assert!(matches!(err, MediaError::CdnHashMismatch { offset: 0 }));

    // 5. FILE_TOKEN_INVALID falls back cleanly to main DC.
    invoker.insert_cdn_file(
        22,
        FakeCdnFile {
            cdn_dc_id: 203,
            file_token: b"tok-22".to_vec(),
            key: [0x55; 32],
            iv: [0x66; 16],
            plaintext: b"fallback-main-dc".to_vec(),
            initial_hash_count: None,
            require_reupload_once: false,
            corrupt_offset: None,
        },
    );
    invoker.inject_cdn_token_invalid_once(b"tok-22".to_vec());
    let fb_req = DownloadRequest::new(
        2,
        TransferLocation::Document {
            id: 22,
            access_hash: 22,
            file_reference: vec![9],
            thumb_size: String::new(),
        },
        16,
    );
    let got_fb = download_file(&invoker, fb_req, None, &cancel, |_| {})
        .await
        .expect("fallback download");
    assert_eq!(got_fb, b"fallback-main-dc");

    // 6. Web file download (unknown initial size).
    invoker.insert_web_file(
        b"https://cdn.example/img.jpg".to_vec(),
        "image/jpeg",
        b"web-bytes".to_vec(),
    );
    let web_req = DownloadRequest::new(
        4,
        TransferLocation::WebFile {
            url: b"https://cdn.example/img.jpg".to_vec(),
            access_hash: 123,
        },
        0,
    );
    let got_web = download_file(&invoker, web_req, None, &cancel, |_| {})
        .await
        .expect("web download");
    assert_eq!(got_web, b"web-bytes");

    // 7. Cooperative cancellation.
    let cancelled = CancelToken::new();
    cancelled.cancel();
    let cancel_err = download_file(
        &invoker,
        DownloadRequest::new(
            2,
            TransferLocation::Document {
                id: 10,
                access_hash: 99,
                file_reference: vec![1],
                thumb_size: String::new(),
            },
            payload.len() as u64,
        ),
        None,
        &cancelled,
        |_| {},
    )
    .await
    .expect_err("cancelled");
    assert!(matches!(cancel_err, MediaError::Cancelled));
}

// ============================================================================
// StreamedFileDownloader, StreamingPriorityQueue & SpeedEstimator
// ============================================================================

#[test]
fn streamed_downloader_priority_queue_and_speed_estimator() {
    let mut pq = StreamingPriorityQueue::new();
    assert!(pq.add(256 * 1024));
    assert!(pq.add(128 * 1024));
    assert_eq!(pq.front(), Some(128 * 1024));

    // Seek bumps priority generation so offset 1MB jumps ahead of older entries.
    pq.reset_priorities();
    assert!(pq.add(1024 * 1024));
    assert_eq!(pq.take(), Some(1024 * 1024));
    assert_eq!(
        pq.take_in_range(0, 512 * 1024),
        vec![128 * 1024, 256 * 1024]
    );
    assert!(pq.is_empty());

    // StreamedFileDownloaderState with 3 parts (2 * 128 KiB + 1000 bytes).
    let total = 2 * STREAMING_PART_SIZE + 1000;
    let mut st = StreamedFileDownloaderState::new(total).expect("state");
    assert_eq!(st.parts_count(), 3);
    let offsets = st.take_next_request_offsets();
    assert_eq!(
        offsets,
        vec![0, STREAMING_PART_SIZE, 2 * STREAMING_PART_SIZE]
    );
    assert!(offsets.len() <= MAX_STREAMED_REQUEST_PARTS);

    // Save out of order: part 2, then part 0, then part 1.
    let p2 = LoadedPart {
        offset: 2 * STREAMING_PART_SIZE,
        bytes: vec![2u8; 1000],
    };
    let p0 = LoadedPart {
        offset: 0,
        bytes: vec![0u8; STREAMING_PART_SIZE as usize],
    };
    let p1 = LoadedPart {
        offset: STREAMING_PART_SIZE,
        bytes: vec![1u8; STREAMING_PART_SIZE as usize],
    };

    assert!(!st.save_part(&p2).expect("save p2"));
    assert_eq!(st.read_loaded_part(0), None);
    assert_eq!(
        st.read_loaded_part(2 * STREAMING_PART_SIZE)
            .map(<[u8]>::len),
        Some(1000)
    );
    assert!(!st.save_part(&p0).expect("save p0"));
    assert!(st.save_part(&p1).expect("save p1 completes"));
    assert!(st.is_finished());

    let assembled = st.into_bytes().expect("assembled");
    assert_eq!(assembled.len() as u64, total);
    assert_eq!(assembled[0], 0);
    assert_eq!(assembled[STREAMING_PART_SIZE as usize], 1);
    assert_eq!(assembled[(2 * STREAMING_PART_SIZE) as usize], 2);

    // SpeedEstimator: 3 parts in 1 second = 384 KiB/s, reliable.
    let mut speed = SpeedEstimator::new();
    speed.record_start(0, 1000);
    speed.record_start(STREAMING_PART_SIZE, 1100);
    speed.record_start(2 * STREAMING_PART_SIZE, 1200);
    speed.record_finish(0, 1400);
    speed.record_finish(STREAMING_PART_SIZE, 1700);
    speed.record_finish(2 * STREAMING_PART_SIZE, 2000);
    let est = speed.estimate(2000).expect("estimate");
    assert!(!est.unreliable);
    assert_eq!(est.bytes_per_second, 3 * STREAMING_PART_SIZE);
}

// ============================================================================
// Upload planning, part sizing, MD5, balancer, and async upload
// ============================================================================

#[test]
fn upload_part_sizing_plan_and_balancer() {
    assert_eq!(BIG_FILE_THRESHOLD, 10 * 1024 * 1024);
    assert_eq!(TDESKTOP_USE_BIG_FILES_FROM, 30 * 1024 * 1024);

    // Empty file fails.
    assert!(choose_upload_part_size(0, false).is_err());
    // < 1 MiB -> 32 KiB
    assert_eq!(
        choose_upload_part_size(500 * 1024, false).expect("tiny"),
        (UPLOAD_PART_SIZE_32K, 16)
    );
    // 1 MiB ..= 32 MiB -> 64 KiB
    assert_eq!(
        choose_upload_part_size(1024 * 1024, false).expect("1mb"),
        (UPLOAD_PART_SIZE_64K, 16)
    );
    assert_eq!(
        choose_upload_part_size(32 * 1024 * 1024, false).expect("32mb"),
        (UPLOAD_PART_SIZE_64K, 512)
    );
    // 100 MiB -> 128 KiB
    assert_eq!(
        choose_upload_part_size(100 * 1024 * 1024, false).expect("100mb"),
        (UPLOAD_PART_SIZE_128K, 800)
    );
    // 600 MiB -> 256 KiB (since 600MB/128KB = 4800 > 4000)
    assert_eq!(
        choose_upload_part_size(600 * 1024 * 1024, false).expect("600mb"),
        (UPLOAD_PART_SIZE_256K, 2400)
    );
    // 1500 MiB -> 512 KiB
    assert_eq!(
        choose_upload_part_size(1500 * 1024 * 1024, false).expect("1500mb"),
        (UPLOAD_PART_SIZE_512K, 3000)
    );
    // 2000 MiB exact limit non-premium -> 4000 parts of 512 KiB
    assert_eq!(
        choose_upload_part_size(MAX_FILE_SIZE_DEFAULT, false).expect("2000mb"),
        (UPLOAD_PART_SIZE_512K, MAX_UPLOAD_PARTS_DEFAULT)
    );
    // 2500 MiB fails non-premium, succeeds premium (5000 parts <= 8000)
    assert!(choose_upload_part_size(2500 * 1024 * 1024, false).is_err());
    assert_eq!(
        choose_upload_part_size(2500 * 1024 * 1024, true).expect("2500mb premium"),
        (UPLOAD_PART_SIZE_512K, 5000)
    );
    assert_eq!(
        choose_upload_part_size(MAX_FILE_SIZE_PREMIUM, true).expect("4000mb premium"),
        (UPLOAD_PART_SIZE_512K, MAX_UPLOAD_PARTS_PREMIUM)
    );
    assert!(choose_upload_part_size(MAX_FILE_SIZE_PREMIUM + 1, true).is_err());

    // Photo plan appends .jpg if missing and uses 32 KiB slices.
    let photo_plan = UploadPlan::with_file_id(
        777,
        "vacation",
        100_000,
        UploadConfig {
            is_photo: true,
            ..Default::default()
        },
    )
    .expect("photo plan");
    assert_eq!(photo_plan.filename, "vacation.jpg");
    assert_eq!(photo_plan.part_size, PHOTO_UPLOAD_PART_SIZE);
    assert_eq!(photo_plan.total_parts, 4);
    assert!(!photo_plan.is_big);

    // Big file (> 10 MiB) sets is_big = true.
    let big_plan = UploadPlan::with_file_id(
        888,
        "movie.mp4",
        BIG_FILE_THRESHOLD + 1,
        UploadConfig::default(),
    )
    .expect("big plan");
    assert!(big_plan.is_big);
    assert_eq!(big_plan.parts().last().map(|p| p.length), Some(1));

    // Known MD5 vector: MD5("hello") = 5d41402abc4b2a76b9719d911017c592.
    assert_eq!(
        compute_md5_hex(b"hello"),
        "5d41402abc4b2a76b9719d911017c592"
    );

    // UploadSessionBalancer dynamic scale-up and scale-down.
    let mut bal = UploadSessionBalancer::new(4);
    let used = BTreeSet::new();
    let s0 = bal.choose_session(512 * 1024, &used, 1000).expect("s0");
    assert_eq!(s0, 0);
    let q0 = bal.on_part_sent(s0, 512 * 1024);
    // Fast completion (< 1s, >= 512KB) marks s0 fast -> allows adding s1.
    bal.on_part_finished(
        s0,
        512 * 1024,
        q0,
        1050,
        1050 + FAST_REQUEST_THRESHOLD_MS - 100,
    );
    assert!(bal.can_add_session());
    bal.on_part_sent(s0, 512 * 1024);
    let s1 = bal.choose_session(512 * 1024, &used, 2000).expect("s1");
    assert_eq!(s1, 1);
    assert_eq!(bal.session_count(), 2);
    // Slow completion removes s1.
    let removed = bal.on_part_finished(s1, 512 * 1024, 0, 2000, 2000 + SLOW_REQUEST_THRESHOLD_MS);
    assert_eq!(removed, Some(1));
    assert_eq!(bal.session_count(), 1);
}

#[tokio::test]
async fn async_upload_small_and_big_files_via_fake_invoker() {
    let invoker = FakeMediaInvoker::new();
    let cancel = CancelToken::new();

    // Small file upload (<= 10 MiB): uses saveFilePart and computes MD5.
    let small_data: Vec<u8> = (0..(100 * 1024)).map(|i| (i % 199) as u8).collect();
    let expected_md5 = compute_md5_hex(&small_data);
    let mut last_progress = None;
    let res = upload_bytes(
        &invoker,
        "doc.bin",
        &small_data,
        UploadConfig::default(),
        &cancel,
        |p| last_progress = Some(p),
    )
    .await
    .expect("small upload");

    match res {
        UploadedInputFile::Small {
            id,
            parts,
            name,
            md5_checksum,
        } => {
            assert_ne!(id, 0);
            assert_eq!(name, "doc.bin");
            assert_eq!(parts, 4); // ceil(100 KiB / 32 KiB) = 4
            assert_eq!(md5_checksum, expected_md5);
            let stored = invoker.uploaded_file(id).expect("stored");
            assert!(!stored.is_big);
            assert_eq!(stored.assemble(), small_data);
        }
        UploadedInputFile::Big { .. } => panic!("expected Small upload"),
    }
    assert_eq!(last_progress.map(|p| p.fraction()), Some(1.0));

    // Big file upload (> threshold): uses saveBigFilePart, no MD5.
    let big_data: Vec<u8> = (0..(180 * 1024)).map(|i| ((i * 3) % 256) as u8).collect();
    let big_cfg = UploadConfig {
        big_file_threshold: 128 * 1024, // custom lower threshold to keep test fast
        ..Default::default()
    };
    let big_res = upload_bytes(&invoker, "big.bin", &big_data, big_cfg, &cancel, |_| {})
        .await
        .expect("big upload");
    match big_res {
        UploadedInputFile::Big { id, parts, name } => {
            assert_ne!(id, 0);
            assert_eq!(name, "big.bin");
            assert_eq!(parts, 6); // ceil(180 KiB / 32 KiB) = 6
            let stored = invoker.uploaded_file(id).expect("stored big");
            assert!(stored.is_big);
            assert_eq!(stored.total_parts, Some(6));
            assert_eq!(stored.assemble(), big_data);
        }
        UploadedInputFile::Small { .. } => panic!("expected Big upload"),
    }
}
