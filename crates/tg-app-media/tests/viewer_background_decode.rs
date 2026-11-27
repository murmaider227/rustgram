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

/// The app's path for video sound (TGB-3041): the decoder thread feeds the mixer, a real-time
/// "sound card" thread renders it, and the picture follows the played position.
#[test]
fn viewer_sound_from_the_decoder_thread_keeps_the_picture_in_sync() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use tg_app_media::media::VideoSoundOutput;
    use tg_audio::{AudioType, MixerCore};

    tg_emoji::background::set_background_decode(true);
    let dir = tempfile::tempdir().expect("tempdir");
    let mp4 = dir.path().join("sound.mp4");
    generate_test_mp4(&mp4, 320, 240, 4, true).expect("generate mp4");
    let backend = MockMediaBackend::new();
    backend
        .register_video_file(MockVideoFile {
            id: MOCK_VIDEO_ID,
            kind: MediaItemKind::Video,
            path: mp4,
            width: 320,
            height: 240,
            duration_ms: 4_000,
            sender_name: "Studio Feed".to_owned(),
            caption: TextWithEntities::simple("Generated test video"),
        })
        .expect("register video");

    let mixer = Arc::new(Mutex::new(MixerCore::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let energy = Arc::new(Mutex::new((0.0_f64, 0_usize)));
    let card = {
        let (mixer, stop, energy) = (Arc::clone(&mixer), Arc::clone(&stop), Arc::clone(&energy));
        std::thread::spawn(move || {
            let start = Instant::now();
            let mut rendered = 0_u64;
            while !stop.load(Ordering::Acquire) {
                // Render whatever is due at 48 kHz, 20 ms ahead of the listener.
                let due = start.elapsed().as_micros() as u64 * 48 / 1_000;
                let frames = due.saturating_sub(rendered) as usize;
                if frames > 0 {
                    let heard_at = start
                        + Duration::from_micros(rendered * 1_000 / 48)
                        + Duration::from_millis(20);
                    let mut buf = vec![0.0_f32; frames * 2];
                    let _ = mixer
                        .lock()
                        .expect("mixer")
                        .render_stereo_48k_at(&mut buf, heard_at);
                    let mut e = energy.lock().expect("energy");
                    e.0 += buf.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>();
                    e.1 += buf.len();
                    rendered += frames as u64;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };

    let base = Instant::now();
    let now = || base.elapsed().as_millis() as i64;
    let mut overlay = MediaOverlayWidget::new();
    overlay.set_video_sound_output(Some(VideoSoundOutput::new(Arc::clone(&mixer), None)));
    overlay
        .open_item(&backend, MOCK_CHAT_PEER_ID, MOCK_VIDEO_ID, now())
        .expect("open video");

    let heard_now = || -> Option<i64> {
        let m = mixer.lock().expect("mixer");
        let point = m.external_sync_point(m.current_state(AudioType::Video).id)?;
        let at = Instant::now();
        Some(if point.at >= at {
            point.position_ms - point.at.duration_since(at).as_millis() as i64
        } else {
            point.position_ms + at.duration_since(point.at).as_millis() as i64
        })
    };
    let mut offsets = Vec::new();
    let mut last_pts = None;
    let until = Instant::now() + Duration::from_millis(3_000);
    while Instant::now() < until {
        overlay.step_video(now());
        let pts = overlay.video_frame_pts_ms();
        if pts != last_pts
            && let (Some(pts), Some(heard)) = (pts, heard_now())
        {
            offsets.push(pts - heard);
        }
        last_pts = pts;
        std::thread::sleep(Duration::from_millis(4));
    }
    stop.store(true, Ordering::Release);
    card.join().expect("card thread");

    let (sum_sq, n) = *energy.lock().expect("energy");
    let rms = (sum_sq / n.max(1) as f64).sqrt();
    let mean = offsets.iter().sum::<i64>() as f64 / offsets.len().max(1) as f64;
    let max = offsets.iter().map(|o| o.abs()).max().unwrap_or(i64::MAX);
    println!(
        "background decode, real time: sound rms {rms:.3}; {} frames, A/V offset mean \
         {mean:.1} ms, max |offset| {max} ms",
        offsets.len()
    );
    assert!(rms > 0.03, "the sound plays: rms {rms}");
    assert!(offsets.len() > 50, "frames presented: {}", offsets.len());
    assert!(
        mean.abs() <= 20.0,
        "picture and sound are {mean:.1} ms apart on average"
    );
}
