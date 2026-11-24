//! Signaling data encryption for `tgcalls` (`phone.sendSignalingData` /
//! `updatePhoneCallSignalingData`).
//!
//! Port of `TelegramMessenger/tgcalls` (`development` branch):
//! - `tgcalls/CryptoHelper.h`, `tgcalls/CryptoHelper.cpp` (`PrepareAesKeyIv`, `AesProcessCtr`)
//! - `tgcalls/EncryptedConnection.h`, `tgcalls/EncryptedConnection.cpp`
//! - `tgcalls/v2/SignalingEncryption.cpp`
//!
//! # Verified Wire Format (MTProto 2.0 AES-256-CTR)
//! Unlike MTProto TCP sessions (which use AES-256-IGE with a 32-byte IV), `tgcalls`
//! uses **AES-256-CTR** (128-bit big-endian counter, matching OpenSSL `CRYPTO_ctr128_encrypt`)
//! with a 16-byte IV:
//! - Directional offset `x`:
//!   - Sending: `(if is_outgoing { 0 } else { 8 }) + (if Signaling { 128 } else { 0 })`
//!   - Receiving: `(if is_outgoing { 8 } else { 0 }) + (if Signaling { 128 } else { 0 })`
//! - `msg_key_large = SHA256(auth_key[88 + x .. 88 + x + 32] || plaintext)`
//! - `msg_key = msg_key_large[8 .. 24]` (16 bytes)
//! - Key derivation (`PrepareAesKeyIv`):
//!   - `sha256_a = SHA256(msg_key || auth_key[x .. x + 36])`
//!   - `sha256_b = SHA256(auth_key[40 + x .. 40 + x + 36] || msg_key)`
//!   - `aes_key (32 bytes) = sha256_a[0..8] || sha256_b[8..24] || sha256_a[24..32]`
//!   - `aes_iv  (16 bytes) = sha256_b[0..4] || sha256_a[8..16] || sha256_b[24..28]`
//! - Wire packet: `msg_key (16 bytes) || AES-256-CTR(plaintext, aes_key, aes_iv)`
//! - Signaling inner framing (`EncryptedConnection::encryptRawPacket` / `decryptRawPacket`):
//!   `seq (u32 BE) || payload` (no padding or extra length prefix).

use tg_crypto::aes::CtrState;
pub use tg_crypto::auth_key::AUTH_KEY_SIZE;
use tg_crypto::hash::{const_time_eq, sha256};

use crate::error::SignalingError;

/// Length of the MTProto 2.0 `msg_key` authentication tag prefix.
pub const MSG_KEY_SIZE: usize = 16;

/// Minimum valid encrypted packet size: 16-byte `msg_key` + 4-byte `seq` + >= 1 byte body.
pub const MIN_ENCRYPTED_PACKET_SIZE: usize = 21;

/// Maximum incoming encrypted packet size accepted before decrypting (`kMaxIncomingPacketSize`).
pub const MAX_INCOMING_PACKET_SIZE: usize = 128 * 1024;

/// Maximum outer UDP transport packet size (`1500 - 48`, `kMaxOuterPacketSize`).
pub const MAX_OUTER_PACKET_SIZE: usize = 1500 - 48;

/// Maximum signaling packet size (`kMaxSignalingPacketSize`).
pub const MAX_SIGNALING_PACKET_SIZE: usize = 16 * 1024;

/// Replay window size in recent incoming counters (`kKeepIncomingCountersCount`).
pub const KEEP_INCOMING_COUNTERS_COUNT: usize = 64;

/// Sequence bit indicating a single message fills the entire packet (`kSingleMessagePacketSeqBit`).
pub const SINGLE_MESSAGE_PACKET_SEQ_BIT: u32 = 1 << 31;

/// Sequence bit indicating the message requires an explicit ACK (`kMessageRequiresAckSeqBit`).
pub const MESSAGE_REQUIRES_ACK_SEQ_BIT: u32 = 1 << 30;

