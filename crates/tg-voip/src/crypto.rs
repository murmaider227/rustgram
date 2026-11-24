//! MTProto 2.0 AES-256-CTR call packet encryption, sequence framing, replay window,
//! and reliable ACK/resend queue.
//!
//! Port of `TelegramMessenger/tgcalls` (`development` branch):
//! - `tgcalls/CryptoHelper.h`, `tgcalls/CryptoHelper.cpp`
//! - `tgcalls/EncryptedConnection.h`, `tgcalls/EncryptedConnection.cpp`
//! - `tgcalls/Message.h`, `tgcalls/Message.cpp`
//! - `tgcalls/v2/SignalingEncryption.cpp`
//!
//! # Verified Wire Format (from `TelegramMessenger/tgcalls`)
//! Unlike MTProto TCP sessions (which use AES-256-IGE with 32-byte IV), `tgcalls`
//! uses **AES-256-CTR** with a 16-byte IV (`PrepareAesKeyIv` + `AesProcessCtr`):
//! - Directional offset `x`:
//!   - Sending: `(if is_outgoing { 0 } else { 8 }) + (if Signaling { 128 } else { 0 })`
//!   - Receiving: `(if is_outgoing { 8 } else { 0 }) + (if Signaling { 128 } else { 0 })`
//! - `msg_key_large = SHA256(auth_key[88 + x .. 88 + x + 32] || plaintext)`
//! - `msg_key = msg_key_large[8 .. 24]` (16 bytes)
//! - `sha256_a = SHA256(msg_key || auth_key[x .. x + 36])`
//! - `sha256_b = SHA256(auth_key[40 + x .. 40 + x + 36] || msg_key)`
//! - `aes_key (32 bytes) = sha256_a[0..8] || sha256_b[8..24] || sha256_a[24..32]`
//! - `aes_iv  (16 bytes) = sha256_b[0..4] || sha256_a[8..16] || sha256_b[24..28]`
//! - Wire packet: `msg_key (16 bytes) || AES-256-CTR(plaintext, aes_key, aes_iv)`

use crate::error::{VoipError, VoipResult};

pub use tg_calls::signaling::{
    AUTH_KEY_SIZE, ChannelType, KEEP_INCOMING_COUNTERS_COUNT, MAX_ALLOWED_COUNTER,
    MAX_INCOMING_PACKET_SIZE, MAX_OUTER_PACKET_SIZE, MAX_SIGNALING_PACKET_SIZE,
    MESSAGE_REQUIRES_ACK_SEQ_BIT, MIN_ENCRYPTED_PACKET_SIZE, MSG_KEY_SIZE,
    SINGLE_MESSAGE_PACKET_SEQ_BIT, aes_256_ctr_apply, compute_msg_key, counter_from_seq,
    prepare_aes_key_iv,
};

/// Maximum queued unacked reliable messages (`kNotAckedMessagesLimit`).
const NOT_ACKED_MESSAGES_LIMIT: usize = 64 * 1024;

/// Internal message type ID for ACK entries (`kAckId = uint8_t(-1)`).
pub const MSG_ID_ACK: u8 = 0xFF;

/// Internal message type ID for empty carrier messages (`kEmptyId = uint8_t(-2)`).
pub const MSG_ID_EMPTY: u8 = 0xFE;

/// Internal message type ID for raw transport messages (`kCustomId = 127`, used by
/// `MtProtoDtlsTransport` via `prepareForSendingRawMessage`).
pub const MSG_ID_CUSTOM_RAW: u8 = 127;

/// Legacy `Message` type IDs from `tgcalls/Message.h`.
pub mod legacy_msg_id {
    /// `CandidatesListMessage::kId`
    pub const CANDIDATES_LIST: u8 = 1;
    /// `VideoFormatsMessage::kId`
    pub const VIDEO_FORMATS: u8 = 2;
    /// `RequestVideoMessage::kId`
    pub const REQUEST_VIDEO: u8 = 3;
    /// `RemoteMediaStateMessage::kId`
    pub const REMOTE_MEDIA_STATE: u8 = 4;
    /// `AudioDataMessage::kId`
    pub const AUDIO_DATA: u8 = 5;
    /// `VideoDataMessage::kId`
    pub const VIDEO_DATA: u8 = 6;
    /// `UnstructuredDataMessage::kId`
    pub const UNSTRUCTURED_DATA: u8 = 7;
    /// `VideoParametersMessage::kId`
    pub const VIDEO_PARAMETERS: u8 = 8;
    /// `RemoteBatteryLevelIsLowMessage::kId`
    pub const REMOTE_BATTERY_LOW: u8 = 9;
    /// `RemoteNetworkStatusMessage::kId`
    pub const REMOTE_NETWORK_STATUS: u8 = 10;
}

/// Output of encrypting a packet with [`EncryptedConnection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedPacket {
    /// Wire bytes: `msg_key (16) || AES-256-CTR(plaintext)`.
    pub bytes: Vec<u8>,
    /// 30-bit monotonic counter assigned to this packet.
    pub counter: u32,
}

/// Decrypted raw transport message (`EncryptedConnection::DecryptedRawMessage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecryptedRawMessage {
    /// Decrypted payload bytes.
    pub payload: Vec<u8>,
    /// 30-bit sequence counter of the message.
    pub counter: u32,
}

