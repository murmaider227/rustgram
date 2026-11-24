//! Original synthesized call ringtones and call sound effects played through `tg-audio`.
//!
//! Port of sound triggers in `Telegram/SourceFiles/calls/calls_call.cpp`
//! (`CallSound`, waiting/ringback loop, `getWaitingSoundPeakValue`) and
//! `Telegram/SourceFiles/calls/group/calls_group_call.cpp` (`GroupCallSound`).
//!
//! All waveforms are freshly synthesized musical tones — no Telegram trademark sounds are used.

use std::f32::consts::TAU;
use std::sync::{Arc, Mutex};

use tg_audio::{AudioOutputEngine, DEFAULT_FREQUENCY, DeviceManager, MixerCore, SoundTrack};
use tg_calls::{CallSound, GroupCallSound};

/// Duration of one synthesized incoming ringtone cycle in milliseconds.
pub const INCOMING_RING_CYCLE_MS: u64 = 2_400;
/// Duration of one synthesized outgoing ringback cycle in milliseconds.
pub const OUTGOING_RINGBACK_CYCLE_MS: u64 = 3_000;

/// Active looping tone kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitingToneKind {
    /// Incoming call ringtone.
    Incoming,
    /// Outgoing call ringback tone.
    Outgoing,
}

/// Diagnostic record of a call audio cue triggered by the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallAudioRecord {
    /// Started looping incoming/outgoing tone.
    StartLoop(WaitingToneKind),
    /// Stopped looping tone.
    StopLoop,
    /// Played a 1:1 call one-shot sound.
    OneShot(CallSound),
    /// Played a group call one-shot sound.
    GroupOneShot(GroupCallSound),
}

/// Call sound and ringtone player backed by `tg-audio`'s [`MixerCore`], [`SoundTrack`], and
/// [`AudioOutputEngine`].
pub struct CallRingtonePlayer {
    mixer: Arc<Mutex<MixerCore>>,
    output: Option<AudioOutputEngine>,
    attach_hardware: bool,
    incoming_track: SoundTrack,
    outgoing_track: SoundTrack,
    connecting_track: SoundTrack,
    busy_track: SoundTrack,
    ended_track: SoundTrack,
    group_started_track: SoundTrack,
    group_allowed_track: SoundTrack,
    active_loop: Option<(WaitingToneKind, u64)>,
    last_loop_trigger_ms: u64,
    history: Vec<CallAudioRecord>,
}

impl std::fmt::Debug for CallRingtonePlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallRingtonePlayer")
            .field("attach_hardware", &self.attach_hardware)
            .field("active_loop", &self.active_loop)
            .field("history_len", &self.history.len())
            .finish()
    }
}

impl Default for CallRingtonePlayer {
    fn default() -> Self {
        Self::new(true)
    }
}

impl CallRingtonePlayer {
    /// Creates a new [`CallRingtonePlayer`].
    ///
    /// Pass `attach_hardware = false` in tests to keep mixing purely in software.
    #[must_use]
    pub fn new(attach_hardware: bool) -> Self {
        let mixer = Arc::new(Mutex::new(MixerCore::new()));
        let output = if attach_hardware {
            let devices = Arc::new(DeviceManager::new());
            Some(AudioOutputEngine::new(devices, Arc::clone(&mixer)))
        } else {
            None
        };
        Self {
            mixer,
            output,
            attach_hardware,
            incoming_track: synthesize_incoming_ringtone_track(),
            outgoing_track: synthesize_outgoing_ringback_track(),
            connecting_track: synthesize_connecting_chime_track(),
            busy_track: synthesize_busy_tone_track(),
            ended_track: synthesize_ended_tone_track(),
            group_started_track: synthesize_group_started_track(),
            group_allowed_track: synthesize_group_allowed_track(),
            active_loop: None,
            last_loop_trigger_ms: 0,
            history: Vec::new(),
        }
    }

    /// Creates a software-only player suitable for unit tests and headless screenshot renders.
    #[must_use]
    pub fn headless() -> Self {
        Self::new(false)
    }

    /// Starts looping either the incoming ringtone (`outgoing == false`) or outgoing ringback.
    pub fn start_waiting_loop(&mut self, outgoing: bool, now_ms: u64) {
        let kind = if outgoing {
            WaitingToneKind::Outgoing
        } else {
            WaitingToneKind::Incoming
        };
        if self.active_loop.map(|(k, _)| k) == Some(kind) {
            return;
        }
        self.active_loop = Some((kind, now_ms));
        self.last_loop_trigger_ms = now_ms;
        self.history.push(CallAudioRecord::StartLoop(kind));
        let track = match kind {
            WaitingToneKind::Incoming => self.incoming_track.clone(),
            WaitingToneKind::Outgoing => self.outgoing_track.clone(),
        };
        self.play_track(track, 0.72);
    }

