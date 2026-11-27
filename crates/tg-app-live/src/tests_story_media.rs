//! TGB-3101: story media loads in the background. The story viewer asks for the media on
//! every frame, so the request must return at once while the network is slow, and a video
//! story must reach the player as a streaming loader before the whole file is downloaded.
//! Child module of `tests_sec3b` for its payload and story helpers.

use std::sync::atomic::AtomicBool;

use tg_app_core::stories::backend::StoriesBackend;
use tg_data::{PhotoData, PhotoId, PhotoSize};
use tg_stories::{StoriesAction, StoriesStore};

use super::*;

/// How long a held `upload.getFile` waits before it is answered anyway.
const MAX_HOLD: Duration = Duration::from_secs(10);

/// Longest time a media request from the UI thread may take while the server is held. The
/// old code waited for the whole download (at least `MAX_HOLD`).
const UI_CALL_LIMIT: Duration = Duration::from_millis(500);

/// Serves `payload` for every `upload.getFile`, holding each request until `open` is set
/// (or `MAX_HOLD` passed), and counts the requests.
fn serve_payload_held(
    fake: &FakeRpcClient,
    payload: &[u8],
    open: &Arc<AtomicBool>,
) -> Arc<AtomicUsize> {
    let payload = Arc::new(payload.to_vec());
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_rpc = Arc::clone(&calls);
    let open = Arc::clone(open);
    fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor != tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            return None;
        }
        calls_rpc.fetch_add(1, Ordering::SeqCst);
        let started = Instant::now();
        while !open.load(Ordering::Acquire) && started.elapsed() < MAX_HOLD {
            std::thread::sleep(Duration::from_millis(2));
        }
        let req = tl_fns::upload::GetFile::from_bytes(raw).expect("decode upload.getFile");
        let start = (req.offset as usize).min(payload.len());
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

fn photo_story(peer: DataPeerId, id: i32, size: usize) -> Story {
    let mut story = video_story(peer, id, video_doc(1, size));
    story.media = StoryMedia::Photo(PhotoData {
        id: PhotoId(908_001),
        access_hash: 77,
        file_reference: vec![1, 2, 3],
        dc_id: 2,
        sizes: vec![PhotoSize {
            kind: "y".to_owned(),
            width: 720,
            height: 1280,
            size: size as i64,
        }],
        ..PhotoData::default()
    });
    story
}

#[test]
fn video_story_media_is_requested_without_waiting_and_streams_before_the_file_is_complete() {
    let h = LiveStorageTestHarness::new();
    let part_size = DOWNLOAD_PART_SIZE as usize;
    let total_parts = 24;
    let payload = marked_payload(part_size * total_parts - 99);
    let open = Arc::new(AtomicBool::new(false));
    let calls = serve_payload_held(&h.fake, &payload, &open);

    let peer = DataPeerId::from_user(UserId(77_020));
    let story = video_story(peer, 41, video_doc(907_101, payload.len()));
    let story_id = story.full_id();
    h.live.register_story_media(&story);

    // The viewer's frame while the server holds every part: no wait, no media yet.
    let mut live = h.live.clone();
    for _ in 0..3 {
        let started = Instant::now();
        let asset = StoriesBackend::load_story_media(&mut live, story_id);
        let took = started.elapsed();
        assert!(asset.is_none(), "nothing is downloaded yet, got {asset:?}");
        assert!(
            took < UI_CALL_LIMIT,
            "load_story_media waited {took:?} for the network on the UI thread"
        );
    }

    open.store(true, Ordering::Release);
    let asset = h
        .live
        .wait_story_media(story_id, Duration::from_secs(20))
        .expect("the story header arrives");
    let StoryMediaAsset::VideoStream(stream) = asset else {
        panic!("a video story reaches the player as a stream, got {asset:?}");
    };
    let parts_when_ready = calls.load(Ordering::SeqCst);
    assert!(
        parts_when_ready < total_parts,
        "the player gets the story after {parts_when_ready} of {total_parts} parts: it must \
         not wait for the whole file"
    );

    // The player streams the rest through the same loader.
    let loader = stream.loader();
    let all = loader.load_range(0, payload.len()).expect("whole story");
    assert_eq!(all, payload);

    // The next frame gets the same stream back from the cache.
    let again = StoriesBackend::load_story_media(&mut live, story_id).expect("cached");
    assert_eq!(again, StoryMediaAsset::VideoStream(stream));
}

#[test]
fn photo_story_media_is_requested_without_waiting() {
    let h = LiveStorageTestHarness::new();
    let payload = marked_payload(DOWNLOAD_PART_SIZE as usize + 4_321);
    let open = Arc::new(AtomicBool::new(false));
    let _calls = serve_payload_held(&h.fake, &payload, &open);

    let peer = DataPeerId::from_user(UserId(77_021));
    let story = photo_story(peer, 42, payload.len());
    let story_id = story.full_id();
    h.live.register_story_media(&story);

    let mut live = h.live.clone();
    let started = Instant::now();
    let asset = StoriesBackend::load_story_media(&mut live, story_id);
    let took = started.elapsed();
    assert!(asset.is_none());
    assert!(
        took < UI_CALL_LIMIT,
        "load_story_media waited {took:?} for the network on the UI thread"
    );

    open.store(true, Ordering::Release);
    let asset = h
        .live
        .wait_story_media(story_id, Duration::from_secs(20))
        .expect("the photo arrives");
    assert_eq!(asset, StoryMediaAsset::PhotoBytes(payload));
}

#[test]
fn story_preload_finishes_after_the_background_load_without_blocking_execute_actions() {
    let h = LiveStorageTestHarness::new();
    let part_size = DOWNLOAD_PART_SIZE as usize;
    let payload = marked_payload(part_size * 6 + 10);
    let open = Arc::new(AtomicBool::new(false));
    let _calls = serve_payload_held(&h.fake, &payload, &open);

    let peer = DataPeerId::from_user(UserId(77_022));
    let story = video_story(peer, 43, video_doc(907_103, payload.len()));
    let story_id = story.full_id();
    let now = story.date;
    let mut store = StoriesStore::new(DataPeerId::from_user(UserId(42_000)));
    store.insert_story(story.clone(), now);
    h.live.register_story_media(&story);

    store.set_preloading_in_viewer(vec![story_id]);
    let actions = store.take_actions();
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, StoriesAction::StartPreload { story_id: id } if *id == story_id)),
        "the viewer queue starts a preload: {actions:?}"
    );
    let mut live = h.live.clone();
    let started = Instant::now();
    StoriesBackend::execute_actions(&mut live, &mut store, actions, now);
    let took = started.elapsed();
    assert!(
        took < UI_CALL_LIMIT,
        "execute_actions waited {took:?} for the preload download"
    );
    assert!(!store.is_preloaded(story_id));
    assert_eq!(store.current_preload(), Some(story_id));

    open.store(true, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !store.is_preloaded(story_id) && Instant::now() < deadline {
        live.drain_story_updates(&mut store, now);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(store.is_preloaded(story_id), "the preload finishes");
    assert_eq!(store.current_preload(), None);
}
