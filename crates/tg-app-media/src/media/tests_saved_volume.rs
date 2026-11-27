//! The viewer and PiP start with the saved video volume and save changes a second later
//! (TGB-3050). Before, every app start played videos at 100 %.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_audio::MixerCore;
use tg_text::TextWithEntities;

use super::*;
use crate::media::{
    MOCK_CHAT_PEER_ID, MOCK_VIDEO_ID, MockMediaBackend, MockVideoFile, generate_test_mp4,
};

fn backend_with_video(dir: &std::path::Path) -> MockMediaBackend {
    let mp4 = dir.join("volume.mp4");
    generate_test_mp4(&mp4, 160, 120, 3, true).expect("generate mp4");
    let backend = MockMediaBackend::new();
    backend
        .register_video_file(MockVideoFile {
            id: MOCK_VIDEO_ID,
            kind: MediaItemKind::Video,
            path: mp4,
            width: 160,
            height: 120,
            duration_ms: 3_000,
            sender_name: "Studio Feed".to_owned(),
            caption: TextWithEntities::simple("Generated test video"),
        })
        .expect("register video");
    backend
}

#[test]
fn viewer_and_pip_start_with_the_saved_volume_and_save_changes_delayed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backend = backend_with_video(dir.path());
    let output = VideoSoundOutput::new(Arc::new(Mutex::new(MixerCore::new())), None);
    let t0 = Instant::now();
    let mut saved = 0.4;

    // App start: the window controller syncs every frame, before any video opens.
    let mut overlay = MediaOverlayWidget::new();
    assert!(!overlay.sync_saved_video_volume(&output, &mut saved, t0));
    overlay
        .open_item(&backend, MOCK_CHAT_PEER_ID, MOCK_VIDEO_ID, 0)
        .expect("open video");
    overlay.set_video_sound_output(Some(output.clone()));
    overlay.step_video(16);
    assert!(
        (overlay.controls().volume - 0.4).abs() < 1e-9,
        "the viewer starts at the saved volume, not 100 %: {}",
        overlay.controls().volume
    );
    assert!(
        (output.volume() - 0.4).abs() < 1e-9,
        "and so does the sound"
    );

    // The user lowers the volume: saved a second later, not on every slider step.
    let item = overlay.gallery.current().cloned().expect("current item");
    let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let mut events = Vec::new();
    overlay.handle_playback_action(
        PlaybackControlsAction::VolumeChanged(0.7),
        rect,
        &item,
        32,
        &mut events,
    );
    assert!((output.volume() - 0.7).abs() < 1e-9);
    assert!(!overlay.sync_saved_video_volume(&output, &mut saved, t0 + Duration::from_millis(100)));
    assert!((saved - 0.4).abs() < 1e-9, "not saved at once");
    assert!(overlay.sync_saved_video_volume(
        &output,
        &mut saved,
        t0 + Duration::from_millis(1_200)
    ));
    assert!((saved - 0.7).abs() < 1e-9, "saved after the delay: {saved}");

    // PiP takes the same volume.
    overlay.handle_playback_action(
        PlaybackControlsAction::ToPictureInPicture,
        rect,
        &item,
        48,
        &mut events,
    );
    let pip = events
        .iter()
        .find_map(|e| match e {
            OverlayEvent::EnterPip(pip) => Some(pip),
            _ => None,
        })
        .expect("entered PiP");
    assert!(
        (pip.volume() - 0.7).abs() < 1e-9,
        "PiP volume {}",
        pip.volume()
    );

    // Next app start: a fresh mixer and viewer read the saved value.
    let output = VideoSoundOutput::new(Arc::new(Mutex::new(MixerCore::new())), None);
    let mut overlay = MediaOverlayWidget::new();
    let _ = overlay.sync_saved_video_volume(&output, &mut saved, t0 + Duration::from_secs(10));
    overlay
        .open_item(&backend, MOCK_CHAT_PEER_ID, MOCK_VIDEO_ID, 0)
        .expect("open video");
    assert!((overlay.controls().volume - 0.7).abs() < 1e-9);
}
