//! RFC 5766 TURN client/server framing, Telegram TCP reflector client/server framing,
//! and SCTP data-channel framing/routing for 1:1 calls.
//!
//! Port origin:
//! - `TelegramMessenger/tgcalls`: `tgcalls/v2/RawTcpSocket.cpp`
//! - `TelegramMessenger/tgcalls`: `tgcalls/v2/ReflectorPort.cpp` (TCP mode)
//! - `TelegramMessenger/tgcalls`: `tgcalls/TurnCustomizerImpl.cpp` (RFC 5766 TURN)
//! - `TelegramMessenger/tgcalls`: `tgcalls/v2/SctpDataChannelProviderInterfaceImpl.cpp`

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha1::Sha1;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Mutex;

use crate::error::{VoipError, VoipResult};
use crate::reflector::{
    REFLECTOR_SPECIAL_TAG, ReflectorIncoming, ReflectorTag, STUN_MAGIC_COOKIE,
    TELEGRAM_STUN_SOFTWARE, decode_reflector_udp_packet, encode_reflector_tcp_hello,
    encode_reflector_udp_data, is_stun_packet,
};

// ============================================================================
// RFC 5766 TURN Constants & Message Types
// ============================================================================

/// TURN Allocate Request (`0x0003`).
pub const TURN_ALLOCATE_REQUEST: u16 = 0x0003;
/// TURN Allocate Success Response (`0x0103`).
pub const TURN_ALLOCATE_RESPONSE: u16 = 0x0103;
/// TURN Allocate Error Response (`0x0113`).
pub const TURN_ALLOCATE_ERROR: u16 = 0x0113;
/// TURN Refresh Request (`0x0004`).
pub const TURN_REFRESH_REQUEST: u16 = 0x0004;
/// TURN Refresh Success Response (`0x0104`).
pub const TURN_REFRESH_RESPONSE: u16 = 0x0104;
/// TURN Send Indication (`0x0016`).
pub const TURN_SEND_INDICATION: u16 = 0x0016;
/// TURN Data Indication (`0x0017`).
pub const TURN_DATA_INDICATION: u16 = 0x0017;
/// TURN CreatePermission Request (`0x0008`).
pub const TURN_CREATE_PERMISSION_REQUEST: u16 = 0x0008;
/// TURN CreatePermission Success Response (`0x0108`).
pub const TURN_CREATE_PERMISSION_RESPONSE: u16 = 0x0108;
/// TURN ChannelBind Request (`0x0009`).
pub const TURN_CHANNEL_BIND_REQUEST: u16 = 0x0009;
/// TURN ChannelBind Success Response (`0x0109`).
pub const TURN_CHANNEL_BIND_RESPONSE: u16 = 0x0109;

/// First valid RFC 5766 ChannelData channel number (`0x4000`).
pub const TURN_MIN_CHANNEL_NUMBER: u16 = 0x4000;
/// Last valid RFC 5766 ChannelData channel number (`0x7FFF`).
pub const TURN_MAX_CHANNEL_NUMBER: u16 = 0x7FFF;
/// Default TURN allocation lifetime in seconds (RFC 5766 §6.2).
pub const TURN_DEFAULT_LIFETIME_SECS: u32 = 600;

/// RFC 5389 / RFC 5766 attribute type constants.
pub mod turn_attr {
    /// `USERNAME`
    pub const USERNAME: u16 = 0x0006;
    /// `MESSAGE-INTEGRITY` (20-byte HMAC-SHA1)
    pub const MESSAGE_INTEGRITY: u16 = 0x0008;
    /// `ERROR-CODE`
    pub const ERROR_CODE: u16 = 0x0009;
    /// `CHANNEL-NUMBER`
    pub const CHANNEL_NUMBER: u16 = 0x000C;
    /// `LIFETIME`
    pub const LIFETIME: u16 = 0x000D;
    /// `XOR-PEER-ADDRESS`
    pub const XOR_PEER_ADDRESS: u16 = 0x0012;
    /// `DATA`
    pub const DATA: u16 = 0x0013;
    /// `REALM`
    pub const REALM: u16 = 0x0014;
    /// `NONCE`
    pub const NONCE: u16 = 0x0015;
    /// `XOR-RELAYED-ADDRESS`
    pub const XOR_RELAYED_ADDRESS: u16 = 0x0016;
    /// `REQUESTED-TRANSPORT` (17 = UDP)
    pub const REQUESTED_TRANSPORT: u16 = 0x0019;
    /// `XOR-MAPPED-ADDRESS`
    pub const XOR_MAPPED_ADDRESS: u16 = 0x0020;
    /// `SOFTWARE`
    pub const SOFTWARE: u16 = 0x8022;
    /// `FINGERPRINT`
    pub const FINGERPRINT: u16 = 0x8028;
}

// ============================================================================
// Pure-Rust RFC 1321 MD5 (for RFC 5389 §15.4 TURN long-term credential key)
// ============================================================================

/// Computes the 16-byte RFC 1321 MD5 digest of `input`.
#[must_use]
pub fn md5_digest(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let mut a0: u32 = 0x6745_2301;
    let mut b0: u32 = 0xefcd_ab89;
    let mut c0: u32 = 0x98ba_dcfe;
    let mut d0: u32 = 0x1032_5476;

    for chunk in msg.as_chunks::<64>().0 {
        let mut m = [0u32; 16];
        for (slot, word) in m.iter_mut().zip(chunk.as_chunks::<4>().0) {
            *slot = u32::from_le_bytes(*word);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = temp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

/// Computes the RFC 5389 §15.4 TURN long-term credential key: `MD5(username ":" realm ":" password)`.
#[must_use]
pub fn turn_long_term_key(username: &str, realm: &str, password: &str) -> [u8; 16] {
    let combined = format!("{username}:{realm}:{password}");
    md5_digest(combined.as_bytes())
}

// ============================================================================
// RFC 5766 TURN Message Builders & Parsers
// ============================================================================

fn append_attr(buf: &mut Vec<u8>, attr_type: u16, value: &[u8]) {
    buf.extend_from_slice(&attr_type.to_be_bytes());
    buf.extend_from_slice(&(value.len() as u16).to_be_bytes());
    buf.extend_from_slice(value);
    let pad = (4 - (value.len() % 4)) % 4;
    for _ in 0..pad {
        buf.push(0);
    }
}

fn crc32_ieee(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn finish_turn_message(
    msg_type: u16,
    tx_id: [u8; 12],
    mut attrs: Vec<u8>,
    integrity_key: Option<&[u8]>,
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(20 + attrs.len() + 32);
    msg.extend_from_slice(&msg_type.to_be_bytes());
    msg.extend_from_slice(&0u16.to_be_bytes());
    msg.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    msg.extend_from_slice(&tx_id);
    msg.append(&mut attrs);

    if let Some(key) = integrity_key {
        let len_with_mi = (msg.len() - 20 + 24) as u16;
        msg[2..4].copy_from_slice(&len_with_mi.to_be_bytes());
        if let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(key) {
            mac.update(&msg);
            let digest = mac.finalize().into_bytes();
            append_attr(&mut msg, turn_attr::MESSAGE_INTEGRITY, &digest);
        }
    }

    let len_with_fp = (msg.len() - 20 + 8) as u16;
    msg[2..4].copy_from_slice(&len_with_fp.to_be_bytes());
    let fp = crc32_ieee(&msg) ^ 0x5354_554E;
    append_attr(&mut msg, turn_attr::FINGERPRINT, &fp.to_be_bytes());
    msg
}

fn encode_xor_addr(addr: SocketAddr, tx_id: &[u8; 12]) -> Vec<u8> {
    let xport = addr.port() ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
    match addr.ip() {
        IpAddr::V4(v4) => {
            let mut out = Vec::with_capacity(8);
            out.push(0);
            out.push(0x01);
            out.extend_from_slice(&xport.to_be_bytes());
            let xip = u32::from(v4) ^ STUN_MAGIC_COOKIE;
            out.extend_from_slice(&xip.to_be_bytes());
            out
        }
        IpAddr::V6(v6) => {
            let mut out = Vec::with_capacity(20);
            out.push(0);
            out.push(0x02);
            out.extend_from_slice(&xport.to_be_bytes());
            let mut mask = [0u8; 16];
            mask[..4].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
            mask[4..16].copy_from_slice(tx_id);
            let octets = v6.octets();
            for i in 0..16 {
                out.push(octets[i] ^ mask[i]);
            }
            out
        }
    }
}

fn decode_xor_addr(val: &[u8], tx_id: &[u8; 12]) -> VoipResult<SocketAddr> {
    if val.len() < 8 {
        return Err(VoipError::Stun("XOR address attribute too short".into()));
    }
    let family = val[1];
    let port = u16::from_be_bytes([val[2], val[3]]) ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
    match family {
        0x01 => {
            let xip = u32::from_be_bytes([val[4], val[5], val[6], val[7]]) ^ STUN_MAGIC_COOKIE;
            Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(xip)), port))
        }
        0x02 if val.len() >= 20 => {
            let mut mask = [0u8; 16];
            mask[..4].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
            mask[4..16].copy_from_slice(tx_id);
            let mut octets = [0u8; 16];
            for i in 0..16 {
                octets[i] = val[4 + i] ^ mask[i];
            }
            Ok(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(octets)), port))
        }
        _ => Err(VoipError::Stun(format!(
            "unsupported XOR address family 0x{family:02x}"
        ))),
    }
}

