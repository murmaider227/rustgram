//! Call ringtones and call sound effects played through `tg-audio`.
//!
//! Port of the sound triggers in `Telegram/SourceFiles/calls/calls_call.cpp`
//! (`Call::startWaitingTrack`, `Call::getWaitingSoundPeakValue`) and
//! `Telegram/SourceFiles/calls/calls_instance.cpp` (`Delegate::callPlaySound`,
//! `Delegate::groupCallPlaySound`, `Instance::ensureSoundLoaded`, `Instance::playSoundOnce`).
//!
//! The sounds are tdesktop's own files from `Telegram/Resources/sounds` (GPLv3 with the OpenSSL
//! exception, like Rustgram; see `THIRD_PARTY_NOTICES.txt`), embedded the way tdesktop embeds
//! them in `sounds.qrc`. The synthesized tones remain only as the fallback for a file that
//! cannot be decoded.
//!
//! Internal design, where it differs from tdesktop:
//! - Each file is decoded once, on first use, and kept as 48 kHz stereo PCM
//!   (`Instance::ensureSoundLoaded` does the same per key); a play hands a copy to the mixer.
//! - The Answer-button ripple reads the peak envelope by the time since the loop started
//!   instead of asking the audio device for its position (`Track::updateState`), so the value
//!   does not depend on an open output device and no lock is taken per frame.

use std::f32::consts::TAU;
use std::sync::{Arc, Mutex};

use tg_audio::{
    AudioOutputEngine, DEFAULT_FREQUENCY, DeviceManager, MixerCore, SoundTrack, SoundTrackId,
    decode_sound_track,
};
use tg_calls::{CallSound, GroupCallSound};

/// Peak-envelope bucket of the waiting track in ms (`Call::kSoundSampleMs`, `calls_call.h:100`).
pub const SOUND_SAMPLE_MS: i64 = 100;

/// One cycle of the synthesized fallback ringtone in milliseconds.
const FALLBACK_INCOMING_CYCLE_MS: usize = 2_400;
/// One cycle of the synthesized fallback ringback in milliseconds.
const FALLBACK_OUTGOING_CYCLE_MS: usize = 3_000;

/// One of tdesktop's call sound files (`Telegram/Resources/sounds/<key>.mp3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallSoundFile {
    /// `call_incoming`: the ringtone looped while an incoming call rings.
    Incoming,
    /// `call_outgoing`: the ringback looped while an outgoing call waits.
    Outgoing,
    /// `call_connect`.
    Connect,
    /// `call_busy`.
    Busy,
    /// `call_end`.
    End,
    /// `group_call_start`.
    GroupStart,
    /// `group_call_connect`.
    GroupConnect,
    /// `group_call_allowed`.
    GroupAllowed,
    /// `group_call_end`.
    GroupEnd,
    /// `group_call_recording_start`.
    GroupRecordingStart,
}

impl CallSoundFile {
    /// Every call sound, in the order of the player's cache.
    pub const ALL: [Self; 10] = [
        Self::Incoming,
        Self::Outgoing,
        Self::Connect,
        Self::Busy,
        Self::End,
        Self::GroupStart,
        Self::GroupConnect,
        Self::GroupAllowed,
        Self::GroupEnd,
        Self::GroupRecordingStart,
    ];

