//! MTProto TCP transport framing implementations.
//!
//! Port of `TcpConnection::Protocol` (`Version0`, `Version1`, `VersionD`) from
//! `mtproto/connection_tcp.cpp`, plus the standard MTProto `Intermediate` and `Full`
//! TCP transports (<https://core.telegram.org/mtproto/mtproto-transports>).

use rand::RngCore;

use crate::error::{Error, TransportErrorCode};

/// Maximum packet size in bytes, matching `kPacketSizeMax = 0x01000000 * sizeof(mtpPrime)`
/// in `mtproto/connection_tcp.cpp`.
pub const MAX_PACKET_SIZE: usize = 0x0100_0000 * 4;

/// 4-byte protocol tag for the Abridged transport (`0xEFEFEFEF`).
pub const TAG_ABRIDGED: [u8; 4] = [0xEF, 0xEF, 0xEF, 0xEF];

/// 4-byte protocol tag for the Intermediate transport (`0xEEEEEEEE`).
pub const TAG_INTERMEDIATE: [u8; 4] = [0xEE, 0xEE, 0xEE, 0xEE];

/// 4-byte protocol tag for the Padded Intermediate transport (`0xDDDDDDDD`).
pub const TAG_PADDED_INTERMEDIATE: [u8; 4] = [0xDD, 0xDD, 0xDD, 0xDD];

/// Single-byte init prefix for plain (unobfuscated) Abridged TCP transport.
pub const PLAIN_INIT_ABRIDGED: &[u8] = &[0xEF];

/// Kind of frame decoded by [`Transport::unpack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// Regular MTProto payload located at `input[data_start..data_end]`.
    Payload,
    /// Server quick-ack token (31-bit value with MSB cleared).
    QuickAck(u32),
    /// 4-byte zero NOP packet.
    Nop,
}

/// Byte offsets of a single decoded frame returned by [`Transport::unpack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnpackedOffset {
    /// Start index of the payload slice inside the input buffer.
    pub data_start: usize,
    /// End index (exclusive) of the payload slice inside the input buffer.
    pub data_end: usize,
    /// Total number of wire bytes consumed from the start of the input buffer.
    pub next_offset: usize,
    /// Semantic kind of the unpacked frame.
    pub kind: FrameKind,
}

impl UnpackedOffset {
    /// Extract the payload byte slice from `input` using [`Self::data_start`] and [`Self::data_end`].
    #[must_use]
    pub fn slice<'a>(&self, input: &'a [u8]) -> &'a [u8] {
        &input[self.data_start..self.data_end]
    }
}

/// MTProto packet framing trait (see `docs/ARCHITECTURE.md`).
pub trait Transport {
    /// Pack a single MTProto payload into `out`, appending the transport header and any
    /// trailing padding or checksum.
    fn pack(&mut self, input: &[u8], out: &mut Vec<u8>);

    /// Attempt to unpack one frame from the beginning of `input`.
    ///
    /// Returns [`Error::MissingBytes`] if `input` does not yet contain a complete frame,
    /// or [`Error::ServerError`] when a 4-byte negative transport error code (such as `-404`)
    /// is received.
    fn unpack(&mut self, input: &[u8]) -> Result<UnpackedOffset, Error>;

    /// 4-byte protocol tag written at bytes `56..60` of an obfuscated2 header, if supported.
    fn protocol_tag(&self) -> Option<[u8; 4]> {
        None
    }

    /// Plain TCP init header sent once before the first frame when obfuscation is disabled.
    fn plain_init_bytes(&self) -> Option<&'static [u8]> {
        None
    }

    /// Enable or disable requesting quick-ack on subsequent packed frames (if supported).
    fn set_quick_ack(&mut self, _enabled: bool) {}

    /// Reset any per-connection framing state (such as sequence numbers).
    fn reset(&mut self) {}
}

/// Selects which MTProto TCP framing protocol to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TransportKind {
    /// Abridged transport (`0xEF` / `0xEFEFEFEF`).
    Abridged,
    /// Intermediate transport (`0xEEEEEEEE`).
    #[default]
    Intermediate,
    /// Padded intermediate transport (`0xDDDDDDDD`), appending 0..15 random bytes per packet.
    PaddedIntermediate,
    /// Full transport (length + seq_no + payload + CRC32).
    Full,
}