fn encode_error_code(code: u16, reason: &str) -> Vec<u8> {
    let class = ((code / 100) & 0x07) as u8;
    let number = (code % 100) as u8;
    let mut out = Vec::with_capacity(4 + reason.len());
    out.push(0);
    out.push(0);
    out.push(class);
    out.push(number);
    out.extend_from_slice(reason.as_bytes());
    out
}

/// Builds an authenticated RFC 5766 TURN Allocate Request (UDP transport).
#[must_use]
pub fn build_turn_allocate_auth_request(
    tx_id: [u8; 12],
    username: &str,
    realm: &str,
    nonce: &str,
    integrity_key: &[u8; 16],
) -> Vec<u8> {
    let mut attrs = Vec::new();
    append_attr(&mut attrs, turn_attr::REQUESTED_TRANSPORT, &[17, 0, 0, 0]);
    append_attr(
        &mut attrs,
        turn_attr::SOFTWARE,
        TELEGRAM_STUN_SOFTWARE.as_bytes(),
    );
    append_attr(&mut attrs, turn_attr::USERNAME, username.as_bytes());
    append_attr(&mut attrs, turn_attr::REALM, realm.as_bytes());
    append_attr(&mut attrs, turn_attr::NONCE, nonce.as_bytes());
    finish_turn_message(TURN_ALLOCATE_REQUEST, tx_id, attrs, Some(integrity_key))
}

/// Builds an authenticated RFC 5766 TURN Refresh Request.
#[must_use]
pub fn build_turn_refresh_request(
    tx_id: [u8; 12],
    lifetime_secs: u32,
    username: &str,
    realm: &str,
    nonce: &str,
    integrity_key: &[u8; 16],
) -> Vec<u8> {
    let mut attrs = Vec::new();
    append_attr(
        &mut attrs,
        turn_attr::LIFETIME,
        &lifetime_secs.to_be_bytes(),
    );
    append_attr(&mut attrs, turn_attr::USERNAME, username.as_bytes());
    append_attr(&mut attrs, turn_attr::REALM, realm.as_bytes());
    append_attr(&mut attrs, turn_attr::NONCE, nonce.as_bytes());
    finish_turn_message(TURN_REFRESH_REQUEST, tx_id, attrs, Some(integrity_key))
}

/// Builds an authenticated RFC 5766 TURN CreatePermission Request for `peer_addr`.
#[must_use]
pub fn build_turn_create_permission_request(
    tx_id: [u8; 12],
    peer_addr: SocketAddr,
    username: &str,
    realm: &str,
    nonce: &str,
    integrity_key: &[u8; 16],
) -> Vec<u8> {
    let mut attrs = Vec::new();
    let xpeer = encode_xor_addr(peer_addr, &tx_id);
    append_attr(&mut attrs, turn_attr::XOR_PEER_ADDRESS, &xpeer);
    append_attr(&mut attrs, turn_attr::USERNAME, username.as_bytes());
    append_attr(&mut attrs, turn_attr::REALM, realm.as_bytes());
    append_attr(&mut attrs, turn_attr::NONCE, nonce.as_bytes());
    finish_turn_message(
        TURN_CREATE_PERMISSION_REQUEST,
        tx_id,
        attrs,
        Some(integrity_key),
    )
}

/// Builds an authenticated RFC 5766 TURN ChannelBind Request binding `channel_number` (`0x4000..=0x7FFF`)
/// to `peer_addr`.
#[must_use]
pub fn build_turn_channel_bind_request(
    tx_id: [u8; 12],
    channel_number: u16,
    peer_addr: SocketAddr,
    username: &str,
    realm: &str,
    nonce: &str,
    integrity_key: &[u8; 16],
) -> Vec<u8> {
    let mut attrs = Vec::new();
    let mut ch = [0u8; 4];
    ch[0..2].copy_from_slice(&channel_number.to_be_bytes());
    append_attr(&mut attrs, turn_attr::CHANNEL_NUMBER, &ch);
    let xpeer = encode_xor_addr(peer_addr, &tx_id);
    append_attr(&mut attrs, turn_attr::XOR_PEER_ADDRESS, &xpeer);
    append_attr(&mut attrs, turn_attr::USERNAME, username.as_bytes());
    append_attr(&mut attrs, turn_attr::REALM, realm.as_bytes());
    append_attr(&mut attrs, turn_attr::NONCE, nonce.as_bytes());
    finish_turn_message(TURN_CHANNEL_BIND_REQUEST, tx_id, attrs, Some(integrity_key))
}

/// Builds an RFC 5766 TURN Send Indication carrying `data` for `peer_addr`.
#[must_use]
pub fn build_turn_send_indication(tx_id: [u8; 12], peer_addr: SocketAddr, data: &[u8]) -> Vec<u8> {
    let mut attrs = Vec::new();
    let xpeer = encode_xor_addr(peer_addr, &tx_id);
    append_attr(&mut attrs, turn_attr::XOR_PEER_ADDRESS, &xpeer);
    append_attr(&mut attrs, turn_attr::DATA, data);
    finish_turn_message(TURN_SEND_INDICATION, tx_id, attrs, None)
}

/// Builds an RFC 5766 TURN Data Indication carrying `data` from `peer_addr`.
#[must_use]
pub fn build_turn_data_indication(tx_id: [u8; 12], peer_addr: SocketAddr, data: &[u8]) -> Vec<u8> {
    let mut attrs = Vec::new();
    let xpeer = encode_xor_addr(peer_addr, &tx_id);
    append_attr(&mut attrs, turn_attr::XOR_PEER_ADDRESS, &xpeer);
    append_attr(&mut attrs, turn_attr::DATA, data);
    finish_turn_message(TURN_DATA_INDICATION, tx_id, attrs, None)
}

