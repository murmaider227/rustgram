//! Software audio mixer, per-track state machine, fades, suppression/ducking,
//! and UI sound playback.
//!
//! Port of `media/audio/media_audio.h` and `media/audio/media_audio.cpp`
//! (`Mixer`, `Mixer::Track`, `Fader`, `ComputeVolume`).

use std::collections::VecDeque;
use std::time::Instant;

use crate::dsp::{Resampler, TimeStretcher, soft_clip, to_stereo_into};
use crate::error::AudioResult;
use crate::sound_track::{SoundTrack, SoundTrackId};
use crate::types::{
    AudioMsgId, AudioType, CHECK_PLAYBACK_POSITION_DELTA, DEFAULT_FREQUENCY,
    DETACH_DEVICE_TIMEOUT_MS, FADE_DURATION_MS, PlaybackState, SUPPRESS_ATTACK_MS,
    SUPPRESS_RATIO_ALL, SUPPRESS_RATIO_SONG, TOGETHER_LIMIT, TrackState, clamp_speed, equal_speeds,
    speed_dependent_position, speed_independent_position,
};

/// A decoded PCM chunk returned by an [`AudioStream`].
#[derive(Debug, Clone)]
pub struct PcmChunk {
    /// Interleaved normalized `f32` PCM samples (`-1.0..=1.0`).
    pub samples: Vec<f32>,
    /// Sample rate in Hz of `samples`.
    pub sample_rate: u32,
    /// Number of interleaved channels in `samples`.
    pub channels: usize,
}

/// Abstract streaming PCM source consumed by [`MixerCore`].
///
/// Implemented by [`crate::decoder::AudioDecoder`] and by in-memory PCM generators
/// in unit tests.
pub trait AudioStream: Send {
    /// Native sample rate of the stream in Hz.
    fn sample_rate(&self) -> u32;

    /// Native channel count of the stream.
    fn channels(&self) -> usize;

    /// Known total duration in milliseconds (0 if unknown).
    fn duration_ms(&self) -> i64;

    /// Seeks to `position_ms` from the beginning of the stream.
    fn seek_ms(&mut self, position_ms: i64) -> AudioResult<()>;

    /// Reads the next chunk of interleaved `f32` samples.
    /// Returns `Ok(None)` on end-of-stream.
    fn read_chunk(&mut self) -> AudioResult<Option<PcmChunk>>;

    /// `true` while the stream has no samples yet but is not finished (a streamed video whose
    /// decoder has not delivered the next audio frame). The mixer then plays silence and keeps
    /// the track position where it is, like tdesktop's `waitingForData` track state.
    fn waiting_for_data(&self) -> bool {
        false
    }
}

/// In-memory PCM [`AudioStream`] backed by a `Vec<f32>` (useful for tests and
/// pre-decoded clips).
pub struct MemoryPcmStream {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: usize,
    cursor_frames: usize,
    chunk_frames: usize,
}

impl MemoryPcmStream {
    /// Creates a stream over interleaved `f32` samples.
    #[must_use]
    pub fn new(samples: Vec<f32>, sample_rate: u32, channels: usize) -> Self {
        Self {
            samples,
            sample_rate: sample_rate.max(1),
            channels: channels.max(1),
            cursor_frames: 0,
            chunk_frames: 2048,
        }
    }

    fn total_frames(&self) -> usize {
        self.samples.len() / self.channels
    }
}

impl AudioStream for MemoryPcmStream {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> usize {
        self.channels
    }

    fn duration_ms(&self) -> i64 {
        ((self.total_frames() as i64) * 1000) / i64::from(self.sample_rate)
    }

    fn seek_ms(&mut self, position_ms: i64) -> AudioResult<()> {
        let target = ((position_ms.max(0) as u64) * u64::from(self.sample_rate)) / 1000;
        self.cursor_frames = (target as usize).min(self.total_frames());
        Ok(())
    }

    fn read_chunk(&mut self) -> AudioResult<Option<PcmChunk>> {
        let total = self.total_frames();
        if self.cursor_frames >= total {
            return Ok(None);
        }
        let take = self.chunk_frames.min(total - self.cursor_frames);
        let start = self.cursor_frames * self.channels;
        let end = (self.cursor_frames + take) * self.channels;
        self.cursor_frames += take;
        Ok(Some(PcmChunk {
            samples: self.samples[start..end].to_vec(),
            sample_rate: self.sample_rate,
            channels: self.channels,
        }))
    }
}

/// Computes tdesktop's cubic perceptual gain (`ComputeVolume` in `media_audio.cpp:251-263`).
///
/// - `Voice`: `(mult_all * song_volume)^3`
/// - `Song`: `(mult_song * song_volume)^3`
/// - `Video`: `video_volume^3`
#[must_use]
pub fn compute_volume(
    audio_type: AudioType,
    song_volume: f64,
    video_volume: f64,
    mult_all: f64,
    mult_song: f64,
) -> f64 {
    let gain = match audio_type {
        AudioType::Voice => mult_all * song_volume,
        AudioType::Song => mult_song * song_volume,
        AudioType::Video => video_volume,
        AudioType::Unknown => 1.0,
    }
    .clamp(0.0, 1.0);
    gain * gain * gain
}