impl TransportKind {
    /// Construct a concrete [`AnyTransport`] instance for this transport kind.
    #[must_use]
    pub fn build(self) -> AnyTransport {
        match self {
            Self::Abridged => AnyTransport::Abridged(Abridged::new()),
            Self::Intermediate => AnyTransport::Intermediate(Intermediate::new()),
            Self::PaddedIntermediate => AnyTransport::PaddedIntermediate(PaddedIntermediate::new()),
            Self::Full => AnyTransport::Full(Full::new()),
        }
    }

    /// Return the 4-byte obfuscated2 protocol tag for this transport, if any.
    #[must_use]
    pub const fn protocol_tag(self) -> Option<[u8; 4]> {
        match self {
            Self::Abridged => Some(TAG_ABRIDGED),
            Self::Intermediate => Some(TAG_INTERMEDIATE),
            Self::PaddedIntermediate => Some(TAG_PADDED_INTERMEDIATE),
            Self::Full => None,
        }
    }
}

/// Abridged MTProto transport (`0xEF` / `0xEFEFEFEF`).
///
/// Port of `TcpConnection::Protocol::Version0` in `mtproto/connection_tcp.cpp`.
#[derive(Debug, Clone, Default)]
pub struct Abridged {
    quick_ack: bool,
}

impl Abridged {
    /// Create a new Abridged transport instance.
    #[must_use]
    pub const fn new() -> Self {
        Self { quick_ack: false }
    }

    /// Create a new Abridged transport with quick-ack bit setting configured.
    #[must_use]
    pub const fn with_quick_ack(quick_ack: bool) -> Self {
        Self { quick_ack }
    }
}

impl Transport for Abridged {
    fn pack(&mut self, input: &[u8], out: &mut Vec<u8>) {
        debug_assert!(!input.is_empty() && input.len().is_multiple_of(4));
        let words = input.len() / 4;
        debug_assert!(words > 0 && words < 0x0100_0000);
        let qa_bit = if self.quick_ack { 0x80u8 } else { 0x00u8 };
        if words < 0x7F {
            out.reserve(1 + input.len());
            out.push((words as u8) | qa_bit);
        } else {
            out.reserve(4 + input.len());
            out.push(0x7F | qa_bit);
            out.push((words & 0xFF) as u8);
            out.push(((words >> 8) & 0xFF) as u8);
            out.push(((words >> 16) & 0xFF) as u8);
        }
        out.extend_from_slice(input);
    }

    fn unpack(&mut self, input: &[u8]) -> Result<UnpackedOffset, Error> {
        let Some(&first) = input.first() else {
            return Err(Error::MissingBytes);
        };

        // Quick-ack token: 4 bytes with MSB of the first byte set (big-endian 31-bit token).
        if (first & 0x80) != 0 {
            if input.len() < 4 {
                return Err(Error::MissingBytes);
            }
            let raw = u32::from_be_bytes([input[0], input[1], input[2], input[3]]);
            return Ok(UnpackedOffset {
                data_start: 0,
                data_end: 4,
                next_offset: 4,
                kind: FrameKind::QuickAck(raw & 0x7FFF_FFFF),
            });
        }

        let (header_len, words) = if first == 0x7F {
            if input.len() < 4 {
                return Err(Error::MissingBytes);
            }
            let w = (input[1] as usize) | ((input[2] as usize) << 8) | ((input[3] as usize) << 16);
            if w < 0x7F {
                return Err(Error::InvalidPacketLength(w * 4));
            }
            (4usize, w)
        } else if first > 0 {
            (1usize, first as usize)
        } else {
            return Err(Error::InvalidPacketLength(0));
        };

        let byte_len = words
            .checked_mul(4)
            .ok_or(Error::InvalidPacketLength(usize::MAX))?;
        let total = header_len
            .checked_add(byte_len)
            .ok_or(Error::InvalidPacketLength(byte_len))?;
        if total >= MAX_PACKET_SIZE {
            return Err(Error::InvalidPacketLength(byte_len));
        }
        if input.len() < total {
            return Err(Error::MissingBytes);
        }

        if byte_len == 4 {
            let code = i32::from_le_bytes([
                input[header_len],
                input[header_len + 1],
                input[header_len + 2],
                input[header_len + 3],
            ]);
            if code < 0 {
                return Err(Error::ServerError(TransportErrorCode::from_code(code)));
            }
            if code == 0 {
                return Ok(UnpackedOffset {
                    data_start: header_len,
                    data_end: total,
                    next_offset: total,
                    kind: FrameKind::Nop,
                });
            }
        }

        Ok(UnpackedOffset {
            data_start: header_len,
            data_end: total,
            next_offset: total,
            kind: FrameKind::Payload,
        })
    }