    /// Stops any active looping ringtone or ringback tone.
    pub fn stop_waiting_loop(&mut self) {
        if self.active_loop.take().is_some() {
            self.history.push(CallAudioRecord::StopLoop);
        }
    }

    /// Advances periodic ringtone re-triggering while a waiting loop is active.
    pub fn tick(&mut self, now_ms: u64) {
        let Some((kind, _)) = self.active_loop else {
            return;
        };
        let period = match kind {
            WaitingToneKind::Incoming => INCOMING_RING_CYCLE_MS,
            WaitingToneKind::Outgoing => OUTGOING_RINGBACK_CYCLE_MS,
        };
        if now_ms.saturating_sub(self.last_loop_trigger_ms) >= period {
            self.last_loop_trigger_ms = now_ms;
            let track = match kind {
                WaitingToneKind::Incoming => self.incoming_track.clone(),
                WaitingToneKind::Outgoing => self.outgoing_track.clone(),
            };
            self.play_track(track, 0.72);
        }
    }

    /// Plays a one-shot 1:1 [`CallSound`].
    pub fn play_call_sound(&mut self, sound: CallSound) {
        self.history.push(CallAudioRecord::OneShot(sound));
        let track = match sound {
            CallSound::Connecting => self.connecting_track.clone(),
            CallSound::Busy => self.busy_track.clone(),
            CallSound::Ended => self.ended_track.clone(),
        };
        self.play_track(track, 0.65);
    }

    /// Plays a one-shot [`GroupCallSound`].
    pub fn play_group_sound(&mut self, sound: GroupCallSound) {
        self.history.push(CallAudioRecord::GroupOneShot(sound));
        let track = match sound {
            GroupCallSound::Started | GroupCallSound::RecordingStarted => {
                self.group_started_track.clone()
            }
            GroupCallSound::Connecting => self.connecting_track.clone(),
            GroupCallSound::AllowedToSpeak => self.group_allowed_track.clone(),
            GroupCallSound::Ended => self.ended_track.clone(),
        };
        self.play_track(track, 0.62);
    }

    /// Computes normalized envelope peak amplitude (`0.0..=1.0`) of the active incoming ringtone
    /// for the pulsing outer ripple on the Answer button (`Call::getWaitingSoundPeakValue`).
    #[must_use]
    pub fn waiting_sound_peak_value(&self, now_ms: u64) -> f32 {
        let Some((WaitingToneKind::Incoming, started_ms)) = self.active_loop else {
            return 0.0;
        };
        let cycle_ms = now_ms.saturating_sub(started_ms) % INCOMING_RING_CYCLE_MS;
        // Our incoming ringtone plays 4 warm bell notes across 0..1150 ms, then rests.
        if cycle_ms >= 1_250 {
            return 0.0;
        }
        let phase = (cycle_ms as f32) / 1_250.0;
        let pulse = (phase * 4.0 * std::f32::consts::PI).sin().abs();
        (0.28 + 0.72 * pulse) * (1.0 - phase * 0.25)
    }

    /// Returns whether a waiting ringtone/ringback is currently active.
    #[must_use]
    pub const fn active_loop(&self) -> Option<WaitingToneKind> {
        match self.active_loop {
            Some((kind, _)) => Some(kind),
            None => None,
        }
    }

    /// Returns all sound playback records triggered so far.
    #[must_use]
    pub fn history(&self) -> &[CallAudioRecord] {
        &self.history
    }

    /// Detaches the hardware output stream if idle.
    pub fn detach_if_idle(&self) {
        if let Some(ref output) = self.output {
            output.detach_if_idle();
        }
    }

    fn play_track(&self, track: SoundTrack, volume: f64) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.play_sound_track(track, false, Some(volume));
        }
        if self.attach_hardware
            && let Some(ref output) = self.output
            && let Err(err) = output.ensure_attached()
        {
            tracing::debug!(%err, "Call audio output unavailable; continuing silently");
        }
    }
}

