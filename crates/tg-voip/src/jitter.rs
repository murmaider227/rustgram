//! Adaptive 48 kHz voice jitter buffer with Opus in-band FEC recovery and PLC.
//!
//! Reference: RFC 3550 §6.4.1 inter-arrival jitter estimator, WebRTC NetEq design,
//! and `crates/tg-calls/docs/media-engine.md` §3.

use std::collections::BTreeMap;

use opus_pure::OpusDecoder;

use crate::error::VoipResult;

/// Standard VoIP sample rate (48 kHz).
pub const VOIP_SAMPLE_RATE: u32 = 48_000;

/// Standard VoIP frame duration in milliseconds (20 ms).
pub const VOIP_FRAME_MS: u32 = 20;

/// Samples per 20 ms mono frame at 48 kHz (`960`).
pub const VOIP_FRAME_SAMPLES: usize = 960;

/// Default minimum target jitter buffer depth in milliseconds (`40 ms` = 2 frames).
pub const DEFAULT_MIN_DELAY_MS: u32 = 40;

/// Default maximum target jitter buffer depth in milliseconds (`200 ms` = 10 frames).
pub const DEFAULT_MAX_DELAY_MS: u32 = 200;

/// Maximum buffered packets before dropping stale frames.
const MAX_BUFFERED_PACKETS: usize = 32;

/// Configuration for [`AdaptiveJitterBuffer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JitterBufferConfig {
    /// Minimum target buffering delay in milliseconds (default 40 ms).
    pub min_delay_ms: u32,
    /// Maximum target buffering delay in milliseconds (default 200 ms).
    pub max_delay_ms: u32,
    /// Frame duration in milliseconds (default 20 ms).
    pub frame_ms: u32,
}

impl Default for JitterBufferConfig {
    fn default() -> Self {
        Self {
            min_delay_ms: DEFAULT_MIN_DELAY_MS,
            max_delay_ms: DEFAULT_MAX_DELAY_MS,
            frame_ms: VOIP_FRAME_MS,
        }
    }
}

/// Statistics tracked by [`AdaptiveJitterBuffer`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct JitterBufferStats {
    /// Total valid RTP audio packets pushed into the buffer.
    pub packets_received: u64,
    /// Packets dropped as duplicates.
    pub packets_duplicate: u64,
    /// Packets dropped because they arrived after their playout slot already passed.
    pub packets_late: u64,
    /// Packets that arrived out of sequence order (but still in time for playout).
    pub packets_reordered: u64,
    /// Missing playout slots (recovered via FEC or concealed via PLC).
    pub packets_lost: u64,
    /// Missing frames recovered using Opus in-band FEC from `seq + 1`.
    pub fec_recovered: u64,
    /// Missing frames synthesized using Opus Packet Loss Concealment (PLC).
    pub plc_frames: u64,
    /// Normal frames decoded directly from their RTP payload.
    pub normal_frames: u64,
    /// Smoothed RFC 3550 inter-arrival jitter estimate in milliseconds.
    pub jitter_ms: f64,
    /// Current adaptive target playout delay in milliseconds.
    pub target_delay_ms: u32,
    /// Number of packets currently queued awaiting playout.
    pub buffered_packets: usize,
}

/// Classification of one pulled 20 ms decoded audio frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullOutcome {
    /// Buffer is still priming initial delay; emitted silence.
    Priming,
    /// Normal Opus packet decoded.
    Normal,
    /// Missing packet recovered via Opus in-band FEC from the next packet.
    FecRecovered,
    /// Missing packet concealed via Opus PLC.
    PlcConcealed,
}

/// Adaptive 48 kHz mono Opus jitter buffer with sequence unwrap, reordering,
/// in-band FEC recovery, and packet loss concealment (PLC).
pub struct AdaptiveJitterBuffer {
    config: JitterBufferConfig,
    decoder: OpusDecoder,
    /// Unwrapped monotonic sequence number -> encoded Opus packet bytes.
    queue: BTreeMap<u64, Vec<u8>>,
    /// Sequence unwrapper state (`u16 -> u64`).
    last_seq_u16: Option<u16>,
    seq_cycles: u64,
    highest_received_seq: Option<u64>,
    /// Next unwrapped sequence number expected for playout.
    next_playout_seq: Option<u64>,
    primed: bool,
    /// RFC 3550 inter-arrival jitter state.
    last_transit_ms: Option<f64>,
    jitter_ms: f64,
    target_delay_ms: u32,
    steady_frames: u32,
    /// Interval loss tracking (for RTCP Receiver Reports).
    interval_expected: u64,
    interval_lost: u64,
    stats: JitterBufferStats,
}