    fn protocol_tag(&self) -> Option<[u8; 4]> {
        Some(TAG_ABRIDGED)
    }

    fn plain_init_bytes(&self) -> Option<&'static [u8]> {
        Some(PLAIN_INIT_ABRIDGED)
    }

    fn set_quick_ack(&mut self, enabled: bool) {
        self.quick_ack = enabled;
    }
}

/// Intermediate MTProto transport (`0xEEEEEEEE`).
///
/// Frames each packet with a 4-byte little-endian byte length prefix (bit 31 set for quick-ack).
#[derive(Debug, Clone, Default)]
pub struct Intermediate {
    quick_ack: bool,
}

impl Intermediate {
    /// Create a new Intermediate transport instance.
    #[must_use]
    pub const fn new() -> Self {
        Self { quick_ack: false }
    }

    /// Create a new Intermediate transport with quick-ack bit setting configured.
    #[must_use]
    pub const fn with_quick_ack(quick_ack: bool) -> Self {
        Self { quick_ack }
    }
}

impl Transport for Intermediate {
    fn pack(&mut self, input: &[u8], out: &mut Vec<u8>) {
        debug_assert!(!input.is_empty() && input.len().is_multiple_of(4));
        debug_assert!(input.len() < MAX_PACKET_SIZE);
        let len_word = (input.len() as u32) | if self.quick_ack { 0x8000_0000 } else { 0 };
        out.reserve(4 + input.len());
        out.extend_from_slice(&len_word.to_le_bytes());
        out.extend_from_slice(input);
    }

    fn unpack(&mut self, input: &[u8]) -> Result<UnpackedOffset, Error> {
        if input.len() < 4 {
            return Err(Error::MissingBytes);
        }
        let raw_len = u32::from_le_bytes([input[0], input[1], input[2], input[3]]);
        if (raw_len & 0x8000_0000) != 0 {
            return Ok(UnpackedOffset {
                data_start: 0,
                data_end: 4,
                next_offset: 4,
                kind: FrameKind::QuickAck(raw_len & 0x7FFF_FFFF),
            });
        }

        let byte_len = raw_len as usize;
        if byte_len < 4 || !byte_len.is_multiple_of(4) || byte_len + 4 >= MAX_PACKET_SIZE {
            return Err(Error::InvalidPacketLength(byte_len));
        }
        let total = 4 + byte_len;
        if input.len() < total {
            return Err(Error::MissingBytes);
        }

        if byte_len == 4 {
            let code = i32::from_le_bytes([input[4], input[5], input[6], input[7]]);
            if code < 0 {
                return Err(Error::ServerError(TransportErrorCode::from_code(code)));
            }
            if code == 0 {
                return Ok(UnpackedOffset {
                    data_start: 4,
                    data_end: 8,
                    next_offset: total,
                    kind: FrameKind::Nop,
                });
            }
        }

        Ok(UnpackedOffset {
            data_start: 4,
            data_end: total,
            next_offset: total,
            kind: FrameKind::Payload,
        })
    }

    fn protocol_tag(&self) -> Option<[u8; 4]> {
        Some(TAG_INTERMEDIATE)
    }

    fn plain_init_bytes(&self) -> Option<&'static [u8]> {
        Some(&TAG_INTERMEDIATE)
    }

    fn set_quick_ack(&mut self, enabled: bool) {
        self.quick_ack = enabled;
    }
}