/// Typed transport message for `tgcalls/Message.h` (legacy/direct protocol).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportMessage {
    /// Audio payload (`AudioDataMessage`, id = 5, unreliable).
    AudioData(Vec<u8>),
    /// Video payload (`VideoDataMessage`, id = 6, unreliable).
    VideoData(Vec<u8>),
    /// Unstructured data (`UnstructuredDataMessage`, id = 7, reliable).
    UnstructuredData(Vec<u8>),
    /// Remote audio/video mute state (`RemoteMediaStateMessage`, id = 4, reliable).
    RemoteMediaState {
        /// `false` = muted (0), `true` = active (1).
        audio_active: bool,
        /// `0` = inactive, `1` = paused, `2` = active.
        video_state: u8,
    },
    /// Video aspect ratio (`VideoParametersMessage`, id = 8, reliable).
    VideoParameters {
        /// Aspect ratio multiplied by 1000 (`uint32_t`).
        aspect_ratio: u32,
    },
    /// Remote low-battery notification (`RemoteBatteryLevelIsLowMessage`, id = 9, reliable).
    RemoteBatteryLow(bool),
    /// Remote network status (`RemoteNetworkStatusMessage`, id = 10, reliable).
    RemoteNetworkStatus {
        /// Whether the peer is on a low-cost network (Wi-Fi/Ethernet).
        is_low_cost: bool,
        /// Whether the peer requested low data usage.
        is_low_data_requested: bool,
    },
}

impl TransportMessage {
    /// Wire message type ID (`kId`).
    #[must_use]
    pub const fn id(&self) -> u8 {
        match self {
            TransportMessage::AudioData(_) => legacy_msg_id::AUDIO_DATA,
            TransportMessage::VideoData(_) => legacy_msg_id::VIDEO_DATA,
            TransportMessage::UnstructuredData(_) => legacy_msg_id::UNSTRUCTURED_DATA,
            TransportMessage::RemoteMediaState { .. } => legacy_msg_id::REMOTE_MEDIA_STATE,
            TransportMessage::VideoParameters { .. } => legacy_msg_id::VIDEO_PARAMETERS,
            TransportMessage::RemoteBatteryLow(_) => legacy_msg_id::REMOTE_BATTERY_LOW,
            TransportMessage::RemoteNetworkStatus { .. } => legacy_msg_id::REMOTE_NETWORK_STATUS,
        }
    }

    /// Whether `tgcalls` marks this message `kRequiresAck = true`.
    #[must_use]
    pub const fn requires_ack(&self) -> bool {
        !matches!(
            self,
            TransportMessage::AudioData(_) | TransportMessage::VideoData(_)
        )
    }

    /// Serialize message with its 4-byte sequence prefix (`SerializeMessageWithSeq`).
    fn serialize_with_seq(&self, seq: u32, single_message_packet: bool) -> VoipResult<Vec<u8>> {
        let mut out = Vec::new();
        out.extend_from_slice(&seq.to_be_bytes());
        out.push(self.id());
        match self {
            TransportMessage::AudioData(data)
            | TransportMessage::VideoData(data)
            | TransportMessage::UnstructuredData(data) => {
                if !single_message_packet {
                    if data.len() > u16::MAX as usize {
                        return Err(VoipError::PacketTooLarge(data.len()));
                    }
                    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
                }
                out.extend_from_slice(data);
            }
            TransportMessage::RemoteMediaState {
                audio_active,
                video_state,
            } => {
                let state = ((*video_state & 0x03) << 1) | u8::from(*audio_active);
                out.push(state);
            }
            TransportMessage::VideoParameters { aspect_ratio } => {
                out.extend_from_slice(&aspect_ratio.to_be_bytes());
            }
            TransportMessage::RemoteBatteryLow(low) => {
                out.push(u8::from(*low));
            }
            TransportMessage::RemoteNetworkStatus {
                is_low_cost,
                is_low_data_requested,
            } => {
                let flags = u8::from(*is_low_cost) | (u8::from(*is_low_data_requested) << 1);
                out.push(flags);
            }
        }
        Ok(out)
    }