    /// tdesktop's sound key (`Core::Settings::getSoundPath(key)`), also the file stem.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Incoming => "call_incoming",
            Self::Outgoing => "call_outgoing",
            Self::Connect => "call_connect",
            Self::Busy => "call_busy",
            Self::End => "call_end",
            Self::GroupStart => "group_call_start",
            Self::GroupConnect => "group_call_connect",
            Self::GroupAllowed => "group_call_allowed",
            Self::GroupEnd => "group_call_end",
            Self::GroupRecordingStart => "group_call_recording_start",
        }
    }

    /// The embedded mp3 file, unmodified from tdesktop v7.4.5.
    #[must_use]
    pub const fn mp3(self) -> &'static [u8] {
        match self {
            Self::Incoming => include_bytes!("../../assets/sounds/call_incoming.mp3"),
            Self::Outgoing => include_bytes!("../../assets/sounds/call_outgoing.mp3"),
            Self::Connect => include_bytes!("../../assets/sounds/call_connect.mp3"),
            Self::Busy => include_bytes!("../../assets/sounds/call_busy.mp3"),
            Self::End => include_bytes!("../../assets/sounds/call_end.mp3"),
            Self::GroupStart => include_bytes!("../../assets/sounds/group_call_start.mp3"),
            Self::GroupConnect => include_bytes!("../../assets/sounds/group_call_connect.mp3"),
            Self::GroupAllowed => include_bytes!("../../assets/sounds/group_call_allowed.mp3"),
            Self::GroupEnd => include_bytes!("../../assets/sounds/group_call_end.mp3"),
            Self::GroupRecordingStart => {
                include_bytes!("../../assets/sounds/group_call_recording_start.mp3")
            }
        }
    }

    /// The file a 1:1 [`CallSound`] plays (`Instance::Delegate::callPlaySound`).
    #[must_use]
    pub const fn for_call_sound(sound: CallSound) -> Self {
        match sound {
            CallSound::Connecting => Self::Connect,
            CallSound::Busy => Self::Busy,
            CallSound::Ended => Self::End,
        }
    }

    /// The file a [`GroupCallSound`] plays (`Instance::Delegate::groupCallPlaySound`).
    #[must_use]
    pub const fn for_group_sound(sound: GroupCallSound) -> Self {
        match sound {
            GroupCallSound::Started => Self::GroupStart,
            GroupCallSound::Connecting => Self::GroupConnect,
            GroupCallSound::AllowedToSpeak => Self::GroupAllowed,
            GroupCallSound::Ended => Self::GroupEnd,
            GroupCallSound::RecordingStarted => Self::GroupRecordingStart,
        }
    }

    /// The file looped while a call waits (`Call::startWaitingTrack`).
    #[must_use]
    pub const fn for_waiting(kind: WaitingToneKind) -> Self {
        match kind {
            WaitingToneKind::Incoming => Self::Incoming,
            WaitingToneKind::Outgoing => Self::Outgoing,
        }
    }

    /// Peak-envelope bucket: only the two waiting tracks sample peaks
    /// (`_waitingTrack->samplePeakEach(kSoundSampleMs)`).
    const fn peak_duration_ms(self) -> i64 {
        match self {
            Self::Incoming | Self::Outgoing => SOUND_SAMPLE_MS,
            _ => 0,
        }
    }

    const fn index(self) -> usize {
        self as usize
    }

    fn fallback_track(self) -> SoundTrack {
        match self {
            Self::Incoming => synthesize_incoming_ringtone_track(),
            Self::Outgoing => synthesize_outgoing_ringback_track(),
            Self::Connect | Self::GroupConnect => synthesize_connecting_chime_track(),
            Self::Busy => synthesize_busy_tone_track(),
            Self::End | Self::GroupEnd => synthesize_ended_tone_track(),
            Self::GroupStart | Self::GroupRecordingStart => synthesize_group_started_track(),
            Self::GroupAllowed => synthesize_group_allowed_track(),
        }
    }
}

/// Decodes one embedded call sound, or `None` when the file cannot be decoded.
#[must_use]
pub fn decode_call_sound(file: CallSoundFile) -> Option<SoundTrack> {
    match decode_sound_track(
        file.mp3().to_vec(),
        Some("mp3".to_owned()),
        file.peak_duration_ms(),
    ) {
        Ok(track) => Some(track),
        Err(err) => {
            tracing::warn!(%err, key = file.key(), "call sound failed to decode");
            None
        }
    }
}

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

#[derive(Debug, Clone, Copy)]
struct ActiveLoop {
    kind: WaitingToneKind,
    started_ms: u64,
    track: SoundTrackId,
}