/// Padded Intermediate MTProto transport (`0xDDDDDDDD`).
///
/// Port of `TcpConnection::Protocol::VersionD` in `mtproto/connection_tcp.cpp`.
/// Each outgoing packet gets `0..=15` random padding bytes (`rand & 0x0F`).
/// On receive, partial-word padding (`len % 4`) is trimmed just like
/// `TcpConnection::parsePacket`; remaining word-aligned padding (`0..=12` bytes) is
/// ignored by MTProto unencrypted/encrypted framing (see [`Self::strip_mtproto_padding`]).
#[derive(Debug, Clone, Default)]
pub struct PaddedIntermediate {
    quick_ack: bool,
}

impl PaddedIntermediate {
    /// Create a new Padded Intermediate transport instance.
    #[must_use]
    pub const fn new() -> Self {
        Self { quick_ack: false }
    }

    /// Create a new Padded Intermediate transport with quick-ack bit setting configured.
    #[must_use]
    pub const fn with_quick_ack(quick_ack: bool) -> Self {
        Self { quick_ack }
    }

    /// Pack a packet using an explicit caller-supplied padding slice (must be 0..=15 bytes).
    ///
    /// Useful for deterministic known-answer tests.
    pub fn pack_with_padding(&self, input: &[u8], padding: &[u8], out: &mut Vec<u8>) {
        debug_assert!(!input.is_empty() && input.len().is_multiple_of(4));
        debug_assert!(padding.len() <= 15);
        let total_len = input.len() + padding.len();
        let len_word = (total_len as u32) | if self.quick_ack { 0x8000_0000 } else { 0 };
        out.reserve(4 + total_len);
        out.extend_from_slice(&len_word.to_le_bytes());
        out.extend_from_slice(input);
        out.extend_from_slice(padding);
    }

    /// Strip trailing padded-intermediate random padding from a complete MTProto packet,
    /// matching tdesktop's `parseNotSecureResponse` (`connection_abstract.cpp`) and
    /// `handleReceived` (`session_private.cpp`).
    ///
    /// - Unencrypted packet (`auth_key_id == 0`, length >= 20): uses `message_length` at `[16..20]`.
    /// - Encrypted packet (`auth_key_id != 0`, length >= 40): trims ciphertext down to a 16-byte
    ///   multiple (`24 + ((len - 24) & !15)`).
    /// - Shorter or malformed packets are returned unchanged (trimmed to a 4-byte multiple).
    #[must_use]
    pub fn strip_mtproto_padding(packet: &[u8]) -> &[u8] {
        let aligned = packet.len() & !3;
        let packet = &packet[..aligned];
        if packet.len() < 20 {
            return packet;
        }
        let auth_key_id = u64::from_le_bytes([
            packet[0], packet[1], packet[2], packet[3], packet[4], packet[5], packet[6], packet[7],
        ]);
        if auth_key_id == 0 {
            let msg_len =
                u32::from_le_bytes([packet[16], packet[17], packet[18], packet[19]]) as usize;
            if msg_len >= 4
                && msg_len.is_multiple_of(4)
                && let Some(exact) = 20usize.checked_add(msg_len)
                && exact <= packet.len()
            {
                return &packet[..exact];
            }
            packet
        } else if packet.len() >= 40 {
            let encrypted_len = (packet.len() - 24) & !15;
            &packet[..24 + encrypted_len]
        } else {
            packet
        }
    }
}

impl Transport for PaddedIntermediate {
    fn pack(&mut self, input: &[u8], out: &mut Vec<u8>) {
        let mut rng = rand::thread_rng();
        let pad_len = (rng.next_u32() & 0x0F) as usize;
        let mut pad = [0u8; 15];
        if pad_len > 0 {
            rng.fill_bytes(&mut pad[..pad_len]);
        }
        self.pack_with_padding(input, &pad[..pad_len], out);
    }