    /// Deserialize one message from `buf`, advancing `cursor`.
    fn deserialize(
        buf: &[u8],
        cursor: &mut usize,
        single_message_packet: bool,
    ) -> VoipResult<Self> {
        if *cursor >= buf.len() {
            return Err(VoipError::InvalidFraming("empty message body".into()));
        }
        let msg_type = buf[*cursor];
        *cursor += 1;

        match msg_type {
            legacy_msg_id::AUDIO_DATA
            | legacy_msg_id::VIDEO_DATA
            | legacy_msg_id::UNSTRUCTURED_DATA => {
                let len = if single_message_packet {
                    buf.len() - *cursor
                } else {
                    if *cursor + 2 > buf.len() {
                        return Err(VoipError::InvalidFraming("truncated buffer length".into()));
                    }
                    let l = u16::from_be_bytes([buf[*cursor], buf[*cursor + 1]]) as usize;
                    *cursor += 2;
                    l
                };
                if *cursor + len > buf.len() {
                    return Err(VoipError::InvalidFraming("truncated payload buffer".into()));
                }
                let data = buf[*cursor..*cursor + len].to_vec();
                *cursor += len;
                Ok(match msg_type {
                    legacy_msg_id::AUDIO_DATA => TransportMessage::AudioData(data),
                    legacy_msg_id::VIDEO_DATA => TransportMessage::VideoData(data),
                    _ => TransportMessage::UnstructuredData(data),
                })
            }
            legacy_msg_id::REMOTE_MEDIA_STATE => {
                if *cursor >= buf.len() {
                    return Err(VoipError::InvalidFraming("missing media state byte".into()));
                }
                let state = buf[*cursor];
                *cursor += 1;
                let video_state = (state >> 1) & 0x03;
                if video_state == 0x03 {
                    return Err(VoipError::InvalidFraming("invalid video state 3".into()));
                }
                Ok(TransportMessage::RemoteMediaState {
                    audio_active: (state & 0x01) != 0,
                    video_state,
                })
            }
            legacy_msg_id::VIDEO_PARAMETERS => {
                if *cursor + 4 > buf.len() {
                    return Err(VoipError::InvalidFraming("missing aspect ratio".into()));
                }
                let aspect_ratio = u32::from_be_bytes([
                    buf[*cursor],
                    buf[*cursor + 1],
                    buf[*cursor + 2],
                    buf[*cursor + 3],
                ]);
                *cursor += 4;
                Ok(TransportMessage::VideoParameters { aspect_ratio })
            }
            legacy_msg_id::REMOTE_BATTERY_LOW => {
                if *cursor >= buf.len() {
                    return Err(VoipError::InvalidFraming("missing battery byte".into()));
                }
                let low = buf[*cursor] != 0;
                *cursor += 1;
                Ok(TransportMessage::RemoteBatteryLow(low))
            }
            legacy_msg_id::REMOTE_NETWORK_STATUS => {
                if *cursor >= buf.len() {
                    return Err(VoipError::InvalidFraming(
                        "missing network status byte".into(),
                    ));
                }
                let flags = buf[*cursor];
                *cursor += 1;
                Ok(TransportMessage::RemoteNetworkStatus {
                    is_low_cost: (flags & 0x01) != 0,
                    is_low_data_requested: (flags & 0x02) != 0,
                })
            }
            other => Err(VoipError::InvalidFraming(format!(
                "unsupported message type id {other}"
            ))),
        }
    }
}

/// Decrypted typed message paired with its 30-bit sequence counter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecryptedTransportMessage {
    /// Decoded message.
    pub message: TransportMessage,
    /// Sender's 30-bit counter.
    pub counter: u32,
}

#[derive(Debug, Clone)]
struct MessageForResend {
    seq: u32,
    data: Vec<u8>,
    last_sent_ms: u64,
}

/// Stateful MTProto-2.0 AES-256-CTR encrypted channel matching `tgcalls::EncryptedConnection`.
///
/// Supports all three `tgcalls` modes:
/// 1. Raw signaling packets (`encrypt_raw_packet` / `decrypt_raw_packet`, used by `v2/SignalingEncryption.cpp`).
/// 2. Framed raw transport packets (`prepare_for_sending_raw_message` / `handle_incoming_raw_packet`,
///    used by `v2/MtProtoDtlsTransport.cpp` in tgcalls v12/v13).
/// 3. Typed transport messages (`prepare_for_sending` / `handle_incoming_packet`, `tgcalls/Message.cpp`).
#[derive(Debug, Clone)]
pub struct EncryptedConnection {
    channel: ChannelType,
    auth_key: [u8; AUTH_KEY_SIZE],
    is_outgoing: bool,
    counter: u32,
    largest_incoming_counters: Vec<u32>,
    acked_incoming_counters: Vec<u32>,
    acks_to_send_seqs: Vec<u32>,
    not_yet_acked: Vec<MessageForResend>,
}

impl EncryptedConnection {
    /// Create a new [`EncryptedConnection`] for `channel` with the 256-byte call `auth_key`.
    #[must_use]
    pub fn new(channel: ChannelType, auth_key: [u8; AUTH_KEY_SIZE], is_outgoing: bool) -> Self {
        Self {
            channel,
            auth_key,
            is_outgoing,
            counter: 0,
            largest_incoming_counters: Vec::with_capacity(KEEP_INCOMING_COUNTERS_COUNT),
            acked_incoming_counters: Vec::new(),
            acks_to_send_seqs: Vec::new(),
            not_yet_acked: Vec::new(),
        }
    }

    /// Convenience constructor for a signaling channel (`ChannelType::Signaling`).
    #[must_use]
    pub fn new_signaling(auth_key: [u8; AUTH_KEY_SIZE], is_outgoing: bool) -> Self {
        Self::new(ChannelType::Signaling, auth_key, is_outgoing)
    }

    /// Convenience constructor for a transport channel (`ChannelType::Transport`).
    #[must_use]
    pub fn new_transport(auth_key: [u8; AUTH_KEY_SIZE], is_outgoing: bool) -> Self {
        Self::new(ChannelType::Transport, auth_key, is_outgoing)
    }

    /// Channel type (`Signaling` or `Transport`).
    #[must_use]
    pub const fn channel(&self) -> ChannelType {
        self.channel
    }

    /// Whether this side initiated the call.
    #[must_use]
    pub const fn is_outgoing(&self) -> bool {
        self.is_outgoing
    }

    /// Current outgoing counter value.
    #[must_use]
    pub const fn current_counter(&self) -> u32 {
        self.counter
    }

    /// Returns `true` if ACKs or unacknowledged reliable messages are queued.
    #[must_use]
    pub fn has_pending_service(&self) -> bool {
        !self.acks_to_send_seqs.is_empty() || !self.not_yet_acked.is_empty()
    }

