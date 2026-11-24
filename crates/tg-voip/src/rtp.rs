//! RFC 3550 RTP audio packetization and VoIP control/keepalive/RTCP-like packets.
//!
//! Reference: RFC 3550 (RTP/RTCP), RFC 7587 (Opus RTP payload), and
//! `tgcalls/v2/MtProtoDtlsTransport.cpp` (`kSctpMagic = 0xdcdcdcdc` demux).

use crate::error::{VoipError, VoipResult};

/// Default RTP payload type for Opus 48 kHz (`Signaling.cpp` / WebRTC default).
pub const OPUS_RTP_PAYLOAD_TYPE: u8 = 111;

/// Default RTP payload type for H.264 video (`90 kHz` clock, RFC 6184).
pub const H264_RTP_PAYLOAD_TYPE: u8 = 102;

/// Standard RTP video clock rate in Hz (`90 kHz`).
pub const H264_CLOCK_RATE: u32 = 90_000;

/// RTCP/control packet type for VoIP keepalive RTT probe (Ping).
pub const RTCP_PT_PING: u8 = 204;

/// RTCP/control packet type for VoIP keepalive RTT response (Pong).
pub const RTCP_PT_PONG: u8 = 205;

/// RTCP Receiver Report payload type (RFC 3550).
pub const RTCP_PT_RR: u8 = 201;

/// RTCP Payload-Specific Feedback payload type (RFC 4585 PLI / RFC 5104 FIR).
pub const RTCP_PT_PSFB: u8 = 206;

/// PSFB FMT value for Picture Loss Indication (RFC 4585 §6.3.1).
pub const RTCP_PSFB_FMT_PLI: u8 = 1;

/// PSFB FMT value for Full Intra Request (RFC 5104 §4.3.1).
pub const RTCP_PSFB_FMT_FIR: u8 = 4;

/// Magic 4-byte SCTP prefix used by `tgcalls/v2/MtProtoDtlsTransport.cpp` (`kSctpMagic`).
pub const SCTP_DEMUX_MAGIC: u32 = 0xDCDC_DCDC;

/// Minimal 12-byte RFC 3550 RTP fixed header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpHeader {
    /// Marker bit (start of talkspurt).
    pub marker: bool,
    /// 7-bit RTP payload type (e.g. `111` for Opus).
    pub payload_type: u8,
    /// 16-bit wrapping RTP sequence number.
    pub sequence_number: u16,
    /// 32-bit RTP timestamp in 48 kHz sample units (advances by 960 per 20 ms frame).
    pub timestamp: u32,
    /// Synchronization source identifier.
    pub ssrc: u32,
}

impl RtpHeader {
    /// Fixed RTP header byte length (with CSRC count = 0).
    pub const LEN: usize = 12;

    /// Serialize header into a 12-byte array.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0] = 0x80; // V=2, P=0, X=0, CC=0
        out[1] = (u8::from(self.marker) << 7) | (self.payload_type & 0x7F);
        out[2..4].copy_from_slice(&self.sequence_number.to_be_bytes());
        out[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        out[8..12].copy_from_slice(&self.ssrc.to_be_bytes());
        out
    }