/// Events emitted by [`MixerCore`] during playback and rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MixerEvent {
    /// Track position advanced by at least [`CHECK_PLAYBACK_POSITION_DELTA`] samples.
    PositionUpdated(AudioMsgId),
    /// Track state changed (playing, paused, fading, etc.).
    StateChanged(AudioMsgId, PlaybackState),
    /// Track stopped (normally, at end, or on error).
    Stopped(AudioMsgId, PlaybackState),
    /// A decoding/playback error stopped the track.
    Error(AudioMsgId),
    /// A one-shot UI/notification [`SoundTrack`] finished playing.
    SoundTrackFinished(SoundTrackId),
}

/// Internal per-track mixer slot (`Mixer::Track` in `media_audio.h:205-280`).
struct MixerTrack {
    state: TrackState,
    volume: f64,
    speed: f64,
    with_speed_position: i64,
    fade_start_position: i64,
    last_emitted_position: i64,
    stream: Option<Box<dyn AudioStream>>,
    resampler: Option<Resampler>,
    stretcher: TimeStretcher,
    pcm_queue: VecDeque<f32>,
    eof_reached: bool,
    stretcher_flushed: bool,
    /// Per-chunk scratch: stereo fold, 48 kHz resample and time-stretch output. Reused so a
    /// decoded chunk costs no allocation on the audio thread.
    scratch_stereo: Vec<f32>,
    scratch_48k: Vec<f32>,
    scratch_stretched: Vec<f32>,
}

impl Default for MixerTrack {
    fn default() -> Self {
        Self {
            state: TrackState::default(),
            volume: 1.0,
            speed: 1.0,
            with_speed_position: 0,
            fade_start_position: 0,
            last_emitted_position: 0,
            stream: None,
            resampler: None,
            stretcher: TimeStretcher::new(DEFAULT_FREQUENCY, 2, 1.0),
            pcm_queue: VecDeque::new(),
            eof_reached: false,
            stretcher_flushed: false,
            scratch_stereo: Vec::new(),
            scratch_48k: Vec::new(),
            scratch_stretched: Vec::new(),
        }
    }
}

impl MixerTrack {
    fn clear(&mut self) {
        self.state = TrackState::default();
        self.volume = 1.0;
        self.speed = 1.0;
        self.with_speed_position = 0;
        self.fade_start_position = 0;
        self.last_emitted_position = 0;
        self.stream = None;
        self.resampler = None;
        self.stretcher = TimeStretcher::new(DEFAULT_FREQUENCY, 2, 1.0);
        self.pcm_queue.clear();
        self.eof_reached = false;
        self.stretcher_flushed = false;
    }

    fn set_stopped(&mut self, state: PlaybackState) {
        self.state.state = state;
        self.state.position = 0;
        self.with_speed_position = 0;
        self.fade_start_position = 0;
        self.last_emitted_position = 0;
        self.pcm_queue.clear();
        self.stream = None;
        self.eof_reached = true;
    }

    fn reset_fade_start(&mut self) {
        self.fade_start_position = self.with_speed_position;
    }

    fn update_state_position(&mut self) {
        self.state.position = speed_independent_position(self.with_speed_position, self.speed);
    }

    fn ensure_buffered(&mut self, min_stereo_samples: usize) -> Result<(), String> {
        while self.pcm_queue.len() < min_stereo_samples && !self.stretcher_flushed {
            if self.eof_reached {
                let mut tail = Vec::new();
                self.stretcher.flush(&mut tail);
                self.pcm_queue.extend(tail);
                self.stretcher_flushed = true;
                break;
            }
            let Some(stream) = &mut self.stream else {
                self.eof_reached = true;
                continue;
            };
            if stream.waiting_for_data() {
                break;
            }
            match stream.read_chunk() {
                Ok(Some(chunk)) => {
                    if chunk.samples.is_empty() {
                        continue;
                    }
                    self.scratch_stereo.clear();
                    to_stereo_into(&chunk.samples, chunk.channels, &mut self.scratch_stereo);
                    let at_48k: &[f32] = if chunk.sample_rate == DEFAULT_FREQUENCY {
                        &self.scratch_stereo
                    } else {
                        let resampler = self.resampler.get_or_insert_with(|| {
                            Resampler::new(chunk.sample_rate, DEFAULT_FREQUENCY, 2)
                        });
                        if resampler.in_rate() != chunk.sample_rate {
                            *resampler = Resampler::new(chunk.sample_rate, DEFAULT_FREQUENCY, 2);
                        }
                        self.scratch_48k.clear();
                        resampler.process(&self.scratch_stereo, &mut self.scratch_48k);
                        &self.scratch_48k
                    };

                    if equal_speeds(self.speed, 1.0) {
                        self.pcm_queue.extend(at_48k.iter().copied());
                    } else {
                        self.scratch_stretched.clear();
                        self.stretcher.process(at_48k, &mut self.scratch_stretched);
                        self.pcm_queue
                            .extend(self.scratch_stretched.iter().copied());
                    }
                }
                Ok(None) => {
                    self.eof_reached = true;
                }
                Err(err) => {
                    return Err(err.to_string());
                }
            }
        }
        Ok(())
    }
}

