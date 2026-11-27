//! TGB-2997: a round video message is downloaded when it enters the list only if it autoplays
//! (`Data::AutoDownload::ShouldAutoPlay` with `AutoPlayVideoMessage`); with autoplay off the
//! list sends no `upload.getFile`, and a click (`start_media_download`) fetches it. The old code
//! downloaded every round video in full as soon as it was listed.

use tg_app_core::core_types::dto::inline_media::VoiceLoadState;

use super::*;

fn file_answer(bytes: &[u8]) -> Vec<u8> {
    tl_enums::upload::File::File(tl_types::upload::File {
        r#type: tl_enums::storage::FileType::FileMp4,
        mtime: 1_750_000_000,
        bytes: bytes.to_vec(),
    })
    .to_bytes()
}

fn round_video(id: u64, size: usize) -> MessageMedia {
    MessageMedia::Document(Box::new(DocumentData {
        id: DocumentId(id),
        access_hash: 5,
        dc_id: 2,
        mime_type: "video/mp4".to_owned(),
        size: size as i64,
        kind: DocumentKind::RoundVideo,
        duration: Some(3.0),
        ..DocumentData::default()
    }))
}

fn count_get_file(fake: &MediaFakeRpcClient, bytes: &[u8]) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    let (bytes, counter) = (bytes.to_vec(), Arc::clone(&calls));
    fake.set_dynamic_handler(Arc::new(move |_dc, ctor, _bytes| {
        (ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID).then(|| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(file_answer(&bytes))
        })
    }));
    calls
}

#[test]
fn a_round_video_with_autoplay_off_is_not_downloaded_when_it_is_listed() {
    let (_dir, fake, live, peer_id) = start_live_session();
    let video = b"synthetic-round-video-mp4".to_vec();
    let calls = count_get_file(&fake, &video);
    if let Ok(mut cache) = live.cache.lock() {
        cache.auto_download.autoplay_round_videos = false;
    }
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            311,
            round_video(23_001, video.len()),
            MessageFlags::empty(),
        ),
    );

    let listed = live
        .inline_media_descriptor(peer_id, MsgId(311))
        .expect("round descriptor");
    assert!(listed.video_data.is_none());
    assert_eq!(listed.voice_load, Some(VoiceLoadState::NotLoaded));
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "listing a round video that does not autoplay must not download it"
    );

    live.start_media_download(peer_id, MsgId(311));
    assert!(
        wait_until(|| {
            live.inline_media_descriptor(peer_id, MsgId(311))
                .is_some_and(|d| d.video_data.is_some())
        }),
        "the round video never arrived after the click"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let loaded = live
        .inline_media_descriptor(peer_id, MsgId(311))
        .expect("round descriptor");
    assert_eq!(loaded.voice_load, None);
    assert_eq!(loaded.audio_bytes.as_deref(), Some(video.as_slice()));
}

#[test]
fn a_round_video_that_autoplays_is_downloaded_when_it_is_listed() {
    let (_dir, fake, live, peer_id) = start_live_session();
    let video = b"synthetic-round-video-mp4".to_vec();
    let calls = count_get_file(&fake, &video);
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            312,
            round_video(23_002, video.len()),
            MessageFlags::empty(),
        ),
    );
    let _ = live.inline_media_descriptor(peer_id, MsgId(312));
    assert!(
        wait_until(|| {
            live.inline_media_descriptor(peer_id, MsgId(312))
                .is_some_and(|d| d.video_data.is_some())
        }),
        "an autoplaying round video loads by itself"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