/// Returns `true` if `packet` starts with an RFC 5766 ChannelData header (`0x4000..=0x7FFF`).
#[must_use]
pub fn is_turn_channel_data(packet: &[u8]) -> bool {
    packet.len() >= 4 && (packet[0] & 0xC0) == 0x40
}

/// Encodes an RFC 5766 ChannelData datagram (`channel_number_be[2] || len_be[2] || payload`).
#[must_use]
pub fn encode_turn_channel_data(channel_number: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&channel_number.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Decodes an RFC 5766 ChannelData datagram into `(channel_number, payload)`.
pub fn decode_turn_channel_data(packet: &[u8]) -> VoipResult<(u16, Vec<u8>)> {
    if !is_turn_channel_data(packet) {
        return Err(VoipError::Stun("not a TURN ChannelData packet".into()));
    }
    let ch = u16::from_be_bytes([packet[0], packet[1]]);
    let len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    if 4 + len > packet.len() {
        return Err(VoipError::Stun("truncated TURN ChannelData payload".into()));
    }
    Ok((ch, packet[4..4 + len].to_vec()))
}

/// Parsed RFC 5766 TURN STUN message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTurnMessage {
    /// STUN/TURN message type (e.g. `TURN_ALLOCATE_RESPONSE`).
    pub msg_type: u16,
    /// 96-bit transaction ID.
    pub tx_id: [u8; 12],
    /// `XOR-RELAYED-ADDRESS` if present.
    pub relayed_addr: Option<SocketAddr>,
    /// `XOR-MAPPED-ADDRESS` if present.
    pub mapped_addr: Option<SocketAddr>,
    /// `XOR-PEER-ADDRESS` if present.
    pub peer_addr: Option<SocketAddr>,
    /// `DATA` payload if present.
    pub data: Option<Vec<u8>>,
    /// `CHANNEL-NUMBER` if present.
    pub channel_number: Option<u16>,
    /// `LIFETIME` in seconds if present.
    pub lifetime_secs: Option<u32>,
    /// `USERNAME` if present.
    pub username: Option<String>,
    /// `REALM` if present.
    pub realm: Option<String>,
    /// `NONCE` if present.
    pub nonce: Option<String>,
    /// `ERROR-CODE` numeric value (e.g. `401`) if present.
    pub error_code: Option<u16>,
    /// Whether `MESSAGE-INTEGRITY` was present and valid against `verify_key` (if provided).
    pub integrity_valid: bool,
}

/// Parses an RFC 5766 TURN STUN message, verifying FINGERPRINT and (when `verify_key` is
/// supplied) HMAC-SHA1 `MESSAGE-INTEGRITY`.
pub fn parse_turn_message(
    packet: &[u8],
    verify_key: Option<&[u8]>,
) -> VoipResult<ParsedTurnMessage> {
    if !is_stun_packet(packet) {
        return Err(VoipError::Stun("not a valid STUN/TURN packet".into()));
    }
    let msg_type = u16::from_be_bytes([packet[0], packet[1]]);
    let msg_len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    if 20 + msg_len > packet.len() {
        return Err(VoipError::Stun("truncated STUN/TURN body".into()));
    }
    let mut tx_id = [0u8; 12];
    tx_id.copy_from_slice(&packet[8..20]);

    let mut out = ParsedTurnMessage {
        msg_type,
        tx_id,
        relayed_addr: None,
        mapped_addr: None,
        peer_addr: None,
        data: None,
        channel_number: None,
        lifetime_secs: None,
        username: None,
        realm: None,
        nonce: None,
        error_code: None,
        integrity_valid: false,
    };

    let mut cursor = 20usize;
    let end = 20 + msg_len;
    while cursor + 4 <= end {
        let attr_type = u16::from_be_bytes([packet[cursor], packet[cursor + 1]]);
        let attr_len = u16::from_be_bytes([packet[cursor + 2], packet[cursor + 3]]) as usize;
        let val_start = cursor + 4;
        let val_end = val_start + attr_len;
        if val_end > end {
            return Err(VoipError::Stun("truncated STUN/TURN attribute".into()));
        }
        let val = &packet[val_start..val_end];
        match attr_type {
            turn_attr::XOR_RELAYED_ADDRESS => {
                out.relayed_addr = Some(decode_xor_addr(val, &tx_id)?);
            }
            turn_attr::XOR_MAPPED_ADDRESS => {
                out.mapped_addr = Some(decode_xor_addr(val, &tx_id)?);
            }
            turn_attr::XOR_PEER_ADDRESS => {
                out.peer_addr = Some(decode_xor_addr(val, &tx_id)?);
            }
            turn_attr::DATA => {
                out.data = Some(val.to_vec());
            }
            turn_attr::CHANNEL_NUMBER if val.len() >= 2 => {
                out.channel_number = Some(u16::from_be_bytes([val[0], val[1]]));
            }
            turn_attr::LIFETIME if val.len() >= 4 => {
                out.lifetime_secs = Some(u32::from_be_bytes([val[0], val[1], val[2], val[3]]));
            }
            turn_attr::USERNAME => {
                out.username = Some(String::from_utf8_lossy(val).into_owned());
            }
            turn_attr::REALM => {
                out.realm = Some(String::from_utf8_lossy(val).into_owned());
            }
            turn_attr::NONCE => {
                out.nonce = Some(String::from_utf8_lossy(val).into_owned());
            }
            turn_attr::ERROR_CODE if val.len() >= 4 => {
                let class = u16::from(val[2] & 0x07);
                let number = u16::from(val[3]);
                out.error_code = Some(class * 100 + number);
            }
            turn_attr::MESSAGE_INTEGRITY if val.len() == 20 => {
                if let Some(key) = verify_key {
                    let mut prefix = packet[..cursor].to_vec();
                    let adjusted_len = (cursor - 20 + 24) as u16;
                    prefix[2..4].copy_from_slice(&adjusted_len.to_be_bytes());
                    if let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(key) {
                        mac.update(&prefix);
                        out.integrity_valid = mac.verify_slice(val).is_ok();
                    }
                }
            }
            turn_attr::FINGERPRINT => {
                if val.len() != 4 {
                    return Err(VoipError::Stun("invalid FINGERPRINT length".into()));
                }
                let got = u32::from_be_bytes([val[0], val[1], val[2], val[3]]);
                let expected = crc32_ieee(&packet[..cursor]) ^ 0x5354_554E;
                if got != expected {
                    return Err(VoipError::Stun("STUN/TURN FINGERPRINT mismatch".into()));
                }
            }
            _ => {}
        }
        let padded = (attr_len + 3) & !3;
        cursor = val_start + padded;
    }

    Ok(out)
}

// ============================================================================
// RFC 5766 TURN Allocation State Machine (`TurnAllocationClient`)
// ============================================================================

/// Lifecycle state of a [`TurnAllocationClient`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnAllocationState {
    /// Initial state before Allocate Request is acknowledged.
    Unallocated,
    /// Received `401 Unauthorized` with `REALM` + `NONCE`, sent authenticated Allocate Request.
    Authenticating {
        /// Server realm string.
        realm: String,
        /// Server nonce string.
        nonce: String,
    },
    /// Allocation succeeded and `relayed_addr` is active.
    Allocated {
        /// Server realm string.
        realm: String,
        /// Server nonce string.
        nonce: String,
        /// Allocated relay address on the TURN server.
        relayed_addr: SocketAddr,
        /// Allocation lifetime in seconds.
        lifetime_secs: u32,
        /// Monotonic timestamp (ms) when refresh should be sent.
        refresh_at_ms: u64,
    },
}

