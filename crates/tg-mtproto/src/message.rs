//! Port of `mtproto/details/mtproto_serialized_request.cpp` and message framing/wrapping logic
//! from `mtproto/session_private.cpp` and `mtproto/connection_abstract.cpp`.
//!
//! Handles unencrypted/encrypted packet framing, `msg_container` packing/unpacking,
//! `gzip_packed` compression/decompression with size & nesting limits, `invokeAfterMsg`,
//! and `invokeWithLayer(initConnection(...))` wrapping.

use std::sync::atomic::{AtomicU64, Ordering};

use tg_crypto::{AuthKey, Side, decrypt_message_v2, encrypt_data_v2};
use tg_tl::mtproto::{CoreMessage, GzipPacked, MsgContainer, types as mtp_types};
use tg_tl::{
    Boxed, Deserializable, Identifiable, RawObject, Serializable, enums as tl_enums,
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
/// Container split threshold in bytes (`kCutContainerOnSize = 16 * 1024` ints = 64 KiB).
pub const CUT_CONTAINER_ON_BYTES: usize = 16 * 1024 * 4;
/// Minimum outgoing request body size before attempting `gzip_packed` compression.
pub const GZIP_COMPRESS_THRESHOLD: usize = 512;

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
    let mut plaintext = Vec::with_capacity(32 + body.len());
    plaintext.extend_from_slice(&salt.to_le_bytes());
    plaintext.extend_from_slice(&session_id.to_le_bytes());
    plaintext.extend_from_slice(&msg_id.to_le_bytes());
    plaintext.extend_from_slice(&seq_no.to_le_bytes());
    plaintext.extend_from_slice(&(body.len() as u32).to_le_bytes());
    plaintext.extend_from_slice(body);
    encrypt_data_v2(&plaintext, auth_key, side)
}

/// Decrypts and parses an incoming MTProto 2.0 wire packet (`SessionPrivate::handleReceived`).
pub fn parse_encrypted_packet(
    packet: &[u8],
    auth_key: &AuthKey,
    expected_side: Side,
) -> Result<DecryptedEnvelope, InvocationError> {
    if packet.len() > MAX_MESSAGE_LENGTH + 24 + 1024 {
        return Err(InvocationError::Protocol(format!(
            "encrypted packet exceeds limit: {} bytes",
            packet.len()
        )));
    }
    let decrypted = decrypt_message_v2(packet, auth_key, expected_side)?;
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
/// MTProto 4-byte message alignment.
pub fn unpack_container(body: &[u8]) -> Result<Vec<CoreMessage>, InvocationError> {
    let container = Boxed::<MsgContainer>::from_bytes(body)?.0;
    for msg in &container.messages {
        let len = msg.body.0.len();
        if len < 4 || (len & 0x03) != 0 {
            return Err(InvocationError::Protocol(format!(
                "invalid inner message length {len} in msg_container"
            )));
        }
    }
    Ok(container.messages)
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
    let mut current = body.to_vec();
    let mut cur_depth = depth;
    while current.len() >= 4
        && u32::from_le_bytes([current[0], current[1], current[2], current[3]])
            == GzipPacked::CONSTRUCTOR_ID
    {
        if cur_depth >= MAX_GZIP_NESTING {
            return Err(InvocationError::Protocol(format!(
                "gzip nesting exceeds {MAX_GZIP_NESTING}"
            )));
        }
        let unpacked = gzip_unpack(&current, *budget)?;
        *budget = budget.saturating_sub(unpacked.len());
        current = unpacked;
        cur_depth += 1;
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