/// Suppression (ducking) envelope state (`Fader` suppression logic in `media_audio.cpp`).
#[derive(Debug, Clone)]
struct SuppressionState {
    // Music ducking while voice plays (500ms linear ramp between 1.0 and 0.05).
    song_suppressed: bool,
    song_animating: bool,
    song_from: f64,
    song_to: f64,
    song_current: f64,
    song_elapsed_ms: f64,

    // Global ducking (e.g. notification chime): 150ms attack to 0.2, hold, 500ms release.
    all_active: bool,
    all_from: f64,
    all_to: f64,
    all_current: f64,
    all_elapsed_ms: f64,
    all_duration_ms: f64,

    mult_all: f64,
    mult_song: f64,
}

impl Default for SuppressionState {
    fn default() -> Self {
        Self {
            song_suppressed: false,
            song_animating: false,
            song_from: 1.0,
            song_to: 1.0,
            song_current: 1.0,
            song_elapsed_ms: 0.0,
            all_active: false,
            all_from: 1.0,
            all_to: 1.0,
            all_current: 1.0,
            all_elapsed_ms: 0.0,
            all_duration_ms: 0.0,
            mult_all: 1.0,
            mult_song: 1.0,
        }
    }
}

impl SuppressionState {
    fn suppress_song(&mut self) {
        if !self.song_suppressed {
            self.song_suppressed = true;
            self.song_animating = true;
            self.song_from = self.song_current;
            self.song_to = SUPPRESS_RATIO_SONG;
            self.song_elapsed_ms = 0.0;
        }
    }

    fn unsuppress_song(&mut self) {
        if self.song_suppressed {
            self.song_suppressed = false;
            self.song_animating = true;
            self.song_from = self.song_current;
            self.song_to = 1.0;
            self.song_elapsed_ms = 0.0;
        }
    }

    fn suppress_all(&mut self, duration_ms: i64) {
        let dur = duration_ms.max(SUPPRESS_ATTACK_MS) as f64;
        self.all_active = true;
        self.all_from = self.all_current;
        self.all_to = SUPPRESS_RATIO_ALL;
        self.all_elapsed_ms = 0.0;
        self.all_duration_ms = dur;
    }

    fn advance_ms(&mut self, dt_ms: f64) {
        if self.all_active {
            self.all_elapsed_ms += dt_ms;
            let fade_ms = FADE_DURATION_MS as f64;
            let attack_ms = SUPPRESS_ATTACK_MS as f64;
            if self.all_elapsed_ms >= self.all_duration_ms {
                self.all_active = false;
                self.all_current = 1.0;
            } else if self.all_elapsed_ms > self.all_duration_ms - fade_ms {
                let remaining = self.all_duration_ms - self.all_elapsed_ms;
                let progress = (1.0 - remaining / fade_ms).clamp(0.0, 1.0);
                self.all_current = SUPPRESS_RATIO_ALL + (1.0 - SUPPRESS_RATIO_ALL) * progress;
            } else if self.all_elapsed_ms >= attack_ms {
                self.all_current = SUPPRESS_RATIO_ALL;
            } else {
                let progress = (self.all_elapsed_ms / attack_ms).clamp(0.0, 1.0);
                self.all_current = self.all_from + (self.all_to - self.all_from) * progress;
            }
            self.mult_all = self.all_current;
        } else {
            self.mult_all = 1.0;
        }

        if self.song_animating {
            self.song_elapsed_ms += dt_ms;
            let fade_ms = FADE_DURATION_MS as f64;
            if self.song_elapsed_ms >= fade_ms {
                self.song_current = self.song_to;
                self.song_animating = false;
            } else {
                let progress = (self.song_elapsed_ms / fade_ms).clamp(0.0, 1.0);
                self.song_current = self.song_from + (self.song_to - self.song_from) * progress;
            }
        }
        self.mult_song = self.song_current.min(self.mult_all);
    }
}

/// The position the video track's sound reaches the listener at a given instant
/// (`Mixer::getExternalSyncTimePoint`: tdesktop's `lastUpdatePosition` / `lastUpdateWhen`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalSyncPoint {
    /// The video sound this point belongs to.
    pub audio: AudioMsgId,
    /// Media position (speed independent) of the first sample of the rendered buffer, in ms.
    pub position_ms: i64,
    /// When that sample is heard: the output callback's playback instant.
    pub at: Instant,
}