/// Maximum usable 30-bit packet counter (`kMaxAllowedCounter`).
pub const MAX_ALLOWED_COUNTER: u32 = !SINGLE_MESSAGE_PACKET_SEQ_BIT & !MESSAGE_REQUIRES_ACK_SEQ_BIT;

/// Encrypted channel kind matching `tgcalls::EncryptedConnection::Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelType {
    /// Signaling messages sent over MTProto (`phone.sendSignalingData`, `x_base = 128`).
    Signaling,
    /// Direct transport / media packets (`x_base = 0`).
    Transport,
}

impl ChannelType {
    /// Directional offset `x` for encrypting outgoing packets.
    ///
    /// Matches `EncryptedConnection.cpp:426`:
    /// `(_key.isOutgoing ? 0 : 8) + (_type == Type::Signaling ? 128 : 0)`.
    #[must_use]
    pub const fn send_x(self, is_outgoing: bool) -> usize {
        let base = match self {
            ChannelType::Signaling => 128,
            ChannelType::Transport => 0,
        };
        base + if is_outgoing { 0 } else { 8 }
    }

    /// Directional offset `x` for decrypting incoming packets from the peer.
    ///
    /// Matches `EncryptedConnection.cpp:205, 475`:
    /// `(_key.isOutgoing ? 8 : 0) + (_type == Type::Signaling ? 128 : 0)`.
    #[must_use]
    pub const fn recv_x(self, is_outgoing: bool) -> usize {
        let base = match self {
            ChannelType::Signaling => 128,
            ChannelType::Transport => 0,
        };
        base + if is_outgoing { 8 } else { 0 }
    }

    /// Maximum packet payload limit (`packetLimit()` in `EncryptedConnection.cpp`).
    #[must_use]
    pub const fn packet_limit(self) -> usize {
        match self {
            ChannelType::Signaling => MAX_SIGNALING_PACKET_SIZE,
            ChannelType::Transport => MAX_OUTER_PACKET_SIZE,
        }
    }

    /// Retransmission intervals `(min_resend_ms, max_resend_ms, max_ack_delay_ms)`.
    ///
    /// Matches `DelayIntervalsByType` in `EncryptedConnection.cpp:652-664`.
    #[must_use]
    pub const fn delay_intervals_ms(self) -> (u64, u64, u64) {
        match self {
            ChannelType::Signaling => (3_000, 5_000, 5_000),
            ChannelType::Transport => (300, 1_000, 1_000),
        }
    }
}

/// Extract the 30-bit monotonic counter from a 32-bit sequence word (`CounterFromSeq`).
#[must_use]
pub const fn counter_from_seq(seq: u32) -> u32 {
    seq & !SINGLE_MESSAGE_PACKET_SEQ_BIT & !MESSAGE_REQUIRES_ACK_SEQ_BIT
}

/// Compute the 16-byte MTProto 2.0 `msg_key` over `plaintext` with directional offset `x`.
///
/// Exact port of `ConcatSHA256(key + 88 + x, 32; plaintext)[8..24]`.
#[must_use]
pub fn compute_msg_key(
    auth_key: &[u8; AUTH_KEY_SIZE],
    plaintext: &[u8],
    x: usize,
) -> [u8; MSG_KEY_SIZE] {
    let large = sha256(&[&auth_key[88 + x..88 + x + 32], plaintext]);
    let mut msg_key = [0u8; MSG_KEY_SIZE];
    msg_key.copy_from_slice(&large[8..24]);
    msg_key
}

