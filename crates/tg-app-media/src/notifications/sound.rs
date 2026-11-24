//! Notification chime decoding, custom ringtones, and playback wired to `tg-audio`.
//!
//! Port of `Telegram/SourceFiles/window/notifications_manager.cpp`
//! (`System::lookupSoundBytes`, `System::lookupSound`, `System::ensureSoundCreated`,
//! `System::playSound`).
//!
//! The default chime is an original synthesized two-tone sound: Telegram's own notification
//! sound is part of its brand and is not shipped with Rustgram.

use std::collections::BTreeMap;
use std::f32::consts::TAU;
use std::sync::{Arc, Mutex};

use tg_audio::{
    AudioOutputEngine, DEFAULT_FREQUENCY, DeviceManager, DocumentId, LocalCache, LocalSound,
    MixerCore, SoundTrack, decode_sound_track,
};

/// Record of a sound playback request (useful for diagnostics and unit tests).
#[derive(Debug, Clone, PartialEq)]
pub struct SoundPlayRecord {
    /// Custom ringtone document ID (`0` = default chime).
    pub sound_id: u64,
    /// Effective volume percentage (`0..=100`).
    pub volume_pct: u8,
    /// Track duration in milliseconds.
    pub duration_ms: i64,
}

/// Notification sound player backed by `tg-audio`'s [`MixerCore`], [`SoundTrack`], and
/// [`AudioOutputEngine`].
pub struct NotificationSoundPlayer {
    mixer: Arc<Mutex<MixerCore>>,
    output: Option<AudioOutputEngine>,
    default_track: Option<SoundTrack>,
    custom_tracks: BTreeMap<u64, SoundTrack>,
    custom_bytes: BTreeMap<u64, Vec<u8>>,
    local_cache: LocalCache,
    attach_hardware: bool,
    history: Vec<SoundPlayRecord>,
}

impl std::fmt::Debug for NotificationSoundPlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotificationSoundPlayer")
            .field("attach_hardware", &self.attach_hardware)
            .field("default_loaded", &self.default_track.is_some())
            .field("custom_count", &self.custom_tracks.len())
            .field("played_count", &self.history.len())
            .finish()
    }
}

impl Default for NotificationSoundPlayer {
    fn default() -> Self {
        Self::new(true)
    }
}