/// Synthesizes an original warm 4-note ascending arpeggio incoming ringtone (`F4 -> A4 -> C5 -> E5`)
/// with soft harmonic overtone and clean fade envelopes (1.25 s active motif at 48 kHz).
#[must_use]
pub fn synthesize_incoming_ringtone_track() -> SoundTrack {
    let sr = DEFAULT_FREQUENCY;
    let notes = [
        (349.23_f32, 0.00_f32, 0.34_f32),
        (440.00_f32, 0.22_f32, 0.34_f32),
        (523.25_f32, 0.44_f32, 0.36_f32),
        (659.25_f32, 0.66_f32, 0.56_f32),
    ];
    let total_samples = (sr as usize * 1_250) / 1000;
    let mut pcm = vec![0.0_f32; total_samples];
    for (freq, start_s, dur_s) in notes {
        let start_idx = (start_s * sr as f32) as usize;
        let len = (dur_s * sr as f32) as usize;
        for i in 0..len {
            let idx = start_idx + i;
            if idx >= total_samples {
                break;
            }
            let t = i as f32 / sr as f32;
            let attack = (t / 0.012).min(1.0);
            let decay = (-t * 4.8).exp();
            let fundamental = (TAU * freq * t).sin();
            let overtone = 0.22 * (TAU * freq * 2.0 * t).sin();
            pcm[idx] += 0.24 * attack * decay * (fundamental + overtone);
        }
    }
    track_from_pcm(&pcm, sr)
}

/// Synthesizes a soft original outgoing ringback tone (dual sine `440 Hz + 480 Hz`, 900 ms).
#[must_use]
pub fn synthesize_outgoing_ringback_track() -> SoundTrack {
    let sr = DEFAULT_FREQUENCY;
    let total = (sr as usize * 900) / 1000;
    let mut pcm = Vec::with_capacity(total);
    for i in 0..total {
        let t = i as f32 / sr as f32;
        let attack = (t / 0.02).min(1.0);
        let release = ((0.90 - t) / 0.04).clamp(0.0, 1.0);
        let s = 0.14 * attack * release * ((TAU * 440.0 * t).sin() + (TAU * 480.0 * t).sin());
        pcm.push(s);
    }
    track_from_pcm(&pcm, sr)
}

/// Synthesizes a brief original connecting shimmer (`E5 -> G5`, 220 ms).
#[must_use]
pub fn synthesize_connecting_chime_track() -> SoundTrack {
    synthesize_two_note_chime(659.25, 783.99, 220, 0.18)
}

/// Synthesizes an original busy double-beep (`480 Hz`, 2 pulses over 520 ms).
#[must_use]
pub fn synthesize_busy_tone_track() -> SoundTrack {
    let sr = DEFAULT_FREQUENCY;
    let total = (sr as usize * 520) / 1000;
    let mut pcm = vec![0.0_f32; total];
    for pulse in 0..2 {
        let start = (pulse * 260 * sr as usize) / 1000;
        let len = (170 * sr as usize) / 1000;
        for i in 0..len {
            let idx = start + i;
            if idx >= total {
                break;
            }
            let t = i as f32 / sr as f32;
            let env = (t / 0.01).min(1.0) * ((0.17 - t) / 0.015).clamp(0.0, 1.0);
            pcm[idx] = 0.20 * env * (TAU * 480.0 * t).sin();
        }
    }
    track_from_pcm(&pcm, sr)
}

/// Synthesizes a soft descending call-ended tone (`E5 -> B4`, 240 ms).
#[must_use]
pub fn synthesize_ended_tone_track() -> SoundTrack {
    synthesize_two_note_chime(659.25, 493.88, 240, 0.20)
}

/// Synthesizes a soft group-call-joined chord (`C5 -> G5`, 260 ms).
#[must_use]
pub fn synthesize_group_started_track() -> SoundTrack {
    synthesize_two_note_chime(523.25, 783.99, 260, 0.20)
}

/// Synthesizes an upbeat allowed-to-speak chime (`G5 -> C6`, 240 ms).
#[must_use]
pub fn synthesize_group_allowed_track() -> SoundTrack {
    synthesize_two_note_chime(783.99, 1046.50, 240, 0.20)
}

fn synthesize_two_note_chime(
    freq_a: f32,
    freq_b: f32,
    duration_ms: usize,
    gain: f32,
) -> SoundTrack {
    let sr = DEFAULT_FREQUENCY;
    let total = (sr as usize * duration_ms) / 1000;
    let split = total / 2;
    let mut pcm = Vec::with_capacity(total);
    for i in 0..total {
        let (freq, local_t) = if i < split {
            (freq_a, i as f32 / sr as f32)
        } else {
            (freq_b, (i - split) as f32 / sr as f32)
        };
        let attack = (local_t / 0.008).min(1.0);
        let decay = (-local_t * 14.0).exp();
        pcm.push(gain * attack * decay * (TAU * freq * local_t).sin());
    }
    track_from_pcm(&pcm, sr)
}

fn track_from_pcm(pcm: &[f32], sr: u32) -> SoundTrack {
    SoundTrack::from_pcm_f32(pcm, sr, 1, 0).unwrap_or_else(|_| {
        SoundTrack::from_pcm_f32(&[0.0, 0.0], sr, 1, 0).expect("valid 2-sample silence")
    })
}