    /// Parse an RTP header from `bytes`, returning `(header, payload_offset)`.
    pub fn parse(bytes: &[u8]) -> VoipResult<(Self, usize)> {
        if bytes.len() < Self::LEN {
            return Err(VoipError::PacketTooShort(bytes.len()));
        }
        let version = bytes[0] >> 6;
        if version != 2 {
            return Err(VoipError::InvalidFraming(format!(
                "unsupported RTP version {version}"
            )));
        }
        let padding = (bytes[0] & 0x20) != 0;
        let has_extension = (bytes[0] & 0x10) != 0;
        let csrc_count = usize::from(bytes[0] & 0x0F);

        let mut offset = Self::LEN + csrc_count * 4;
        if offset > bytes.len() {
            return Err(VoipError::InvalidFraming("RTP CSRC list truncated".into()));
        }
        if has_extension {
            if offset + 4 > bytes.len() {
                return Err(VoipError::InvalidFraming(
                    "RTP extension header truncated".into(),
                ));
            }
            let ext_words = u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]) as usize;
            offset += 4 + ext_words * 4;
            if offset > bytes.len() {
                return Err(VoipError::InvalidFraming(
                    "RTP extension body truncated".into(),
                ));
            }
        }
        if padding && offset >= bytes.len() {
            return Err(VoipError::InvalidFraming(
                "RTP padding flag on empty payload".into(),
            ));
        }

        let marker = (bytes[1] & 0x80) != 0;
        let payload_type = bytes[1] & 0x7F;
        let sequence_number = u16::from_be_bytes([bytes[2], bytes[3]]);
        let timestamp = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let ssrc = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);

        Ok((
            Self {
                marker,
                payload_type,
                sequence_number,
                timestamp,
                ssrc,
            },
            offset,
        ))
    }
}

/// Decoded VoIP transport packet (carried inside `EncryptedConnection`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoipPacket {
    /// RTP-encapsulated Opus audio packet.
    RtpAudio {
        /// RTP fixed header.
        header: RtpHeader,
        /// Encoded Opus payload bytes.
        payload: Vec<u8>,
    },
    /// RTP-encapsulated video packet (e.g. RFC 6184 H.264).
    RtpVideo {
        /// RTP fixed header (`90 kHz` clock, e.g. PT `102`).
        header: RtpHeader,
        /// Encoded RTP video payload (Single NAL, STAP-A, or FU-A).
        payload: Vec<u8>,
    },
    /// Keepalive & RTT probe sent periodically on active routes.
    Ping {
        /// Sender's monotonic timestamp in milliseconds.
        sent_ms: u64,
        /// Ping identifier.
        nonce: u32,
    },
    /// Echo of a [`VoipPacket::Ping`] for RTT measurement.
    Pong {
        /// Echoed sender timestamp in milliseconds.
        sent_ms: u64,
        /// Echoed ping identifier.
        nonce: u32,
    },
    /// Receiver quality report for adaptive bitrate & signal bars.
    ReceiverReport {
        /// Fraction of packets lost since last report (`0..=255`, RFC 3550 `fraction_lost`).
        fraction_lost: u8,
        /// Inter-arrival jitter in milliseconds.
        jitter_ms: u16,
        /// Estimated round-trip time in milliseconds.
        rtt_ms: u16,
        /// Cumulative packets received.
        packets_received: u32,
    },
    /// RTCP Picture Loss Indication (RFC 4585 §6.3.1, PT=206, FMT=1) requesting a keyframe.
    RtcpPli {
        /// SSRC of the feedback sender.
        sender_ssrc: u32,
        /// SSRC of the remote video stream that must emit an IDR keyframe.
        media_ssrc: u32,
    },
    /// RTCP Full Intra Request (RFC 5104 §4.3.1, PT=206, FMT=4) requesting a decoder refresh.
    RtcpFir {
        /// SSRC of the feedback sender.
        sender_ssrc: u32,
        /// SSRC of the remote video stream that must emit an IDR keyframe.
        media_ssrc: u32,
        /// Monotonically increasing FIR command sequence number.
        seq_nr: u8,
    },
    /// SCTP data-channel payload demuxed via `0xdcdcdcdc` (`MtProtoDtlsTransport.cpp`).
    Sctp(Vec<u8>),
}

