//! Port of `mtproto/details/mtproto_serialized_request.cpp` and message framing/wrapping logic
//! from `mtproto/session_private.cpp` and `mtproto/connection_abstract.cpp`.
//!
//! Handles unencrypted/encrypted packet framing, `msg_container` packing/unpacking,
//! `gzip_packed` compression/decompression with size & nesting limits, `invokeAfterMsg`,
//! and `invokeWithLayer(initConnection(...))` wrapping.

use std::sync::atomic::{AtomicU64, Ordering};

use tg_crypto::{
    AuthKey, DecryptedMessage, MAX_RANDOM_PADDING_V2, PACKET_HEADER_SIZE, Side, decrypt_message_v2,
    encrypt_reserved_v2,
};
use tg_tl::mtproto::{CoreMessage, GzipPacked, MsgContainer, types as mtp_types};
use tg_tl::{
    Boxed, Cursor, Deserializable, Identifiable, RawObject, Serializable, enums as tl_enums,
    functions as tl_functions, types as tl_types,
};

use crate::config::ClientConfig;
use crate::error::InvocationError;

/// Maximum incoming/outgoing MTProto message length (`kMaxMessageLength = 16 MiB`).
pub const MAX_MESSAGE_LENGTH: usize = 16 * 1024 * 1024;
/// Maximum unpacked gzip message budget (`kMaxUnpackedMessageLength = 32 MiB`).
pub const MAX_UNPACKED_MESSAGE_LENGTH: usize = 32 * 1024 * 1024;
/// Maximum nested `gzip_packed` recursion depth (`kMaxGzipNesting = 64`).
pub const MAX_GZIP_NESTING: usize = 64;
/// Maximum combined nesting depth across `msg_container` and `gzip_packed` wrappers.
pub const MAX_RECEIVE_NESTING: usize = MAX_GZIP_NESTING;
/// Maximum cumulative inner messages accepted from one incoming MTProto packet across
/// all nested `msg_container` and `gzip_packed` layers.
pub const MAX_RECEIVE_MESSAGES: usize = 4_096;
/// Maximum cumulative allocated/unpacked bytes across all `msg_container` and `gzip_packed`
/// layers within one incoming MTProto packet.
pub const MAX_RECEIVE_ALLOC_BYTES: usize = MAX_UNPACKED_MESSAGE_LENGTH;
/// Container split threshold in bytes (`kCutContainerOnSize = 16 * 1024` ints = 64 KiB).
pub const CUT_CONTAINER_ON_BYTES: usize = 16 * 1024 * 4;
/// Minimum outgoing request body size before attempting `gzip_packed` compression.
pub const GZIP_COMPRESS_THRESHOLD: usize = 512;

/// Shared nesting-depth, message-count, and allocation budget enforced across
/// `msg_container` and `gzip_packed` unpacking for a single incoming MTProto packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiveBudget {
    depth: usize,
    max_depth: usize,
    messages_remaining: usize,
    bytes_remaining: usize,
}

impl ReceiveBudget {
    /// Creates a new [`ReceiveBudget`] with explicit depth, message-count, and byte limits.
    pub const fn new(max_depth: usize, max_messages: usize, max_bytes: usize) -> Self {
        Self {
            depth: 0,
            max_depth,
            messages_remaining: max_messages,
            bytes_remaining: max_bytes,
        }
    }

    /// Returns the current combined container/gzip nesting depth.
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Returns the remaining inner message count budget.
    pub const fn messages_remaining(&self) -> usize {
        self.messages_remaining
    }

    /// Returns the remaining allocation/decompression byte budget.
    pub const fn bytes_remaining(&self) -> usize {
        self.bytes_remaining
    }

    /// Enters one level of container or gzip nesting, failing if `max_depth` is reached.
    pub fn enter_nesting(&mut self) -> Result<(), InvocationError> {
        if self.depth >= self.max_depth {
            return Err(InvocationError::Protocol(format!(
                "receive nesting exceeds {}",
                self.max_depth
            )));
        }
        self.depth += 1;
        Ok(())
    }

