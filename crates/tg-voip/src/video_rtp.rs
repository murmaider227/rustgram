//! RFC 6184 H.264 / AVC RTP packetization (Single NAL, STAP-A, FU-A) and depacketization.
//!
//! Reference: RFC 6184 (§5.6 Single NAL Unit Packet, §5.7.1 STAP-A, §5.8 FU-A) and
//! `TelegramMessenger/tgcalls` video RTP pipeline.
//!
//! - Splits Annex-B H.264 bitstreams (`00 00 01` / `00 00 00 01`) into NAL units.
//! - Packetizes small consecutive NAL units (e.g. SPS + PPS) into STAP-A packets,
//!   medium NAL units into Single NAL packets, and large NAL units (> MTU) into
//!   FU-A fragments with `S` / `E` bits and RTP marker bit on the final packet of a frame.
//! - Reassembles incoming RTP video packets into complete Annex-B access units,
//!   tracking SPS/PPS parameter sets and flagging `needs_keyframe` on sequence gaps
//!   or dropped FU-A fragments so the caller can issue RTCP PLI/FIR immediately.

use crate::error::{VoipError, VoipResult};
use crate::rtp::{H264_CLOCK_RATE, H264_RTP_PAYLOAD_TYPE, RtpHeader};

/// Default maximum RTP payload size in bytes (fits safely inside 1280-byte IPv6/MTProto MTU).
pub const DEFAULT_MAX_RTP_PAYLOAD: usize = 1_150;

/// H.264 NAL unit type: Coded slice of a non-IDR picture.
pub const NAL_TYPE_NON_IDR: u8 = 1;
/// H.264 NAL unit type: Coded slice of an IDR picture (keyframe).
pub const NAL_TYPE_IDR: u8 = 5;
/// H.264 NAL unit type: Supplemental enhancement information (SEI).
pub const NAL_TYPE_SEI: u8 = 6;
/// H.264 NAL unit type: Sequence parameter set (SPS).
pub const NAL_TYPE_SPS: u8 = 7;
/// H.264 NAL unit type: Picture parameter set (PPS).
pub const NAL_TYPE_PPS: u8 = 8;
/// H.264 NAL unit type: Access unit delimiter (AUD).
pub const NAL_TYPE_AUD: u8 = 9;
/// RFC 6184 Single-Time Aggregation Packet type A (STAP-A).
pub const NAL_TYPE_STAP_A: u8 = 24;
/// RFC 6184 Fragmentation Unit type A (FU-A).
pub const NAL_TYPE_FU_A: u8 = 28;

/// 4-byte Annex-B start code (`00 00 00 01`).
pub const ANNEX_B_START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];

/// Extract the 5-bit H.264 NAL unit type from the first byte of a NAL unit.
#[must_use]
pub const fn nal_unit_type(nal_header: u8) -> u8 {
    nal_header & 0x1F
}

/// Split an Annex-B H.264 byte stream (`00 00 01` or `00 00 00 01` prefixed) into individual
/// raw NAL unit slices (without start codes).
#[must_use]
pub fn split_annex_b_nalus(bitstream: &[u8]) -> Vec<&[u8]> {
    let mut positions: Vec<(usize, usize)> = Vec::new();
    let len = bitstream.len();
    let mut i = 0usize;

    while i + 2 < len {
        if bitstream[i] == 0 && bitstream[i + 1] == 0 {
            if bitstream[i + 2] == 1 {
                positions.push((i, i + 3));
                i += 3;
                continue;
            } else if i + 3 < len && bitstream[i + 2] == 0 && bitstream[i + 3] == 1 {
                positions.push((i, i + 4));
                i += 4;
                continue;
            }
        }
        i += 1;
    }

    if positions.is_empty() {
        return if bitstream.is_empty() {
            Vec::new()
        } else {
            vec![bitstream]
        };
    }

    let mut nalus = Vec::with_capacity(positions.len());
    for idx in 0..positions.len() {
        let start = positions[idx].1;
        let end = if idx + 1 < positions.len() {
            positions[idx + 1].0
        } else {
            len
        };
        if start < end {
            nalus.push(&bitstream[start..end]);
        }
    }
    nalus
}