    /// Encrypt a raw signaling/control payload (`EncryptedConnection::encryptRawPacket`).
    ///
    /// Wire plaintext: `seq (u32 BE) || payload`.
    /// Used by `tgcalls::SignalingEncryption::encryptOutgoing` for v2 JSON signaling.
    pub fn encrypt_raw_packet(&mut self, payload: &[u8]) -> VoipResult<Vec<u8>> {
        if self.counter >= MAX_ALLOWED_COUNTER {
            return Err(VoipError::CounterExhausted);
        }
        self.counter += 1;
        let seq = self.counter;

        let mut plaintext = Vec::with_capacity(4 + payload.len());
        plaintext.extend_from_slice(&seq.to_be_bytes());
        plaintext.extend_from_slice(payload);

        Ok(self.encrypt_prepared(&plaintext).bytes)
    }

    /// Decrypt and authenticate a raw signaling/control packet (`EncryptedConnection::decryptRawPacket`).
    pub fn decrypt_raw_packet(&mut self, packet: &[u8]) -> VoipResult<Vec<u8>> {
        let plaintext = self.decrypt_and_verify_envelope(packet)?;
        let incoming_seq =
            u32::from_be_bytes([plaintext[0], plaintext[1], plaintext[2], plaintext[3]]);
        let incoming_counter = counter_from_seq(incoming_seq);
        if !self.register_incoming_counter(incoming_counter) {
            return Err(VoipError::ReplayDetected(incoming_counter));
        }
        Ok(plaintext[4..].to_vec())
    }

    /// Prepare a framed raw transport message (`EncryptedConnection::prepareForSendingRawMessage`),
    /// matching `MtProtoDtlsTransport::SendPacket` in `tgcalls` v12/v13.
    ///
    /// Wire plaintext entry: `seq (u32 BE) || 127 (kCustomId) || len (u32 BE) || payload`
    /// plus any piggybacked ACKs or unacked reliable resends.
    pub fn prepare_for_sending_raw_message(
        &mut self,
        payload: &[u8],
        requires_ack: bool,
        now_ms: u64,
    ) -> VoipResult<EncryptedPacket> {
        let single_message_packet = !self.has_pending_service() && !requires_ack;
        let seq = self.compute_next_seq(requires_ack, single_message_packet)?;

        let mut serialized = Vec::with_capacity(9 + payload.len());
        serialized.extend_from_slice(&seq.to_be_bytes());
        serialized.push(MSG_ID_CUSTOM_RAW);
        serialized.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        serialized.extend_from_slice(payload);

        self.prepare_message_internal(serialized, seq, requires_ack, now_ms)
    }

    /// Decrypt and unpack a framed raw transport packet (`EncryptedConnection::handleIncomingRawPacket`).
    ///
    /// Returns all decrypted raw messages contained in the packet (main + any retransmitted
    /// additional messages) while processing piggybacked ACKs and queueing ACKs for reliable messages.
    pub fn handle_incoming_raw_packet(
        &mut self,
        packet: &[u8],
    ) -> VoipResult<Vec<DecryptedRawMessage>> {
        let plaintext = self.decrypt_and_verify_envelope(packet)?;
        let packet_seq =
            u32::from_be_bytes([plaintext[0], plaintext[1], plaintext[2], plaintext[3]]);
        let packet_counter = counter_from_seq(packet_seq);
        if !self.register_incoming_counter(packet_counter) {
            return Err(VoipError::ReplayDetected(packet_counter));
        }

        let mut cursor = 4usize;
        let mut current_seq = packet_seq;
        let mut current_counter = packet_counter;
        let mut additional_message = false;
        let mut first_requiring_ack = true;
        let mut out = Vec::new();

        loop {
            if cursor >= plaintext.len() {
                return Err(VoipError::InvalidFraming("unexpected end of packet".into()));
            }
            let msg_type = plaintext[cursor];
            let single_message = (current_seq & SINGLE_MESSAGE_PACKET_SEQ_BIT) != 0;
            if single_message && additional_message {
                return Err(VoipError::InvalidFraming(
                    "single-message bit set on additional message".into(),
                ));
            }

            if msg_type == MSG_ID_EMPTY {
                if additional_message {
                    return Err(VoipError::InvalidFraming(
                        "empty message must be first in packet".into(),
                    ));
                }
                cursor += 1;
            } else if msg_type == MSG_ID_ACK {
                if !additional_message {
                    return Err(VoipError::InvalidFraming(
                        "ack message must not be first in packet".into(),
                    ));
                }
                self.ack_my_message(current_seq);
                cursor += 1;
            } else if msg_type == MSG_ID_CUSTOM_RAW {
                cursor += 1;
                if cursor + 4 > plaintext.len() {
                    return Err(VoipError::InvalidFraming(
                        "truncated custom message length".into(),
                    ));
                }
                let len = u32::from_be_bytes([
                    plaintext[cursor],
                    plaintext[cursor + 1],
                    plaintext[cursor + 2],
                    plaintext[cursor + 3],
                ]) as usize;
                cursor += 4;
                if cursor + len > plaintext.len() {
                    return Err(VoipError::InvalidFraming(
                        "truncated custom message payload".into(),
                    ));
                }
                let payload = plaintext[cursor..cursor + len].to_vec();
                cursor += len;

                let requires_ack = (current_seq & MESSAGE_REQUIRES_ACK_SEQ_BIT) != 0;
                let skip = if requires_ack {
                    !self.register_sent_ack(current_counter, first_requiring_ack)
                } else {
                    additional_message
                        && (current_counter > packet_counter
                            || !self.register_incoming_counter(current_counter))
                };
                if requires_ack {
                    first_requiring_ack = false;
                    self.queue_ack(current_seq);
                }
                if !skip {
                    out.push(DecryptedRawMessage {
                        payload,
                        counter: current_counter,
                    });
                }
            } else {
                return Err(VoipError::InvalidFraming(format!(
                    "unexpected raw packet message type {msg_type}"
                )));
            }

            if cursor == plaintext.len() {
                break;
            }
            if single_message {
                return Err(VoipError::InvalidFraming(
                    "single message did not fill entire packet".into(),
                ));
            }
            if plaintext.len() - cursor < 5 {
                return Err(VoipError::InvalidFraming(format!(
                    "bad trailing data size {}",
                    plaintext.len() - cursor
                )));
            }
            current_seq = u32::from_be_bytes([
                plaintext[cursor],
                plaintext[cursor + 1],
                plaintext[cursor + 2],
                plaintext[cursor + 3],
            ]);
            cursor += 4;
            current_counter = counter_from_seq(current_seq);
            additional_message = true;
        }

        Ok(out)
    }

