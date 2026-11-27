//! The media viewer as the desktop binary runs it: background decoding on (TGB-3031).
//!
//! A separate test binary, because the switch is process-wide and the library tests decode
//! synchronously.

use std::time::{Duration, Instant};

use tg_app_media::media::{
    MOCK_CHAT_PEER_ID, MOCK_VIDEO_ID, MediaItemKind, MediaOverlayWidget, MockMediaBackend,
    MockVideoFile, generate_test_mp4,
};
use tg_text::TextWithEntities;

#[test]
fn viewer_plays_and_seeks_with_background_decoding() {
    tg_emoji::background::set_background_decode(true);
    let dir = tempfile::tempdir().expect("tempdir");
    let mp4 = dir.path().join("viewer.mp4");
    generate_test_mp4(&mp4, 320, 240, 3, true).expect("generate mp4");
    let backend = MockMediaBackend::new();
    backend
        .register_video_file(MockVideoFile {
            id: MOCK_VIDEO_ID,
            kind: MediaItemKind::Video,
            path: mp4,
            width: 320,
            height: 240,
            duration_ms: 3_000,
            sender_name: "Studio Feed".to_owned(),
            caption: TextWithEntities::simple("Generated test video"),
        })
        .expect("register video");

    let base = Instant::now();
    let now = || base.elapsed().as_millis() as i64;
    let mut overlay = MediaOverlayWidget::new();
    overlay
        .open_item(&backend, MOCK_CHAT_PEER_ID, MOCK_VIDEO_ID, now())
        .expect("open video");

    let mut max_step = Duration::ZERO;
    let mut step_for = |overlay: &mut MediaOverlayWidget, ms: u64| {
        let until = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < until {
            let t0 = Instant::now();
            overlay.step_video(now());
            max_step = max_step.max(t0.elapsed());
            std::thread::sleep(Duration::from_millis(16));
        }
    };

    step_for(&mut overlay, 600);
    let played = overlay.controls().position_ms;
    assert!(
        (300..=900).contains(&played),
        "position after 0.6 s of playback: {played} ms"
    );

    overlay.seek_video(2_000, now());
    step_for(&mut overlay, 400);
    let after_seek = overlay.controls().position_ms;
    assert!(
        (2_000..=2_600).contains(&after_seek),
        "position 0.4 s after a seek to 2000 ms: {after_seek} ms"
    );
    assert!(
        max_step < Duration::from_millis(50),
        "one viewer step took {max_step:?}"
    );
}