/// Derive the 32-byte AES-256 key and 16-byte CTR IV (`PrepareAesKeyIv` in `CryptoHelper.cpp`).
#[must_use]
pub fn prepare_aes_key_iv(
    auth_key: &[u8; AUTH_KEY_SIZE],
    msg_key: &[u8; MSG_KEY_SIZE],
    x: usize,
) -> ([u8; 32], [u8; 16]) {
    let sha256_a = sha256(&[msg_key.as_slice(), &auth_key[x..x + 36]]);
    let sha256_b = sha256(&[&auth_key[40 + x..40 + x + 36], msg_key.as_slice()]);

    let mut aes_key = [0u8; 32];
    aes_key[0..8].copy_from_slice(&sha256_a[0..8]);
    aes_key[8..24].copy_from_slice(&sha256_b[8..24]);
    aes_key[24..32].copy_from_slice(&sha256_a[24..32]);

    let mut aes_iv = [0u8; 16];
    aes_iv[0..4].copy_from_slice(&sha256_b[0..4]);
    aes_iv[4..12].copy_from_slice(&sha256_a[8..16]);
    aes_iv[12..16].copy_from_slice(&sha256_b[24..28]);

    (aes_key, aes_iv)
}

/// Alias for [`prepare_aes_key_iv`] deriving `(aes_key, aes_iv_16)`.
#[must_use]
pub fn derive_aes_key_iv(
    auth_key: &[u8; AUTH_KEY_SIZE],
    msg_key: &[u8; MSG_KEY_SIZE],
    x: usize,
) -> ([u8; 32], [u8; 16]) {
    prepare_aes_key_iv(auth_key, msg_key, x)
}

/// Apply AES-256-CTR (128-bit big-endian counter, matching OpenSSL `CRYPTO_ctr128_encrypt`).
#[must_use]
pub fn aes_256_ctr_apply(data: &[u8], key: &[u8; 32], iv: &[u8; 16]) -> Vec<u8> {
    let mut out = data.to_vec();
    let mut ctr = CtrState::new(*key, *iv);
    ctr.apply_keystream(&mut out);
    out
}

/// Encrypt a plaintext buffer into `msg_key (16) || AES-256-CTR(plaintext)`.
#[must_use]
pub fn encrypt_ctr_envelope(auth_key: &[u8; AUTH_KEY_SIZE], plaintext: &[u8], x: usize) -> Vec<u8> {
    let msg_key = compute_msg_key(auth_key, plaintext, x);
    let (aes_key, aes_iv) = prepare_aes_key_iv(auth_key, &msg_key, x);
    let encrypted = aes_256_ctr_apply(plaintext, &aes_key, &aes_iv);

    let mut packet = Vec::with_capacity(MSG_KEY_SIZE + encrypted.len());
    packet.extend_from_slice(&msg_key);
    packet.extend_from_slice(&encrypted);
    packet
}

/// Decrypt a `msg_key (16) || AES-256-CTR(plaintext)` packet and verify its `msg_key` MAC.
pub fn decrypt_ctr_envelope(
    auth_key: &[u8; AUTH_KEY_SIZE],
    packet: &[u8],
    x: usize,
) -> Result<Vec<u8>, SignalingError> {
    if packet.len() < MIN_ENCRYPTED_PACKET_SIZE {
        return Err(SignalingError::PacketTooShort(packet.len()));
    }
    if packet.len() > MAX_INCOMING_PACKET_SIZE {
        return Err(SignalingError::PacketTooLarge(packet.len()));
    }

    let mut received_msg_key = [0u8; MSG_KEY_SIZE];
    received_msg_key.copy_from_slice(&packet[..MSG_KEY_SIZE]);

    let (aes_key, aes_iv) = prepare_aes_key_iv(auth_key, &received_msg_key, x);
    let plaintext = aes_256_ctr_apply(&packet[MSG_KEY_SIZE..], &aes_key, &aes_iv);

    let expected_msg_key = compute_msg_key(auth_key, &plaintext, x);
    if !const_time_eq(&received_msg_key, &expected_msg_key) {
        return Err(SignalingError::MsgKeyMismatch);
    }

    Ok(plaintext)
}

/// Encrypt a raw plaintext buffer into `msg_key (16) || AES-256-CTR(plaintext)`.
pub fn encrypt_raw_mtproto2(
    auth_key: &[u8; AUTH_KEY_SIZE],
    plaintext: &[u8],
    x: usize,
) -> Result<Vec<u8>, SignalingError> {
    Ok(encrypt_ctr_envelope(auth_key, plaintext, x))
}