/// Event produced by [`TurnAllocationClient::handle_incoming`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnIncomingOutcome {
    /// Follow-up TURN control datagram(s) to send immediately (e.g. authenticated Allocate).
    SendControl(Vec<Vec<u8>>),
    /// Newly allocated relay address ready to advertise as a `typ relay` ICE candidate.
    Allocated {
        /// Relayed UDP address assigned by the TURN server.
        relayed_addr: SocketAddr,
    },
    /// Relayed application payload received from `peer_addr`.
    RelayedData {
        /// Remote peer address that sent the packet to our TURN relay.
        peer_addr: SocketAddr,
        /// Unwrapped payload bytes.
        payload: Vec<u8>,
    },
    /// Control acknowledgement (Refresh, CreatePermission, or ChannelBind succeeded).
    ControlAck,
}

/// Client-side RFC 5766 TURN allocation, permission, and channel-binding state machine.
#[derive(Debug, Clone)]
pub struct TurnAllocationClient {
    /// TURN server UDP address.
    pub server_addr: SocketAddr,
    /// Long-term credential username.
    pub username: String,
    /// Long-term credential password.
    pub password: String,
    /// Current allocation state.
    pub state: TurnAllocationState,
    /// Next channel number to assign (`0x4000..=0x7FFF`).
    next_channel: u16,
    /// Bound `peer_addr -> channel_number`.
    peer_channels: HashMap<SocketAddr, u16>,
    /// Reverse `channel_number -> peer_addr`.
    channel_peers: HashMap<u16, SocketAddr>,
    /// Peers for which a CreatePermission / ChannelBind has already been initiated.
    permitted_peers: HashSet<SocketAddr>,
    /// Whether the `typ relay` candidate has already been announced.
    pub announced: bool,
}

impl TurnAllocationClient {
    /// Creates a new unallocated TURN client for `server_addr`.
    #[must_use]
    pub fn new(server_addr: SocketAddr, username: String, password: String) -> Self {
        Self {
            server_addr,
            username,
            password,
            state: TurnAllocationState::Unallocated,
            next_channel: TURN_MIN_CHANNEL_NUMBER,
            peer_channels: HashMap::new(),
            channel_peers: HashMap::new(),
            permitted_peers: HashSet::new(),
            announced: false,
        }
    }

    fn random_tx_id() -> [u8; 12] {
        let mut tx_id = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut tx_id);
        tx_id
    }

    /// Returns the allocated relay address if active.
    #[must_use]
    pub const fn relayed_addr(&self) -> Option<SocketAddr> {
        match self.state {
            TurnAllocationState::Allocated { relayed_addr, .. } => Some(relayed_addr),
            _ => None,
        }
    }

    /// Returns the current `(realm, nonce, key)` credentials once a challenge has been received.
    fn credentials(&self) -> Option<(&str, &str, [u8; 16])> {
        match &self.state {
            TurnAllocationState::Authenticating { realm, nonce }
            | TurnAllocationState::Allocated { realm, nonce, .. } => Some((
                realm.as_str(),
                nonce.as_str(),
                turn_long_term_key(&self.username, realm, &self.password),
            )),
            TurnAllocationState::Unallocated => None,
        }
    }

    /// Builds the initial unauthenticated TURN Allocate Request to elicit a `401` realm/nonce
    /// challenge (or direct allocation on servers without auth).
    #[must_use]
    pub fn start_allocate(&self) -> Vec<u8> {
        let tx_id = Self::random_tx_id();
        crate::reflector::build_turn_allocate_request(tx_id, Some(&self.username), None)
    }

    /// Builds a TURN Refresh Request when `now_ms` has passed the refresh threshold.
    #[must_use]
    pub fn maybe_refresh(&mut self, now_ms: u64) -> Option<Vec<u8>> {
        let (realm, nonce, lifetime_secs) = match &mut self.state {
            TurnAllocationState::Allocated {
                realm,
                nonce,
                lifetime_secs,
                refresh_at_ms,
                ..
            } if now_ms >= *refresh_at_ms => {
                *refresh_at_ms = now_ms.saturating_add(u64::from((*lifetime_secs).max(30)) * 500);
                (realm.clone(), nonce.clone(), *lifetime_secs)
            }
            _ => return None,
        };
        let key = turn_long_term_key(&self.username, &realm, &self.password);
        Some(build_turn_refresh_request(
            Self::random_tx_id(),
            lifetime_secs,
            &self.username,
            &realm,
            &nonce,
            &key,
        ))
    }

    /// Prepares `CreatePermission` and `ChannelBind` requests for `peer_addr` if not yet bound.
    pub fn ensure_peer_channel(&mut self, peer_addr: SocketAddr) -> Vec<Vec<u8>> {
        if self.permitted_peers.contains(&peer_addr) {
            return Vec::new();
        }
        let Some((realm, nonce, key)) = self.credentials() else {
            return Vec::new();
        };
        let realm = realm.to_owned();
        let nonce = nonce.to_owned();
        let ch = if let Some(&existing) = self.peer_channels.get(&peer_addr) {
            existing
        } else {
            let assigned = self.next_channel;
            self.next_channel = if self.next_channel >= TURN_MAX_CHANNEL_NUMBER {
                TURN_MIN_CHANNEL_NUMBER
            } else {
                self.next_channel + 1
            };
            self.peer_channels.insert(peer_addr, assigned);
            self.channel_peers.insert(assigned, peer_addr);
            assigned
        };
        self.permitted_peers.insert(peer_addr);
        vec![
            build_turn_create_permission_request(
                Self::random_tx_id(),
                peer_addr,
                &self.username,
                &realm,
                &nonce,
                &key,
            ),
            build_turn_channel_bind_request(
                Self::random_tx_id(),
                ch,
                peer_addr,
                &self.username,
                &realm,
                &nonce,
                &key,
            ),
        ]
    }

    /// Encapsulates `payload` for `peer_addr` using `ChannelData` when a channel is bound, or
    /// `SendIndication` otherwise.
    #[must_use]
    pub fn wrap_outgoing(&self, peer_addr: SocketAddr, payload: &[u8]) -> Vec<u8> {
        if let Some(&ch) = self.peer_channels.get(&peer_addr) {
            encode_turn_channel_data(ch, payload)
        } else {
            build_turn_send_indication(Self::random_tx_id(), peer_addr, payload)
        }
    }

    /// Processes an incoming UDP datagram from `self.server_addr`.
    pub fn handle_incoming(
        &mut self,
        packet: &[u8],
        now_ms: u64,
    ) -> VoipResult<TurnIncomingOutcome> {
        if is_turn_channel_data(packet) {
            let (ch, payload) = decode_turn_channel_data(packet)?;
            let peer_addr = self
                .channel_peers
                .get(&ch)
                .copied()
                .ok_or_else(|| VoipError::Stun(format!("unbound TURN channel 0x{ch:04x}")))?;
            return Ok(TurnIncomingOutcome::RelayedData { peer_addr, payload });
        }

        let parsed = parse_turn_message(packet, None)?;
        match parsed.msg_type {
            TURN_ALLOCATE_ERROR if parsed.error_code == Some(401) => {
                let realm = parsed.realm.unwrap_or_default();
                let nonce = parsed.nonce.unwrap_or_default();
                let key = turn_long_term_key(&self.username, &realm, &self.password);
                let auth_req = build_turn_allocate_auth_request(
                    Self::random_tx_id(),
                    &self.username,
                    &realm,
                    &nonce,
                    &key,
                );
                self.state = TurnAllocationState::Authenticating { realm, nonce };
                Ok(TurnIncomingOutcome::SendControl(vec![auth_req]))
            }
            TURN_ALLOCATE_RESPONSE => {
                let relayed_addr = parsed.relayed_addr.ok_or_else(|| {
                    VoipError::Stun("TURN AllocateResponse missing XOR-RELAYED-ADDRESS".into())
                })?;
                let lifetime_secs = parsed.lifetime_secs.unwrap_or(TURN_DEFAULT_LIFETIME_SECS);
                let (realm, nonce) = match &self.state {
                    TurnAllocationState::Authenticating { realm, nonce }
                    | TurnAllocationState::Allocated { realm, nonce, .. } => {
                        (realm.clone(), nonce.clone())
                    }
                    TurnAllocationState::Unallocated => (String::new(), String::new()),
                };
                self.state = TurnAllocationState::Allocated {
                    realm,
                    nonce,
                    relayed_addr,
                    lifetime_secs,
                    refresh_at_ms: now_ms.saturating_add(u64::from(lifetime_secs.max(30)) * 500),
                };
                Ok(TurnIncomingOutcome::Allocated { relayed_addr })
            }
            TURN_DATA_INDICATION => {
                let peer_addr = parsed.peer_addr.ok_or_else(|| {
                    VoipError::Stun("TURN DataIndication missing XOR-PEER-ADDRESS".into())
                })?;
                let payload = parsed.data.unwrap_or_default();
                Ok(TurnIncomingOutcome::RelayedData { peer_addr, payload })
            }
            TURN_REFRESH_RESPONSE
            | TURN_CREATE_PERMISSION_RESPONSE
            | TURN_CHANNEL_BIND_RESPONSE => Ok(TurnIncomingOutcome::ControlAck),
            other => Err(VoipError::Stun(format!(
                "unexpected TURN message type 0x{other:04x}"
            ))),
        }
    }
}