/// Call sound and ringtone player backed by `tg-audio`'s [`MixerCore`], [`SoundTrack`], and
/// [`AudioOutputEngine`].
pub struct CallRingtonePlayer {
    mixer: Arc<Mutex<MixerCore>>,
    output: Option<AudioOutputEngine>,
    attach_hardware: bool,
    tracks: [Option<SoundTrack>; CallSoundFile::ALL.len()],
    active_loop: Option<ActiveLoop>,
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
    /// Creates a new [`CallRingtonePlayer`]. Nothing is decoded until a sound is first needed.
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
            tracks: std::array::from_fn(|_| None),
            active_loop: None,
            history: Vec::new(),
        }
    }

    /// Creates a software-only player suitable for unit tests and headless screenshot renders.
    #[must_use]
    pub fn headless() -> Self {
        Self::new(false)
    }

    /// The decoded track for `file` (`Instance::ensureSoundLoaded`): tdesktop's file, or the
    /// synthesized fallback when it cannot be decoded.
    pub fn track(&mut self, file: CallSoundFile) -> &SoundTrack {
        self.tracks[file.index()]
            .get_or_insert_with(|| decode_call_sound(file).unwrap_or_else(|| file.fallback_track()))
    }

    /// Starts looping either the incoming ringtone (`outgoing == false`) or outgoing ringback
    /// (`Call::startWaitingTrack`: `playInLoop`).
    pub fn start_waiting_loop(&mut self, outgoing: bool, now_ms: u64) {
        let kind = if outgoing {
            WaitingToneKind::Outgoing
        } else {
            WaitingToneKind::Incoming
        };
        if self.active_loop.map(|active| active.kind) == Some(kind) {
            return;
        }
        self.stop_loop_track();
        let track = self.track(CallSoundFile::for_waiting(kind)).clone();
        self.active_loop = Some(ActiveLoop {
            kind,
            started_ms: now_ms,
            track: track.id,
        });
        self.history.push(CallAudioRecord::StartLoop(kind));
        self.play_track(track, true);
    }

    /// Stops any active looping ringtone or ringback tone (`_waitingTrack.reset()`).
    pub fn stop_waiting_loop(&mut self) {
        if self.stop_loop_track() {
            self.history.push(CallAudioRecord::StopLoop);
        }
    }

    fn stop_loop_track(&mut self) -> bool {
        let Some(active) = self.active_loop.take() else {
            return false;
        };
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.stop_sound_track(active.track);
        }
        true
    }

    /// Plays a one-shot 1:1 [`CallSound`].
    pub fn play_call_sound(&mut self, sound: CallSound) {
        self.history.push(CallAudioRecord::OneShot(sound));
        self.play_once(CallSoundFile::for_call_sound(sound));
    }

    /// Plays a one-shot [`GroupCallSound`].
    pub fn play_group_sound(&mut self, sound: GroupCallSound) {
        self.history.push(CallAudioRecord::GroupOneShot(sound));
        self.play_once(CallSoundFile::for_group_sound(sound));
    }

    fn play_once(&mut self, file: CallSoundFile) {
        let track = self.track(file).clone();
        self.play_track(track, false);
    }

    /// Normalized peak (`0.0..=1.0`) of the looping incoming ringtone, for the pulsing ripple on
    /// the Answer button (`Call::getWaitingSoundPeakValue`: the peak a quarter of a bucket
    /// ahead of the playback position).
    #[must_use]
    pub fn waiting_sound_peak_value(&self, now_ms: u64) -> f32 {
        let Some(active) = self.active_loop else {
            return 0.0;
        };
        if active.kind != WaitingToneKind::Incoming {
            return 0.0;
        }
        let Some(track) = &self.tracks[CallSoundFile::Incoming.index()] else {
            return 0.0;
        };
        let elapsed_ms = i64::try_from(now_ms.saturating_sub(active.started_ms)).unwrap_or(0);
        track.peak_value_at_ms(elapsed_ms + SOUND_SAMPLE_MS / 4) as f32
    }

    /// Returns whether a waiting ringtone/ringback is currently active.
    #[must_use]
    pub fn active_loop(&self) -> Option<WaitingToneKind> {
        self.active_loop.map(|active| active.kind)
    }

    /// Returns all sound playback records triggered so far.
    #[must_use]
    pub fn history(&self) -> &[CallAudioRecord] {
        &self.history
    }

    /// The shared software [`MixerCore`] the call sounds play into.
    #[must_use]
    pub fn mixer(&self) -> &Arc<Mutex<MixerCore>> {
        &self.mixer
    }

    /// Detaches the hardware output stream if idle.
    pub fn detach_if_idle(&self) {
        if let Some(ref output) = self.output {
            output.detach_if_idle();
        }
    }

    /// Plays `track` at full gain (tdesktop's call tracks keep `Track::_volume = 1`). A sound
    /// that is still playing restarts instead of overlapping itself, as one `Track` per key does.
    fn play_track(&self, track: SoundTrack, looping: bool) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.stop_sound_track(track.id);
            mixer.play_sound_track(track, looping, Some(1.0));
        }
        if self.attach_hardware
            && let Some(ref output) = self.output
            && let Err(err) = output.ensure_attached()
        {
            tracing::debug!(%err, "Call audio output unavailable; continuing silently");
        }
    }
}