    /// Leaves one level of container or gzip nesting.
    pub fn leave_nesting(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// Charges `count` messages against the shared receive message-count budget.
    pub fn charge_messages(&mut self, count: usize) -> Result<(), InvocationError> {
        if count > self.messages_remaining {
            return Err(InvocationError::Protocol(
                "receive message count budget exhausted".to_string(),
            ));
        }
        self.messages_remaining -= count;
        Ok(())
    }

    /// Charges `bytes` against the shared receive allocation/decompression byte budget.
    pub fn charge_bytes(&mut self, bytes: usize) -> Result<(), InvocationError> {
        if bytes > self.bytes_remaining {
            return Err(InvocationError::Protocol(
                "receive allocation budget exhausted".to_string(),
            ));
        }
        self.bytes_remaining -= bytes;
        Ok(())
    }
}

impl Default for ReceiveBudget {
    fn default() -> Self {
        Self::new(
            MAX_RECEIVE_NESTING,
            MAX_RECEIVE_MESSAGES,
            MAX_RECEIVE_ALLOC_BYTES,
        )
    }
}

/// Unique client-side request identifier (`mtpRequestId`).
pub type RequestId = u64;

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Allocates the next globally unique non-zero [`RequestId`] (`details::GetNextRequestId`).
pub fn next_request_id() -> RequestId {
    let id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    if id == 0 {
        NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
    } else {
        id
    }
}

/// Returns `true` if a TL message starting with `constructor_id` is content-related and requires
/// acknowledgement (odd `seq_no`), matching `SerializedRequest::needAck()`.
pub const fn need_ack_for_constructor(constructor_id: u32) -> bool {
    !matches!(
        constructor_id,
        MsgContainer::CONSTRUCTOR_ID
            | mtp_types::MsgsAck::CONSTRUCTOR_ID
            | mtp_types::HttpWait::CONSTRUCTOR_ID
            | mtp_types::BadMsgNotification::CONSTRUCTOR_ID
            | mtp_types::MsgsAllInfo::CONSTRUCTOR_ID
            | mtp_types::MsgsStateInfo::CONSTRUCTOR_ID
            | mtp_types::MsgDetailedInfo::CONSTRUCTOR_ID
            | mtp_types::MsgNewDetailedInfo::CONSTRUCTOR_ID
    )
}

/// Inspects the first 4 bytes of a serialized TL body to determine whether it needs ack.
pub fn body_needs_ack(body: &[u8]) -> bool {
    if body.len() < 4 {
        return true;
    }
    let ctor = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
    need_ack_for_constructor(ctor)
}

/// Encodes an unencrypted MTProto message packet (`auth_key_id = 0`, `msg_id`, `msg_len`, `body`)
/// matching `AbstractConnection::prepareNotSecurePacket`.
pub fn encode_unencrypted_packet(msg_id: u64, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(20 + body.len());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&msg_id.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// Parses an unencrypted MTProto response packet (`AbstractConnection::parseNotSecureResponse`).
///
/// Returns `(msg_id, body_slice)`.
pub fn decode_unencrypted_packet(packet: &[u8]) -> Result<(u64, &[u8]), InvocationError> {
    if packet.len() < 24 {
        return Err(InvocationError::Protocol(format!(
            "unencrypted response too short: {} bytes",
            packet.len()
        )));
    }
    let auth_key_id = u64::from_le_bytes([
        packet[0], packet[1], packet[2], packet[3], packet[4], packet[5], packet[6], packet[7],
    ]);
    if auth_key_id != 0 {
        return Err(InvocationError::Protocol(format!(
            "expected auth_key_id 0 in unencrypted response, got 0x{auth_key_id:016x}"
        )));
    }
    let msg_id = u64::from_le_bytes([
        packet[8], packet[9], packet[10], packet[11], packet[12], packet[13], packet[14],
        packet[15],
    ]);
    if (msg_id & 0x03) != 1 {
        return Err(InvocationError::Protocol(format!(
            "invalid server msg_id 0x{msg_id:016x} in unencrypted response"
        )));
    }
    let msg_len = u32::from_le_bytes([packet[16], packet[17], packet[18], packet[19]]) as usize;
    if msg_len < 4 || (msg_len & 0x03) != 0 || 20 + msg_len > packet.len() {
        return Err(InvocationError::Protocol(format!(
            "invalid msg_len {msg_len} in unencrypted response of {} bytes",
            packet.len()
        )));
    }
    Ok((msg_id, &packet[20..20 + msg_len]))
}

/// Decrypted MTProto 2.0 inner message envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecryptedEnvelope {
    /// Server salt echoed by the server.
    pub server_salt: u64,
    /// Session ID.
    pub session_id: u64,
    /// Outer message ID.
    pub msg_id: u64,
    /// Outer sequence number.
    pub seq_no: u32,
    /// Inner TL message body.
    pub body: Vec<u8>,
}

impl DecryptedEnvelope {
    /// Returns true if the outer message is content-related (`(seq_no & 1) != 0`).
    pub const fn need_ack(&self) -> bool {
        (self.seq_no & 1) != 0
    }