/// Formats an SDP `typ relay` candidate for an allocated TURN relay address.
#[must_use]
pub fn format_turn_relay_sdp_candidate(relayed: SocketAddr, base_local: SocketAddr) -> String {
    format!(
        "candidate:turn-1 1 udp 16777215 {} {} typ relay raddr {} rport {}",
        relayed.ip(),
        relayed.port(),
        base_local.ip(),
        base_local.port()
    )
}

// ============================================================================
// Telegram TCP Reflector Framing & Client (`RawTcpSocket.cpp`)
// ============================================================================

/// Encodes a TCP reflector frame with a 2-byte big-endian length prefix (`RawTcpSocket.cpp`).
#[must_use]
pub fn encode_tcp_reflector_frame(payload: &[u8]) -> Vec<u8> {
    let len = payload.len().min(u16::MAX as usize) as u16;
    let mut out = Vec::with_capacity(2 + usize::from(len));
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&payload[..usize::from(len)]);
    out
}

/// Extracts complete length-prefixed frames from `buf`, draining consumed bytes.
pub fn drain_tcp_reflector_frames(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    loop {
        if buf.len() < 2 {
            break;
        }
        let len = u16::from_be_bytes([buf[0], buf[1]]) as usize;
        if buf.len() < 2 + len {
            break;
        }
        let frame = buf[2..2 + len].to_vec();
        buf.drain(..2 + len);
        frames.push(frame);
    }
    frames
}

/// Writes all bytes to `stream` asynchronously without requiring `tokio::io::AsyncWriteExt`.
pub async fn tcp_write_all(stream: &TcpStream, mut data: &[u8]) -> VoipResult<()> {
    while !data.is_empty() {
        stream.writable().await?;
        match stream.try_write(data) {
            Ok(0) => {
                return Err(VoipError::Reflector(
                    "TCP reflector closed while writing".into(),
                ));
            }
            Ok(n) => data = &data[n..],
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Reads available bytes from `stream` into `buf`, returning `Ok(0)` on clean EOF.
pub async fn tcp_read_chunk(stream: &TcpStream, buf: &mut Vec<u8>) -> VoipResult<usize> {
    let mut tmp = [0u8; 4096];
    loop {
        stream.readable().await?;
        match stream.try_read(&mut tmp) {
            Ok(0) => return Ok(0),
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                return Ok(n);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

/// Connected async TCP reflector socket carrying Telegram's 20-byte hello and length-prefixed
/// reflector packets (`ReflectorPort.cpp` + `RawTcpSocket.cpp`).
pub struct TcpReflectorRoute {
    /// Server connection ID (`Endpoint::endpoint_id`).
    pub endpoint_id: i64,
    /// 1-based server ID used in synthetic candidate hostname.
    pub server_id: u8,
    /// Remote TCP reflector address.
    pub addr: SocketAddr,
    /// 16-byte peer tag split into shared 12-byte prefix + 4-byte local random tag.
    pub tag: ReflectorTag,
    /// Remote participant's 4-byte tag (learned from signaling or first packet).
    pub remote_tag: Option<u32>,
    /// Whether the TCP connection and 20-byte hello succeeded.
    pub ready: bool,
    /// Whether the reflector candidate has been announced.
    pub announced: bool,
    /// Active TCP stream once connected.
    pub stream: Option<TcpStream>,
    /// Stream reassembly buffer.
    pub read_buf: Vec<u8>,
}

impl TcpReflectorRoute {
    /// Creates an unconnected [`TcpReflectorRoute`].
    #[must_use]
    pub fn new(endpoint_id: i64, server_id: u8, addr: SocketAddr, tag: ReflectorTag) -> Self {
        Self {
            endpoint_id,
            server_id,
            addr,
            tag,
            remote_tag: None,
            ready: false,
            announced: false,
            stream: None,
            read_buf: Vec::new(),
        }
    }

    /// Connects to the TCP reflector and sends the 20-byte TCP hello (`local_peer_tag || 0u32`).
    pub async fn connect_and_hello(&mut self) -> VoipResult<usize> {
        let stream = TcpStream::connect(self.addr).await?;
        let _ = stream.set_nodelay(true);
        let hello = encode_reflector_tcp_hello(&self.tag);
        let frame = encode_tcp_reflector_frame(&hello);
        tcp_write_all(&stream, &frame).await?;
        self.stream = Some(stream);
        self.ready = true;
        Ok(frame.len())
    }

    /// Sends an encrypted VoIP payload through the TCP reflector to `target_tag`.
    pub async fn send_data(&mut self, target_tag: u32, encrypted: &[u8]) -> VoipResult<usize> {
        let Some(ref stream) = self.stream else {
            return Err(VoipError::Reflector("TCP reflector not connected".into()));
        };
        let wire = encode_reflector_udp_data(&self.tag, target_tag, encrypted);
        let frame = encode_tcp_reflector_frame(&wire);
        tcp_write_all(stream, &frame).await?;
        Ok(frame.len())
    }
}

// ============================================================================
// SCTP Data-Channel Framing & Router (`SctpDataChannelProviderInterfaceImpl`)
// ============================================================================

/// WebRTC DCEP (Data Channel Establishment Protocol) PPID (`50`).
pub const SCTP_PPID_DCEP: u32 = 50;
/// WebRTC Data Channel UTF-8 String PPID (`51`).
pub const SCTP_PPID_STRING: u32 = 51;
/// WebRTC Data Channel Binary PPID (`53`).
pub const SCTP_PPID_BINARY: u32 = 53;

/// SCTP DATA chunk type (`RFC 4960 §3.3.1`).
const SCTP_CHUNK_DATA: u8 = 0x00;
/// SCTP SACK chunk type (`RFC 4960 §3.3.4`).
const SCTP_CHUNK_SACK: u8 = 0x03;

/// Decoded SCTP data-channel application message routed by [`SctpDataChannelRouter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SctpDataMessage {
    /// SCTP stream identifier (`0..=65535`).
    pub stream_id: u16,
    /// Human-readable data channel label (e.g. `"data"`).
    pub label: String,
    /// WebRTC Payload Protocol Identifier (`SCTP_PPID_STRING` or `SCTP_PPID_BINARY`).
    pub ppid: u32,
    /// Application payload bytes.
    pub payload: Vec<u8>,
}

/// Decoded low-level SCTP chunk carried inside `VoipPacket::Sctp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SctpChunk {
    /// Reliable ordered DATA chunk with TSN, stream ID, PPID, label, and user payload.
    Data {
        /// Transmission Sequence Number.
        tsn: u32,
        /// SCTP stream identifier.
        stream_id: u16,
        /// Stream sequence number.
        stream_seq: u16,
        /// Payload Protocol Identifier (`50`/`51`/`53`).
        ppid: u32,
        /// Data channel label.
        label: String,
        /// Payload bytes.
        payload: Vec<u8>,
    },
    /// Selective Acknowledgement carrying the cumulative TSN received.
    Sack {
        /// Highest contiguous TSN acknowledged.
        cumulative_tsn_ack: u32,
    },
}

impl SctpChunk {
    /// Serializes this SCTP chunk into bytes suitable for `VoipPacket::Sctp`.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        match self {
            Self::Data {
                tsn,
                stream_id,
                stream_seq,
                ppid,
                label,
                payload,
            } => {
                let label_bytes = label.as_bytes();
                let label_len = label_bytes.len().min(u8::MAX as usize);
                let mut out = Vec::with_capacity(14 + label_len + payload.len());
                out.push(SCTP_CHUNK_DATA);
                out.push(0x03); // B|E flags (complete unfragmented user message)
                out.extend_from_slice(&tsn.to_be_bytes());
                out.extend_from_slice(&stream_id.to_be_bytes());
                out.extend_from_slice(&stream_seq.to_be_bytes());
                out.extend_from_slice(&ppid.to_be_bytes());
                out.push(label_len as u8);
                out.extend_from_slice(&label_bytes[..label_len]);
                out.extend_from_slice(payload);
                out
            }
            Self::Sack { cumulative_tsn_ack } => {
                let mut out = Vec::with_capacity(6);
                out.push(SCTP_CHUNK_SACK);
                out.push(0);
                out.extend_from_slice(&cumulative_tsn_ack.to_be_bytes());
                out
            }
        }
    }

    /// Parses an SCTP chunk from `bytes`.
    pub fn parse(bytes: &[u8]) -> VoipResult<Self> {
        if bytes.len() < 6 {
            return Err(VoipError::PacketTooShort(bytes.len()));
        }
        match bytes[0] {
            SCTP_CHUNK_DATA => {
                if bytes.len() < 15 {
                    return Err(VoipError::PacketTooShort(bytes.len()));
                }
                let tsn = u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
                let stream_id = u16::from_be_bytes([bytes[6], bytes[7]]);
                let stream_seq = u16::from_be_bytes([bytes[8], bytes[9]]);
                let ppid = u32::from_be_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]);
                let label_len = usize::from(bytes[14]);
                if bytes.len() < 15 + label_len {
                    return Err(VoipError::PacketTooShort(bytes.len()));
                }
                let label = String::from_utf8_lossy(&bytes[15..15 + label_len]).into_owned();
                let payload = bytes[15 + label_len..].to_vec();
                Ok(Self::Data {
                    tsn,
                    stream_id,
                    stream_seq,
                    ppid,
                    label,
                    payload,
                })
            }
            SCTP_CHUNK_SACK => {
                let cumulative_tsn_ack =
                    u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
                Ok(Self::Sack { cumulative_tsn_ack })
            }
            other => Err(VoipError::InvalidFraming(format!(
                "unknown SCTP chunk type 0x{other:02x}"
            ))),
        }
    }
}