    fn unpack(&mut self, input: &[u8]) -> Result<UnpackedOffset, Error> {
        if input.len() < 4 {
            return Err(Error::MissingBytes);
        }
        let raw_len = u32::from_le_bytes([input[0], input[1], input[2], input[3]]);
        if (raw_len & 0x8000_0000) != 0 {
            return Ok(UnpackedOffset {
                data_start: 0,
                data_end: 4,
                next_offset: 4,
                kind: FrameKind::QuickAck(raw_len & 0x7FFF_FFFF),
            });
        }

        let byte_len = raw_len as usize;
        // Match VersionD::readPacketLength: value = raw_len + 4; value >= 8 && value < kPacketSizeMax.
        let total = byte_len
            .checked_add(4)
            .ok_or(Error::InvalidPacketLength(byte_len))?;
        if !(8..MAX_PACKET_SIZE).contains(&total) {
            return Err(Error::InvalidPacketLength(byte_len));
        }
        if input.len() < total {
            return Err(Error::MissingBytes);
        }

        // Match TcpConnection::parsePacket: count = packet.size() / sizeof(mtpPrime).
        let aligned_len = byte_len & !3;
        if aligned_len < 12 {
            let code = i32::from_le_bytes([input[4], input[5], input[6], input[7]]);
            if code < 0 {
                return Err(Error::ServerError(TransportErrorCode::from_code(code)));
            }
            if code == 0 {
                return Ok(UnpackedOffset {
                    data_start: 4,
                    data_end: 8,
                    next_offset: total,
                    kind: FrameKind::Nop,
                });
            }
        }

        Ok(UnpackedOffset {
            data_start: 4,
            data_end: 4 + aligned_len,
            next_offset: total,
            kind: FrameKind::Payload,
        })
    }

    fn protocol_tag(&self) -> Option<[u8; 4]> {
        Some(TAG_PADDED_INTERMEDIATE)
    }

    fn plain_init_bytes(&self) -> Option<&'static [u8]> {
        Some(&TAG_PADDED_INTERMEDIATE)
    }

    fn set_quick_ack(&mut self, enabled: bool) {
        self.quick_ack = enabled;
    }
}

/// Full MTProto TCP transport (`length + seq_no + payload + crc32`).
///
/// See <https://core.telegram.org/mtproto/mtproto-transports#full>.
#[derive(Debug, Clone, Default)]
pub struct Full {
    send_seq: u32,
    recv_seq: u32,
}

impl Full {
    /// Create a new Full transport with send and receive sequence counters starting at 0.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            send_seq: 0,
            recv_seq: 0,
        }
    }

    /// Current outgoing sequence number (for inspection/testing).
    #[must_use]
    pub const fn send_seq(&self) -> u32 {
        self.send_seq
    }

    /// Current expected incoming sequence number (for inspection/testing).
    #[must_use]
    pub const fn recv_seq(&self) -> u32 {
        self.recv_seq
    }
}

impl Transport for Full {
    fn pack(&mut self, input: &[u8], out: &mut Vec<u8>) {
        debug_assert!(!input.is_empty() && input.len().is_multiple_of(4));
        let total_len = (input.len() + 12) as u32;
        let seq = self.send_seq;
        self.send_seq = self.send_seq.wrapping_add(1);

        let len_bytes = total_len.to_le_bytes();
        let seq_bytes = seq.to_le_bytes();

        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&len_bytes);
        hasher.update(&seq_bytes);
        hasher.update(input);
        let crc = hasher.finalize();

        out.reserve(total_len as usize);
        out.extend_from_slice(&len_bytes);
        out.extend_from_slice(&seq_bytes);
        out.extend_from_slice(input);
        out.extend_from_slice(&crc.to_le_bytes());
    }

    fn unpack(&mut self, input: &[u8]) -> Result<UnpackedOffset, Error> {
        if input.len() < 4 {
            return Err(Error::MissingBytes);
        }
        let raw_i32 = i32::from_le_bytes([input[0], input[1], input[2], input[3]]);
        if raw_i32 < 0 {
            return Err(Error::ServerError(TransportErrorCode::from_code(raw_i32)));
        }

        let total_len = raw_i32 as usize;
        if total_len < 12 || !total_len.is_multiple_of(4) || total_len >= MAX_PACKET_SIZE {
            return Err(Error::InvalidPacketLength(total_len));
        }
        if input.len() < total_len {
            return Err(Error::MissingBytes);
        }

        let expected_crc = u32::from_le_bytes([
            input[total_len - 4],
            input[total_len - 3],
            input[total_len - 2],
            input[total_len - 1],
        ]);
        let actual_crc = crc32fast::hash(&input[..total_len - 4]);
        if expected_crc != actual_crc {
            return Err(Error::CrcMismatch {
                expected: expected_crc,
                actual: actual_crc,
            });
        }

        let seq = u32::from_le_bytes([input[4], input[5], input[6], input[7]]);
        if total_len == 16 {
            let code = i32::from_le_bytes([input[8], input[9], input[10], input[11]]);
            if code < 0 {
                return Err(Error::ServerError(TransportErrorCode::from_code(code)));
            }
            if seq != self.recv_seq {
                return Err(Error::SequenceMismatch {
                    expected: self.recv_seq,
                    actual: seq,
                });
            }
            self.recv_seq = self.recv_seq.wrapping_add(1);
            if code == 0 {
                return Ok(UnpackedOffset {
                    data_start: 8,
                    data_end: 12,
                    next_offset: total_len,
                    kind: FrameKind::Nop,
                });
            }
            return Ok(UnpackedOffset {
                data_start: 8,
                data_end: 12,
                next_offset: total_len,
                kind: FrameKind::Payload,
            });
        }

        if seq != self.recv_seq {
            return Err(Error::SequenceMismatch {
                expected: self.recv_seq,
                actual: seq,
            });
        }
        self.recv_seq = self.recv_seq.wrapping_add(1);

        Ok(UnpackedOffset {
            data_start: 8,
            data_end: total_len - 4,
            next_offset: total_len,
            kind: FrameKind::Payload,
        })
    }

    fn reset(&mut self) {
        self.send_seq = 0;
        self.recv_seq = 0;
    }
}

