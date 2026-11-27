//! Security review #3, package SEC3-B (data at rest): regression tests for streamed media on
//! disk. Child module of `tests_storage_wiring` so it can reuse that module's fake RPC client
//! and `LiveStorageTestHarness`.

use std::collections::BTreeSet;
use std::path::Path;

use tg_app_core::stories::backend::StoryMediaAsset;
use tg_storage::cache::legacy_temp::EPHEMERAL_STREAM_DIR_PREFIX;
use tg_storage::streamed_downloader::STREAMED_ENC_MAGIC;
use tg_stories::{Story, StoryMedia, StoryPrivacy, StoryRepostInfo};

use super::*;

/// Recognizable plaintext repeated through every test payload.
const MARKER: &[u8] = b"SEC3B_PLAINTEXT_VIDEO_FRAME_";

fn marked_payload(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 64);
    let mut i = 0u32;
    while out.len() < len {
        out.extend_from_slice(MARKER);
        out.extend_from_slice(format!("{i:08}").as_bytes());
        i += 1;
    }
    out.truncate(len);
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Serves `payload` for every `upload.getFile` and counts the requests.
fn serve_payload(fake: &FakeRpcClient, payload: &[u8]) -> Arc<AtomicUsize> {
    let payload = Arc::new(payload.to_vec());
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_rpc = Arc::clone(&calls);
    fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor != tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            return None;
        }
        calls_rpc.fetch_add(1, Ordering::SeqCst);
        let req = tl_fns::upload::GetFile::from_bytes(raw).expect("decode upload.getFile");
        let start = req.offset as usize;
        let end = start.saturating_add(req.limit as usize).min(payload.len());
        let resp = tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileMp4,
            mtime: 1_740_000_600,
            bytes: payload[start..end].to_vec(),
        });
        Some(Ok(resp.to_bytes()))
    }));
    calls
}

fn video_doc(doc_id: u64, size: usize) -> DocumentData {
    DocumentData {
        id: DocumentId(doc_id),
        access_hash: 31_337,
        file_reference: vec![9, 9, 9],
        dc_id: 2,
        size: size as i64,
        mime_type: "video/mp4".to_owned(),
        file_name: "clip.mp4".to_owned(),
        kind: DocumentKind::Video,
        ..DocumentData::default()
    }
}

fn video_story(peer: DataPeerId, id: i32, document: DocumentData) -> Story {
    Story {
        id,
        peer_id: peer,
        date: 1_740_000_000,
        expires: 1_740_086_400,
        last_update_time: 1_740_000_000,
        media: StoryMedia::Video {
            document,
            silent: false,
        },
        caption: DataText::default(),
        out: false,
        in_profile: false,
        pinned_to_top: false,
        privacy: StoryPrivacy::CloseFriends,
        privacy_allowed: Vec::new(),
        privacy_disallowed: Vec::new(),
        no_forwards: false,
        edited: false,
        from_peer: None,
        repost: StoryRepostInfo::default(),
        music: None,
        album_ids: BTreeSet::new(),
        sent_reaction: None,
        recent_viewers: Vec::new(),
        views: Default::default(),
        channel_reactions: Default::default(),
        locations: Vec::new(),
        suggested_reactions: Vec::new(),
        channel_posts: Vec::new(),
        url_areas: Vec::new(),
        weather_areas: Vec::new(),
    }
}

/// Asserts that no file under `dir` (recursively) contains the plaintext marker and returns the
/// number of files checked.
fn assert_no_plaintext_under(dir: &Path) -> usize {
    let mut checked = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).expect("read dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let raw = fs::read(&path).expect("read file");
            assert!(
                !contains(&raw, MARKER),
                "{} holds plaintext media bytes",
                path.display()
            );
            checked += 1;
        }
    }
    checked
}

#[test]
fn sec3_b1_loader_without_storage_dir_encrypts_with_ephemeral_key() {
    let h = LiveStorageTestHarness::new();
    let part_size = DOWNLOAD_PART_SIZE as usize;
    let payload = marked_payload(part_size + 4_096);
    serve_payload(&h.fake, &payload);

    let doc_id = 906_001u64;
    let loader = TelegramRangeLoader::new(
        h.fake.clone(),
        h.runtime.tokio_handle().clone(),
        document_to_location(&video_doc(doc_id, payload.len())),
    );
    let slice = loader.load_range(100, 5_000).expect("range read");
    assert_eq!(slice.as_slice(), &payload[100..5_100]);

    let part_path = loader.part_path().expect("part path");
    let dir = part_path.parent().expect("stream dir").to_path_buf();
    let dir_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .expect("dir name")
        .to_owned();
    assert!(
        dir_name.starts_with(&format!(
            "{EPHEMERAL_STREAM_DIR_PREFIX}{}_",
            std::process::id()
        )),
        "unexpected ephemeral stream folder {dir_name}"
    );
    assert!(
        !dir_name.contains(&format!("{doc_id:016x}")),
        "the folder name must not carry the document id"
    );

    let raw_part = fs::read(&part_path).expect("read .part");
    assert!(
        raw_part.starts_with(&STREAMED_ENC_MAGIC),
        "the .part file must be an encrypted streamed file"
    );
    assert!(assert_no_plaintext_under(&dir) >= 1);

    drop(loader);
    assert!(
        !dir.exists(),
        "the ephemeral folder is removed with the loader"
    );
}