impl AdaptiveJitterBuffer {
    /// Create a new 48 kHz mono adaptive jitter buffer and Opus decoder.
    pub fn new(config: JitterBufferConfig) -> VoipResult<Self> {
        let decoder = OpusDecoder::new(VOIP_SAMPLE_RATE as i32, 1)?;
        let target_delay_ms = config.min_delay_ms.max(config.frame_ms);
        Ok(Self {
            config,
            decoder,
            queue: BTreeMap::new(),
            last_seq_u16: None,
            seq_cycles: 0,
            highest_received_seq: None,
            next_playout_seq: None,
            primed: false,
            last_transit_ms: None,
            jitter_ms: 0.0,
            target_delay_ms,
            steady_frames: 0,
            interval_expected: 0,
            interval_lost: 0,
            stats: JitterBufferStats {
                target_delay_ms,
                ..JitterBufferStats::default()
            },
        })
    }

    /// Push a received RTP Opus audio packet into the jitter buffer.
    ///
    /// `rtp_timestamp` is in 48 kHz sample ticks; `arrival_ms` is monotonic receive time in ms.
    pub fn push(&mut self, seq_u16: u16, rtp_timestamp: u32, arrival_ms: u64, payload: Vec<u8>) {
        let unwrapped = self.unwrap_seq(seq_u16);
        self.stats.packets_received += 1;

        // Update RFC 3550 inter-arrival jitter estimate: J += (|D| - J) / 16.
        let rtp_ms = f64::from(rtp_timestamp) / (f64::from(VOIP_SAMPLE_RATE) / 1000.0);
        let transit = arrival_ms as f64 - rtp_ms;
        if let Some(prev_transit) = self.last_transit_ms {
            let d = (transit - prev_transit).abs();
            // Clamp huge spikes (e.g. DTX gaps) to avoid destabilizing target delay.
            let d_clamped = d.min(f64::from(self.config.max_delay_ms));
            self.jitter_ms += (d_clamped - self.jitter_ms) / 16.0;
        }
        self.last_transit_ms = Some(transit);
        self.stats.jitter_ms = self.jitter_ms;

        // Track out-of-order arrivals.
        if let Some(highest) = self.highest_received_seq {
            if unwrapped < highest {
                self.stats.packets_reordered += 1;
            } else {
                self.highest_received_seq = Some(unwrapped);
            }
        } else {
            self.highest_received_seq = Some(unwrapped);
        }

        // Reject packets whose playout slot already passed.
        if let Some(next_playout) = self.next_playout_seq
            && unwrapped < next_playout
        {
            self.stats.packets_late += 1;
            self.bump_target_delay_on_late();
            return;
        }

        // Duplicate check.
        if self.queue.contains_key(&unwrapped) {
            self.stats.packets_duplicate += 1;
            return;
        }

        self.queue.insert(unwrapped, payload);

        // Initialize playout cursor on the very first packet.
        if self.next_playout_seq.is_none() {
            self.next_playout_seq = Some(unwrapped);
        } else if !self.primed
            && let Some(first_key) = self.queue.keys().next().copied()
        {
            // While still priming, allow an earlier reordered packet to become the start.
            self.next_playout_seq = Some(first_key);
        }

        // Prevent unbounded queue growth on severe clock skew or pauses.
        while self.queue.len() > MAX_BUFFERED_PACKETS {
            if let Some((&oldest_seq, _)) = self.queue.iter().next() {
                self.queue.remove(&oldest_seq);
                if let Some(next) = self.next_playout_seq
                    && next <= oldest_seq
                {
                    self.next_playout_seq = Some(oldest_seq + 1);
                }
            }
        }

        self.recompute_target_delay();
        self.stats.buffered_packets = self.queue.len();
    }