/// Real-time 48 kHz stereo software audio mixer.
///
/// Faithfully implements tdesktop's `Media::Player::Mixer` + `Fader` +
/// `Media::Audio::Track` mixing without requiring an audio device to run,
/// enabling deterministic testing and clean integration with `cpal`.
pub struct MixerCore {
    voice_tracks: [MixerTrack; TOGETHER_LIMIT],
    voice_current: usize,
    song_tracks: [MixerTrack; TOGETHER_LIMIT],
    song_current: usize,
    video_track: MixerTrack,
    sound_tracks: Vec<SoundTrack>,
    song_volume: f64,
    video_volume: f64,
    notifications_volume_pct: u8,
    suppression: SuppressionState,
    idle_samples: i64,
    /// Video track position at the start of the last rendered buffer, if it played any samples.
    last_video_render: Option<(AudioMsgId, i64)>,
    video_sync: Option<ExternalSyncPoint>,
}

impl Default for MixerCore {
    fn default() -> Self {
        Self::new()
    }
}

impl MixerCore {
    /// Creates a new idle [`MixerCore`] at full volume.
    #[must_use]
    pub fn new() -> Self {
        Self {
            voice_tracks: std::array::from_fn(|_| MixerTrack::default()),
            voice_current: 0,
            song_tracks: std::array::from_fn(|_| MixerTrack::default()),
            song_current: 0,
            video_track: MixerTrack::default(),
            sound_tracks: Vec::new(),
            song_volume: 1.0,
            video_volume: 1.0,
            notifications_volume_pct: 100,
            suppression: SuppressionState::default(),
            idle_samples: 0,
            last_video_render: None,
            video_sync: None,
        }
    }

    /// Sets music & voice volume (`0.0..=1.0`, matching `Mixer::setSongVolume`).
    pub fn set_song_volume(&mut self, volume: f64) {
        self.song_volume = volume.clamp(0.0, 1.0);
    }

    /// Returns music & voice volume (`0.0..=1.0`).
    #[must_use]
    pub const fn song_volume(&self) -> f64 {
        self.song_volume
    }

    /// Sets video audio volume (`0.0..=1.0`, matching `Mixer::setVideoVolume`).
    pub fn set_video_volume(&mut self, volume: f64) {
        self.video_volume = volume.clamp(0.0, 1.0);
    }

    /// Returns video audio volume (`0.0..=1.0`).
    #[must_use]
    pub const fn video_volume(&self) -> f64 {
        self.video_volume
    }

    /// Sets UI/notification sound volume percentage (`0..=100`).
    pub fn set_notifications_volume_pct(&mut self, pct: u8) {
        self.notifications_volume_pct = pct.min(100);
    }

    /// Returns the current suppression multipliers `(all, song)`.
    #[must_use]
    pub const fn suppression_multipliers(&self) -> (f64, f64) {
        (self.suppression.mult_all, self.suppression.mult_song)
    }

    /// Temporarily ducks all active playback for `duration_ms` (e.g. notification chime).
    pub fn suppress_all(&mut self, duration_ms: i64) {
        self.suppression.suppress_all(duration_ms);
    }

    /// Starts playing `audio` from `stream` at `position_ms` and `speed`.
    ///
    /// Port of `Mixer::play` (`media_audio.cpp:669-736`).
    pub fn play(
        &mut self,
        audio: AudioMsgId,
        mut stream: Box<dyn AudioStream>,
        position_ms: i64,
        speed: f64,
        start_paused: bool,
    ) -> AudioResult<Option<AudioMsgId>> {
        let audio_type = audio.audio_type;
        if audio_type == AudioType::Unknown {
            return Ok(None);
        }

        if position_ms > 0 {
            stream.seek_ms(position_ms)?;
        }

        let mut stopped_prev = None;
        let mut faded_start = false;

        let cur_matches = self
            .current_track(audio_type)
            .is_some_and(|t| t.state.id == audio);

        if !cur_matches {
            if let Some(prev_stopped) = self.faded_stop(audio_type, &mut faded_start) {
                stopped_prev = Some(prev_stopped);
            }
            if audio_type != AudioType::Video {
                self.advance_slot(audio_type, audio);
            }
        }

        let stream_duration_ms = stream.duration_ms();
        let duration_ms = if stream_duration_ms > 0 {
            stream_duration_ms
        } else {
            audio.duration_ms
        };
        let speed = clamp_speed(speed);
        let start_pos_normal = (position_ms.max(0) * i64::from(DEFAULT_FREQUENCY)) / 1000;
        let start_pos_speed = speed_dependent_position(start_pos_normal, speed);
        let total_length_normal = (duration_ms.max(0) * i64::from(DEFAULT_FREQUENCY)) / 1000;

        if let Some(track) = self.current_track_mut(audio_type) {
            track.clear();
            track.state.id = audio;
            track.state.frequency = DEFAULT_FREQUENCY;
            track.state.position = start_pos_normal;
            track.state.length = total_length_normal;
            track.state.received_till = total_length_normal;
            track.speed = speed;
            track.with_speed_position = start_pos_speed;
            track.fade_start_position = start_pos_speed;
            track.last_emitted_position = start_pos_speed;
            track.stretcher = TimeStretcher::new(DEFAULT_FREQUENCY, 2, speed);
            track.stream = Some(stream);
            track.state.state = if start_paused {
                PlaybackState::Paused
            } else if faded_start {
                PlaybackState::Starting
            } else {
                PlaybackState::Playing
            };
        }

        if audio_type == AudioType::Voice && !start_paused {
            self.suppression.suppress_song();
        }
        if audio_type == AudioType::Video {
            self.video_sync = None;
        }
        self.idle_samples = 0;

        Ok(stopped_prev)
    }

