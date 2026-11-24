//! Multi-SSRC group call audio receive, mixing, per-SSRC volume, and speaking levels.
//!
//! Reference:
//! - `TelegramMessenger/tgcalls` (`GroupInstanceCustomImpl.cpp`, audio mixer & `GroupLevelsUpdate`)
//! - `crates/tg-calls/src/group/state.rs` (`AudioLevelSample`, `GroupCallAction::SetInstanceVolume`,
//!   `GroupCallAction::RemoveInstanceSsrcs`)
//! - `crates/tg-calls/src/group/mod.rs` (`SPEAK_LEVEL_THRESHOLD = 0.2`)

use std::collections::HashMap;

use tg_audio::dsp::soft_clip;
use tg_calls::{AudioLevelSample, group::SPEAK_LEVEL_THRESHOLD};

use crate::dsp::compute_rms_level;
use crate::error::VoipResult;
use crate::jitter::{AdaptiveJitterBuffer, JitterBufferConfig, PullOutcome, VOIP_FRAME_SAMPLES};

/// Maximum idle time (ms) before an inactive remote SSRC decoder stream is pruned.
const IDLE_STREAM_PRUNE_MS: u64 = 15_000;

/// Per-participant audio receive stream state inside [`GroupAudioMixer`].
struct ParticipantStream {
    jitter: AdaptiveJitterBuffer,
    volume: f64,
    last_packet_ms: u64,
    smoothed_level: f32,
    last_ext_level: Option<(bool, f32)>,
}

/// Multi-SSRC Opus audio receiver, soft-clipped PCM mixer, and speaking-level meter.
///
/// Decodes each remote participant's SSRC independently through its own [`AdaptiveJitterBuffer`]
/// (48 kHz mono `opus-pure` decoder with reordering, in-band FEC, and PLC), applies per-SSRC
/// linear volume scaling (`0.0..=2.0`), sums active streams into a single 20 ms playout frame,
/// and reports [`AudioLevelSample`]s directly compatible with `GroupCallSession::audio_levels_updated`.
pub struct GroupAudioMixer {
    jitter_config: JitterBufferConfig,
    streams: HashMap<u32, ParticipantStream>,
    /// Pre-configured volumes for SSRCs before their first audio packet arrives.
    pending_volumes: HashMap<u32, f64>,
    scratch: [f32; VOIP_FRAME_SAMPLES],
}

impl GroupAudioMixer {
    /// Create a new multi-SSRC group audio mixer.
    #[must_use]
    pub fn new(jitter_config: JitterBufferConfig) -> Self {
        Self {
            jitter_config,
            streams: HashMap::new(),
            pending_volumes: HashMap::new(),
            scratch: [0.0; VOIP_FRAME_SAMPLES],
        }
    }

    /// Number of currently active decoded SSRC streams.
    #[must_use]
    pub fn active_stream_count(&self) -> usize {
        self.streams.len()
    }

    /// Set playback linear volume multiplier (`0.0..=2.0`) for `ssrc`
    /// (driven by `GroupCallAction::SetInstanceVolume`).
    pub fn set_volume(&mut self, ssrc: u32, volume: f64) {
        let clamped = volume.clamp(0.0, 2.0);
        self.pending_volumes.insert(ssrc, clamped);
        if let Some(stream) = self.streams.get_mut(&ssrc) {
            stream.volume = clamped;
        }
    }

    /// Current volume multiplier for `ssrc` (defaults to `1.0`).
    #[must_use]
    pub fn volume(&self, ssrc: u32) -> f64 {
        self.streams
            .get(&ssrc)
            .map(|s| s.volume)
            .or_else(|| self.pending_volumes.get(&ssrc).copied())
            .unwrap_or(1.0)
    }

    /// Remove departed participants' SSRCs immediately
    /// (driven by `GroupCallAction::RemoveInstanceSsrcs`).
    pub fn remove_ssrcs(&mut self, ssrcs: &[u32]) {
        for ssrc in ssrcs {
            self.streams.remove(ssrc);
            self.pending_volumes.remove(ssrc);
        }
    }