/// Enum wrapper over all concrete [`Transport`] implementations.
#[derive(Debug, Clone)]
pub enum AnyTransport {
    /// Abridged (`0xEF` / `0xEFEFEFEF`).
    Abridged(Abridged),
    /// Intermediate (`0xEEEEEEEE`).
    Intermediate(Intermediate),
    /// Padded Intermediate (`0xDDDDDDDD`).
    PaddedIntermediate(PaddedIntermediate),
    /// Full (length + seq_no + payload + CRC32).
    Full(Full),
}

impl AnyTransport {
    /// Return the [`TransportKind`] of this instance.
    #[must_use]
    pub const fn kind(&self) -> TransportKind {
        match self {
            Self::Abridged(_) => TransportKind::Abridged,
            Self::Intermediate(_) => TransportKind::Intermediate,
            Self::PaddedIntermediate(_) => TransportKind::PaddedIntermediate,
            Self::Full(_) => TransportKind::Full,
        }
    }
}

impl Transport for AnyTransport {
    fn pack(&mut self, input: &[u8], out: &mut Vec<u8>) {
        match self {
            Self::Abridged(t) => t.pack(input, out),
            Self::Intermediate(t) => t.pack(input, out),
            Self::PaddedIntermediate(t) => t.pack(input, out),
            Self::Full(t) => t.pack(input, out),
        }
    }

    fn unpack(&mut self, input: &[u8]) -> Result<UnpackedOffset, Error> {
        match self {
            Self::Abridged(t) => t.unpack(input),
            Self::Intermediate(t) => t.unpack(input),
            Self::PaddedIntermediate(t) => t.unpack(input),
            Self::Full(t) => t.unpack(input),
        }
    }

    fn protocol_tag(&self) -> Option<[u8; 4]> {
        match self {
            Self::Abridged(t) => t.protocol_tag(),
            Self::Intermediate(t) => t.protocol_tag(),
            Self::PaddedIntermediate(t) => t.protocol_tag(),
            Self::Full(t) => t.protocol_tag(),
        }
    }

    fn plain_init_bytes(&self) -> Option<&'static [u8]> {
        match self {
            Self::Abridged(t) => t.plain_init_bytes(),
            Self::Intermediate(t) => t.plain_init_bytes(),
            Self::PaddedIntermediate(t) => t.plain_init_bytes(),
            Self::Full(t) => t.plain_init_bytes(),
        }
    }

    fn set_quick_ack(&mut self, enabled: bool) {
        match self {
            Self::Abridged(t) => t.set_quick_ack(enabled),
            Self::Intermediate(t) => t.set_quick_ack(enabled),
            Self::PaddedIntermediate(t) => t.set_quick_ack(enabled),
            Self::Full(t) => t.set_quick_ack(enabled),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Abridged(t) => t.reset(),
            Self::Intermediate(t) => t.reset(),
            Self::PaddedIntermediate(t) => t.reset(),
            Self::Full(t) => t.reset(),
        }
    }
}