    /// Prepare a typed [`TransportMessage`] packet (`EncryptedConnection::prepareForSending`).
    pub fn prepare_for_sending(
        &mut self,
        message: &TransportMessage,
        now_ms: u64,
    ) -> VoipResult<EncryptedPacket> {
        let requires_ack = message.requires_ack();
        let single_message_packet = !self.has_pending_service() && !requires_ack;
        let seq = self.compute_next_seq(requires_ack, single_message_packet)?;
        let serialized = message.serialize_with_seq(seq, single_message_packet)?;
        self.prepare_message_internal(serialized, seq, requires_ack, now_ms)
    }

    /// Decrypt and unpack a typed transport packet (`EncryptedConnection::handleIncomingPacket`).
    pub fn handle_incoming_packet(
        &mut self,
        packet: &[u8],
    ) -> VoipResult<Vec<DecryptedTransportMessage>> {
        let plaintext = self.decrypt_and_verify_envelope(packet)?;
        let packet_seq =
            u32::from_be_bytes([plaintext[0], plaintext[1], plaintext[2], plaintext[3]]);
        let packet_counter = counter_from_seq(packet_seq);
        if !self.register_incoming_counter(packet_counter) {
            return Err(VoipError::ReplayDetected(packet_counter));
        }

        let mut cursor = 4usize;
        let mut current_seq = packet_seq;
        let mut current_counter = packet_counter;
        let mut additional_message = false;
        let mut first_requiring_ack = true;
        let mut out = Vec::new();

        loop {
            if cursor >= plaintext.len() {
                return Err(VoipError::InvalidFraming("unexpected end of packet".into()));
            }
            let msg_type = plaintext[cursor];
            let single_message = (current_seq & SINGLE_MESSAGE_PACKET_SEQ_BIT) != 0;
            if single_message && additional_message {
                return Err(VoipError::InvalidFraming(
                    "single-message bit set on additional message".into(),
                ));
            }

            if msg_type == MSG_ID_EMPTY {
                if additional_message {
                    return Err(VoipError::InvalidFraming(
                        "empty message must be first in packet".into(),
                    ));
                }
                cursor += 1;
            } else if msg_type == MSG_ID_ACK {
                if !additional_message {
                    return Err(VoipError::InvalidFraming(
                        "ack message must not be first in packet".into(),
                    ));
                }
                self.ack_my_message(current_seq);
                cursor += 1;
            } else {
                let message =
                    TransportMessage::deserialize(&plaintext, &mut cursor, single_message)?;
                let requires_ack = (current_seq & MESSAGE_REQUIRES_ACK_SEQ_BIT) != 0;
                let skip = if requires_ack {
                    !self.register_sent_ack(current_counter, first_requiring_ack)
                } else {
                    additional_message
                        && (current_counter > packet_counter
                            || !self.register_incoming_counter(current_counter))
                };
                if requires_ack {
                    first_requiring_ack = false;
                    self.queue_ack(current_seq);
                }
                if !skip {
                    out.push(DecryptedTransportMessage {
                        message,
                        counter: current_counter,
                    });
                }
            }

            if cursor == plaintext.len() {
                break;
            }
            if single_message {
                return Err(VoipError::InvalidFraming(
                    "single message did not fill entire packet".into(),
                ));
            }
            if plaintext.len() - cursor < 5 {
                return Err(VoipError::InvalidFraming(format!(
                    "bad trailing data size {}",
                    plaintext.len() - cursor
                )));
            }
            current_seq = u32::from_be_bytes([
                plaintext[cursor],
                plaintext[cursor + 1],
                plaintext[cursor + 2],
                plaintext[cursor + 3],
            ]);
            cursor += 4;
            current_counter = counter_from_seq(current_seq);
            additional_message = true;
        }

        Ok(out)
    }

    /// Emit a dedicated service packet carrying pending ACKs and due retransmissions
    /// (`EncryptedConnection::prepareForSendingService`).
    pub fn prepare_for_sending_service(
        &mut self,
        now_ms: u64,
    ) -> VoipResult<Option<EncryptedPacket>> {
        if !self.has_pending_service() {
            return Ok(None);
        }
        let seq = self.compute_next_seq(false, false)?;
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(&seq.to_be_bytes());
        buf.push(MSG_ID_EMPTY);
        self.append_additional_messages(&mut buf, now_ms);
        Ok(Some(self.encrypt_prepared(&buf)))
    }

