//! Videos in the media viewer play their sound through the app's mixer, and the picture
//! follows the sound (TGB-3041).
//!
//! No audio device: the test renders the shared [`MixerCore`] itself, like a sound card whose
//! clock runs 0.2 % fast for five minutes, and stamps each buffer with the instant it would be heard (20 ms of
//! device latency). Time is the app's virtual test clock, so the run is deterministic.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_app_core::clock::set_test_now;
use tg_app_media::media::{
    MOCK_CHAT_PEER_ID, MOCK_VIDEO_ID, MediaItemKind, MediaOverlayWidget, MockMediaBackend,
    MockVideoFile, VideoSoundOutput, generate_test_mp4,
};
use tg_audio::{AudioMsgId, AudioType, MemoryPcmStream, MixerCore, PlaybackState};
use tg_text::TextWithEntities;

const TICK_MS: i64 = 10;
/// Frames the fake card plays per 10 ms tick: 480 at 48 kHz, plus 0.2 % clock skew.
const CARD_FRAMES_PER_TICK: usize = 481;
const LATENCY_MS: u64 = 20;

struct Rig {
    base: Instant,
    world_ms: i64,
    mixer: Arc<Mutex<MixerCore>>,
    sum_sq: f64,
    samples: usize,
    max_abs_offset_ms: i64,
    offsets: Vec<i64>,
    last_pts: Option<i64>,
}

impl Rig {
    fn instant(&self) -> Instant {
        self.base + Duration::from_millis(self.world_ms.unsigned_abs())
    }

    /// The media position the listener hears now, from the last stamped buffer.
    fn heard_now_ms(&self) -> Option<i64> {
        let mixer = self.mixer.lock().expect("mixer");
        let id = mixer.current_state(AudioType::Video).id;
        let point = mixer.external_sync_point(id)?;
        let now = self.instant();
        let delta = if point.at >= now {
            -(point.at.duration_since(now).as_millis() as i64)
        } else {
            now.duration_since(point.at).as_millis() as i64
        };
        Some(point.position_ms + delta)
    }

    /// One 10 ms tick: the card renders a buffer, then the viewer steps.
    fn tick(&mut self, overlay: &mut MediaOverlayWidget, measure: bool) {
        self.world_ms += TICK_MS;
        let now = self.instant();
        set_test_now(Some(now));
        let mut buf = vec![0.0_f32; CARD_FRAMES_PER_TICK * 2];
        let _ = self
            .mixer
            .lock()
            .expect("mixer")
            .render_stereo_48k_at(&mut buf, now + Duration::from_millis(LATENCY_MS));
        self.sum_sq += buf
            .iter()
            .map(|s| f64::from(*s) * f64::from(*s))
            .sum::<f64>();
        self.samples += buf.len();

        overlay.step_video(self.world_ms);
        let pts = overlay.video_frame_pts_ms();
        if measure
            && pts != self.last_pts
            && let (Some(pts), Some(heard)) = (pts, self.heard_now_ms())
        {
            let offset = pts - heard;
            self.max_abs_offset_ms = self.max_abs_offset_ms.max(offset.abs());
            self.offsets.push(offset);
        }
        self.last_pts = pts;
    }

    fn rms(&self) -> f64 {
        (self.sum_sq / self.samples.max(1) as f64).sqrt()
    }
}