    /// Push one decrypted RTP Opus packet for `ssrc` into its per-SSRC jitter buffer.
    ///
    /// Optionally accepts an RFC 6464 `ssrc-audio-level` header extension `(voice, level)`.
    pub fn push_opus_packet(
        &mut self,
        ssrc: u32,
        seq: u16,
        rtp_timestamp: u32,
        arrival_ms: u64,
        opus_payload: Vec<u8>,
        ext_audio_level: Option<(bool, f32)>,
    ) -> VoipResult<()> {
        if ssrc == 0 {
            return Ok(());
        }
        if !self.streams.contains_key(&ssrc) {
            let jitter = AdaptiveJitterBuffer::new(self.jitter_config)?;
            let volume = self.pending_volumes.get(&ssrc).copied().unwrap_or(1.0);
            self.streams.insert(
                ssrc,
                ParticipantStream {
                    jitter,
                    volume,
                    last_packet_ms: arrival_ms,
                    smoothed_level: 0.0,
                    last_ext_level: ext_audio_level,
                },
            );
        }
        if let Some(stream) = self.streams.get_mut(&ssrc) {
            stream.last_packet_ms = arrival_ms;
            if ext_audio_level.is_some() {
                stream.last_ext_level = ext_audio_level;
            }
            stream
                .jitter
                .push(seq, rtp_timestamp, arrival_ms, opus_payload);
        }
        Ok(())
    }