    /// Pauses `audio` (with 500 ms fade-out unless `fast == true`).
    ///
    /// Port of `Mixer::pause` (`media_audio.cpp:824-869`).
    pub fn pause(&mut self, audio: AudioMsgId, fast: bool) -> bool {
        let audio_type = audio.audio_type;
        let mut unsuppress = false;
        let changed = if let Some(track) = self.current_track_mut(audio_type) {
            if track.state.id != audio {
                return false;
            }
            match track.state.state {
                PlaybackState::Starting | PlaybackState::Resuming | PlaybackState::Playing => {
                    track.state.state = if fast {
                        PlaybackState::Paused
                    } else {
                        PlaybackState::Pausing
                    };
                    track.reset_fade_start();
                    if audio_type == AudioType::Voice {
                        unsuppress = true;
                    }
                    true
                }
                PlaybackState::Pausing | PlaybackState::Stopping => {
                    track.state.state = if fast {
                        PlaybackState::Paused
                    } else {
                        PlaybackState::Pausing
                    };
                    true
                }
                _ => false,
            }
        } else {
            false
        };
        if unsuppress {
            self.suppression.unsuppress_song();
        }
        changed
    }

    /// Resumes `audio` (with 500 ms fade-in unless `fast == true`).
    ///
    /// Port of `Mixer::resume` (`media_audio.cpp:871-934`).
    pub fn resume(&mut self, audio: AudioMsgId, fast: bool) -> bool {
        let audio_type = audio.audio_type;
        let mut suppress = false;
        let changed = if let Some(track) = self.current_track_mut(audio_type) {
            if track.state.id != audio {
                return false;
            }
            match track.state.state {
                PlaybackState::Pausing | PlaybackState::Paused | PlaybackState::PausedAtEnd => {
                    if track.state.state == PlaybackState::Paused {
                        track.reset_fade_start();
                    }
                    track.state.state = if fast {
                        PlaybackState::Playing
                    } else {
                        PlaybackState::Resuming
                    };
                    if audio_type == AudioType::Voice {
                        suppress = true;
                    }
                    true
                }
                _ => false,
            }
        } else {
            false
        };
        if suppress {
            self.suppression.suppress_song();
        }
        if changed {
            self.idle_samples = 0;
        }
        changed
    }

    /// Stops `audio` with fade-out (`Mixer::stop(const AudioMsgId &)`).
    pub fn stop(&mut self, audio: AudioMsgId) -> bool {
        let audio_type = audio.audio_type;
        let matches = self
            .current_track(audio_type)
            .is_some_and(|t| t.state.id == audio);
        if !matches {
            return false;
        }
        let mut _faded = false;
        self.faded_stop(audio_type, &mut _faded);
        if audio_type == AudioType::Voice {
            self.suppression.unsuppress_song();
        } else if audio_type == AudioType::Video {
            self.video_track.clear();
        }
        true
    }

    /// Immediately stops `audio` with an explicit stopped [`PlaybackState`].
    pub fn stop_with_state(&mut self, audio: AudioMsgId, state: PlaybackState) -> bool {
        if !state.is_stopped() {
            return false;
        }
        let audio_type = audio.audio_type;
        let changed = if let Some(track) = self.current_track_mut(audio_type) {
            if track.state.id != audio || track.state.state.is_stopped() {
                return false;
            }
            track.set_stopped(state);
            true
        } else {
            false
        };
        if changed {
            if audio_type == AudioType::Voice {
                self.suppression.unsuppress_song();
            } else if audio_type == AudioType::Video {
                self.video_track.clear();
            }
        }
        changed
    }

    /// Stops and clears all tracks across every slot (`Mixer::stopAndClear`).
    pub fn stop_and_clear(&mut self) {
        for t in &mut self.voice_tracks {
            t.clear();
        }
        for t in &mut self.song_tracks {
            t.clear();
        }
        self.video_track.clear();
        self.sound_tracks.clear();
        self.suppression.unsuppress_song();
    }