impl VoipPacket {
    /// Serialize to wire bytes suitable for `EncryptedConnection`.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        match self {
            VoipPacket::RtpAudio { header, payload } | VoipPacket::RtpVideo { header, payload } => {
                let mut out = Vec::with_capacity(RtpHeader::LEN + payload.len());
                out.extend_from_slice(&header.to_bytes());
                out.extend_from_slice(payload);
                out
            }
            VoipPacket::Ping { sent_ms, nonce } => {
                // RTCP-shaped header: V=2, PT=204, length=3 (16 bytes total).
                let mut out = Vec::with_capacity(16);
                out.push(0x80);
                out.push(RTCP_PT_PING);
                out.extend_from_slice(&3u16.to_be_bytes());
                out.extend_from_slice(&sent_ms.to_be_bytes());
                out.extend_from_slice(&nonce.to_be_bytes());
                out
            }
            VoipPacket::Pong { sent_ms, nonce } => {
                let mut out = Vec::with_capacity(16);
                out.push(0x80);
                out.push(RTCP_PT_PONG);
                out.extend_from_slice(&3u16.to_be_bytes());
                out.extend_from_slice(&sent_ms.to_be_bytes());
                out.extend_from_slice(&nonce.to_be_bytes());
                out
            }
            VoipPacket::ReceiverReport {
                fraction_lost,
                jitter_ms,
                rtt_ms,
                packets_received,
            } => {
                // RTCP-shaped header: V=2, PT=201, length=3 (16 bytes total).
                let mut out = Vec::with_capacity(16);
                out.push(0x80);
                out.push(RTCP_PT_RR);
                out.extend_from_slice(&3u16.to_be_bytes());
                out.push(*fraction_lost);
                out.push(0); // reserved
                out.extend_from_slice(&jitter_ms.to_be_bytes());
                out.extend_from_slice(&rtt_ms.to_be_bytes());
                out.extend_from_slice(&0u16.to_be_bytes());
                out.extend_from_slice(&packets_received.to_be_bytes());
                out
            }
            VoipPacket::RtcpPli {
                sender_ssrc,
                media_ssrc,
            } => {
                // RFC 4585 §6.3.1: V=2, P=0, FMT=1, PT=206, length=2 (12 bytes total).
                let mut out = Vec::with_capacity(12);
                out.push(0x80 | RTCP_PSFB_FMT_PLI);
                out.push(RTCP_PT_PSFB);
                out.extend_from_slice(&2u16.to_be_bytes());
                out.extend_from_slice(&sender_ssrc.to_be_bytes());
                out.extend_from_slice(&media_ssrc.to_be_bytes());
                out
            }
            VoipPacket::RtcpFir {
                sender_ssrc,
                media_ssrc,
                seq_nr,
            } => {
                // RFC 5104 §4.3.1: V=2, P=0, FMT=4, PT=206, length=4 (20 bytes total).
                let mut out = Vec::with_capacity(20);
                out.push(0x80 | RTCP_PSFB_FMT_FIR);
                out.push(RTCP_PT_PSFB);
                out.extend_from_slice(&4u16.to_be_bytes());
                out.extend_from_slice(&sender_ssrc.to_be_bytes());
                out.extend_from_slice(&0u32.to_be_bytes()); // media source SSRC unused in FIR header
                out.extend_from_slice(&media_ssrc.to_be_bytes());
                out.push(*seq_nr);
                out.extend_from_slice(&[0, 0, 0]);
                out
            }
            VoipPacket::Sctp(data) => {
                let mut out = Vec::with_capacity(4 + data.len());
                out.extend_from_slice(&SCTP_DEMUX_MAGIC.to_le_bytes());
                out.extend_from_slice(data);
                out
            }
        }
    }

    /// Parse a decrypted transport payload into a [`VoipPacket`].
    pub fn parse(bytes: &[u8]) -> VoipResult<Self> {
        if bytes.len() < 4 {
            return Err(VoipError::PacketTooShort(bytes.len()));
        }

        // Check `MtProtoDtlsTransport` SCTP demux prefix first.
        let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if magic == SCTP_DEMUX_MAGIC {
            return Ok(VoipPacket::Sctp(bytes[4..].to_vec()));
        }

        // RFC 5761 RTP/RTCP demux: PT 200..=207 in byte 1 indicates RTCP.
        let pt_full = bytes[1];
        if (bytes[0] >> 6) == 2 && (200..=207).contains(&pt_full) {
            match pt_full {
                RTCP_PT_PING | RTCP_PT_PONG => {
                    if bytes.len() < 16 {
                        return Err(VoipError::PacketTooShort(bytes.len()));
                    }
                    let sent_ms = u64::from_be_bytes([
                        bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10],
                        bytes[11],
                    ]);
                    let nonce = u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
                    return Ok(if pt_full == RTCP_PT_PING {
                        VoipPacket::Ping { sent_ms, nonce }
                    } else {
                        VoipPacket::Pong { sent_ms, nonce }
                    });
                }
                RTCP_PT_RR => {
                    if bytes.len() < 16 {
                        return Err(VoipError::PacketTooShort(bytes.len()));
                    }
                    let fraction_lost = bytes[4];
                    let jitter_ms = u16::from_be_bytes([bytes[6], bytes[7]]);
                    let rtt_ms = u16::from_be_bytes([bytes[8], bytes[9]]);
                    let packets_received =
                        u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
                    return Ok(VoipPacket::ReceiverReport {
                        fraction_lost,
                        jitter_ms,
                        rtt_ms,
                        packets_received,
                    });
                }
                RTCP_PT_PSFB => {
                    let fmt = bytes[0] & 0x1F;
                    match fmt {
                        RTCP_PSFB_FMT_PLI => {
                            if bytes.len() < 12 {
                                return Err(VoipError::PacketTooShort(bytes.len()));
                            }
                            let sender_ssrc =
                                u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                            let media_ssrc =
                                u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
                            return Ok(VoipPacket::RtcpPli {
                                sender_ssrc,
                                media_ssrc,
                            });
                        }
                        RTCP_PSFB_FMT_FIR => {
                            if bytes.len() < 20 {
                                return Err(VoipError::PacketTooShort(bytes.len()));
                            }
                            let sender_ssrc =
                                u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                            let media_ssrc =
                                u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
                            let seq_nr = bytes[16];
                            return Ok(VoipPacket::RtcpFir {
                                sender_ssrc,
                                media_ssrc,
                                seq_nr,
                            });
                        }
                        other => {
                            return Err(VoipError::InvalidFraming(format!(
                                "unsupported RTCP PSFB FMT {other}"
                            )));
                        }
                    }
                }
                other => {
                    return Err(VoipError::InvalidFraming(format!(
                        "unsupported RTCP payload type {other}"
                    )));
                }
            }
        }

        let (header, offset) = RtpHeader::parse(bytes)?;
        let mut payload_end = bytes.len();
        if (bytes[0] & 0x20) != 0 {
            let pad_len = usize::from(bytes[bytes.len() - 1]);
            if pad_len == 0 || offset + pad_len > bytes.len() {
                return Err(VoipError::InvalidFraming("invalid RTP padding".into()));
            }
            payload_end -= pad_len;
        }
        let payload = bytes[offset..payload_end].to_vec();
        if header.payload_type == H264_RTP_PAYLOAD_TYPE {
            Ok(VoipPacket::RtpVideo { header, payload })
        } else {
            Ok(VoipPacket::RtpAudio { header, payload })
        }
    }
}