#[test]
fn sec3_b1_story_video_streams_into_account_streamed_dir() {
    let h = LiveStorageTestHarness::new();
    let part_size = DOWNLOAD_PART_SIZE as usize;
    let payload = marked_payload(part_size * 2 + 777);
    serve_payload(&h.fake, &payload);

    let streamed_dir = h
        .live
        .media_cache()
        .streamed_dir()
        .expect("persistent cache has a streamed dir");
    assert!(streamed_dir.starts_with(h.tmp.path()));

    let peer = DataPeerId::from_user(UserId(77_010));
    let story = video_story(peer, 31, video_doc(907_001, payload.len()));
    let story_id = story.full_id();
    h.live.register_story_media(&story);

    // 1. The streaming loader of the story viewer.
    let loader = h
        .live
        .open_story_video_range_loader(story_id)
        .expect("story video loader");
    let tail = loader
        .load_range(part_size as u64 + 10, 2_000)
        .expect("story range read");
    assert_eq!(tail.as_slice(), &payload[part_size + 10..part_size + 2_010]);
    let part_path = loader.part_path().expect("part path");
    assert!(
        part_path.starts_with(&streamed_dir),
        "story video must stream into the account cache, got {}",
        part_path.display()
    );
    assert!(
        fs::read(&part_path)
            .expect("read .part")
            .starts_with(&STREAMED_ENC_MAGIC)
    );
    loader.stop();
    drop(loader);

    // 2. The viewer's path (`StoriesBackend::load_story_media`): a streaming loader whose
    // header is downloaded in the background.
    let asset = h
        .live
        .wait_story_media(story_id, Duration::from_secs(10))
        .expect("story media");
    let StoryMediaAsset::VideoStream(stream) = asset else {
        panic!("a video story is streamed, got {asset:?}");
    };
    let loader = stream.loader();
    let all = loader
        .load_range(0, payload.len())
        .expect("whole story read");
    assert_eq!(all, payload);
    drop(loader);
    drop(stream);

    assert!(
        assert_no_plaintext_under(&streamed_dir) >= 1,
        "the story video must be stored in the streamed dir"
    );
}

#[test]
fn sec3_b1_story_video_without_account_folder_uses_ephemeral_key() {
    let h = LiveStorageTestHarness::new();
    // Back to the state before an account folder is attached.
    h.live.media_cache().close_to_memory();
    assert_eq!(h.live.media_cache().streamed_dir(), None);

    let payload = marked_payload(DOWNLOAD_PART_SIZE as usize + 1_234);
    serve_payload(&h.fake, &payload);
    let peer = DataPeerId::from_user(UserId(77_011));
    let story = video_story(peer, 32, video_doc(907_002, payload.len()));
    h.live.register_story_media(&story);

    let loader = h
        .live
        .open_story_video_range_loader(story.full_id())
        .expect("story video loader");
    let head = loader.load_range(0, 3_000).expect("story range read");
    assert_eq!(head.as_slice(), &payload[..3_000]);

    let part_path = loader.part_path().expect("part path");
    let dir = part_path.parent().expect("stream dir").to_path_buf();
    assert!(
        fs::read(&part_path)
            .expect("read .part")
            .starts_with(&STREAMED_ENC_MAGIC)
    );
    assert!(assert_no_plaintext_under(&dir) >= 1);
    drop(loader);
    assert!(!dir.exists());
}

fn stream_cache_key(streamed_dir: &Path) -> tg_storage::StorageEncryptionKey {
    tg_storage::cache::media_cache::lookup_cache_key_for_path(streamed_dir)
        .expect("cache key registered for the streamed dir")
}

fn keyed_target(streamed_dir: &Path, doc_id: u64) -> PathBuf {
    let stem = tg_storage::cache::media_cache::keyed_stream_file_stem(
        &stream_cache_key(streamed_dir),
        doc_id,
        2,
    );
    streamed_dir.join(format!("{stem}.bin"))
}