    fn prepare_message_internal(
        &mut self,
        mut serialized: Vec<u8>,
        seq: u32,
        requires_ack: bool,
        now_ms: u64,
    ) -> VoipResult<EncryptedPacket> {
        if !self.enough_space_in_packet(serialized.len(), 0) {
            return Err(VoipError::PacketTooLarge(serialized.len()));
        }
        if !requires_ack {
            self.append_additional_messages(&mut serialized, now_ms);
            return Ok(self.encrypt_prepared(&serialized));
        }

        let had_enqueued = !self.not_yet_acked.is_empty();
        let copy = serialized.clone();
        if !had_enqueued {
            self.append_additional_messages(&mut serialized, now_ms);
        }
        self.not_yet_acked.push(MessageForResend {
            seq,
            data: copy,
            last_sent_ms: now_ms,
        });
        if !had_enqueued {
            return Ok(self.encrypt_prepared(&serialized));
        }
        for queued in &mut self.not_yet_acked {
            queued.last_sent_ms = 0;
        }
        self.prepare_for_sending_service(now_ms)?
            .ok_or_else(|| VoipError::InvalidState("missing enqueued service packet".into()))
    }

    fn compute_next_seq(&mut self, requires_ack: bool, single_message: bool) -> VoipResult<u32> {
        if requires_ack && self.not_yet_acked.len() >= NOT_ACKED_MESSAGES_LIMIT {
            return Err(VoipError::InvalidState(
                "too many unacknowledged messages".into(),
            ));
        }
        if self.counter >= MAX_ALLOWED_COUNTER {
            return Err(VoipError::CounterExhausted);
        }
        self.counter += 1;
        Ok(self.counter
            | if single_message {
                SINGLE_MESSAGE_PACKET_SEQ_BIT
            } else {
                0
            }
            | if requires_ack {
                MESSAGE_REQUIRES_ACK_SEQ_BIT
            } else {
                0
            })
    }

    fn enough_space_in_packet(&self, current_len: usize, extra: usize) -> bool {
        let limit = self.channel.packet_limit();
        extra < limit && MSG_KEY_SIZE + current_len + extra <= limit
    }

    fn append_additional_messages(&mut self, buf: &mut Vec<u8>, now_ms: u64) {
        let mut consumed_acks = 0usize;
        for &ack_seq in &self.acks_to_send_seqs {
            if self.enough_space_in_packet(buf.len(), 5) {
                buf.extend_from_slice(&ack_seq.to_be_bytes());
                buf.push(MSG_ID_ACK);
                consumed_acks += 1;
            } else {
                break;
            }
        }
        self.acks_to_send_seqs.drain(..consumed_acks);

        let (min_resend_ms, _, _) = self.channel.delay_intervals_ms();
        let limit = self.channel.packet_limit();
        for item in &mut self.not_yet_acked {
            let when = if item.last_sent_ms > 0 {
                item.last_sent_ms.saturating_add(min_resend_ms)
            } else {
                0
            };
            if when > now_ms {
                break;
            }
            if item.data.len() < limit && MSG_KEY_SIZE + buf.len() + item.data.len() <= limit {
                buf.extend_from_slice(&item.data);
                item.last_sent_ms = now_ms;
            } else {
                break;
            }
        }
    }

    fn encrypt_prepared(&self, plaintext: &[u8]) -> EncryptedPacket {
        let seq = u32::from_be_bytes([plaintext[0], plaintext[1], plaintext[2], plaintext[3]]);
        let counter = counter_from_seq(seq);
        let x = self.channel.send_x(self.is_outgoing);
        let bytes = tg_calls::signaling::encrypt_ctr_envelope(&self.auth_key, plaintext, x);

        EncryptedPacket { bytes, counter }
    }

    fn decrypt_and_verify_envelope(&self, packet: &[u8]) -> VoipResult<Vec<u8>> {
        let x = self.channel.recv_x(self.is_outgoing);
        tg_calls::signaling::decrypt_ctr_envelope(&self.auth_key, packet, x).map_err(
            |err| match err {
                tg_calls::SignalingError::PacketTooShort(n) => VoipError::PacketTooShort(n),
                tg_calls::SignalingError::PacketTooLarge(n) => VoipError::PacketTooLarge(n),
                tg_calls::SignalingError::MsgKeyMismatch => VoipError::MsgKeyMismatch,
                tg_calls::SignalingError::ReplayDetected(c) => VoipError::ReplayDetected(c),
                tg_calls::SignalingError::CounterExhausted => VoipError::CounterExhausted,
                tg_calls::SignalingError::InvalidFraming => {
                    VoipError::InvalidFraming("invalid signaling packet framing".into())
                }
            },
        )
    }

    /// Exact port of `EncryptedConnection::registerIncomingCounter` (`EncryptedConnection.cpp:445-467`).
    fn register_incoming_counter(&mut self, incoming_counter: u32) -> bool {
        let list = &mut self.largest_incoming_counters;
        let largest = list.last().copied().unwrap_or(0);
        if list.binary_search(&incoming_counter).is_ok() {
            return false;
        }
        if incoming_counter.saturating_add(KEEP_INCOMING_COUNTERS_COUNT as u32) <= largest {
            return false;
        }
        let keep_from = list
            .iter()
            .position(|&c| c.saturating_add(KEEP_INCOMING_COUNTERS_COUNT as u32) > incoming_counter)
            .unwrap_or(list.len());
        list.drain(..keep_from);
        let insert_pos = list.partition_point(|&c| c < incoming_counter);
        list.insert(insert_pos, incoming_counter);
        true
    }