/// Decrypt a `msg_key (16) || AES-256-CTR(plaintext)` packet and verify its `msg_key` MAC.
pub fn decrypt_raw_mtproto2(
    auth_key: &[u8; AUTH_KEY_SIZE],
    packet: &[u8],
    x: usize,
) -> Result<Vec<u8>, SignalingError> {
    decrypt_ctr_envelope(auth_key, packet, x)
}

/// Stateful encrypted signaling channel with monotonic sequence numbers and 64-entry replay window.
///
/// Exact wire-compatible port of `tgcalls::EncryptedConnection` (`Type::Signaling`,
/// `encryptRawPacket` / `decryptRawPacket` used by `v2/SignalingEncryption.cpp`).
#[derive(Debug, Clone)]
pub struct SignalingCipher {
    auth_key: [u8; AUTH_KEY_SIZE],
    is_outgoing: bool,
    channel: ChannelType,
    counter: u32,
    largest_incoming_counters: Vec<u32>,
}

/// Decrypted signaling message paired with its 30-bit sequence counter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecryptedSignalingPacket {
    /// Monotonic sender sequence counter.
    pub seq: u32,
    /// Decrypted signaling payload bytes.
    pub payload: Vec<u8>,
}

impl SignalingCipher {
    /// Create a new signaling cipher bound to the 256-byte call `auth_key`.
    #[must_use]
    pub fn new(auth_key: [u8; AUTH_KEY_SIZE], is_outgoing: bool) -> Self {
        Self::with_channel(auth_key, is_outgoing, ChannelType::Signaling)
    }

    /// Create a new cipher with an explicit [`ChannelType`].
    #[must_use]
    pub fn with_channel(
        auth_key: [u8; AUTH_KEY_SIZE],
        is_outgoing: bool,
        channel: ChannelType,
    ) -> Self {
        Self {
            auth_key,
            is_outgoing,
            channel,
            counter: 0,
            largest_incoming_counters: Vec::with_capacity(KEEP_INCOMING_COUNTERS_COUNT),
        }
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

    /// Current outgoing sequence counter value.
    #[must_use]
    pub const fn current_counter(&self) -> u32 {
        self.counter
    }

    /// Encrypt an outgoing signaling payload (`seq (u32 BE) || payload`) using AES-256-CTR.
    pub fn encrypt(&mut self, payload: &[u8]) -> Result<Vec<u8>, SignalingError> {
        if self.counter >= MAX_ALLOWED_COUNTER {
            return Err(SignalingError::CounterExhausted);
        }
        self.counter += 1;
        let seq = self.counter;

        let mut plaintext = Vec::with_capacity(4 + payload.len());
        plaintext.extend_from_slice(&seq.to_be_bytes());
        plaintext.extend_from_slice(payload);

        let x = self.channel.send_x(self.is_outgoing);
        Ok(encrypt_ctr_envelope(&self.auth_key, &plaintext, x))
    }

    /// Decrypt and authenticate an incoming signaling packet, rejecting replays.
    pub fn decrypt(&mut self, packet: &[u8]) -> Result<DecryptedSignalingPacket, SignalingError> {
        let x = self.channel.recv_x(self.is_outgoing);
        let plaintext = decrypt_ctr_envelope(&self.auth_key, packet, x)?;

        let incoming_seq =
            u32::from_be_bytes([plaintext[0], plaintext[1], plaintext[2], plaintext[3]]);
        let incoming_counter = counter_from_seq(incoming_seq);
        if !self.register_incoming_counter(incoming_counter) {
            return Err(SignalingError::ReplayDetected(incoming_counter));
        }

        Ok(DecryptedSignalingPacket {
            seq: incoming_counter,
            payload: plaintext[4..].to_vec(),
        })
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
}