/// Returns `true` if any NAL unit in the slice is an IDR keyframe slice (type 5).
#[must_use]
pub fn nalus_contain_keyframe(nalus: &[&[u8]]) -> bool {
    nalus
        .iter()
        .any(|n| !n.is_empty() && nal_unit_type(n[0]) == NAL_TYPE_IDR)
}

/// Serialize raw NAL units back into an Annex-B byte stream with 4-byte start codes.
#[must_use]
pub fn join_annex_b_nalus(nalus: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = nalus.iter().map(|n| 4 + n.len()).sum();
    let mut out = Vec::with_capacity(total);
    for nal in nalus {
        if !nal.is_empty() {
            out.extend_from_slice(&ANNEX_B_START_CODE);
            out.extend_from_slice(nal);
        }
    }
    out
}

/// One RTP-packetized video datagram ready for transmission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct H264RtpPacket {
    /// RFC 3550 RTP fixed header.
    pub header: RtpHeader,
    /// RFC 6184 H.264 RTP payload (Single NAL, STAP-A, or FU-A).
    pub payload: Vec<u8>,
}

/// Stateful RFC 6184 H.264 RTP packetizer.
#[derive(Debug, Clone)]
pub struct H264RtpPacketizer {
    ssrc: u32,
    payload_type: u8,
    max_payload_size: usize,
    next_seq: u16,
    /// Cached SPS/PPS so every outgoing keyframe carries them if the encoder omitted them.
    last_sps: Option<Vec<u8>>,
    last_pps: Option<Vec<u8>>,
}

impl H264RtpPacketizer {
    /// Create a new packetizer for the given video `ssrc`.
    #[must_use]
    pub fn new(ssrc: u32) -> Self {
        Self {
            ssrc,
            payload_type: H264_RTP_PAYLOAD_TYPE,
            max_payload_size: DEFAULT_MAX_RTP_PAYLOAD,
            next_seq: 0,
            last_sps: None,
            last_pps: None,
        }
    }

    /// Override RTP payload type (default `102`).
    #[must_use]
    pub const fn with_payload_type(mut self, payload_type: u8) -> Self {
        self.payload_type = payload_type;
        self
    }

    /// Override maximum RTP payload size in bytes (minimum `32`).
    #[must_use]
    pub fn with_max_payload_size(mut self, max_payload_size: usize) -> Self {
        self.max_payload_size = max_payload_size.max(32);
        self
    }

    /// Convert a presentation timestamp in microseconds to a 90 kHz RTP timestamp.
    #[must_use]
    pub const fn timestamp_us_to_rtp(timestamp_us: u64) -> u32 {
        ((timestamp_us.wrapping_mul(H264_CLOCK_RATE as u64)) / 1_000_000) as u32
    }