    /// Exact port of `EncryptedConnection::registerSentAck` (`EncryptedConnection.cpp:610-626`).
    fn register_sent_ack(&mut self, counter: u32, first_in_packet: bool) -> bool {
        let list = &mut self.acked_incoming_counters;
        let pos = list.partition_point(|&c| c < counter);
        let already = pos < list.len() && list[pos] == counter;
        if first_in_packet {
            list.drain(..pos);
            if !already {
                list.insert(0, counter);
            }
        } else if !already {
            list.insert(pos, counter);
        }
        !already
    }

    fn queue_ack(&mut self, incoming_seq: u32) {
        if !self.acks_to_send_seqs.contains(&incoming_seq) {
            self.acks_to_send_seqs.push(incoming_seq);
        }
    }

    fn ack_my_message(&mut self, seq: u32) {
        if let Some(pos) = self.not_yet_acked.iter().position(|m| m.seq == seq) {
            self.not_yet_acked.remove(pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn test_key() -> [u8; AUTH_KEY_SIZE] {
        let mut key = [0u8; AUTH_KEY_SIZE];
        for (i, b) in key.iter_mut().enumerate() {
            *b = ((i * 37 + 13) & 0xFF) as u8;
        }
        key
    }

    #[test]
    fn kat_prepare_aes_key_iv_and_msg_key() {
        let key = test_key();
        let plaintext = b"\x00\x00\x00\x01hello-tgcalls";
        let x = ChannelType::Signaling.send_x(true);
        assert_eq!(x, 128);
        assert_eq!(ChannelType::Signaling.recv_x(false), 128);
        assert_eq!(ChannelType::Transport.send_x(true), 0);
        assert_eq!(ChannelType::Transport.recv_x(false), 0);
        assert_eq!(ChannelType::Transport.send_x(false), 8);
        assert_eq!(ChannelType::Transport.recv_x(true), 8);

        let msg_key = compute_msg_key(&key, plaintext, x);
        let (aes_key, aes_iv) = prepare_aes_key_iv(&key, &msg_key, x);

        // Verify key/IV match direct SHA-256 reference formulas from CryptoHelper.cpp.
        let mut ha = Sha256::new();
        ha.update(msg_key);
        ha.update(&key[x..x + 36]);
        let sha_a = ha.finalize();
        let mut hb = Sha256::new();
        hb.update(&key[40 + x..40 + x + 36]);
        hb.update(msg_key);
        let sha_b = hb.finalize();

        assert_eq!(&aes_key[0..8], &sha_a[0..8]);
        assert_eq!(&aes_key[8..24], &sha_b[8..24]);
        assert_eq!(&aes_key[24..32], &sha_a[24..32]);
        assert_eq!(&aes_iv[0..4], &sha_b[0..4]);
        assert_eq!(&aes_iv[4..12], &sha_a[8..16]);
        assert_eq!(&aes_iv[12..16], &sha_b[24..28]);

        // AES-CTR is symmetric (involution).
        let ct = aes_256_ctr_apply(plaintext, &aes_key, &aes_iv);
        assert_ne!(&ct[..], plaintext);
        let pt = aes_256_ctr_apply(&ct, &aes_key, &aes_iv);
        assert_eq!(&pt[..], plaintext);
    }

    #[test]
    fn raw_signaling_packet_roundtrip_and_replay_rejection() {
        let key = test_key();
        let mut caller = EncryptedConnection::new_signaling(key, true);
        let mut callee = EncryptedConnection::new_signaling(key, false);

        let payload = br#"{"@type":"MediaState","muted":false}"#;
        let pkt = caller.encrypt_raw_packet(payload).unwrap();
        assert_eq!(pkt.len(), MSG_KEY_SIZE + 4 + payload.len());

        let dec = callee.decrypt_raw_packet(&pkt).unwrap();
        assert_eq!(dec, payload);

        // Replay must fail.
        assert!(matches!(
            callee.decrypt_raw_packet(&pkt),
            Err(VoipError::ReplayDetected(1))
        ));

        // Tampered ciphertext must fail msg_key MAC check.
        let mut bad = caller.encrypt_raw_packet(payload).unwrap();
        bad[MSG_KEY_SIZE + 5] ^= 0x01;
        assert!(matches!(
            callee.decrypt_raw_packet(&bad),
            Err(VoipError::MsgKeyMismatch)
        ));
    }

    #[test]
    fn raw_transport_message_roundtrip_with_reliable_ack() {
        let key = test_key();
        let mut caller = EncryptedConnection::new_transport(key, true);
        let mut callee = EncryptedConnection::new_transport(key, false);

        // Reliable message requires ACK.
        let p1 = caller
            .prepare_for_sending_raw_message(b"rtp-or-sctp-control", true, 1_000)
            .unwrap();
        assert!(caller.has_pending_service());

        let recv1 = callee.handle_incoming_raw_packet(&p1.bytes).unwrap();
        assert_eq!(recv1.len(), 1);
        assert_eq!(recv1[0].payload, b"rtp-or-sctp-control");
        assert!(callee.has_pending_service()); // ACK queued

        // Callee sends unreliable media; ACK gets piggybacked!
        let p2 = callee
            .prepare_for_sending_raw_message(b"opus-frame-1", false, 1_020)
            .unwrap();
        let recv2 = caller.handle_incoming_raw_packet(&p2.bytes).unwrap();
        assert_eq!(recv2.len(), 1);
        assert_eq!(recv2[0].payload, b"opus-frame-1");
        assert!(!caller.has_pending_service()); // Reliable message was ACKed!
    }

    #[test]
    fn typed_transport_messages_roundtrip() {
        let key = test_key();
        let mut caller = EncryptedConnection::new_transport(key, true);
        let mut callee = EncryptedConnection::new_transport(key, false);

        let audio = TransportMessage::AudioData(vec![0xAA, 0xBB, 0xCC, 0xDD]);
        let pkt = caller.prepare_for_sending(&audio, 100).unwrap();
        let decoded = callee.handle_incoming_packet(&pkt.bytes).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].message, audio);

        let state = TransportMessage::RemoteMediaState {
            audio_active: false,
            video_state: 2,
        };
        let pkt2 = caller.prepare_for_sending(&state, 120).unwrap();
        let decoded2 = callee.handle_incoming_packet(&pkt2.bytes).unwrap();
        assert_eq!(decoded2.len(), 1);
        assert_eq!(decoded2[0].message, state);
    }

    #[test]
    fn out_of_order_within_64_window_accepted() {
        let key = test_key();
        let mut sender = EncryptedConnection::new_transport(key, true);
        let mut receiver = EncryptedConnection::new_transport(key, false);

        let p1 = sender
            .prepare_for_sending_raw_message(b"one", false, 0)
            .unwrap();
        let p2 = sender
            .prepare_for_sending_raw_message(b"two", false, 20)
            .unwrap();
        let p3 = sender
            .prepare_for_sending_raw_message(b"three", false, 40)
            .unwrap();

        // Deliver 3, then 1, then 2.
        assert_eq!(
            receiver.handle_incoming_raw_packet(&p3.bytes).unwrap()[0].payload,
            b"three"
        );
        assert_eq!(
            receiver.handle_incoming_raw_packet(&p1.bytes).unwrap()[0].payload,
            b"one"
        );
        assert_eq!(
            receiver.handle_incoming_raw_packet(&p2.bytes).unwrap()[0].payload,
            b"two"
        );
    }

    #[test]
    fn signaling_cipher_interops_byte_for_byte_with_tg_calls() {
        let key = test_key();
        let mut calls_caller = tg_calls::SignalingCipher::new(key, true);
        let mut calls_callee = tg_calls::SignalingCipher::new(key, false);
        let mut voip_caller = EncryptedConnection::new_signaling(key, true);
        let mut voip_callee = EncryptedConnection::new_signaling(key, false);

        // Verify AES-256-CTR output matches independent RustCrypto `ctr::Ctr128BE<Aes256>`.
        let sample_plain = b"\x00\x00\x00\x01{\"@type\":\"InitialSetup\"}";
        let x = ChannelType::Signaling.send_x(true);
        let msg_key = compute_msg_key(&key, sample_plain, x);
        let (aes_k, aes_iv) = prepare_aes_key_iv(&key, &msg_key, x);
        let mut ref_ct = sample_plain.to_vec();
        {
            use aes::Aes256;
            use ctr::cipher::{KeyIvInit, StreamCipher};
            type RefCtr = ctr::Ctr128BE<Aes256>;
            RefCtr::new((&aes_k).into(), (&aes_iv).into()).apply_keystream(&mut ref_ct);
        }
        assert_eq!(aes_256_ctr_apply(sample_plain, &aes_k, &aes_iv), ref_ct);

        // 1. tg-calls caller -> tg-voip callee (and exact ciphertext identity with tg-voip caller).
        let msg1 = br#"{"@type":"InitialSetup","ufrag":"ab12","pwd":"secret"}"#;
        let pkt_from_calls = calls_caller.encrypt(msg1).unwrap();
        let pkt_from_voip = voip_caller.encrypt_raw_packet(msg1).unwrap();
        assert_eq!(pkt_from_calls, pkt_from_voip);
        assert_eq!(
            voip_callee.decrypt_raw_packet(&pkt_from_calls).unwrap(),
            msg1
        );

        // 2. tg-voip callee -> tg-calls caller.
        let msg2 = br#"{"@type":"MediaState","muted":true}"#;
        let reply_from_voip = voip_callee.encrypt_raw_packet(msg2).unwrap();
        let dec = calls_caller.decrypt(&reply_from_voip).unwrap();
        assert_eq!(dec.seq, 1);
        assert_eq!(dec.payload, msg2);

        // 3. Out-of-order packets from tg-voip caller -> tg-calls callee.
        let p2 = voip_caller.encrypt_raw_packet(b"second").unwrap();
        let p3 = voip_caller.encrypt_raw_packet(b"third").unwrap();
        let d3 = calls_callee.decrypt(&p3).unwrap();
        assert_eq!((d3.seq, d3.payload.as_slice()), (3, b"third".as_slice()));
        let d2 = calls_callee.decrypt(&p2).unwrap();
        assert_eq!((d2.seq, d2.payload.as_slice()), (2, b"second".as_slice()));
        assert_eq!(
            calls_callee.decrypt(&p2),
            Err(tg_calls::SignalingError::ReplayDetected(2))
        );
    }
}