    /// Updates playback speed on the active track matching `audio`.
    ///
    /// Adjusts `with_speed_position` and `fade_start_position` proportionally
    /// matching `Loaders::rebufferOnSpeedChange` (`media_audio_loaders.cpp:490-504`).
    pub fn set_speed(&mut self, audio: AudioMsgId, speed: f64) -> bool {
        let new_speed = clamp_speed(speed);
        let Some(track) = self.current_track_mut(audio.audio_type) else {
            return false;
        };
        if track.state.id != audio || equal_speeds(track.speed, new_speed) {
            return false;
        }
        let ratio = track.speed / new_speed;
        track.with_speed_position = ((track.with_speed_position as f64) * ratio).round() as i64;
        track.fade_start_position = ((track.fade_start_position as f64) * ratio).round() as i64;
        track.last_emitted_position = track.with_speed_position;
        track.speed = new_speed;
        track.stretcher.set_speed(new_speed);
        true
    }

    /// Sets per-track volume multiplier (`Mixer::setVolumeFromExternal`).
    pub fn set_track_volume(&mut self, audio: AudioMsgId, volume: f64) -> bool {
        let Some(track) = self.current_track_mut(audio.audio_type) else {
            return false;
        };
        if track.state.id != audio {
            return false;
        }
        track.volume = volume.clamp(0.0, 1.0);
        true
    }

    /// Seeks the active track matching `audio` to `position_ms`.
    pub fn seek(&mut self, audio: AudioMsgId, position_ms: i64) -> AudioResult<bool> {
        let Some(track) = self.current_track_mut(audio.audio_type) else {
            return Ok(false);
        };
        if track.state.id != audio {
            return Ok(false);
        }
        if let Some(stream) = &mut track.stream {
            stream.seek_ms(position_ms)?;
        }
        if let Some(r) = &mut track.resampler {
            r.reset();
        }
        track.stretcher.reset();
        track.pcm_queue.clear();
        track.eof_reached = false;
        track.stretcher_flushed = false;

        let pos_normal = (position_ms.max(0) * i64::from(DEFAULT_FREQUENCY)) / 1000;
        let pos_speed = speed_dependent_position(pos_normal, track.speed);
        track.state.position = pos_normal;
        track.with_speed_position = pos_speed;
        track.fade_start_position = pos_speed;
        track.last_emitted_position = pos_speed;
        if audio.audio_type == AudioType::Video {
            self.video_sync = None;
        }
        Ok(true)
    }

    /// Renders like [`Self::render_stereo_48k`] and records that the buffer's first sample is
    /// heard at `heard_at`, the time point the video clock follows
    /// ([`Self::external_sync_point`]).
    pub fn render_stereo_48k_at(
        &mut self,
        out_stereo: &mut [f32],
        heard_at: Instant,
    ) -> Vec<MixerEvent> {
        let events = self.render_stereo_48k(out_stereo);
        if let Some((audio, position_ms)) = self.last_video_render.take() {
            self.video_sync = Some(ExternalSyncPoint {
                audio,
                position_ms,
                at: heard_at,
            });
        }
        events
    }

    /// The latest played position of the video sound `audio`
    /// (`Mixer::getExternalSyncTimePoint`); `None` before it played any sample.
    #[must_use]
    pub fn external_sync_point(&self, audio: AudioMsgId) -> Option<ExternalSyncPoint> {
        self.video_sync
            .filter(|p| p.audio == audio && self.video_track.state.id == audio)
    }

    /// Plays a short UI or notification sound (`Media::Audio::Track::playOnce/playInLoop`).
    pub fn play_sound_track(
        &mut self,
        mut track: SoundTrack,
        looping: bool,
        volume_override: Option<f64>,
    ) -> SoundTrackId {
        let id = track.id;
        if looping {
            track.play_in_loop(volume_override, self.notifications_volume_pct);
        } else {
            track.play_once(volume_override, self.notifications_volume_pct);
        }
        self.sound_tracks.push(track);
        self.idle_samples = 0;
        id
    }

    /// Stops and removes a playing UI/notification sound by ID.
    pub fn stop_sound_track(&mut self, id: SoundTrackId) -> bool {
        let before = self.sound_tracks.len();
        self.sound_tracks.retain(|t| t.id != id);
        self.sound_tracks.len() != before
    }

    /// Returns a snapshot of the current [`TrackState`] for `audio_type`.
    #[must_use]
    pub fn current_state(&self, audio_type: AudioType) -> TrackState {
        self.current_track(audio_type)
            .map(|t| t.state.clone())
            .unwrap_or_default()
    }

    /// Returns `true` if any track or UI sound is actively playing or fading.
    #[must_use]
    pub fn has_active_audio(&self) -> bool {
        let active_track =
            |t: &MixerTrack| t.state.state == PlaybackState::Playing || t.state.state.is_fading();
        self.voice_tracks.iter().any(active_track)
            || self.song_tracks.iter().any(active_track)
            || active_track(&self.video_track)
            || self.sound_tracks.iter().any(SoundTrack::is_active)
    }