    /// Pull one 20 ms (`960`-sample) mixed 48 kHz mono frame into `out_frame`, returning per-SSRC
    /// [`AudioLevelSample`] speaking levels for `GroupCallSession::audio_levels_updated`.
    pub fn mix_frame(
        &mut self,
        now_ms: u64,
        out_frame: &mut [f32; VOIP_FRAME_SAMPLES],
    ) -> Vec<AudioLevelSample> {
        out_frame.fill(0.0);

        // Prune long-idle SSRCs so memory stays bounded in large group calls.
        self.streams
            .retain(|_, s| now_ms.saturating_sub(s.last_packet_ms) <= IDLE_STREAM_PRUNE_MS);

        let mut levels = Vec::with_capacity(self.streams.len());
        let mut ssrcs: Vec<u32> = self.streams.keys().copied().collect();
        ssrcs.sort_unstable();

        for ssrc in ssrcs {
            let Some(stream) = self.streams.get_mut(&ssrc) else {
                continue;
            };
            // Skip pulling stale streams that have stopped sending packets for > 1 s.
            if now_ms.saturating_sub(stream.last_packet_ms) > 1_000 {
                stream.smoothed_level *= 0.5;
                if stream.smoothed_level >= 0.01 {
                    levels.push(AudioLevelSample {
                        ssrc,
                        level: stream.smoothed_level,
                        voice: false,
                    });
                }
                continue;
            }

            let Ok(outcome) = stream.jitter.pull_frame(&mut self.scratch) else {
                continue;
            };

            // Measure pre-volume RMS so locally muting a participant still reports accurate
            // speaking indicators unless volume == 0 suppresses playback only.
            let raw_rms = if outcome == PullOutcome::Priming {
                0.0
            } else {
                compute_rms_level(&self.scratch)
            };

            let (level, voice) = if let Some((ext_voice, ext_lvl)) = stream.last_ext_level.take() {
                let blended = ext_lvl.max(raw_rms);
                stream.smoothed_level = stream.smoothed_level * 0.4 + blended * 0.6;
                (
                    stream.smoothed_level.clamp(0.0, 1.0),
                    ext_voice || stream.smoothed_level >= SPEAK_LEVEL_THRESHOLD,
                )
            } else {
                // Boost speech RMS (~0.1-0.4) into Telegram's 0..=1 UI level curve where 0.2 = speaking.
                let normalized = (raw_rms * 2.5).clamp(0.0, 1.0);
                stream.smoothed_level = stream.smoothed_level * 0.4 + normalized * 0.6;
                (
                    stream.smoothed_level,
                    stream.smoothed_level >= SPEAK_LEVEL_THRESHOLD,
                )
            };

            if level > 0.005 || voice {
                levels.push(AudioLevelSample { ssrc, level, voice });
            }

            let vol = stream.volume as f32;
            if vol > 0.0 && outcome != PullOutcome::Priming {
                for (dst, &src) in out_frame.iter_mut().zip(self.scratch.iter()) {
                    *dst += src * vol;
                }
            }
        }

        // Apply smooth soft clipping so overlapping speakers never wrap or harshly clip.
        for sample in out_frame.iter_mut() {
            *sample = soft_clip(*sample);
        }

        levels
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::{DEFAULT_VOICE_BITRATE_BPS, VoiceOpusEncoder};
    use crate::jitter::VOIP_SAMPLE_RATE;

    fn encode_sine_frames(freq_hz: f32, amplitude: f32, count: usize) -> Vec<Vec<u8>> {
        let mut enc = VoiceOpusEncoder::new(DEFAULT_VOICE_BITRATE_BPS).unwrap();
        let mut out = Vec::with_capacity(count);
        for f in 0..count {
            let mut pcm = [0.0f32; VOIP_FRAME_SAMPLES];
            for (i, s) in pcm.iter_mut().enumerate() {
                let t = (f * VOIP_FRAME_SAMPLES + i) as f32 / VOIP_SAMPLE_RATE as f32;
                *s = (2.0 * std::f32::consts::PI * freq_hz * t).sin() * amplitude;
            }
            out.push(enc.encode_frame(&pcm).unwrap());
        }
        out
    }

    fn goertzel(samples: &[f32], freq_hz: f32) -> f64 {
        let n = samples.len() as f64;
        let omega = 2.0 * std::f64::consts::PI * f64::from(freq_hz) / f64::from(VOIP_SAMPLE_RATE);
        let coeff = 2.0 * omega.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &x in samples {
            let s = f64::from(x) + coeff * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        (s2 * s2 + s1 * s1 - coeff * s1 * s2).max(0.0).sqrt() / n
    }

    #[test]
    fn group_mixer_combines_multiple_ssrcs_and_emits_speaking_levels() {
        let mut mixer = GroupAudioMixer::new(JitterBufferConfig {
            min_delay_ms: 40,
            max_delay_ms: 200,
            frame_ms: 20,
        });

        let ssrc_a = 1001u32;
        let ssrc_b = 2002u32;
        let ssrc_silent = 3003u32;

        let pkts_a = encode_sine_frames(440.0, 0.35, 25);
        let pkts_b = encode_sine_frames(880.0, 0.35, 25);
        let pkts_silent = encode_sine_frames(440.0, 0.0, 25);

        let mut mixed_all = Vec::new();
        let mut saw_a_speaking = false;
        let mut saw_b_speaking = false;
        let mut saw_silent_speaking = false;

        for i in 0..25usize {
            let ts = (i * VOIP_FRAME_SAMPLES) as u32;
            let t_ms = (i * 20) as u64;
            mixer
                .push_opus_packet(ssrc_a, i as u16, ts, t_ms, pkts_a[i].clone(), None)
                .unwrap();
            mixer
                .push_opus_packet(ssrc_b, i as u16, ts, t_ms, pkts_b[i].clone(), None)
                .unwrap();
            mixer
                .push_opus_packet(
                    ssrc_silent,
                    i as u16,
                    ts,
                    t_ms,
                    pkts_silent[i].clone(),
                    None,
                )
                .unwrap();

            let mut frame = [0.0f32; VOIP_FRAME_SAMPLES];
            let levels = mixer.mix_frame(t_ms, &mut frame);
            mixed_all.extend_from_slice(&frame);

            for lvl in levels {
                if lvl.ssrc == ssrc_a && lvl.voice {
                    saw_a_speaking = true;
                }
                if lvl.ssrc == ssrc_b && lvl.voice {
                    saw_b_speaking = true;
                }
                if lvl.ssrc == ssrc_silent && lvl.voice {
                    saw_silent_speaking = true;
                }
            }
        }

        assert_eq!(mixer.active_stream_count(), 3);
        assert!(saw_a_speaking, "SSRC A (440 Hz) must report speaking");
        assert!(saw_b_speaking, "SSRC B (880 Hz) must report speaking");
        assert!(!saw_silent_speaking, "silent SSRC must not report speaking");

        // Steady-state window must contain BOTH 440 Hz and 880 Hz spectral peaks!
        let steady = &mixed_all[VOIP_FRAME_SAMPLES * 5..];
        let m440 = goertzel(steady, 440.0);
        let m880 = goertzel(steady, 880.0);
        let m1500 = goertzel(steady, 1500.0);
        assert!(
            m440 > 0.06 && m880 > 0.06 && m440 > m1500 * 5.0 && m880 > m1500 * 5.0,
            "mixed audio should contain both 440 Hz and 880 Hz (m440={m440:.4}, m880={m880:.4}, m1500={m1500:.4})"
        );

        // Removing SSRCs cleans up immediately.
        mixer.remove_ssrcs(&[ssrc_b, ssrc_silent]);
        assert_eq!(mixer.active_stream_count(), 1);
    }
}
