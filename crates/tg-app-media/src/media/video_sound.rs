//! Sound of videos in the media viewer and Picture-in-Picture (TGB-3041).
//!
//! tdesktop plays a streamed video's audio track through the one global mixer
//! (`Media::Streaming::AudioTrack` → `Mixer` video slot) and lets the picture follow the
//! mixer's played position (`syncVideoByAudio`). Here the viewer's [`ViewerPlayer`] gets a
//! [`PlayerAudio`] that feeds the same [`MixerCore`] and output device as voice and music
//! playback (the [`crate::media::player_bar::AudioPlayerBar`]'s), so the device choice is
//! shared, and the viewer's volume is the mixer's video volume (cubic, like
//! `Mixer::setVideoVolume`).
//!
//! [`ViewerPlayer`]: crate::media::video_playback::ViewerPlayer

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use tg_audio::{AudioMsgId, AudioOutputEngine, AudioType, ExternalPcmFeed, MixerCore};
use tg_video::{AudioFrame, AudioSink, PlayerAudio, TimePoint};

/// Where viewer videos play their sound: the app's mixer and, in the app, its output device.
#[derive(Clone)]
pub struct VideoSoundOutput {
    mixer: Arc<Mutex<MixerCore>>,
    output: Option<Arc<AudioOutputEngine>>,
    clock: fn() -> Instant,
}

impl std::fmt::Debug for VideoSoundOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoSoundOutput")
            .field("hardware", &self.output.is_some())
            .finish_non_exhaustive()
    }
}

impl VideoSoundOutput {
    /// Plays into `mixer`; `output` is the device engine rendering it (`None` when something
    /// else renders the mixer, e.g. a test's fake sink).
    #[must_use]
    pub fn new(mixer: Arc<Mutex<MixerCore>>, output: Option<Arc<AudioOutputEngine>>) -> Self {
        Self {
            mixer,
            output,
            clock: tg_app_core::clock::now,
        }
    }

    /// The shared mixer.
    #[must_use]
    pub const fn mixer(&self) -> &Arc<Mutex<MixerCore>> {
        &self.mixer
    }

    /// The video volume every video player plays at (the mixer's, `0.0..=1.0`).
    #[must_use]
    pub fn volume(&self) -> f64 {
        self.lock().video_volume()
    }

    /// Sets the video volume for every video player (`Mixer::setVideoVolume`), also when the
    /// open video has no sound, like tdesktop's viewer.
    pub fn set_volume(&self, volume: f64) {
        self.lock().set_video_volume(volume);
    }

    /// `true` when both handles point at the same mixer and device engine.
    #[must_use]
    pub fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.mixer, &other.mixer)
            && match (&self.output, &other.output) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }

    /// A new sound output for one player.
    #[must_use]
    pub fn player_audio(&self) -> Box<dyn PlayerAudio> {
        Box::new(ViewerSound {
            out: self.clone(),
            feed: ExternalPcmFeed::new(),
            id: AudioMsgId::for_video(),
            started: false,
            playing: false,
            speed: 1.0,
        })
    }

    fn lock(&self) -> MutexGuard<'_, MixerCore> {
        self.mixer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn ensure_device(&self) {
        if let Some(output) = &self.output
            && let Err(err) = output.ensure_attached()
        {
            tracing::warn!("video sound: no output device: {err}");
        }
    }
}

struct FeedSink(ExternalPcmFeed);

impl AudioSink for FeedSink {
    fn push(&self, frame: AudioFrame) {
        self.0.push(
            frame.samples,
            frame.sample_rate,
            usize::from(frame.channels),
            frame.pts_ms,
        );
    }

    fn clear(&self) {
        self.0.clear();
    }
}

/// One player's sound: a feed into the mixer's video slot.
struct ViewerSound {
    out: VideoSoundOutput,
    feed: ExternalPcmFeed,
    id: AudioMsgId,
    /// The mixer's video slot plays this feed (from its first frame's position).
    started: bool,
    playing: bool,
    speed: f64,
}

impl ViewerSound {
    /// Starts the mixer track once the first decoded frame tells where the sound begins
    /// (tdesktop's `AudioTrack::mixerInit` with `_startedPosition`).
    fn start_when_ready(&mut self) {
        if self.started || !self.playing {
            return;
        }
        let Some(first_pts) = self.feed.first_pts_ms() else {
            return;
        };
        let mut mixer = self.out.lock();
        if mixer
            .play(
                self.id,
                Box::new(self.feed.stream()),
                first_pts.max(0),
                self.speed,
                false,
            )
            .is_ok()
        {
            self.started = true;
        }
        drop(mixer);
        if self.started {
            self.out.ensure_device();
        }
    }

    /// Pauses voice messages and music while a video with sound plays
    /// (`OverlayWidget::playbackPauseMusic`).
    fn pause_music(mixer: &mut MixerCore) {
        for audio_type in [AudioType::Voice, AudioType::Song] {
            let state = mixer.current_state(audio_type);
            if !state.state.is_stopped() && !state.state.is_paused() {
                mixer.pause(state.id, false);
            }
        }
    }
}