    /// Returns `true` if the mixer has been silent for at least [`DETACH_DEVICE_TIMEOUT_MS`].
    #[must_use]
    pub fn should_detach_device(&self) -> bool {
        let threshold = (DETACH_DEVICE_TIMEOUT_MS * i64::from(DEFAULT_FREQUENCY)) / 1000;
        !self.has_active_audio() && self.idle_samples >= threshold
    }

    /// Renders `out_stereo.len() / 2` stereo frames at 48 kHz into `out_stereo`,
    /// mixing all active tracks and UI sounds with fades and suppression.
    pub fn render_stereo_48k(&mut self, out_stereo: &mut [f32]) -> Vec<MixerEvent> {
        out_stereo.fill(0.0);
        let frames = out_stereo.len() / 2;
        if frames == 0 {
            return Vec::new();
        }

        let dt_ms = ((frames as f64) * 1000.0) / f64::from(DEFAULT_FREQUENCY);
        self.suppression.advance_ms(dt_ms);

        let voice_base_gain = compute_volume(
            AudioType::Voice,
            self.song_volume,
            self.video_volume,
            self.suppression.mult_all,
            self.suppression.mult_song,
        );
        let song_base_gain = compute_volume(
            AudioType::Song,
            self.song_volume,
            self.video_volume,
            self.suppression.mult_all,
            self.suppression.mult_song,
        );
        let video_base_gain = compute_volume(
            AudioType::Video,
            self.song_volume,
            self.video_volume,
            self.suppression.mult_all,
            self.suppression.mult_song,
        );

        let mut events = Vec::new();
        let mut any_active = false;
        let mut unsuppress_song_after = false;
        let video_start = (
            self.video_track.state.id,
            self.video_track.with_speed_position,
            self.video_track.speed,
        );
        self.last_video_render = None;

        for (idx, track) in self.voice_tracks.iter_mut().enumerate() {
            let is_current = idx == self.voice_current;
            let (active, ended) =
                render_one_track(track, out_stereo, frames, voice_base_gain, &mut events);
            any_active |= active;
            if ended && is_current {
                unsuppress_song_after = true;
            }
        }
        for track in &mut self.song_tracks {
            let (active, _) =
                render_one_track(track, out_stereo, frames, song_base_gain, &mut events);
            any_active |= active;
        }
        {
            let (active, _) = render_one_track(
                &mut self.video_track,
                out_stereo,
                frames,
                video_base_gain,
                &mut events,
            );
            any_active |= active;
        }
        if self.video_track.state.id == video_start.0
            && self.video_track.with_speed_position > video_start.1
        {
            let normal = speed_independent_position(video_start.1, video_start.2);
            self.last_video_render = Some((
                video_start.0,
                (normal * 1000) / i64::from(DEFAULT_FREQUENCY),
            ));
        }

        if unsuppress_song_after {
            self.suppression.unsuppress_song();
        }

        // Mix active UI / notification sound tracks.
        let mut finished_sounds = Vec::new();
        for st in &mut self.sound_tracks {
            if st.is_active() {
                any_active = true;
                if st.mix_into(out_stereo) {
                    finished_sounds.push(st.id);
                    events.push(MixerEvent::SoundTrackFinished(st.id));
                }
            }
        }
        if !finished_sounds.is_empty() {
            self.sound_tracks
                .retain(|st| !finished_sounds.contains(&st.id));
        }

        // Soft-clip mixed output to prevent digital harsh clipping when multiple tracks overlap.
        for s in out_stereo.iter_mut() {
            *s = soft_clip(*s);
        }

        if any_active {
            self.idle_samples = 0;
        } else {
            self.idle_samples = self.idle_samples.saturating_add(frames as i64);
        }

        events
    }

    fn faded_stop(&mut self, audio_type: AudioType, faded_start: &mut bool) -> Option<AudioMsgId> {
        let track = self.current_track_mut(audio_type)?;
        match track.state.state {
            PlaybackState::Starting | PlaybackState::Resuming | PlaybackState::Playing => {
                track.state.state = PlaybackState::Stopping;
                track.reset_fade_start();
                *faded_start = true;
                None
            }
            PlaybackState::Pausing => {
                track.state.state = PlaybackState::Stopping;
                *faded_start = true;
                None
            }
            PlaybackState::Paused | PlaybackState::PausedAtEnd => {
                let id = track.state.id;
                track.set_stopped(PlaybackState::Stopped);
                Some(id)
            }
            _ => None,
        }
    }