    /// Packetize one Annex-B H.264 access unit into RFC 6184 RTP packets
    /// (STAP-A for small consecutive NALs, Single NAL, or FU-A for oversized NALs).
    pub fn packetize_annex_b(&mut self, annex_b: &[u8], rtp_timestamp: u32) -> Vec<H264RtpPacket> {
        let raw_nalus = split_annex_b_nalus(annex_b);
        if raw_nalus.is_empty() {
            return Vec::new();
        }

        // Cache SPS/PPS and prepend cached SPS/PPS before bare IDR frames.
        let mut has_sps = false;
        let mut has_pps = false;
        let mut has_idr = false;
        for nal in &raw_nalus {
            if let Some(&hdr) = nal.first() {
                match nal_unit_type(hdr) {
                    NAL_TYPE_SPS => {
                        has_sps = true;
                        self.last_sps = Some(nal.to_vec());
                    }
                    NAL_TYPE_PPS => {
                        has_pps = true;
                        self.last_pps = Some(nal.to_vec());
                    }
                    NAL_TYPE_IDR => {
                        has_idr = true;
                    }
                    _ => {}
                }
            }
        }

        let mut nalus: Vec<Vec<u8>> = Vec::with_capacity(raw_nalus.len() + 2);
        if has_idr
            && !has_sps
            && let Some(sps) = &self.last_sps
        {
            nalus.push(sps.clone());
        }
        if has_idr
            && !has_pps
            && let Some(pps) = &self.last_pps
        {
            nalus.push(pps.clone());
        }
        for nal in raw_nalus {
            if !nal.is_empty() {
                nalus.push(nal.to_vec());
            }
        }

        let mut payloads: Vec<Vec<u8>> = Vec::new();
        let mut idx = 0usize;

        while idx < nalus.len() {
            let nal = &nalus[idx];

            if nal.len() > self.max_payload_size {
                // Fragment using RFC 6184 §5.8 FU-A.
                self.fragment_fu_a(nal, &mut payloads);
                idx += 1;
                continue;
            }

            // Try to aggregate 2+ small consecutive NALs into a STAP-A packet (§5.7.1)
            // whenever they fit within `max_payload_size` (common for SPS + PPS + SEI).
            if idx + 1 < nalus.len() {
                let next_nal = &nalus[idx + 1];
                let two_len = 1 + (2 + nal.len()) + (2 + next_nal.len());
                if two_len <= self.max_payload_size
                    && nal.len() <= usize::from(u16::MAX)
                    && next_nal.len() <= usize::from(u16::MAX)
                {
                    let mut stap = Vec::with_capacity(self.max_payload_size.min(two_len + 64));
                    let mut max_nri = nal[0] & 0x60;
                    stap.push(NAL_TYPE_STAP_A); // placeholder header, updated with max NRI

                    let mut cur_len = 1usize;
                    while idx < nalus.len() {
                        let candidate = &nalus[idx];
                        let needed = 2 + candidate.len();
                        if cur_len + needed > self.max_payload_size
                            || candidate.len() > usize::from(u16::MAX)
                        {
                            break;
                        }
                        max_nri = max_nri.max(candidate[0] & 0x60);
                        stap.extend_from_slice(&(candidate.len() as u16).to_be_bytes());
                        stap.extend_from_slice(candidate);
                        cur_len += needed;
                        idx += 1;
                    }
                    stap[0] = max_nri | NAL_TYPE_STAP_A;
                    payloads.push(stap);
                    continue;
                }
            }

            // Single NAL Unit packet (§5.6).
            payloads.push(nal.clone());
            idx += 1;
        }

        let last_idx = payloads.len().saturating_sub(1);
        payloads
            .into_iter()
            .enumerate()
            .map(|(i, payload)| {
                let seq = self.next_seq;
                self.next_seq = self.next_seq.wrapping_add(1);
                H264RtpPacket {
                    header: RtpHeader {
                        marker: i == last_idx,
                        payload_type: self.payload_type,
                        sequence_number: seq,
                        timestamp: rtp_timestamp,
                        ssrc: self.ssrc,
                    },
                    payload,
                }
            })
            .collect()
    }

    fn fragment_fu_a(&self, nal: &[u8], out: &mut Vec<Vec<u8>>) {
        let nal_hdr = nal[0];
        let f_nri = nal_hdr & 0xE0; // F bit + NRI bits
        let nal_type = nal_unit_type(nal_hdr);
        let fu_indicator = f_nri | NAL_TYPE_FU_A;

        let body = &nal[1..];
        let chunk_max = self.max_payload_size.saturating_sub(2).max(1);
        let total_chunks = body.len().div_ceil(chunk_max).max(1);

        for (chunk_idx, chunk) in body.chunks(chunk_max).enumerate() {
            let is_start = chunk_idx == 0;
            let is_end = chunk_idx + 1 == total_chunks;
            let fu_header = (u8::from(is_start) << 7) | (u8::from(is_end) << 6) | nal_type;

            let mut pkt = Vec::with_capacity(2 + chunk.len());
            pkt.push(fu_indicator);
            pkt.push(fu_header);
            pkt.extend_from_slice(chunk);
            out.push(pkt);
        }
    }
}

/// Complete Annex-B H.264 access unit assembled by [`H264RtpDepacketizer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledH264Frame {
    /// 90 kHz RTP timestamp of this access unit.
    pub rtp_timestamp: u32,
    /// Whether this frame contains an IDR keyframe slice.
    pub is_keyframe: bool,
    /// Annex-B bitstream (`00 00 00 01` prefixed NAL units) ready for [`crate::VideoDecoder`].
    pub annex_b: Vec<u8>,
}