/// Fallback ringtone: a 4-note ascending arpeggio (`F4 -> A4 -> C5 -> E5`) followed by silence,
/// one 2.4 s loop cycle at 48 kHz.
#[must_use]
pub fn synthesize_incoming_ringtone_track() -> SoundTrack {
    let sr = DEFAULT_FREQUENCY;
    let notes = [
        (349.23_f32, 0.00_f32, 0.34_f32),
        (440.00_f32, 0.22_f32, 0.34_f32),
        (523.25_f32, 0.44_f32, 0.36_f32),
        (659.25_f32, 0.66_f32, 0.56_f32),
    ];
    let total_samples = (sr as usize * FALLBACK_INCOMING_CYCLE_MS) / 1000;
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
            pcm[idx] += 0.17 * attack * decay * (fundamental + overtone);
        }
    }
    track_from_pcm(&pcm, sr, SOUND_SAMPLE_MS)
}

/// Fallback ringback: a dual sine (`440 Hz + 480 Hz`, 900 ms) followed by silence, one 3 s loop
/// cycle.
#[must_use]
pub fn synthesize_outgoing_ringback_track() -> SoundTrack {
    let sr = DEFAULT_FREQUENCY;
    let tone = (sr as usize * 900) / 1000;
    let mut pcm = vec![0.0_f32; (sr as usize * FALLBACK_OUTGOING_CYCLE_MS) / 1000];
    for (i, sample) in pcm.iter_mut().take(tone).enumerate() {
        let t = i as f32 / sr as f32;
        let attack = (t / 0.02).min(1.0);
        let release = ((0.90 - t) / 0.04).clamp(0.0, 1.0);
        *sample = 0.10 * attack * release * ((TAU * 440.0 * t).sin() + (TAU * 480.0 * t).sin());
    }
    track_from_pcm(&pcm, sr, SOUND_SAMPLE_MS)
}

/// Fallback connecting shimmer (`E5 -> G5`, 220 ms).
#[must_use]
pub fn synthesize_connecting_chime_track() -> SoundTrack {
    synthesize_two_note_chime(659.25, 783.99, 220, 0.12)
}

/// Fallback busy double-beep (`480 Hz`, 2 pulses over 520 ms).
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
            pcm[idx] = 0.13 * env * (TAU * 480.0 * t).sin();
        }
    }
    track_from_pcm(&pcm, sr, 0)
}

/// Fallback descending call-ended tone (`E5 -> B4`, 240 ms).
#[must_use]
pub fn synthesize_ended_tone_track() -> SoundTrack {
    synthesize_two_note_chime(659.25, 493.88, 240, 0.13)
}

/// Fallback group-call-joined chord (`C5 -> G5`, 260 ms).
#[must_use]
pub fn synthesize_group_started_track() -> SoundTrack {
    synthesize_two_note_chime(523.25, 783.99, 260, 0.12)
}

/// Fallback allowed-to-speak chime (`G5 -> C6`, 240 ms).
#[must_use]
pub fn synthesize_group_allowed_track() -> SoundTrack {
    synthesize_two_note_chime(783.99, 1046.50, 240, 0.12)
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
    track_from_pcm(&pcm, sr, 0)
}