    /// Extracts server timestamp (upper 32 bits of `msg_id`).
    pub const fn server_time(&self) -> i32 {
        (self.msg_id >> 32) as i32
    }
}

/// Builds and encrypts a complete MTProto 2.0 wire packet (`SessionPrivate::sendSecureRequest`).
pub fn build_encrypted_packet(
    salt: u64,
    session_id: u64,
    msg_id: u64,
    seq_no: u32,
    body: &[u8],
    auth_key: &AuthKey,
    side: Side,
) -> Vec<u8> {
    // One buffer end to end: reserved `auth_key_id || msg_key`, the 32-byte inner header, the
    // body and room for the random padding, encrypted in place by tg-crypto.
    let mut packet =
        Vec::with_capacity(PACKET_HEADER_SIZE + 32 + body.len() + MAX_RANDOM_PADDING_V2);
    packet.resize(PACKET_HEADER_SIZE, 0);
    packet.extend_from_slice(&salt.to_le_bytes());
    packet.extend_from_slice(&session_id.to_le_bytes());
    packet.extend_from_slice(&msg_id.to_le_bytes());
    packet.extend_from_slice(&seq_no.to_le_bytes());
    packet.extend_from_slice(&(body.len() as u32).to_le_bytes());
    packet.extend_from_slice(body);
    match encrypt_reserved_v2(packet, auth_key, side) {
        Ok(wire) => wire,
        Err(_) => unreachable!("the 24-byte packet header is reserved above"),
    }
}

/// Decrypts and parses an incoming MTProto 2.0 wire packet (`SessionPrivate::handleReceived`).
pub fn parse_encrypted_packet(
    packet: &[u8],
    auth_key: &AuthKey,
    expected_side: Side,
) -> Result<DecryptedEnvelope, InvocationError> {
    check_encrypted_packet_len(packet.len())?;
    envelope_from(decrypt_message_v2(packet, auth_key, expected_side)?)
}

fn check_encrypted_packet_len(len: usize) -> Result<(), InvocationError> {
    if len > MAX_MESSAGE_LENGTH + 24 + 1024 {
        return Err(InvocationError::Protocol(format!(
            "encrypted packet exceeds limit: {len} bytes"
        )));
    }
    Ok(())
}

fn envelope_from(decrypted: DecryptedMessage) -> Result<DecryptedEnvelope, InvocationError> {
    if decrypted.body.len() > MAX_MESSAGE_LENGTH {
        return Err(InvocationError::Protocol(format!(
            "invalid inner message length {}",
            decrypted.body.len()
        )));
    }
    Ok(DecryptedEnvelope {
        server_salt: decrypted.salt as u64,
        session_id: decrypted.session_id as u64,
        msg_id: decrypted.msg_id as u64,
        seq_no: decrypted.seq_no as u32,
        body: decrypted.body,
    })
}

/// Packs multiple messages into a `msg_container#73f1f8dc`.
pub fn pack_container(messages: Vec<CoreMessage>) -> Vec<u8> {
    Boxed(MsgContainer { messages }).to_bytes()
}

/// Unpacks a `msg_container#73f1f8dc` body into its inner [`CoreMessage`]s, validating
/// MTProto 4-byte message alignment and enforcing the default [`ReceiveBudget`].
pub fn unpack_container(body: &[u8]) -> Result<Vec<CoreMessage>, InvocationError> {
    let mut budget = ReceiveBudget::default();
    unpack_container_with_budget(body, &mut budget)
}

/// Unpacks a `msg_container#73f1f8dc` body into its inner [`CoreMessage`]s while charging
/// message count and allocation bytes against the shared `budget` before allocating.
pub fn unpack_container_with_budget(
    body: &[u8],
    budget: &mut ReceiveBudget,
) -> Result<Vec<CoreMessage>, InvocationError> {
    let mut cursor = Cursor::new(body);
    let ctor = cursor.read_u32()?;
    if ctor != MsgContainer::CONSTRUCTOR_ID {
        return Err(InvocationError::Protocol(format!(
            "expected msg_container constructor 0x{:08x}, got 0x{ctor:08x}",
            MsgContainer::CONSTRUCTOR_ID
        )));
    }
    let count = cursor.read_i32()?;
    if count < 0 {
        return Err(InvocationError::Protocol(format!(
            "negative msg_container message count {count}"
        )));
    }
    let count_usize = count as usize;
    // Charge message count before allocating or iterating so container bombs fail fast.
    budget.charge_messages(count_usize)?;

    let per_msg_overhead = core::mem::size_of::<CoreMessage>();
    let prealloc = count_usize
        .min(cursor.remaining() / 20)
        .min(budget.bytes_remaining() / per_msg_overhead.max(1))
        .min(1024);
    let mut messages = Vec::with_capacity(prealloc);

    for _ in 0..count_usize {
        let msg_id = cursor.read_i64()?;
        let seqno = cursor.read_i32()?;
        let bytes = cursor.read_i32()?;
        if bytes < 4 || (bytes & 0x03) != 0 {
            return Err(InvocationError::Protocol(format!(
                "invalid inner message length {bytes} in msg_container"
            )));
        }
        let len = bytes as usize;
        let payload = cursor.read_slice(len)?;
        budget.charge_bytes(len.saturating_add(per_msg_overhead))?;
        messages.push(CoreMessage {
            msg_id,
            seqno,
            body: RawObject(payload.to_vec()),
        });
    }
    Ok(messages)
}

/// Returns `true` for `upload.saveFilePart` / `upload.saveBigFilePart` requests. Their payload is
/// usually already-compressed media, so deflating it costs CPU without shrinking it (tdesktop
/// never compresses outgoing requests).
pub fn is_file_part_upload(body: &[u8]) -> bool {
    body.first_chunk::<4>()
        .map(|ctor| u32::from_le_bytes(*ctor))
        .is_some_and(|ctor| {
            ctor == tl_functions::upload::SaveFilePart::CONSTRUCTOR_ID
                || ctor == tl_functions::upload::SaveBigFilePart::CONSTRUCTOR_ID
        })
}

/// Compresses `body` into `gzip_packed#3072cfa1` if `body.len() >= GZIP_COMPRESS_THRESHOLD`
/// and the gzipped TL encoding is strictly smaller than `body`.
pub fn gzip_pack_if_smaller(body: &[u8]) -> Vec<u8> {
    if body.len() < GZIP_COMPRESS_THRESHOLD {
        return body.to_vec();
    }
    let Ok(packed) = GzipPacked::pack(body) else {
        return body.to_vec();
    };
    let wrapped = Boxed(packed).to_bytes();
    if wrapped.len() < body.len() {
        wrapped
    } else {
        body.to_vec()
    }
}

/// Decompresses a `gzip_packed#3072cfa1` payload with strict size limit and 4-byte alignment
/// checking (`SessionPrivate::ungzip`).
pub fn gzip_unpack(body: &[u8], size_limit: usize) -> Result<Vec<u8>, InvocationError> {
    if size_limit == 0 {
        return Err(InvocationError::Protocol(
            "gzip unpacked budget exhausted".to_string(),
        ));
    }
    let packed = Boxed::<GzipPacked>::from_bytes(body)?.0;
    let out = packed.unpack_with_limit(size_limit)?;
    if out.is_empty() || (out.len() & 0x03) != 0 {
        return Err(InvocationError::Protocol(format!(
            "unpacked gzip payload has invalid length {}",
            out.len()
        )));
    }
    Ok(out)
}

/// Unwraps one or more nested `gzip_packed` layers enforcing `MAX_GZIP_NESTING` and `budget`.
pub fn unwrap_gzip_layers(
    body: &[u8],
    budget: &mut usize,
    depth: usize,
) -> Result<Vec<u8>, InvocationError> {
    let remaining_depth = MAX_GZIP_NESTING.saturating_sub(depth);
    let mut shared = ReceiveBudget::new(remaining_depth, MAX_RECEIVE_MESSAGES, *budget);
    let result = unwrap_gzip_layers_with_budget(body, &mut shared);
    *budget = shared.bytes_remaining();
    result
}

/// Unwraps one or more nested `gzip_packed` layers using the shared [`ReceiveBudget`] for
/// combined depth and allocation accounting.
pub fn unwrap_gzip_layers_with_budget(
    body: &[u8],
    budget: &mut ReceiveBudget,
) -> Result<Vec<u8>, InvocationError> {
    let mut current = body.to_vec();
    let mut layers = 0usize;
    while current.len() >= 4
        && u32::from_le_bytes([current[0], current[1], current[2], current[3]])
            == GzipPacked::CONSTRUCTOR_ID
    {
        if let Err(e) = budget.enter_nesting() {
            for _ in 0..layers {
                budget.leave_nesting();
            }
            return Err(e);
        }
        layers += 1;
        let unpacked = match gzip_unpack(&current, budget.bytes_remaining()) {
            Ok(u) => u,
            Err(e) => {
                for _ in 0..layers {
                    budget.leave_nesting();
                }
                return Err(e);
            }
        };
        if let Err(e) = budget.charge_bytes(unpacked.len()) {
            for _ in 0..layers {
                budget.leave_nesting();
            }
            return Err(e);
        }
        current = unpacked;
    }
    for _ in 0..layers {
        budget.leave_nesting();
    }
    Ok(current)
}

/// Wraps `body` in `invokeAfterMsg#cb9f372d { msg_id: after_msg_id, query: body }`
/// using the generated generic [`tg_tl::functions::InvokeAfterMsg`] (`WrapInvokeAfter` in
/// `session_private.cpp`).
pub fn wrap_invoke_after_msg(after_msg_id: u64, body: &[u8]) -> Vec<u8> {
    tl_functions::InvokeAfterMsg {
        msg_id: after_msg_id as i64,
        query: RawObject(body.to_vec()),
    }
    .to_bytes()
}

/// Wraps `body` in `invokeWithLayer(layer, initConnection(config..., body))` using the
/// generated generic [`tg_tl::functions::InvokeWithLayer`] and [`tg_tl::functions::InitConnection`]
/// (`SessionPrivate::tryToSend` initWrapper in `session_private.cpp`).
pub fn wrap_init_connection(
    config: &ClientConfig,
    layer: i32,
    is_cdn: bool,
    body: &[u8],
) -> Vec<u8> {
    let device_model = if is_cdn {
        "n/a".to_string()
    } else {
        config.device_model.clone()
    };
    let system_version = if is_cdn {
        "n/a".to_string()
    } else {
        config.system_version.clone()
    };
    let proxy = config.effective_init_proxy().map(|p| {
        tl_enums::InputClientProxy::InputClientProxy(tl_types::InputClientProxy {
            address: p.address,
            port: p.port,
        })
    });
    // Match `SessionPrivate::prepareInitParams` (`mtproto/session_private.cpp`):
    // wrap timezone offset into `[-12h, +14h]` modulo 24h and round symmetrically to 15-min steps.
    let params = config.tz_offset_secs.map(|tz_offset| {
        let mut sliced = tz_offset;
        while sliced < -12 * 3600 {
            sliced += 24 * 3600;
        }
        while sliced > 14 * 3600 {
            sliced -= 24 * 3600;
        }
        let sign = if sliced < 0 { -1.0 } else { 1.0 };
        let rounded = ((f64::from(sliced.abs()) / 900.0).round() * 900.0) * sign;
        tl_enums::JSONValue::JsonObject(tl_types::JsonObject {
            value: vec![tl_enums::JSONObjectValue::JsonObjectValue(
                tl_types::JsonObjectValue {
                    key: "tz_offset".to_string(),
                    value: tl_enums::JSONValue::JsonNumber(tl_types::JsonNumber { value: rounded }),
                },
            )],
        })
    });

    tl_functions::InvokeWithLayer {
        layer,
        query: tl_functions::InitConnection {
            api_id: config.api_id,
            device_model,
            system_version,
            app_version: config.app_version.clone(),
            system_lang_code: config.system_lang_code.clone(),
            lang_pack: config.lang_pack.clone(),
            lang_code: config.lang_code.clone(),
            proxy,
            params,
            query: RawObject(body.to_vec()),
        },
    }
    .to_bytes()
}