/// Outcome returned by [`H264RtpDepacketizer::push_packet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepacketizeOutcome {
    /// Completed Annex-B frame if the packet finished an access unit.
    pub frame: Option<AssembledH264Frame>,
    /// `true` if packet loss or a broken FU-A fragment occurred and the receiver should
    /// request a remote keyframe via RTCP PLI / FIR.
    pub needs_keyframe: bool,
}

/// Stateful RFC 6184 H.264 RTP depacketizer (Single NAL, STAP-A, FU-A).
#[derive(Debug, Default)]
pub struct H264RtpDepacketizer {
    last_seq: Option<u16>,
    current_ts: Option<u32>,
    current_nalus: Vec<Vec<u8>>,
    fu_buffer: Option<Vec<u8>>,
    frame_corrupted: bool,
    awaiting_keyframe: bool,
    cached_sps: Option<Vec<u8>>,
    cached_pps: Option<Vec<u8>>,
}

impl H264RtpDepacketizer {
    /// Create a fresh H.264 depacketizer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset assembly state (e.g. after remote stream pause or SSRC change).
    pub fn reset(&mut self) {
        self.last_seq = None;
        self.current_ts = None;
        self.current_nalus.clear();
        self.fu_buffer = None;
        self.frame_corrupted = false;
        self.awaiting_keyframe = true;
    }

    /// Feed one incoming H.264 RTP packet into the depacketizer.
    pub fn push_packet(
        &mut self,
        header: &RtpHeader,
        payload: &[u8],
    ) -> VoipResult<DepacketizeOutcome> {
        if payload.is_empty() {
            return Err(VoipError::InvalidFraming("empty H.264 RTP payload".into()));
        }

        let mut needs_keyframe = false;

        // Detect sequence gaps.
        if let Some(prev_seq) = self.last_seq {
            let expected = prev_seq.wrapping_add(1);
            if header.sequence_number != expected {
                // Dropped or reordered packet: discard in-progress FU-A and request a keyframe.
                self.fu_buffer = None;
                self.frame_corrupted = true;
                self.awaiting_keyframe = true;
                needs_keyframe = true;
            }
        }
        self.last_seq = Some(header.sequence_number);

        // If the RTP timestamp advanced before seeing a marker bit, flush/drop previous partial frame.
        let mut emitted_frame: Option<AssembledH264Frame> = None;
        if let Some(cur_ts) = self.current_ts
            && cur_ts != header.timestamp
        {
            if !self.frame_corrupted && self.fu_buffer.is_none() && !self.current_nalus.is_empty() {
                emitted_frame = self.finalize_frame(cur_ts);
            } else {
                self.current_nalus.clear();
                self.fu_buffer = None;
            }
            self.frame_corrupted = false;
        }
        self.current_ts = Some(header.timestamp);

        let nalu_type = nal_unit_type(payload[0]);
        match nalu_type {
            1..=23 => {
                // Single NAL Unit packet (§5.6).
                self.fu_buffer = None;
                self.record_nal(payload.to_vec());
            }
            NAL_TYPE_STAP_A => {
                // Single-Time Aggregation Packet type A (§5.7.1).
                self.fu_buffer = None;
                let mut offset = 1usize;
                while offset + 2 <= payload.len() {
                    let nal_len =
                        u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
                    offset += 2;
                    if nal_len == 0 || offset + nal_len > payload.len() {
                        self.frame_corrupted = true;
                        needs_keyframe = true;
                        break;
                    }
                    self.record_nal(payload[offset..offset + nal_len].to_vec());
                    offset += nal_len;
                }
            }
            NAL_TYPE_FU_A => {
                // Fragmentation Unit type A (§5.8).
                if payload.len() < 2 {
                    self.frame_corrupted = true;
                    self.fu_buffer = None;
                    needs_keyframe = true;
                } else {
                    let fu_indicator = payload[0];
                    let fu_header = payload[1];
                    let start = (fu_header & 0x80) != 0;
                    let end = (fu_header & 0x40) != 0;
                    let orig_type = fu_header & 0x1F;
                    let reconstructed_hdr = (fu_indicator & 0xE0) | orig_type;

                    if start {
                        let mut buf = Vec::with_capacity(payload.len());
                        buf.push(reconstructed_hdr);
                        buf.extend_from_slice(&payload[2..]);
                        if end {
                            self.record_nal(buf);
                            self.fu_buffer = None;
                        } else {
                            self.fu_buffer = Some(buf);
                        }
                    } else if let Some(ref mut buf) = self.fu_buffer {
                        buf.extend_from_slice(&payload[2..]);
                        if end && let Some(finished) = self.fu_buffer.take() {
                            self.record_nal(finished);
                        }
                    } else {
                        // Mid/end fragment without start fragment -> loss.
                        self.frame_corrupted = true;
                        self.awaiting_keyframe = true;
                        needs_keyframe = true;
                    }
                }
            }
            other => {
                return Err(VoipError::InvalidFraming(format!(
                    "unsupported H.264 RTP NAL type {other}"
                )));
            }
        }

        if header.marker {
            let ts = header.timestamp;
            if !self.frame_corrupted && self.fu_buffer.is_none() && !self.current_nalus.is_empty() {
                emitted_frame = self.finalize_frame(ts);
            } else {
                self.current_nalus.clear();
                self.fu_buffer = None;
                self.frame_corrupted = false;
                needs_keyframe = true;
            }
            self.current_ts = None;
        }

        Ok(DepacketizeOutcome {
            frame: emitted_frame,
            needs_keyframe,
        })
    }