#[test]
fn viewer_video_plays_its_sound_and_the_picture_follows_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mp4 = dir.path().join("sound.mp4");
    generate_test_mp4(&mp4, 160, 120, 302, true).expect("generate mp4");
    let backend = MockMediaBackend::new();
    backend
        .register_video_file(MockVideoFile {
            id: MOCK_VIDEO_ID,
            kind: MediaItemKind::Video,
            path: mp4,
            width: 160,
            height: 120,
            duration_ms: 302_000,
            sender_name: "Studio Feed".to_owned(),
            caption: TextWithEntities::simple("Generated test video"),
        })
        .expect("register video");

    let base = Instant::now();
    set_test_now(Some(base));
    let mixer = Arc::new(Mutex::new(MixerCore::new()));

    // Music was playing: a video with sound pauses it (`playbackPauseMusic`).
    let song = AudioMsgId {
        external_play_id: AudioMsgId::create_external_play_id(),
        audio_type: AudioType::Song,
        ..AudioMsgId::default()
    };
    mixer
        .lock()
        .expect("mixer")
        .play(
            song,
            Box::new(MemoryPcmStream::new(vec![0.1; 48_000 * 2 * 10], 48_000, 2)),
            0,
            1.0,
            false,
        )
        .expect("play song");

    let mut overlay = MediaOverlayWidget::new();
    overlay.set_video_sound_output(Some(VideoSoundOutput::new(Arc::clone(&mixer), None)));
    overlay
        .open_item(&backend, MOCK_CHAT_PEER_ID, MOCK_VIDEO_ID, 0)
        .expect("open video");

    let mut rig = Rig {
        base,
        world_ms: 0,
        mixer: Arc::clone(&mixer),
        sum_sq: 0.0,
        samples: 0,
        max_abs_offset_ms: 0,
        offsets: Vec::new(),
        last_pts: None,
    };
    // One tick lets the viewer start; the music fades out before the sound is measured.
    rig.tick(&mut overlay, false);
    let song_state = mixer
        .lock()
        .expect("mixer")
        .current_state(AudioType::Song)
        .state;
    assert!(
        matches!(song_state, PlaybackState::Pausing | PlaybackState::Paused),
        "music pauses while the video plays its sound: {song_state:?}"
    );
    for _ in 0..60 {
        rig.tick(&mut overlay, false);
    }
    rig.sum_sq = 0.0;
    rig.samples = 0;

    // 5 minutes of playback.
    for _ in 0..30_000 {
        rig.tick(&mut overlay, true);
    }
    let heard = rig
        .heard_now_ms()
        .expect("the mixer played the video sound");
    let mean = rig.offsets.iter().sum::<i64>() as f64 / rig.offsets.len().max(1) as f64;
    println!(
        "5 min with the card 0.2 % fast: sound rms {:.3}, heard at {heard} ms after {} ms of \
         world time; A/V offset over {} frames: max |offset| {} ms, mean {mean:.1} ms",
        rig.rms(),
        rig.world_ms,
        rig.offsets.len(),
        rig.max_abs_offset_ms
    );
    assert!(
        rig.rms() > 0.03,
        "the video's sound is audible: rms {}",
        rig.rms()
    );
    assert!(
        heard > 300_000,
        "the sound kept the card's (fast) pace: heard {heard} ms"
    );
    assert!(
        rig.offsets.len() > 7_000,
        "frames presented: {}",
        rig.offsets.len()
    );
    assert!(
        rig.max_abs_offset_ms <= 40,
        "picture and sound drift apart by up to {} ms",
        rig.max_abs_offset_ms
    );

    // A seek restarts the sound at the target, and the picture follows it again.
    overlay.seek_video(20_000, rig.world_ms);
    rig.offsets.clear();
    rig.max_abs_offset_ms = 0;
    for _ in 0..100 {
        rig.tick(&mut overlay, true);
    }
    let heard = rig.heard_now_ms().expect("sound after the seek");
    assert!(
        (20_500..=21_100).contains(&heard),
        "1 s after a seek to 20 s the sound is at {heard} ms"
    );
    assert!(
        rig.max_abs_offset_ms <= 40,
        "after the seek picture and sound are {} ms apart",
        rig.max_abs_offset_ms
    );

    // Closing the viewer stops the sound.
    overlay.close();
    let video_state = mixer.lock().expect("mixer").current_state(AudioType::Video);
    assert!(
        video_state.state.is_stopped(),
        "closing the viewer stops the sound: {:?}",
        video_state.state
    );
    set_test_now(None);
}