fn track_from_pcm(pcm: &[f32], sr: u32, peak_duration_ms: i64) -> SoundTrack {
    SoundTrack::from_pcm_f32(pcm, sr, 1, peak_duration_ms).unwrap_or_else(|_| {
        SoundTrack::from_pcm_f32(&[0.0, 0.0], sr, 1, 0).expect("valid 2-sample silence")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TGB-2979: every call sound is tdesktop's file and decodes; none falls back to a tone.
    #[test]
    fn every_embedded_call_sound_decodes_to_tdesktops_length() {
        // Durations of tdesktop v7.4.5's files (ffprobe), in ms.
        let expected_ms: [(CallSoundFile, i64); 10] = [
            (CallSoundFile::Incoming, 2_000),
            (CallSoundFile::Outgoing, 2_685),
            (CallSoundFile::Connect, 1_074),
            (CallSoundFile::Busy, 3_500),
            (CallSoundFile::End, 990),
            (CallSoundFile::GroupStart, 407),
            (CallSoundFile::GroupConnect, 1_018),
            (CallSoundFile::GroupAllowed, 1_000),
            (CallSoundFile::GroupEnd, 176),
            (CallSoundFile::GroupRecordingStart, 1_000),
        ];
        let mut total_bytes = 0;
        for (file, expected) in expected_ms {
            let track =
                decode_call_sound(file).unwrap_or_else(|| panic!("{} must decode", file.key()));
            assert!(
                (track.length_ms() - expected).abs() <= 60,
                "{}: {} ms, tdesktop's file lasts {expected} ms",
                file.key(),
                track.length_ms()
            );
            total_bytes += file.mp3().len();
        }
        assert_eq!(total_bytes, 360_184, "the ten files of tdesktop v7.4.5");
    }

    /// TGB-2979: the player plays the files, not the synthesized tones, and maps each
    /// `CallSound` / `GroupCallSound` to tdesktop's key.
    #[test]
    fn player_uses_tdesktop_files_for_every_cue() {
        let mut player = CallRingtonePlayer::headless();
        for file in CallSoundFile::ALL {
            let decoded = decode_call_sound(file).expect("decodes").length_ms();
            assert_eq!(player.track(file).length_ms(), decoded, "{}", file.key());
            assert_ne!(
                decoded,
                file.fallback_track().length_ms(),
                "{} must differ from its synthesized fallback",
                file.key()
            );
        }
        let keys = |sounds: &[GroupCallSound]| -> Vec<&'static str> {
            sounds
                .iter()
                .map(|sound| CallSoundFile::for_group_sound(*sound).key())
                .collect()
        };
        assert_eq!(
            keys(&[
                GroupCallSound::Started,
                GroupCallSound::Ended,
                GroupCallSound::AllowedToSpeak,
                GroupCallSound::Connecting,
                GroupCallSound::RecordingStarted,
            ]),
            [
                "group_call_start",
                "group_call_end",
                "group_call_allowed",
                "group_call_connect",
                "group_call_recording_start",
            ]
        );
        assert_eq!(
            [CallSound::Busy, CallSound::Ended, CallSound::Connecting]
                .map(|sound| CallSoundFile::for_call_sound(sound).key()),
            ["call_busy", "call_end", "call_connect"]
        );
    }

    /// The waiting track loops in the mixer until stopped, and the ripple follows its peaks.
    #[test]
    fn waiting_loop_plays_in_the_mixer_until_stopped_and_reports_peaks() {
        let mut player = CallRingtonePlayer::headless();
        player.start_waiting_loop(false, 1_000);
        assert_eq!(player.active_loop(), Some(WaitingToneKind::Incoming));
        assert!(player.mixer().lock().unwrap().has_active_audio());

        let length_ms = player.track(CallSoundFile::Incoming).length_ms() as u64;
        let peaks: Vec<f32> = (0..length_ms)
            .step_by(SOUND_SAMPLE_MS as usize)
            .map(|at| player.waiting_sound_peak_value(1_000 + at))
            .collect();
        assert!(peaks.iter().all(|peak| (0.0..=1.0).contains(peak)));
        assert!(peaks.iter().any(|peak| *peak > 0.9), "{peaks:?}");
        assert!(peaks.iter().any(|peak| *peak < 0.1), "{peaks:?}");
        // The loop repeats with the file's own length.
        assert_eq!(
            player.waiting_sound_peak_value(1_000 + 300),
            player.waiting_sound_peak_value(1_000 + 300 + length_ms)
        );

        player.stop_waiting_loop();
        assert!(player.active_loop().is_none());
        assert!(!player.mixer().lock().unwrap().has_active_audio());
        assert_eq!(player.waiting_sound_peak_value(1_500), 0.0);
        assert_eq!(
            player.history(),
            [
                CallAudioRecord::StartLoop(WaitingToneKind::Incoming),
                CallAudioRecord::StopLoop
            ]
        );
    }

    /// Switching from ringback to ringtone replaces the looping track; a repeated one-shot
    /// restarts instead of stacking.
    #[test]
    fn loops_and_one_shots_do_not_stack_in_the_mixer() {
        let mut player = CallRingtonePlayer::headless();
        player.start_waiting_loop(true, 0);
        player.start_waiting_loop(false, 10);
        player.stop_waiting_loop();
        assert!(!player.mixer().lock().unwrap().has_active_audio());

        player.play_call_sound(CallSound::Ended);
        player.play_call_sound(CallSound::Ended);
        let end_id = player.track(CallSoundFile::End).id;
        let mut mixer = player.mixer().lock().unwrap();
        assert!(mixer.stop_sound_track(end_id));
        assert!(!mixer.has_active_audio());
    }
}