    /// Pull the next 20 ms (`960` samples) of 48 kHz mono `f32` PCM into `out_frame`.
    ///
    /// Returns the [`PullOutcome`] describing whether audio was decoded normally,
    /// recovered via Opus in-band FEC, concealed via PLC, or silenced while priming.
    pub fn pull_frame(
        &mut self,
        out_frame: &mut [f32; VOIP_FRAME_SAMPLES],
    ) -> VoipResult<PullOutcome> {
        let Some(playout_seq) = self.next_playout_seq else {
            out_frame.fill(0.0);
            return Ok(PullOutcome::Priming);
        };

        let target_frames = self.target_delay_frames();
        if !self.primed {
            // Wait until we have accumulated `target_frames` worth of span or packets.
            let span_frames = self
                .highest_received_seq
                .map(|h| h.saturating_sub(playout_seq) + 1)
                .unwrap_or(0) as usize;
            if self.queue.len() < target_frames && span_frames < target_frames {
                out_frame.fill(0.0);
                return Ok(PullOutcome::Priming);
            }
            self.primed = true;
        }

        self.interval_expected += 1;

        let outcome = if let Some(packet) = self.queue.remove(&playout_seq) {
            let decoded = self
                .decoder
                .decode(&packet, VOIP_FRAME_SAMPLES, out_frame)?;
            if decoded < VOIP_FRAME_SAMPLES {
                out_frame[decoded..].fill(0.0);
            }
            self.stats.normal_frames += 1;
            self.steady_frames += 1;
            if self.steady_frames >= 50 {
                // After ~1 s of steady arrival, allow target delay to relax toward min.
                self.steady_frames = 0;
                self.recompute_target_delay();
            }
            PullOutcome::Normal
        } else {
            // Packet missing at `playout_seq`.
            self.stats.packets_lost += 1;
            self.interval_lost += 1;
            self.steady_frames = 0;

            // If the NEXT packet (`playout_seq + 1`) is already buffered, try Opus in-band FEC!
            if let Some(next_pkt) = self.queue.get(&(playout_seq + 1)) {
                match self
                    .decoder
                    .decode_fec(next_pkt, VOIP_FRAME_SAMPLES, out_frame)
                {
                    Ok(decoded) if decoded > 0 => {
                        if decoded < VOIP_FRAME_SAMPLES {
                            out_frame[decoded..].fill(0.0);
                        }
                        self.stats.fec_recovered += 1;
                        PullOutcome::FecRecovered
                    }
                    _ => {
                        self.conceal_plc(out_frame)?;
                        PullOutcome::PlcConcealed
                    }
                }
            } else {
                self.conceal_plc(out_frame)?;
                PullOutcome::PlcConcealed
            }
        };

        self.next_playout_seq = Some(playout_seq + 1);

        // If the buffer has grown way past `target_frames + 4`, fast-forward one frame
        // to keep latency bounded.
        if self.queue.len() > target_frames + 4
            && let Some(next) = self.next_playout_seq
            && let Some((&oldest, _)) = self.queue.iter().next()
            && oldest >= next
        {
            self.queue.remove(&oldest);
            self.next_playout_seq = Some(oldest + 1);
        }

        self.stats.buffered_packets = self.queue.len();
        self.stats.target_delay_ms = self.target_delay_ms;
        Ok(outcome)
    }

    /// Take the RFC 3550 fraction lost (`0..=255`) since the last call and reset interval counters.
    pub fn take_interval_fraction_lost(&mut self) -> u8 {
        if self.interval_expected == 0 {
            return 0;
        }
        let frac = ((self.interval_lost * 256) / self.interval_expected).min(255) as u8;
        self.interval_expected = 0;
        self.interval_lost = 0;
        frac
    }

    /// Snapshot of current jitter buffer statistics.
    #[must_use]
    pub fn stats(&self) -> JitterBufferStats {
        let mut s = self.stats;
        s.buffered_packets = self.queue.len();
        s.target_delay_ms = self.target_delay_ms;
        s
    }

    fn conceal_plc(&mut self, out_frame: &mut [f32; VOIP_FRAME_SAMPLES]) -> VoipResult<()> {
        let decoded = self.decoder.decode(&[], VOIP_FRAME_SAMPLES, out_frame)?;
        if decoded < VOIP_FRAME_SAMPLES {
            out_frame[decoded..].fill(0.0);
        }
        self.stats.plc_frames += 1;
        Ok(())
    }

    fn target_delay_frames(&self) -> usize {
        let frames = self.target_delay_ms.div_ceil(self.config.frame_ms.max(1));
        (frames as usize).max(1)
    }

    fn bump_target_delay_on_late(&mut self) {
        self.target_delay_ms =
            (self.target_delay_ms + self.config.frame_ms).min(self.config.max_delay_ms);
        self.stats.target_delay_ms = self.target_delay_ms;
    }

    fn recompute_target_delay(&mut self) {
        // Target = max(min_delay, frame_ms + 3 * jitter_ms), clamped to max_delay.
        let desired = (f64::from(self.config.frame_ms) + 3.0 * self.jitter_ms).ceil() as u32;
        let clamped = desired.clamp(self.config.min_delay_ms, self.config.max_delay_ms);
        if clamped > self.target_delay_ms {
            self.target_delay_ms = clamped;
        } else if clamped < self.target_delay_ms {
            // Shrink slowly by at most 1 frame per relaxation step.
            self.target_delay_ms = self
                .target_delay_ms
                .saturating_sub(self.config.frame_ms)
                .max(clamped);
        }
        self.stats.target_delay_ms = self.target_delay_ms;
    }