/// Output produced by [`SctpDataChannelRouter::handle_incoming`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SctpIncomingOutcome {
    /// Application data-channel message ready to deliver to the caller, if any.
    pub message: Option<SctpDataMessage>,
    /// Optional serialized SACK chunk to send back via `VoipPacket::Sctp`.
    pub reply_sack: Option<Vec<u8>>,
}

/// Lightweight stateful SCTP data-channel router for `MediaEngine` (`VoipPacket::Sctp`).
#[derive(Debug, Default)]
pub struct SctpDataChannelRouter {
    next_tx_tsn: u32,
    stream_tx_seq: HashMap<u16, u16>,
    seen_rx_tsns: HashSet<u32>,
    highest_rx_tsn: u32,
    last_acked_tx_tsn: u32,
    sent_count: u64,
    received_count: u64,
    acks_received: u64,
}

impl SctpDataChannelRouter {
    /// Creates a new [`SctpDataChannelRouter`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_tx_tsn: 1,
            stream_tx_seq: HashMap::new(),
            seen_rx_tsns: HashSet::new(),
            highest_rx_tsn: 0,
            last_acked_tx_tsn: 0,
            sent_count: 0,
            received_count: 0,
            acks_received: 0,
        }
    }

    /// Encodes an outgoing application message into an SCTP DATA chunk payload.
    pub fn prepare_send(
        &mut self,
        stream_id: u16,
        label: impl Into<String>,
        ppid: u32,
        payload: Vec<u8>,
    ) -> Vec<u8> {
        let tsn = self.next_tx_tsn;
        self.next_tx_tsn = self.next_tx_tsn.wrapping_add(1).max(1);
        let seq_entry = self.stream_tx_seq.entry(stream_id).or_insert(0);
        let stream_seq = *seq_entry;
        *seq_entry = seq_entry.wrapping_add(1);
        self.sent_count += 1;
        SctpChunk::Data {
            tsn,
            stream_id,
            stream_seq,
            ppid,
            label: label.into(),
            payload,
        }
        .serialize()
    }

    /// Processes an incoming `VoipPacket::Sctp(raw)` payload, deduplicating retransmissions and
    /// producing a SACK reply for DATA chunks.
    pub fn handle_incoming(&mut self, raw: &[u8]) -> VoipResult<SctpIncomingOutcome> {
        match SctpChunk::parse(raw)? {
            SctpChunk::Data {
                tsn,
                stream_id,
                ppid,
                label,
                payload,
                ..
            } => {
                if tsn > self.highest_rx_tsn {
                    self.highest_rx_tsn = tsn;
                }
                let reply_sack = Some(
                    SctpChunk::Sack {
                        cumulative_tsn_ack: self.highest_rx_tsn,
                    }
                    .serialize(),
                );
                if !self.seen_rx_tsns.insert(tsn) {
                    return Ok(SctpIncomingOutcome {
                        message: None,
                        reply_sack,
                    });
                }
                self.received_count += 1;
                Ok(SctpIncomingOutcome {
                    message: Some(SctpDataMessage {
                        stream_id,
                        label,
                        ppid,
                        payload,
                    }),
                    reply_sack,
                })
            }
            SctpChunk::Sack { cumulative_tsn_ack } => {
                if cumulative_tsn_ack > self.last_acked_tx_tsn {
                    self.last_acked_tx_tsn = cumulative_tsn_ack;
                }
                self.acks_received += 1;
                Ok(SctpIncomingOutcome::default())
            }
        }
    }

    /// Highest transmitted TSN acknowledged by the remote peer.
    #[must_use]
    pub const fn last_acked_tx_tsn(&self) -> u32 {
        self.last_acked_tx_tsn
    }

    /// Total SCTP DATA messages sent.
    #[must_use]
    pub const fn sent_count(&self) -> u64 {
        self.sent_count
    }

    /// Total unique SCTP DATA messages delivered.
    #[must_use]
    pub const fn received_count(&self) -> u64 {
        self.received_count
    }

    /// Total SCTP SACK acknowledgements received.
    #[must_use]
    pub const fn acks_received(&self) -> u64 {
        self.acks_received
    }
}