    fn advance_slot(&mut self, audio_type: AudioType, audio: AudioMsgId) {
        let (tracks, current) = match audio_type {
            AudioType::Voice => (&self.voice_tracks, &mut self.voice_current),
            AudioType::Song => (&self.song_tracks, &mut self.song_current),
            _ => return,
        };
        if let Some(idx) = tracks.iter().position(|t| t.state.id == audio) {
            *current = idx;
        } else {
            *current = (*current + 1) % TOGETHER_LIMIT;
        }
    }

    fn current_track(&self, audio_type: AudioType) -> Option<&MixerTrack> {
        match audio_type {
            AudioType::Voice => Some(&self.voice_tracks[self.voice_current]),
            AudioType::Song => Some(&self.song_tracks[self.song_current]),
            AudioType::Video => Some(&self.video_track),
            AudioType::Unknown => None,
        }
    }

    fn current_track_mut(&mut self, audio_type: AudioType) -> Option<&mut MixerTrack> {
        match audio_type {
            AudioType::Voice => Some(&mut self.voice_tracks[self.voice_current]),
            AudioType::Song => Some(&mut self.song_tracks[self.song_current]),
            AudioType::Video => Some(&mut self.video_track),
            AudioType::Unknown => None,
        }
    }
}

/// Renders one `MixerTrack` into `out_stereo`, applying sample-accurate 500 ms fades
/// and updating positions/states identically to `Fader::updateOnePlayback`.
///
/// Returns `(was_active, stopped_or_errored)`.
fn render_one_track(
    track: &mut MixerTrack,
    out_stereo: &mut [f32],
    frames: usize,
    base_gain: f64,
    events: &mut Vec<MixerEvent>,
) -> (bool, bool) {
    let st = track.state.state;
    if st.is_stopped() || st.is_paused() {
        return (false, false);
    }

    let needed_samples = frames * 2;
    if let Err(err) = track.ensure_buffered(needed_samples) {
        tracing::error!("track decode error: {err}");
        let id = track.state.id;
        track.set_stopped(PlaybackState::StoppedAtError);
        events.push(MixerEvent::Error(id));
        events.push(MixerEvent::Stopped(id, PlaybackState::StoppedAtError));
        return (false, true);
    }

    let fade_total_samples = (FADE_DURATION_MS * i64::from(DEFAULT_FREQUENCY)) / 1000;
    let total_gain = base_gain * track.volume;
    let mut rendered_frames = 0usize;
    let mut reached_end = false;

    for f in 0..frames {
        let Some(left) = track.pcm_queue.pop_front() else {
            reached_end = true;
            break;
        };
        let right = track.pcm_queue.pop_front().unwrap_or(left);

        let cur_state = track.state.state;
        let fade_gain = if cur_state.is_fading() {
            let elapsed = (track.with_speed_position - track.fade_start_position).max(0);
            if elapsed >= fade_total_samples {
                match cur_state {
                    PlaybackState::Stopping => {
                        let id = track.state.id;
                        track.set_stopped(PlaybackState::Stopped);
                        events.push(MixerEvent::Stopped(id, PlaybackState::Stopped));
                        return (true, true);
                    }
                    PlaybackState::Pausing => {
                        track.state.state = PlaybackState::Paused;
                        track.update_state_position();
                        events.push(MixerEvent::StateChanged(
                            track.state.id,
                            PlaybackState::Paused,
                        ));
                        return (true, false);
                    }
                    PlaybackState::Starting | PlaybackState::Resuming => {
                        track.state.state = PlaybackState::Playing;
                        events.push(MixerEvent::StateChanged(
                            track.state.id,
                            PlaybackState::Playing,
                        ));
                        1.0
                    }
                    _ => 1.0,
                }
            } else {
                let frac = (elapsed as f64) / (fade_total_samples as f64);
                if matches!(cur_state, PlaybackState::Pausing | PlaybackState::Stopping) {
                    (1.0 - frac).clamp(0.0, 1.0)
                } else {
                    frac.clamp(0.0, 1.0)
                }
            }
        } else {
            1.0
        };

        let gain = (fade_gain * total_gain) as f32;
        out_stereo[f * 2] += left * gain;
        out_stereo[f * 2 + 1] += right * gain;

        track.with_speed_position += 1;
        rendered_frames += 1;
    }

    if rendered_frames > 0
        && track.with_speed_position >= track.last_emitted_position + CHECK_PLAYBACK_POSITION_DELTA
    {
        track.last_emitted_position = track.with_speed_position;
        track.update_state_position();
        events.push(MixerEvent::PositionUpdated(track.state.id));
    }

    if reached_end && track.stretcher_flushed {
        let id = track.state.id;
        let final_state = match track.state.state {
            PlaybackState::Pausing => PlaybackState::PausedAtEnd,
            PlaybackState::Stopping => PlaybackState::Stopped,
            _ => PlaybackState::StoppedAtEnd,
        };
        track.set_stopped(final_state);
        events.push(MixerEvent::Stopped(id, final_state));
        return (rendered_frames > 0, true);
    }

    (true, false)
}