    /// Unwrap 16-bit RTP sequence numbers into monotonic `u64` values handling wrap-around
    /// and moderate reordering cleanly.
    fn unwrap_seq(&mut self, seq: u16) -> u64 {
        let Some(last) = self.last_seq_u16 else {
            self.last_seq_u16 = Some(seq);
            return u64::from(seq);
        };
        let delta = seq.wrapping_sub(last) as i16;
        if delta >= 0 {
            if seq < last {
                self.seq_cycles += 1;
            }
            self.last_seq_u16 = Some(seq);
            (self.seq_cycles << 16) | u64::from(seq)
        } else {
            // Reordered packet earlier than `last`.
            let cycles = if seq > last {
                self.seq_cycles.saturating_sub(1)
            } else {
                self.seq_cycles
            };
            (cycles << 16) | u64::from(seq)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opus_pure::{Application, MAX_PACKET_BYTES, OpusEncoder};

    fn make_sine_packets(count: usize) -> Vec<Vec<u8>> {
        let mut enc = OpusEncoder::new(VOIP_SAMPLE_RATE as i32, 1, Application::Voip).unwrap();
        enc.bitrate_bps = 32_000;
        enc.use_inband_fec = true;
        enc.packet_loss_perc = 20;

        let mut pkt = vec![0u8; MAX_PACKET_BYTES];
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let mut pcm = [0.0f32; VOIP_FRAME_SAMPLES];
            for (j, s) in pcm.iter_mut().enumerate() {
                let t = (i * VOIP_FRAME_SAMPLES + j) as f32 / VOIP_SAMPLE_RATE as f32;
                *s = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5;
            }
            let n = enc.encode(&pcm, VOIP_FRAME_SAMPLES, &mut pkt).unwrap();
            out.push(pkt[..n].to_vec());
        }
        out
    }

    #[test]
    fn jitter_buffer_reorders_and_conceals_losses() {
        let packets = make_sine_packets(10);
        let mut jb = AdaptiveJitterBuffer::new(JitterBufferConfig::default()).unwrap();

        // Push packets 0, 2, 1 (reordered), skip 3 (lost!), push 4, 5.
        jb.push(0, 0, 0, packets[0].clone());
        jb.push(2, 1920, 40, packets[2].clone());
        jb.push(1, 960, 45, packets[1].clone());
        // packet 3 dropped!
        jb.push(4, 3840, 80, packets[4].clone());
        jb.push(5, 4800, 100, packets[5].clone());

        let mut frame = [0.0f32; VOIP_FRAME_SAMPLES];
        // Pull seq 0, 1, 2 (normal), 3 (FEC or PLC), 4, 5 (normal).
        assert_eq!(jb.pull_frame(&mut frame).unwrap(), PullOutcome::Normal);
        assert_eq!(jb.pull_frame(&mut frame).unwrap(), PullOutcome::Normal);
        assert_eq!(jb.pull_frame(&mut frame).unwrap(), PullOutcome::Normal);
        let lost_outcome = jb.pull_frame(&mut frame).unwrap();
        assert!(matches!(
            lost_outcome,
            PullOutcome::FecRecovered | PullOutcome::PlcConcealed
        ));
        assert_eq!(jb.pull_frame(&mut frame).unwrap(), PullOutcome::Normal);
        assert_eq!(jb.pull_frame(&mut frame).unwrap(), PullOutcome::Normal);

        let stats = jb.stats();
        assert_eq!(stats.packets_reordered, 1);
        assert_eq!(stats.packets_lost, 1);
        assert_eq!(stats.normal_frames, 5);
        assert_eq!(stats.fec_recovered + stats.plc_frames, 1);
    }

    #[test]
    fn jitter_buffer_handles_u16_sequence_wraparound() {
        let packets = make_sine_packets(6);
        let mut jb = AdaptiveJitterBuffer::new(JitterBufferConfig {
            min_delay_ms: 40,
            max_delay_ms: 200,
            frame_ms: 20,
        })
        .unwrap();

        let seqs: [u16; 6] = [65534, 65535, 0, 1, 2, 3];
        for (idx, &s) in seqs.iter().enumerate() {
            jb.push(
                s,
                (idx * VOIP_FRAME_SAMPLES) as u32,
                (idx * 20) as u64,
                packets[idx].clone(),
            );
        }

        let mut frame = [0.0f32; VOIP_FRAME_SAMPLES];
        for _ in 0..6 {
            assert_eq!(jb.pull_frame(&mut frame).unwrap(), PullOutcome::Normal);
        }
        assert_eq!(jb.stats().packets_lost, 0);
        assert_eq!(jb.stats().packets_late, 0);
    }
}