    fn record_nal(&mut self, nal: Vec<u8>) {
        if let Some(&hdr) = nal.first() {
            match nal_unit_type(hdr) {
                NAL_TYPE_SPS => self.cached_sps = Some(nal.clone()),
                NAL_TYPE_PPS => self.cached_pps = Some(nal.clone()),
                _ => {}
            }
        }
        self.current_nalus.push(nal);
    }

    fn finalize_frame(&mut self, rtp_timestamp: u32) -> Option<AssembledH264Frame> {
        let mut nalus = std::mem::take(&mut self.current_nalus);
        if nalus.is_empty() {
            return None;
        }
        let has_idr = nalus
            .iter()
            .any(|n| !n.is_empty() && nal_unit_type(n[0]) == NAL_TYPE_IDR);
        let has_sps = nalus
            .iter()
            .any(|n| !n.is_empty() && nal_unit_type(n[0]) == NAL_TYPE_SPS);
        let has_pps = nalus
            .iter()
            .any(|n| !n.is_empty() && nal_unit_type(n[0]) == NAL_TYPE_PPS);

        if has_idr {
            self.awaiting_keyframe = false;
            // Ensure SPS/PPS precede IDR if previously cached.
            let mut prefix = Vec::new();
            if !has_sps && let Some(sps) = &self.cached_sps {
                prefix.push(sps.clone());
            }
            if !has_pps && let Some(pps) = &self.cached_pps {
                prefix.push(pps.clone());
            }
            if !prefix.is_empty() {
                prefix.append(&mut nalus);
                nalus = prefix;
            }
        } else if self.awaiting_keyframe {
            // Drop P-frames until a clean IDR arrives after packet loss.
            return None;
        }

        let annex_b = join_annex_b_nalus(&nalus);
        Some(AssembledH264Frame {
            rtp_timestamp,
            is_keyframe: has_idr,
            annex_b,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_nal(nal_header: u8, payload_len: usize, seed: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + payload_len);
        out.push(nal_header);
        for i in 0..payload_len {
            out.push(seed.wrapping_add((i & 0xFF) as u8));
        }
        out
    }

    #[test]
    fn h264_rtp_single_nal_stap_a_and_fu_a_roundtrip() {
        let sps = make_nal(0x67, 24, 0x10); // NAL type 7 (SPS)
        let pps = make_nal(0x68, 8, 0x20); // NAL type 8 (PPS)
        let idr = make_nal(0x65, 3500, 0x30); // NAL type 5 (IDR, forces FU-A)
        let p_slice = make_nal(0x41, 400, 0x70); // NAL type 1 (non-IDR, Single NAL)

        let keyframe_annex_b = join_annex_b_nalus(&[sps.clone(), pps.clone(), idr.clone()]);
        let pframe_annex_b = join_annex_b_nalus(std::slice::from_ref(&p_slice));

        let mut packetizer = H264RtpPacketizer::new(0x1122_3344).with_max_payload_size(500);
        let mut depacketizer = H264RtpDepacketizer::new();

        // 1. Keyframe: SPS+PPS aggregated as STAP-A, IDR fragmented as FU-A.
        let kf_pkts = packetizer.packetize_annex_b(&keyframe_annex_b, 90_000);
        assert!(
            kf_pkts.len() >= 8,
            "expected STAP-A + multiple FU-A packets"
        );
        assert_eq!(
            nal_unit_type(kf_pkts[0].payload[0]),
            NAL_TYPE_STAP_A,
            "SPS+PPS should be aggregated into STAP-A"
        );
        assert_eq!(
            nal_unit_type(kf_pkts[1].payload[0]),
            NAL_TYPE_FU_A,
            "large IDR should be fragmented as FU-A"
        );
        assert!(!kf_pkts[0].header.marker);
        assert!(kf_pkts.last().unwrap().header.marker);

        let mut assembled_kf = None;
        for pkt in &kf_pkts {
            let outcome = depacketizer.push_packet(&pkt.header, &pkt.payload).unwrap();
            assert!(!outcome.needs_keyframe);
            if let Some(frame) = outcome.frame {
                assembled_kf = Some(frame);
            }
        }
        let kf = assembled_kf.expect("keyframe should reassemble on marker");
        assert!(kf.is_keyframe);
        assert_eq!(kf.rtp_timestamp, 90_000);
        assert_eq!(kf.annex_b, keyframe_annex_b);

        // 2. P-frame: Single NAL Unit packet.
        let pf_pkts = packetizer.packetize_annex_b(&pframe_annex_b, 93_000);
        assert_eq!(pf_pkts.len(), 1);
        assert_eq!(nal_unit_type(pf_pkts[0].payload[0]), NAL_TYPE_NON_IDR);
        assert!(pf_pkts[0].header.marker);

        let pf_out = depacketizer
            .push_packet(&pf_pkts[0].header, &pf_pkts[0].payload)
            .unwrap();
        assert!(!pf_out.needs_keyframe);
        let pf = pf_out.frame.expect("P-frame should reassemble");
        assert!(!pf.is_keyframe);
        assert_eq!(pf.annex_b, pframe_annex_b);
    }

    #[test]
    fn h264_rtp_packet_loss_triggers_keyframe_request_and_recovers_on_idr() {
        let sps = make_nal(0x67, 16, 1);
        let pps = make_nal(0x68, 6, 2);
        let idr = make_nal(0x65, 1600, 3);
        let p_frame = make_nal(0x41, 200, 4);

        let kf_bytes = join_annex_b_nalus(&[sps, pps, idr]);
        let pf_bytes = join_annex_b_nalus(&[p_frame]);

        let mut packetizer = H264RtpPacketizer::new(0x5555).with_max_payload_size(400);
        let mut depacketizer = H264RtpDepacketizer::new();

        // Drop middle FU-A packet of first frame!
        let pkts1 = packetizer.packetize_annex_b(&kf_bytes, 3_000);
        assert!(pkts1.len() >= 4);
        let mut saw_keyframe_req = false;
        for (idx, pkt) in pkts1.iter().enumerate() {
            if idx == 2 {
                continue; // simulate loss
            }
            let out = depacketizer.push_packet(&pkt.header, &pkt.payload).unwrap();
            if out.needs_keyframe {
                saw_keyframe_req = true;
            }
            assert!(out.frame.is_none(), "corrupted frame must not be emitted");
        }
        assert!(saw_keyframe_req, "loss must trigger needs_keyframe");

        // Subsequent P-frame while awaiting keyframe is suppressed cleanly.
        let pkts2 = packetizer.packetize_annex_b(&pf_bytes, 6_000);
        let out2 = depacketizer
            .push_packet(&pkts2[0].header, &pkts2[0].payload)
            .unwrap();
        assert!(out2.frame.is_none());

        // Fresh IDR recovers decoding cleanly.
        let pkts3 = packetizer.packetize_annex_b(&kf_bytes, 9_000);
        let mut recovered = None;
        for pkt in &pkts3 {
            let out = depacketizer.push_packet(&pkt.header, &pkt.payload).unwrap();
            if let Some(f) = out.frame {
                recovered = Some(f);
            }
        }
        let rec = recovered.expect("IDR should recover stream");
        assert!(rec.is_keyframe);
        assert_eq!(rec.annex_b, kf_bytes);
    }
}
