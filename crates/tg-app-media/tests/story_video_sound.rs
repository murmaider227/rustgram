//! Video stories play their sound through the app's mixer, like the media viewer's videos
//! (TGB-3049). Before, the story player threw the decoded sound away.
//!
//! No audio device: the test renders the shared [`MixerCore`] itself, like a sound card, and
//! stamps each buffer with the instant it would be heard (20 ms of device latency). Time is the
//! app's virtual test clock, so the run is deterministic.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_app_core::clock::set_test_now;
use tg_app_media::media::{VideoSoundOutput, generate_test_mp4};
use tg_app_media::stories::{StoryMediaAsset, StoryMediaCache};
use tg_audio::{AudioType, MixerCore, PlaybackState};
use tg_data::{FullStoryId, PeerId};

const TICK_MS: i64 = 10;
const CARD_FRAMES_PER_TICK: usize = 480;
const LATENCY_MS: u64 = 20;

struct Rig {
    base: Instant,
    world_ms: i64,
    mixer: Arc<Mutex<MixerCore>>,
    sum_sq: f64,
    samples: usize,
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

    fn video_state(&self) -> PlaybackState {
        self.mixer
            .lock()
            .expect("mixer")
            .current_state(AudioType::Video)
            .state
    }

    /// One 10 ms tick: the card renders a buffer, then the story steps. Returns the A/V
    /// offset of the frame on screen, if both are known.
    fn tick(&mut self, cache: &mut StoryMediaCache, id: FullStoryId) -> Option<i64> {
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
        let video = cache.active_video_mut(id)?;
        let _ = video.step(None, self.world_ms);
        let pts = video.current_frame().map(|f| f.pts_ms)?;
        Some(pts - self.heard_now_ms()?)
    }

    fn take_rms(&mut self) -> f64 {
        let rms = (self.sum_sq / self.samples.max(1) as f64).sqrt();
        self.sum_sq = 0.0;
        self.samples = 0;
        rms
    }
}

fn story_id(story: i32) -> FullStoryId {
    FullStoryId {
        peer: PeerId::from_raw(777_000),
        story,
    }
}

#[test]
fn video_story_plays_its_sound_in_sync_and_stops_with_the_story() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mp4 = dir.path().join("story.mp4");
    generate_test_mp4(&mp4, 180, 320, 12, true).expect("generate mp4");

    let base = Instant::now();
    set_test_now(Some(base));
    let mixer = Arc::new(Mutex::new(MixerCore::new()));
    let mut cache = StoryMediaCache::new();
    cache.set_video_sound_output(Some(VideoSoundOutput::new(Arc::clone(&mixer), None)));
    let mut rig = Rig {
        base,
        world_ms: 0,
        mixer: Arc::clone(&mixer),
        sum_sq: 0.0,
        samples: 0,
    };

    let id = story_id(1);
    let asset = StoryMediaAsset::VideoPath(mp4.clone());
    let video = cache
        .ensure_video(None, id, &asset, false, 0.8, false, rig.world_ms)
        .expect("story video opens");
    assert!(video.has_sound_output(), "a story with sound plays it");

    // Let the sound start, then measure 5 s of playback.
    for _ in 0..50 {
        let _ = rig.tick(&mut cache, id);
    }
    let _ = rig.take_rms();
    let mut max_offset = 0i64;
    let mut offsets = 0usize;
    for _ in 0..500 {
        if let Some(offset) = rig.tick(&mut cache, id) {
            max_offset = max_offset.max(offset.abs());
            offsets += 1;
        }
    }
    let rms = rig.take_rms();
    println!("story sound rms {rms:.3}, max |A/V offset| {max_offset} ms over {offsets} ticks");
    assert!(rms > 0.01, "the story's sound is audible: rms {rms}");
    assert!(
        offsets > 400,
        "picture and sound positions known: {offsets}"
    );
    assert!(
        max_offset <= 80,
        "picture and sound drift apart by up to {max_offset} ms"
    );

    // Mute: the mixer's video volume goes to 0 and nothing is heard.
    cache
        .active_video_mut(id)
        .expect("active")
        .set_volume(0.8, true);
    for _ in 0..20 {
        let _ = rig.tick(&mut cache, id);
    }
    let _ = rig.take_rms();
    for _ in 0..50 {
        let _ = rig.tick(&mut cache, id);
    }
    let muted_rms = rig.take_rms();
    assert!(muted_rms < 1e-4, "muted story is silent: rms {muted_rms}");
    cache
        .active_video_mut(id)
        .expect("active")
        .set_volume(0.8, false);

    // Pause (hold to pause, a box opened): the sound pauses with the picture.
    cache
        .active_video_mut(id)
        .expect("active")
        .set_paused(true, rig.world_ms);
    for _ in 0..30 {
        let _ = rig.tick(&mut cache, id);
    }
    assert!(
        matches!(
            rig.video_state(),
            PlaybackState::Pausing | PlaybackState::Paused
        ),
        "paused story pauses its sound: {:?}",
        rig.video_state()
    );
    cache
        .active_video_mut(id)
        .expect("active")
        .set_paused(false, rig.world_ms);
    for _ in 0..30 {
        let _ = rig.tick(&mut cache, id);
    }
    assert_eq!(rig.video_state(), PlaybackState::Playing);

    // Switching to the next story (or closing the viewer) stops the sound.
    cache.clear_active_video();
    assert!(
        rig.video_state().is_stopped(),
        "switching stories stops the sound: {:?}",
        rig.video_state()
    );
}

#[test]
fn silent_video_story_attaches_no_sound() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mp4 = dir.path().join("silent.mp4");
    generate_test_mp4(&mp4, 180, 320, 2, true).expect("generate mp4");
    let mixer = Arc::new(Mutex::new(MixerCore::new()));
    let mut cache = StoryMediaCache::new();
    cache.set_video_sound_output(Some(VideoSoundOutput::new(Arc::clone(&mixer), None)));
    let video = cache
        .ensure_video(
            None,
            story_id(2),
            &StoryMediaAsset::VideoPath(mp4),
            true,
            0.8,
            false,
            0,
        )
        .expect("story video opens");
    assert!(!video.has_sound_output());
}