impl NotificationSoundPlayer {
    /// Creates a new [`NotificationSoundPlayer`].
    ///
    /// Pass `attach_hardware = false` in unit tests so sound mixing runs purely in software
    /// without opening an OS audio device.
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
            default_track: None,
            custom_tracks: BTreeMap::new(),
            custom_bytes: BTreeMap::new(),
            local_cache: LocalCache::new(),
            attach_hardware,
            history: Vec::new(),
        }
    }

    /// Creates a headless software-only sound player suitable for unit tests.
    #[must_use]
    pub fn headless() -> Self {
        Self::new(false)
    }

    /// Registers raw encoded bytes for a custom cloud ringtone (`DocumentId`).
    pub fn register_custom_ringtone(&mut self, document_id: u64, bytes: Vec<u8>) {
        if document_id == 0 || bytes.is_empty() {
            return;
        }
        self.custom_bytes.insert(document_id, bytes);
        self.custom_tracks.remove(&document_id);
    }

    /// Returns raw encoded bytes for `sound_id`, falling back to [`default_chime_wav`].
    #[must_use]
    pub fn lookup_sound_bytes(&self, sound_id: u64) -> Vec<u8> {
        if sound_id != 0
            && let Some(bytes) = self.custom_bytes.get(&sound_id)
        {
            return bytes.clone();
        }
        default_chime_wav()
    }

    /// Prepares a cached local WAV [`LocalSound`] via `tg-audio::LocalCache` for OS notification APIs.
    pub fn resolve_local_sound(&mut self, sound_id: u64) -> Option<LocalSound> {
        let custom = if sound_id != 0 {
            self.custom_bytes.get(&sound_id).cloned()
        } else {
            None
        };
        let resolved = self.local_cache.sound(
            DocumentId(sound_id),
            || custom.unwrap_or_default(),
            Some(default_chime_wav),
        );
        if resolved.is_valid() {
            Some(resolved)
        } else {
            None
        }
    }

    /// Plays the notification chime for `sound_id` (`0` = default) at `volume_pct` (`0..=100`).
    ///
    /// Mirrors `System::showNext` sound playback in `notifications_manager.cpp:879-884`:
    /// mixes the [`SoundTrack`] into [`MixerCore`], applies `suppress_all(track.length_ms())`
    /// ducking for ongoing music/voice playback, and lazily opens the hardware audio stream.
    pub fn play(&mut self, sound_id: u64, volume_pct: u8) -> Option<SoundPlayRecord> {
        let clamped_vol = volume_pct.min(100);
        if clamped_vol == 0 {
            return None;
        }
        let track = self.lookup_track(sound_id).clone();
        let duration_ms = track.length_ms();
        let volume_override = Some(f64::from(clamped_vol) / 100.0);

        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.set_notifications_volume_pct(clamped_vol);
            mixer.play_sound_track(track, false, volume_override);
            if duration_ms > 0 {
                mixer.suppress_all(duration_ms);
            }
        }

        if self.attach_hardware
            && let Some(ref output) = self.output
            && let Err(err) = output.ensure_attached()
        {
            tracing::debug!(%err, "Notification audio output unavailable; continuing silently");
        }

        let rec = SoundPlayRecord {
            sound_id,
            volume_pct: clamped_vol,
            duration_ms,
        };
        self.history.push(rec.clone());
        Some(rec)
    }

    /// Detaches the hardware output stream if idle past `DETACH_DEVICE_TIMEOUT_MS`.
    pub fn detach_if_idle(&self) {
        if let Some(ref output) = self.output {
            output.detach_if_idle();
        }
    }

    /// Returns a reference to the shared software [`MixerCore`].
    #[must_use]
    pub fn mixer(&self) -> &Arc<Mutex<MixerCore>> {
        &self.mixer
    }

    /// Returns all sound playback records triggered so far.
    #[must_use]
    pub fn play_history(&self) -> &[SoundPlayRecord] {
        &self.history
    }

    /// Clears recorded playback history.
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Returns the decoded default chime [`SoundTrack`], initializing it lazily.
    pub fn default_track(&mut self) -> &SoundTrack {
        self.ensure_default_track();
        self.default_track
            .as_ref()
            .unwrap_or_else(|| unreachable!("ensure_default_track initializes default_track"))
    }

    fn lookup_track(&mut self, sound_id: u64) -> &SoundTrack {
        if sound_id != 0 {
            if !self.custom_tracks.contains_key(&sound_id)
                && let Some(bytes) = self.custom_bytes.get(&sound_id)
                && let Ok(track) = decode_sound_track(bytes.clone(), None, 0)
            {
                self.custom_tracks.insert(sound_id, track);
            }
            if self.custom_tracks.contains_key(&sound_id) {
                return &self.custom_tracks[&sound_id];
            }
        }
        self.ensure_default_track();
        self.default_track
            .as_ref()
            .unwrap_or_else(|| unreachable!("ensure_default_track initializes default_track"))
    }

    fn ensure_default_track(&mut self) {
        if self.default_track.is_some() {
            return;
        }
        self.default_track = Some(generate_fallback_chime_track());
    }
}

/// Synthesizes a clean, short 180 ms original two-tone notification chime at 48 kHz
/// (`G5 -> C6` gentle sine bell with exponential decay).
#[must_use]
pub fn generate_fallback_chime_track() -> SoundTrack {
    let sample_rate = DEFAULT_FREQUENCY;
    let pcm = chime_pcm(sample_rate);
    SoundTrack::from_pcm_f32(&pcm, sample_rate, 1, 0).unwrap_or_else(|_| {
        SoundTrack::from_pcm_f32(&[0.0, 0.0], sample_rate, 1, 0).expect("valid 2-sample silence")
    })
}

/// The default chime as a 16-bit mono WAV file, for OS notification APIs that need a file.
#[must_use]
pub fn default_chime_wav() -> Vec<u8> {
    let sample_rate = DEFAULT_FREQUENCY;
    let pcm = chime_pcm(sample_rate);
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + pcm.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in pcm {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

fn chime_pcm(sample_rate: u32) -> Vec<f32> {
    let total_frames = ((sample_rate as usize) * 180) / 1000;
    let split_frame = total_frames * 42 / 100;
    let mut pcm = Vec::with_capacity(total_frames);

    for i in 0..total_frames {
        let t = i as f32 / sample_rate as f32;
        let (freq, local_t) = if i < split_frame {
            (783.99_f32, t)
        } else {
            (1046.50_f32, (i - split_frame) as f32 / sample_rate as f32)
        };
        let attack = (local_t / 0.005).min(1.0);
        let decay = (-local_t * 22.0).exp();
        let sample = 0.25 * attack * decay * (TAU * freq * local_t).sin();
        pcm.push(sample);
    }
    pcm
}