// ============================================================================
// Local Fake Servers for Loopback Integration Tests
// ============================================================================

/// Local loopback fake Telegram TCP reflector (`127.0.0.1:0`) for integration tests.
pub struct LoopbackTcpReflector {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    hellos_received: Arc<AtomicU64>,
    packets_forwarded: Arc<AtomicU64>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl LoopbackTcpReflector {
    /// Binds a fake Telegram TCP reflector on `127.0.0.1:0`.
    pub async fn start(peer_tag_prefix: [u8; 12]) -> VoipResult<Self> {
        let listener =
            TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let hellos_received = Arc::new(AtomicU64::new(0));
        let packets_forwarded = Arc::new(AtomicU64::new(0));

        let peers: Arc<Mutex<HashMap<u32, Arc<TcpStream>>>> = Arc::new(Mutex::new(HashMap::new()));
        let stop_bg = Arc::clone(&stop);
        let hellos_bg = Arc::clone(&hellos_received);
        let fwd_bg = Arc::clone(&packets_forwarded);

        let handle = tokio::spawn(async move {
            loop {
                if stop_bg.load(Ordering::Acquire) {
                    break;
                }
                let accepted = tokio::select! {
                    res = listener.accept() => res,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => continue,
                };
                let Ok((stream, _peer_addr)) = accepted else {
                    break;
                };
                let _ = stream.set_nodelay(true);
                let stream = Arc::new(stream);
                let peers_c = Arc::clone(&peers);
                let stop_c = Arc::clone(&stop_bg);
                let hellos_c = Arc::clone(&hellos_bg);
                let fwd_c = Arc::clone(&fwd_bg);
                tokio::spawn(async move {
                    let mut read_buf = Vec::new();
                    loop {
                        if stop_c.load(Ordering::Acquire) {
                            break;
                        }
                        let read_res = tokio::select! {
                            r = tcp_read_chunk(&stream, &mut read_buf) => r,
                            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => continue,
                        };
                        match read_res {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                        for frame in drain_tcp_reflector_frames(&mut read_buf) {
                            if frame.len() == 20 && frame[0..12] == peer_tag_prefix {
                                let local_tag = u32::from_le_bytes([
                                    frame[12], frame[13], frame[14], frame[15],
                                ]);
                                peers_c.lock().await.insert(local_tag, Arc::clone(&stream));
                                hellos_c.fetch_add(1, Ordering::AcqRel);
                                // Echo hello ack.
                                let mut ack = [0u8; 28];
                                ack[0..16].copy_from_slice(&frame[0..16]);
                                ack[16..28].copy_from_slice(&REFLECTOR_SPECIAL_TAG);
                                let _ =
                                    tcp_write_all(&stream, &encode_tcp_reflector_frame(&ack)).await;
                            } else if let Ok(ReflectorIncoming::Data {
                                sender_tag,
                                payload,
                            }) = decode_reflector_udp_packet(&peer_tag_prefix, &frame)
                            {
                                let target_tag = u32::from_le_bytes([
                                    frame[12], frame[13], frame[14], frame[15],
                                ]);
                                let target = peers_c.lock().await.get(&target_tag).cloned();
                                if let Some(dst) = target {
                                    let mut fwd = Vec::with_capacity(24 + payload.len() + 3);
                                    fwd.extend_from_slice(&peer_tag_prefix);
                                    fwd.extend_from_slice(&target_tag.to_le_bytes());
                                    fwd.extend_from_slice(&sender_tag.to_le_bytes());
                                    fwd.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                                    fwd.extend_from_slice(&payload);
                                    while !fwd.len().is_multiple_of(4) {
                                        fwd.push(0);
                                    }
                                    if tcp_write_all(&dst, &encode_tcp_reflector_frame(&fwd))
                                        .await
                                        .is_ok()
                                    {
                                        fwd_c.fetch_add(1, Ordering::AcqRel);
                                    }
                                }
                            }
                        }
                    }
                });
            }
        });

        Ok(Self {
            addr,
            stop,
            hellos_received,
            packets_forwarded,
            handle: Some(handle),
        })
    }

    /// Listening TCP socket address.
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Number of TCP hellos processed.
    #[must_use]
    pub fn hellos_received(&self) -> u64 {
        self.hellos_received.load(Ordering::Acquire)
    }

    /// Number of TCP data frames forwarded between peers.
    #[must_use]
    pub fn packets_forwarded(&self) -> u64 {
        self.packets_forwarded.load(Ordering::Acquire)
    }
}

impl Drop for LoopbackTcpReflector {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

struct TurnClientAllocation {
    client_addr: SocketAddr,
    relay_socket: Arc<UdpSocket>,
    relay_addr: SocketAddr,
    permissions: HashSet<SocketAddr>,
    channel_to_peer: HashMap<u16, SocketAddr>,
    peer_to_channel: HashMap<SocketAddr, u16>,
}

/// Local loopback fake RFC 5766 TURN server (`127.0.0.1:0`) supporting 401 challenge/auth,
/// `Allocate`, `Refresh`, `CreatePermission`, `ChannelBind`, `SendIndication`, `DataIndication`,
/// and `ChannelData` for integration tests.
pub struct LoopbackTurnServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    allocations_created: Arc<AtomicU64>,
    packets_relayed: Arc<AtomicU64>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl LoopbackTurnServer {
    /// Starts a fake RFC 5766 TURN server on `127.0.0.1:0` requiring long-term credentials
    /// `(username, password)` under `realm`.
    pub async fn start(
        username: impl Into<String>,
        password: impl Into<String>,
        realm: impl Into<String>,
    ) -> VoipResult<Self> {
        let server_sock =
            Arc::new(UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?);
        let addr = server_sock.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let allocations_created = Arc::new(AtomicU64::new(0));
        let packets_relayed = Arc::new(AtomicU64::new(0));

        let expected_user = username.into();
        let expected_realm = realm.into();
        let expected_nonce = "rustgram-test-nonce-1".to_owned();
        let expected_key = turn_long_term_key(&expected_user, &expected_realm, &password.into());

        let allocs: Arc<Mutex<HashMap<SocketAddr, TurnClientAllocation>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let stop_bg = Arc::clone(&stop);
        let alloc_count = Arc::clone(&allocations_created);
        let relay_count = Arc::clone(&packets_relayed);

        let handle = tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                if stop_bg.load(Ordering::Acquire) {
                    break;
                }
                let recv = tokio::select! {
                    r = server_sock.recv_from(&mut buf) => r,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => continue,
                };
                let Ok((n, src)) = recv else {
                    break;
                };
                let data = &buf[..n];

                if is_turn_channel_data(data) {
                    if let Ok((ch, payload)) = decode_turn_channel_data(data) {
                        let target = {
                            let guard = allocs.lock().await;
                            guard.get(&src).and_then(|a| {
                                a.channel_to_peer
                                    .get(&ch)
                                    .copied()
                                    .map(|p| (Arc::clone(&a.relay_socket), p))
                            })
                        };
                        if let Some((rsock, peer_addr)) = target
                            && rsock.send_to(&payload, peer_addr).await.is_ok()
                        {
                            relay_count.fetch_add(1, Ordering::AcqRel);
                        }
                    }
                    continue;
                }

                let Ok(parsed) = parse_turn_message(data, Some(&expected_key)) else {
                    continue;
                };

                match parsed.msg_type {
                    TURN_ALLOCATE_REQUEST => {
                        let authed = parsed.integrity_valid
                            && parsed.username.as_deref() == Some(expected_user.as_str())
                            && parsed.realm.as_deref() == Some(expected_realm.as_str())
                            && parsed.nonce.as_deref() == Some(expected_nonce.as_str());
                        if !authed {
                            let mut attrs = Vec::new();
                            append_attr(
                                &mut attrs,
                                turn_attr::ERROR_CODE,
                                &encode_error_code(401, "Unauthorized"),
                            );
                            append_attr(&mut attrs, turn_attr::REALM, expected_realm.as_bytes());
                            append_attr(&mut attrs, turn_attr::NONCE, expected_nonce.as_bytes());
                            let resp =
                                finish_turn_message(TURN_ALLOCATE_ERROR, parsed.tx_id, attrs, None);
                            let _ = server_sock.send_to(&resp, src).await;
                            continue;
                        }

                        let relay_addr = {
                            let mut guard = allocs.lock().await;
                            if let Some(existing) = guard.get(&src) {
                                existing.relay_addr
                            } else {
                                let Ok(rsock) = UdpSocket::bind(SocketAddr::new(
                                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                                    0,
                                ))
                                .await
                                else {
                                    continue;
                                };
                                let Ok(raddr) = rsock.local_addr() else {
                                    continue;
                                };
                                let rsock = Arc::new(rsock);
                                guard.insert(
                                    src,
                                    TurnClientAllocation {
                                        client_addr: src,
                                        relay_socket: Arc::clone(&rsock),
                                        relay_addr: raddr,
                                        permissions: HashSet::new(),
                                        channel_to_peer: HashMap::new(),
                                        peer_to_channel: HashMap::new(),
                                    },
                                );
                                alloc_count.fetch_add(1, Ordering::AcqRel);

                                // Spawn relay->client forwarding pump for this allocation.
                                let allocs_fwd = Arc::clone(&allocs);
                                let srv_fwd = Arc::clone(&server_sock);
                                let stop_fwd = Arc::clone(&stop_bg);
                                let relay_fwd = Arc::clone(&relay_count);
                                tokio::spawn(async move {
                                    let mut rbuf = [0u8; 4096];
                                    loop {
                                        if stop_fwd.load(Ordering::Acquire) {
                                            break;
                                        }
                                        let r = tokio::select! {
                                            res = rsock.recv_from(&mut rbuf) => res,
                                            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => continue,
                                        };
                                        let Ok((rn, peer_src)) = r else {
                                            break;
                                        };
                                        let payload = &rbuf[..rn];
                                        let route = {
                                            let g = allocs_fwd.lock().await;
                                            g.get(&src).and_then(|a| {
                                                if !a.permissions.contains(&peer_src)
                                                    && !a.peer_to_channel.contains_key(&peer_src)
                                                {
                                                    return None;
                                                }
                                                Some((
                                                    a.client_addr,
                                                    a.peer_to_channel.get(&peer_src).copied(),
                                                ))
                                            })
                                        };
                                        if let Some((client_dst, bound_ch)) = route {
                                            let wire = if let Some(ch) = bound_ch {
                                                encode_turn_channel_data(ch, payload)
                                            } else {
                                                let mut tx = [0u8; 12];
                                                rand::rngs::OsRng.fill_bytes(&mut tx);
                                                build_turn_data_indication(tx, peer_src, payload)
                                            };
                                            if srv_fwd.send_to(&wire, client_dst).await.is_ok() {
                                                relay_fwd.fetch_add(1, Ordering::AcqRel);
                                            }
                                        }
                                    }
                                });
                                raddr
                            }
                        };

                        let mut attrs = Vec::new();
                        append_attr(
                            &mut attrs,
                            turn_attr::XOR_RELAYED_ADDRESS,
                            &encode_xor_addr(relay_addr, &parsed.tx_id),
                        );
                        append_attr(
                            &mut attrs,
                            turn_attr::XOR_MAPPED_ADDRESS,
                            &encode_xor_addr(src, &parsed.tx_id),
                        );
                        append_attr(
                            &mut attrs,
                            turn_attr::LIFETIME,
                            &TURN_DEFAULT_LIFETIME_SECS.to_be_bytes(),
                        );
                        let resp = finish_turn_message(
                            TURN_ALLOCATE_RESPONSE,
                            parsed.tx_id,
                            attrs,
                            Some(&expected_key),
                        );
                        let _ = server_sock.send_to(&resp, src).await;
                    }
                    TURN_REFRESH_REQUEST if parsed.integrity_valid => {
                        let life = parsed.lifetime_secs.unwrap_or(TURN_DEFAULT_LIFETIME_SECS);
                        let mut attrs = Vec::new();
                        append_attr(&mut attrs, turn_attr::LIFETIME, &life.to_be_bytes());
                        let resp = finish_turn_message(
                            TURN_REFRESH_RESPONSE,
                            parsed.tx_id,
                            attrs,
                            Some(&expected_key),
                        );
                        let _ = server_sock.send_to(&resp, src).await;
                    }
                    TURN_CREATE_PERMISSION_REQUEST if parsed.integrity_valid => {
                        if let Some(peer_addr) = parsed.peer_addr {
                            if let Some(a) = allocs.lock().await.get_mut(&src) {
                                a.permissions.insert(peer_addr);
                            }
                            let resp = finish_turn_message(
                                TURN_CREATE_PERMISSION_RESPONSE,
                                parsed.tx_id,
                                Vec::new(),
                                Some(&expected_key),
                            );
                            let _ = server_sock.send_to(&resp, src).await;
                        }
                    }
                    TURN_CHANNEL_BIND_REQUEST if parsed.integrity_valid => {
                        if let (Some(ch), Some(peer_addr)) =
                            (parsed.channel_number, parsed.peer_addr)
                        {
                            if let Some(a) = allocs.lock().await.get_mut(&src) {
                                a.permissions.insert(peer_addr);
                                a.channel_to_peer.insert(ch, peer_addr);
                                a.peer_to_channel.insert(peer_addr, ch);
                            }
                            let resp = finish_turn_message(
                                TURN_CHANNEL_BIND_RESPONSE,
                                parsed.tx_id,
                                Vec::new(),
                                Some(&expected_key),
                            );
                            let _ = server_sock.send_to(&resp, src).await;
                        }
                    }
                    TURN_SEND_INDICATION => {
                        if let (Some(peer_addr), Some(payload)) = (parsed.peer_addr, parsed.data) {
                            let rsock = {
                                let mut g = allocs.lock().await;
                                g.get_mut(&src).map(|a| {
                                    a.permissions.insert(peer_addr);
                                    Arc::clone(&a.relay_socket)
                                })
                            };
                            if let Some(sock) = rsock
                                && sock.send_to(&payload, peer_addr).await.is_ok()
                            {
                                relay_count.fetch_add(1, Ordering::AcqRel);
                            }
                        }
                    }
                    _ => {}
                }
            }
        });

        Ok(Self {
            addr,
            stop,
            allocations_created,
            packets_relayed,
            handle: Some(handle),
        })
    }

    /// Listening TURN server UDP address.
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Number of authenticated TURN allocations created.
    #[must_use]
    pub fn allocations_created(&self) -> u64 {
        self.allocations_created.load(Ordering::Acquire)
    }

    /// Number of data packets relayed between clients and peers.
    #[must_use]
    pub fn packets_relayed(&self) -> u64 {
        self.packets_relayed.load(Ordering::Acquire)
    }
}

impl Drop for LoopbackTurnServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}