#[test]
fn sec3_b3_planted_plaintext_stream_file_is_not_served() {
    let h = LiveStorageTestHarness::new();
    let part_size = DOWNLOAD_PART_SIZE as usize;
    let payload = marked_payload(part_size + 2_048);
    let calls = serve_payload(&h.fake, &payload);
    let streamed_dir = h.live.media_cache().streamed_dir().expect("streamed dir");
    fs::create_dir_all(&streamed_dir).expect("mkdir streamed");

    // Someone who can write the cache folder plants a plaintext file of the document's exact
    // size where a finished download is expected: under the name older builds used and under
    // the current keyed name.
    let doc_id = 908_001u64;
    let planted = vec![0x41u8; payload.len()];
    let old_name = streamed_dir.join(format!("stream_{doc_id:016x}_{:016x}.bin", 2u64));
    let new_name = keyed_target(&streamed_dir, doc_id);
    fs::write(&old_name, &planted).expect("plant old name");
    fs::write(&new_name, &planted).expect("plant keyed name");

    let loader = TelegramRangeLoader::new(
        h.fake.clone(),
        h.runtime.tokio_handle().clone(),
        document_to_location(&video_doc(doc_id, payload.len())),
    )
    .with_storage_dir(&streamed_dir);
    let head = loader.load_range(0, 4_096).expect("range read");
    assert_eq!(
        head.as_slice(),
        &payload[..4_096],
        "planted bytes must never be returned"
    );
    assert!(
        calls.load(Ordering::SeqCst) >= 1,
        "content comes from the server"
    );

    let finalized = loader.download_to_file().expect("finish download");
    assert_eq!(finalized, new_name);
    assert!(
        fs::read(&finalized)
            .expect("read finalized")
            .starts_with(&STREAMED_ENC_MAGIC),
        "the planted file is replaced by an encrypted one"
    );
    assert_eq!(
        loader.load_range(0, payload.len()).expect("full read"),
        payload
    );
}

#[test]
fn sec3_b3_tampered_stream_cache_file_is_dropped_and_downloaded_again() {
    let h = LiveStorageTestHarness::new();
    let part_size = DOWNLOAD_PART_SIZE as usize;
    let payload = marked_payload(part_size * 2 + 512);
    let calls = serve_payload(&h.fake, &payload);
    let streamed_dir = h.live.media_cache().streamed_dir().expect("streamed dir");
    let doc_id = 908_002u64;
    let open_loader = || {
        TelegramRangeLoader::new(
            h.fake.clone(),
            h.runtime.tokio_handle().clone(),
            document_to_location(&video_doc(doc_id, payload.len())),
        )
        .with_storage_dir(&streamed_dir)
    };

    let loader = open_loader();
    let finalized = loader.download_to_file().expect("download");
    drop(loader);
    assert_eq!(calls.load(Ordering::SeqCst), 3, "three parts fetched");

    // A finished download is reused without any request.
    let loader = open_loader();
    assert_eq!(
        loader.load_range(10, 1_000).expect("cached read"),
        payload[10..1_010]
    );
    drop(loader);
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    // Alter one byte of part 1 in the cache file.
    let mut raw = fs::read(&finalized).expect("read cache file");
    let at = tg_storage::streamed_downloader::STREAMED_ENC_HEADER_LEN as usize + part_size + 77;
    raw[at] ^= 0x80;
    fs::write(&finalized, &raw).expect("write altered cache file");

    let loader = open_loader();
    let slice = loader
        .load_range(part_size as u64 + 50, 100)
        .expect("read over the altered part");
    assert_eq!(
        slice.as_slice(),
        &payload[part_size + 50..part_size + 150],
        "the altered content must not be returned"
    );
    assert!(
        calls.load(Ordering::SeqCst) > 3,
        "the altered file is dropped and the part is downloaded again"
    );
}

#[test]
fn sec3_b3_stream_cache_names_and_sidecar_do_not_reveal_the_document() {
    let h = LiveStorageTestHarness::new();
    let part_size = DOWNLOAD_PART_SIZE as usize;
    let payload = marked_payload(part_size * 2);
    serve_payload(&h.fake, &payload);

    let peer = DataPeerId::from_user(UserId(77_012));
    let doc_id = 908_003u64;
    let item_id = seed_video_message(&h, peer, 404, doc_id, vec![4, 0, 4], payload.len());
    let loader = h
        .live
        .open_range_loader(item_id)
        .expect("open range loader");
    // Only part 1: the download stays partial, so the `.part.meta` sidecar exists.
    loader
        .load_range(part_size as u64, 1_000)
        .expect("partial read");
    loader.stop();
    drop(loader);

    let streamed_dir = h.live.media_cache().streamed_dir().expect("streamed dir");
    let doc_hex = format!("{doc_id:x}");
    let mut names = Vec::new();
    for entry in fs::read_dir(&streamed_dir)
        .expect("read streamed dir")
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        assert!(
            name.starts_with(tg_storage::cache::media_cache::STREAM_FILE_PREFIX),
            "unexpected file name {name}"
        );
        assert!(!name.contains(&doc_hex), "{name} carries the document id");
        let raw = fs::read(entry.path()).expect("read cache file");
        assert!(
            !raw.windows(8).any(|w| w == doc_id.to_le_bytes()),
            "{name} holds the document id in clear"
        );
        if name.ends_with(".meta") {
            assert!(
                PartialFileMetadata::load(&entry.path()).is_err(),
                "the sidecar must not be readable without the key"
            );
        }
        names.push(name);
    }
    assert!(names.iter().any(|n| n.ends_with(".part")), "{names:?}");
    assert!(names.iter().any(|n| n.ends_with(".part.meta")), "{names:?}");
    assert_no_plaintext_under(&streamed_dir);
}

#[path = "tests_story_media.rs"]
#[cfg(test)]
mod tests_story_media;