/// Parse an RFC 6464 client-to-mixer `ssrc-audio-level` one-byte RTP header extension, if present.
///
/// Returns `Some((voice_activity, level_linear))` where `level_linear` is normalized to `0.0..=1.0`
/// from RFC 6464 `-dBov` (`0..=127`, where `0` = 0 dBov loudest and `127` = digital silence).
#[must_use]
pub fn parse_rtp_audio_level_ext(rtp_packet: &[u8], ext_id: u8) -> Option<(bool, f32)> {
    if rtp_packet.len() < RtpHeader::LEN || (rtp_packet[0] & 0x10) == 0 {
        return None;
    }
    let cc = usize::from(rtp_packet[0] & 0x0F);
    let ext_start = RtpHeader::LEN + cc * 4;
    if ext_start + 4 > rtp_packet.len() {
        return None;
    }
    // RFC 8285 one-byte header extension profile = 0xBEDE.
    if rtp_packet[ext_start] != 0xBE || rtp_packet[ext_start + 1] != 0xDE {
        return None;
    }
    let ext_words =
        u16::from_be_bytes([rtp_packet[ext_start + 2], rtp_packet[ext_start + 3]]) as usize;
    let body_start = ext_start + 4;
    let body_end = body_start + ext_words * 4;
    if body_end > rtp_packet.len() {
        return None;
    }
    let mut i = body_start;
    while i < body_end {
        let b = rtp_packet[i];
        if b == 0 {
            i += 1;
            continue;
        }
        let id = b >> 4;
        if id == 15 {
            break;
        }
        let len = usize::from(b & 0x0F) + 1;
        i += 1;
        if i + len > body_end {
            break;
        }
        if id == ext_id && len >= 1 {
            let val = rtp_packet[i];
            let voice = (val & 0x80) != 0;
            let dbov = val & 0x7F;
            let linear = if dbov >= 127 {
                0.0
            } else {
                10.0f32.powf(-(f32::from(dbov)) / 20.0).clamp(0.0, 1.0)
            };
            return Some((voice, linear));
        }
        i += len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voip_packet_variants_roundtrip() {
        let cases = vec![
            VoipPacket::RtpAudio {
                header: RtpHeader {
                    marker: true,
                    payload_type: OPUS_RTP_PAYLOAD_TYPE,
                    sequence_number: 42,
                    timestamp: 960 * 42,
                    ssrc: 0xAABB_CCDD,
                },
                payload: vec![0xFC, 0x01, 0x02, 0x03],
            },
            VoipPacket::Ping {
                sent_ms: 123_456_789,
                nonce: 7,
            },
            VoipPacket::Pong {
                sent_ms: 123_456_789,
                nonce: 7,
            },
            VoipPacket::ReceiverReport {
                fraction_lost: 25,
                jitter_ms: 14,
                rtt_ms: 88,
                packets_received: 500,
            },
            VoipPacket::RtpVideo {
                header: RtpHeader {
                    marker: true,
                    payload_type: H264_RTP_PAYLOAD_TYPE,
                    sequence_number: 100,
                    timestamp: 90_000,
                    ssrc: 0x5566_7788,
                },
                payload: vec![0x65, 0x88, 0x84, 0x21],
            },
            VoipPacket::RtcpPli {
                sender_ssrc: 0x1111_2222,
                media_ssrc: 0x5566_7788,
            },
            VoipPacket::RtcpFir {
                sender_ssrc: 0x1111_2222,
                media_ssrc: 0x5566_7788,
                seq_nr: 9,
            },
            VoipPacket::Sctp(b"sctp-chunk".to_vec()),
        ];

        for pkt in cases {
            let bytes = pkt.serialize();
            let parsed = VoipPacket::parse(&bytes).unwrap();
            assert_eq!(parsed, pkt);
        }
    }

    #[test]
    fn parse_rfc6464_audio_level_extension() {
        // RTP header with X=1, ext profile 0xBEDE, 1 word: id=1, len=0 (1 byte), val=0x80|10 (-10 dBov, voice=1)
        let mut pkt = RtpHeader {
            marker: false,
            payload_type: OPUS_RTP_PAYLOAD_TYPE,
            sequence_number: 1,
            timestamp: 960,
            ssrc: 42,
        }
        .to_bytes()
        .to_vec();
        pkt[0] |= 0x10; // X bit
        pkt.extend_from_slice(&[0xBE, 0xDE, 0x00, 0x01, 0x10, 0x8A, 0x00, 0x00]);
        let (voice, linear) = parse_rtp_audio_level_ext(&pkt, 1).unwrap();
        assert!(voice);
        assert!((linear - 0.3162).abs() < 0.01, "got {linear}");
    }
}