impl PlayerAudio for ViewerSound {
    fn sink(&self) -> Arc<dyn AudioSink> {
        Arc::new(FeedSink(self.feed.clone()))
    }

    fn reset(&mut self) {
        // The queued sound is dropped through `AudioSink::clear`, atomically with the seek.
        if self.started {
            self.out.lock().stop(self.id);
            self.started = false;
        }
    }

    fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
        {
            let mut mixer = self.out.lock();
            if playing {
                Self::pause_music(&mut mixer);
            }
            if self.started {
                if playing {
                    mixer.resume(self.id, true);
                } else {
                    mixer.pause(self.id, true);
                }
            }
        }
        if playing && self.started {
            self.out.ensure_device();
        }
        self.start_when_ready();
    }

    fn set_speed(&mut self, speed: f64) {
        self.speed = speed;
        if self.started {
            self.out.lock().set_speed(self.id, speed);
        }
    }

    fn set_volume(&mut self, volume: f64) {
        self.out.lock().set_video_volume(volume);
    }

    fn sync_point(&mut self, world_now_ms: i64) -> Option<TimePoint> {
        self.start_when_ready();
        if !self.started {
            return None;
        }
        let point = self.out.lock().external_sync_point(self.id)?;
        // `point.at` is on the monotonic clock; move the point to `world_now_ms`, which is the
        // caller's world time for the same instant.
        let now = (self.out.clock)();
        let ahead_ms = if point.at >= now {
            point.at.duration_since(now).as_secs_f64() * 1000.0
        } else {
            -(now.duration_since(point.at).as_secs_f64() * 1000.0)
        };
        let track = point.position_ms as f64 - ahead_ms * self.speed;
        Some(TimePoint::new(track.round() as i64, world_now_ms))
    }
}

impl Drop for ViewerSound {
    fn drop(&mut self) {
        if self.started {
            self.out.lock().stop(self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_audio::PlaybackState;

    fn frame(pts_ms: i64) -> AudioFrame {
        AudioFrame {
            sample_rate: 48_000,
            channels: 2,
            pts_ms,
            duration_ms: 20,
            samples: vec![0.25; 960 * 2],
        }
    }

    fn render(mixer: &Arc<Mutex<MixerCore>>, ms: usize) -> f32 {
        let mut buf = vec![0.0_f32; ms * 48 * 2];
        let _ = mixer
            .lock()
            .expect("mixer")
            .render_stereo_48k_at(&mut buf, Instant::now());
        buf.iter().fold(0.0_f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn volume_pause_seek_and_drop_act_on_the_mixer_video_slot() {
        let mixer = Arc::new(Mutex::new(MixerCore::new()));
        let output = VideoSoundOutput::new(Arc::clone(&mixer), None);
        let mut sound = output.player_audio();
        let sink = sound.sink();
        for i in 0..50 {
            sink.push(frame(5_000 + i * 20));
        }
        assert!(
            mixer
                .lock()
                .expect("mixer")
                .current_state(AudioType::Video)
                .state
                .is_stopped(),
            "nothing plays before the player starts"
        );
        sound.set_volume(0.5);
        sound.set_playing(true);
        let state = mixer.lock().expect("mixer").current_state(AudioType::Video);
        assert_eq!(state.state, PlaybackState::Playing);
        assert_eq!(
            state.position_ms(),
            5_000,
            "starts at the first frame's position"
        );
        assert!((mixer.lock().expect("mixer").video_volume() - 0.5).abs() < 1e-9);
        let peak = render(&mixer, 40);
        assert!(
            (peak - 0.25 * 0.125).abs() < 0.01,
            "the volume is cubic like Mixer::setVideoVolume: peak {peak}"
        );
        assert!(sound.sync_point(1_000).is_some());

        sound.set_playing(false);
        let paused_at = mixer.lock().expect("mixer").current_state(AudioType::Video);
        assert_eq!(paused_at.state, PlaybackState::Paused);
        assert!(render(&mixer, 40) < 1e-6, "paused: silence");

        sound.reset();
        sink.clear();
        assert!(
            mixer
                .lock()
                .expect("mixer")
                .current_state(AudioType::Video)
                .state
                .is_stopped(),
            "a seek stops the old sound"
        );
        sink.push(frame(30_000));
        assert!(
            sound.sync_point(2_000).is_none(),
            "paused: does not restart yet"
        );
        sound.set_playing(true);
        assert_eq!(
            mixer
                .lock()
                .expect("mixer")
                .current_state(AudioType::Video)
                .position_ms(),
            30_000,
            "restarts at the first frame after the seek"
        );

        drop(sound);
        assert!(
            mixer
                .lock()
                .expect("mixer")
                .current_state(AudioType::Video)
                .state
                .is_stopped(),
            "closing the video stops its sound"
        );
    }
}
