//! TGB-2958: a voice message is not downloaded when it enters the list
//! (`Data::AutoDownload::Should` is false for voice); the first play downloads it and it then
//! counts as loaded for as long as the cache holds it.
//!
//! TGB-2982: a running photo download can be cancelled; the photo then stays not loaded until
//! the user starts it again (`PhotoCancelClickHandler` / `PhotoSaveClickHandler`).

use std::sync::atomic::AtomicBool;

use tg_app_core::core_types::dto::inline_media::VoiceLoadState;

use super::*;

fn file_answer(bytes: &[u8]) -> Vec<u8> {
    tl_enums::upload::File::File(tl_types::upload::File {
        r#type: tl_enums::storage::FileType::FileJpeg,
        mtime: 1_750_000_000,
        bytes: bytes.to_vec(),
    })
    .to_bytes()
}

#[test]
fn a_voice_message_is_downloaded_by_the_first_play_not_when_it_is_listed() {
    let (_dir, fake, live, peer_id) = start_live_session();
    let voice_bytes = b"OggS-synthetic-voice-payload".to_vec();
    let calls = Arc::new(AtomicUsize::new(0));
    {
        let (voice_bytes, calls) = (voice_bytes.clone(), Arc::clone(&calls));
        fake.set_dynamic_handler(Arc::new(move |_dc, ctor, _bytes| {
            (ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID).then(|| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(file_answer(&voice_bytes))
            })
        }));
    }
    let voice = DocumentData {
        id: DocumentId(21_001),
        access_hash: 5,
        dc_id: 2,
        mime_type: "audio/ogg".to_owned(),
        size: voice_bytes.len() as i64,
        kind: DocumentKind::Voice,
        duration: Some(4.0),
        ..DocumentData::default()
    };
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            301,
            MessageMedia::Document(Box::new(voice)),
            MessageFlags::empty(),
        ),
    );

    let listed = live
        .inline_media_descriptor(peer_id, MsgId(301))
        .expect("voice descriptor");
    assert!(listed.audio_bytes.is_none());
    assert_eq!(listed.voice_load, Some(VoiceLoadState::NotLoaded));
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "listing a voice message must not download it"
    );

    live.start_media_download(peer_id, MsgId(301));
    assert!(
        wait_until(|| {
            live.inline_media_descriptor(peer_id, MsgId(301))
                .is_some_and(|d| d.audio_bytes.is_some())
        }),
        "the voice file never arrived after the play"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Reopening the chat asks again: the file is in the cache, so it is loaded (the status
    // line shows no size) and nothing is downloaded.
    let reopened = live
        .inline_media_descriptor(peer_id, MsgId(301))
        .expect("voice descriptor");
    assert_eq!(
        reopened.audio_bytes.as_deref(),
        Some(voice_bytes.as_slice())
    );
    assert_eq!(reopened.voice_load, None);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_cancelled_photo_download_stays_not_loaded_until_it_is_started_again() {
    let (_dir, fake, live, peer_id) = start_live_session();
    let photo_bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4, 5, 6, 7, 8];
    let calls = Arc::new(AtomicUsize::new(0));
    // The first `upload.getFile` waits until the test lets it answer.
    let release = Arc::new(AtomicBool::new(false));
    {
        let (photo_bytes, calls, release) = (
            photo_bytes.clone(),
            Arc::clone(&calls),
            Arc::clone(&release),
        );
        fake.set_dynamic_handler(Arc::new(move |_dc, ctor, _bytes| {
            if ctor != tl_fns::upload::GetFile::CONSTRUCTOR_ID {
                return None;
            }
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let deadline = Instant::now() + TEST_TIMEOUT;
                while !release.load(Ordering::SeqCst) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Some(Ok(file_answer(&photo_bytes)))
        }));
    }
    let photo = PhotoData {
        id: PhotoId(22_001),
        access_hash: 99,
        file_reference: vec![9, 8],
        dc_id: 2,
        sizes: vec![PhotoSize {
            kind: "x".to_owned(),
            width: 800,
            height: 600,
            size: photo_bytes.len() as i64,
        }],
        ..PhotoData::default()
    };
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            302,
            MessageMedia::Photo(Box::new(photo)),
            MessageFlags::empty(),
        ),
    );
    let state = || {
        live.inline_media_descriptor(peer_id, MsgId(302))
            .and_then(|d| d.photo_load)
    };

    assert!(
        wait_until(|| matches!(state(), Some(PhotoLoadState::Loading { .. }))),
        "the photo never started loading: {:?}",
        state()
    );
    assert!(wait_until(|| calls.load(Ordering::SeqCst) == 1));

    live.cancel_media_download(peer_id, MsgId(302));
    assert!(
        matches!(state(), Some(PhotoLoadState::NotLoaded)),
        "after the cancel: {:?}",
        state()
    );

    // The request that was in flight answers now; its bytes must not be stored, and asking
    // for the photo again (the list repaints) must not start a new download.
    release.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        matches!(state(), Some(PhotoLoadState::NotLoaded)),
        "a cancelled photo came back by itself: {:?}",
        state()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(live.local_storage_stats().photos_bytes, 0);

    // The click on the download arrow.
    live.start_media_download(peer_id, MsgId(302));
    assert!(
        wait_until(|| matches!(state(), Some(PhotoLoadState::Ready { .. }))),
        "the photo did not load after it was started again: {:?}",
        state()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    match state() {
        Some(PhotoLoadState::Ready { bytes }) => assert_eq!(&*bytes, photo_bytes.as_slice()),
        other => panic!("expected the photo bytes, got {other:?}"),
    }
}
