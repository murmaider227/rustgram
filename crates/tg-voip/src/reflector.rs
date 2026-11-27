//! Telegram UDP/TCP reflector relay framing, RFC 5389 STUN, RFC 5766 TURN helpers,
//! and ICE candidate generation/selection.
//!
//! Port of `TelegramMessenger/tgcalls` (`development` branch):
//! - `tgcalls/v2/ReflectorPort.cpp`
//! - `tgcalls/TurnCustomizerImpl.cpp`
//!
//! # Verified Telegram Reflector Wire Format (`ReflectorPort.cpp`)
//! - A server connection provides a 16-byte `peer_tag` (`phoneConnection.peer_tag` or
//!   lowercase hex `RtcServer.password` when `login == "reflector"`).
//! - Each client chooses a random non-zero 32-bit `random_tag` and forms its 16-byte
//!   `local_peer_tag = peer_tag[0..12] || random_tag.to_le_bytes()`.
//!   (Note: `ReflectorPort.cpp` writes `(uint8_t*)&randomTag_` directly in host byte order,
//!   which is little-endian on x86-64 / ARM64 Telegram clients.)
//! - **UDP Hello / Keepalive** (sent every 500 ms while connecting, every 10 000 ms once ready):
//!   `local_peer_tag[16] || [0xff; 12] || 0xfe || [0xff; 3] || 123u64.to_be_bytes()` (40 bytes).
//! - **TCP Hello**:
//!   `local_peer_tag[16] || 0u32.to_be_bytes()` (20 bytes).
//! - **UDP Data Packet**:
//!   `peer_tag[0..12] || target_tag_le[4] || sender_tag_le[4] || payload_len_be[4] || payload || pad4`.
//! - **Synthetic Candidate Hostname**:
//!   `reflector-<server_id>-<random_tag>.reflector:<port>`.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use hmac::{Hmac, Mac};
use rand::Rng;
use sha1::Sha1;
use tg_calls::{Endpoint, RtcServer};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

use crate::error::{VoipError, VoipResult};

/// Default Telegram reflector UDP port when none is specified (`ReflectorPort.cpp`).
pub const DEFAULT_REFLECTOR_PORT: u16 = 599;

/// Hello ping interval while waiting for first reflector response (500 ms).
pub const REFLECTOR_CONNECTING_PING_MS: u64 = 500;

/// Keepalive ping interval once reflector is ready (10 000 ms).
pub const REFLECTOR_READY_PING_MS: u64 = 10_000;

/// Special 12-byte marker identifying reflector hello/control packets (`ReflectorPort.cpp`).
pub const REFLECTOR_SPECIAL_TAG: [u8; 12] = [0xFF; 12];

/// Hello suffix byte + 3 padding bytes (`0xfe, 0xff, 0xff, 0xff`).
const REFLECTOR_HELLO_SUFFIX: [u8; 4] = [0xFE, 0xFF, 0xFF, 0xFF];

/// Hello magic u64 written in big-endian (`bufferWriter.WriteUInt64(123)`).
const REFLECTOR_HELLO_MAGIC: u64 = 123;

/// Software attribute value injected by `TurnCustomizerImpl.cpp` (`"Telegram "`).
pub const TELEGRAM_STUN_SOFTWARE: &str = "Telegram ";

/// Parsed 16-byte Telegram reflector `peer_tag` split into its 12-byte shared call prefix
/// and 4-byte local client `random_tag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReflectorTag {
    /// First 12 bytes shared by both call participants on this reflector.
    pub prefix: [u8; 12],
    /// Local non-zero 32-bit client tag appended to `prefix`.
    pub local_tag: u32,
}

impl ReflectorTag {
    /// Construct from a 16-byte raw `peer_tag` and an explicit non-zero `local_tag`.
    #[must_use]
    pub fn new(raw_peer_tag: &[u8; 16], local_tag: u32) -> Self {
        let mut prefix = [0u8; 12];
        prefix.copy_from_slice(&raw_peer_tag[..12]);
        let local_tag = if local_tag == 0 { 1 } else { local_tag };
        Self { prefix, local_tag }
    }

    /// Construct from a 16-byte raw `peer_tag` using a random non-zero `local_tag`.
    #[must_use]
    pub fn with_random_tag(raw_peer_tag: &[u8; 16]) -> Self {
        let mut rng = rand::thread_rng();
        let mut tag = 0u32;
        while tag == 0 {
            tag = rng.r#gen();
        }
        Self::new(raw_peer_tag, tag)
    }

    /// Parse a 32-char lowercase hex `peer_tag` string (from `RtcServer.password`).
    pub fn from_hex(hex: &str, local_tag: u32) -> VoipResult<Self> {
        let bytes = decode_hex_16(hex)?;
        Ok(Self::new(&bytes, local_tag))
    }

    /// Build the 16-byte local peer tag (`prefix[12] || local_tag_le[4]`).
    #[must_use]
    pub fn local_peer_tag_bytes(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[..12].copy_from_slice(&self.prefix);
        out[12..16].copy_from_slice(&self.local_tag.to_le_bytes());
        out
    }

    /// Build the 16-byte destination peer tag for a remote peer (`prefix[12] || target_tag_le[4]`).
    #[must_use]
    pub fn target_peer_tag_bytes(&self, target_tag: u32) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[..12].copy_from_slice(&self.prefix);
        out[12..16].copy_from_slice(&target_tag.to_le_bytes());
        out
    }

    /// Format the synthetic candidate hostname (`reflector-<server_id>-<local_tag>.reflector`).
    #[must_use]
    pub fn candidate_hostname(&self, server_id: u8) -> String {
        format_reflector_hostname(server_id, self.local_tag)
    }
}

/// Format a Telegram reflector synthetic hostname (`ReflectorPort.cpp:695`).
#[must_use]
pub fn format_reflector_hostname(server_id: u8, tag: u32) -> String {
    format!("reflector-{server_id}-{tag}.reflector")
}

/// Parse `(server_id, peer_tag_u32)` from a synthetic reflector hostname.
#[must_use]
pub fn parse_reflector_hostname(hostname: &str) -> Option<(u8, u32)> {
    let rest = hostname
        .strip_prefix("reflector-")?
        .strip_suffix(".reflector")?;
    let (id_str, tag_str) = rest.split_once('-')?;
    let server_id: u8 = id_str.parse().ok()?;
    let tag: u32 = tag_str.parse().ok()?;
    if server_id == 0 || tag == 0 {
        return None;
    }
    Some((server_id, tag))
}

/// Decode a 32-character hex string into a 16-byte `peer_tag`.
pub fn decode_hex_16(hex: &str) -> VoipResult<[u8; 16]> {
    if hex.len() != 32 || !hex.is_ascii() {
        return Err(VoipError::Reflector(format!(
            "expected 32 hex chars for peer_tag, got {} bytes",
            hex.len()
        )));
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        let byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|e| VoipError::Reflector(format!("invalid hex in peer_tag: {e}")))?;
        out[i] = byte;
    }
    Ok(out)
}

/// Encode a Telegram UDP reflector hello/keepalive packet (`ReflectorPort::SendReflectorHello`).
#[must_use]
pub fn encode_reflector_udp_hello(tag: &ReflectorTag) -> [u8; 40] {
    let mut buf = [0u8; 40];
    buf[0..16].copy_from_slice(&tag.local_peer_tag_bytes());
    buf[16..28].copy_from_slice(&REFLECTOR_SPECIAL_TAG);
    buf[28..32].copy_from_slice(&REFLECTOR_HELLO_SUFFIX);
    buf[32..40].copy_from_slice(&REFLECTOR_HELLO_MAGIC.to_be_bytes());
    buf
}

/// Encode a Telegram TCP reflector hello packet (`ReflectorPort::SendReflectorHello`).
#[must_use]
pub fn encode_reflector_tcp_hello(tag: &ReflectorTag) -> [u8; 20] {
    let mut buf = [0u8; 20];
    buf[0..16].copy_from_slice(&tag.local_peer_tag_bytes());
    buf[16..20].copy_from_slice(&0u32.to_be_bytes());
    buf
}

/// Encode a UDP data packet routed through a Telegram reflector (`ReflectorPort::SendTo`).
///
/// Layout: `prefix[12] || target_tag_le[4] || local_tag_le[4] || len_be[4] || payload || pad4`.
#[must_use]
pub fn encode_reflector_udp_data(tag: &ReflectorTag, target_tag: u32, payload: &[u8]) -> Vec<u8> {
    let unpadded = 24 + payload.len();
    let padded = (unpadded + 3) & !3;
    let mut out = Vec::with_capacity(padded);
    out.extend_from_slice(&tag.target_peer_tag_bytes(target_tag));
    out.extend_from_slice(&tag.local_tag.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out.resize(padded, 0);
    out
}

/// Decoded packet received from a Telegram UDP reflector (`ReflectorPort::HandleIncomingPacket`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReflectorIncoming {
    /// Hello/keepalive acknowledgement (special `0xff * 12` marker or bare 16-byte echo).
    HelloAck {
        /// Echoed 4-byte tag from bytes `12..16`.
        echoed_tag: u32,
    },
    /// Relayed media/control payload from a remote participant.
    Data {
        /// Remote sender's 4-byte `random_tag`.
        sender_tag: u32,
        /// Unwrapped payload bytes.
        payload: Vec<u8>,
    },
}

/// Parse and validate an incoming UDP reflector datagram (`ReflectorPort::HandleIncomingPacket`).
pub fn decode_reflector_udp_packet(
    expected_prefix: &[u8; 12],
    packet: &[u8],
) -> VoipResult<ReflectorIncoming> {
    if packet.len() < 16 {
        return Err(VoipError::PacketTooShort(packet.len()));
    }
    if &packet[0..12] != expected_prefix {
        return Err(VoipError::Reflector(
            "peer_tag 12-byte prefix mismatch".into(),
        ));
    }
    let echoed_tag = u32::from_le_bytes([packet[12], packet[13], packet[14], packet[15]]);

    if packet.len() <= 24 {
        return Ok(ReflectorIncoming::HelloAck { echoed_tag });
    }
    if packet.len() >= 28 && packet[16..28] == REFLECTOR_SPECIAL_TAG {
        return Ok(ReflectorIncoming::HelloAck { echoed_tag });
    }

    let sender_tag = u32::from_le_bytes([packet[16], packet[17], packet[18], packet[19]]);
    let data_size = u32::from_be_bytes([packet[20], packet[21], packet[22], packet[23]]) as usize;
    if data_size > packet.len() - 24 {
        return Err(VoipError::Reflector(format!(
            "invalid payload size {data_size} in {}-byte datagram",
            packet.len()
        )));
    }
    Ok(ReflectorIncoming::Data {
        sender_tag,
        payload: packet[24..24 + data_size].to_vec(),
    })
}

// ---------------------------------------------------------------------------
// RFC 5389 STUN & RFC 5766 TURN Helpers
// ---------------------------------------------------------------------------

/// RFC 5389 STUN magic cookie (`0x2112A442`).
pub const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;

/// STUN Binding Request message type (`0x0001`).
pub const STUN_BINDING_REQUEST: u16 = 0x0001;

/// STUN Binding Success Response message type (`0x0101`).
pub const STUN_BINDING_RESPONSE: u16 = 0x0101;

/// TURN Allocate Request message type (`0x0003`).
pub const TURN_ALLOCATE_REQUEST: u16 = 0x0003;

/// STUN attribute types (RFC 5389 / RFC 5766).
pub mod stun_attr {
    /// `MAPPED-ADDRESS`
    pub const MAPPED_ADDRESS: u16 = 0x0001;
    /// `USERNAME`
    pub const USERNAME: u16 = 0x0006;
    /// `MESSAGE-INTEGRITY` (20-byte HMAC-SHA1)
    pub const MESSAGE_INTEGRITY: u16 = 0x0008;
    /// `REQUESTED-TRANSPORT` (TURN, 17 = UDP)
    pub const REQUESTED_TRANSPORT: u16 = 0x0019;
    /// `XOR-MAPPED-ADDRESS`
    pub const XOR_MAPPED_ADDRESS: u16 = 0x0020;
    /// `SOFTWARE` (`"Telegram "` via `TurnCustomizerImpl`)
    pub const SOFTWARE: u16 = 0x8022;
    /// `FINGERPRINT` (4-byte CRC-32 XOR `0x5354554e`)
    pub const FINGERPRINT: u16 = 0x8028;
}

/// Build an RFC 5389 STUN Binding Request with `SOFTWARE: "Telegram "` and `FINGERPRINT`.
#[must_use]
pub fn build_stun_binding_request(tx_id: [u8; 12]) -> Vec<u8> {
    let mut attrs = Vec::new();
    append_stun_attr(
        &mut attrs,
        stun_attr::SOFTWARE,
        TELEGRAM_STUN_SOFTWARE.as_bytes(),
    );
    finish_stun_message(STUN_BINDING_REQUEST, tx_id, attrs, None)
}

/// Build an RFC 5766 TURN Allocate Request (UDP) with optional `USERNAME` and `MESSAGE-INTEGRITY`.
#[must_use]
pub fn build_turn_allocate_request(
    tx_id: [u8; 12],
    username: Option<&str>,
    integrity_key: Option<&[u8]>,
) -> Vec<u8> {
    let mut attrs = Vec::new();
    // REQUESTED-TRANSPORT: 17 (UDP), 3 zero bytes.
    append_stun_attr(&mut attrs, stun_attr::REQUESTED_TRANSPORT, &[17, 0, 0, 0]);
    append_stun_attr(
        &mut attrs,
        stun_attr::SOFTWARE,
        TELEGRAM_STUN_SOFTWARE.as_bytes(),
    );
    if let Some(user) = username
        && !user.is_empty()
    {
        append_stun_attr(&mut attrs, stun_attr::USERNAME, user.as_bytes());
    }
    finish_stun_message(TURN_ALLOCATE_REQUEST, tx_id, attrs, integrity_key)
}

/// Build a STUN Binding Success Response containing `XOR-MAPPED-ADDRESS` (for tests/servers).
#[must_use]
pub fn build_stun_binding_response(tx_id: [u8; 12], mapped: SocketAddr) -> Vec<u8> {
    let mut attrs = Vec::new();
    let xor_val = encode_xor_address(mapped, &tx_id);
    append_stun_attr(&mut attrs, stun_attr::XOR_MAPPED_ADDRESS, &xor_val);
    finish_stun_message(STUN_BINDING_RESPONSE, tx_id, attrs, None)
}

/// Parsed STUN Binding Response fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StunBindingResponse {
    /// 96-bit STUN transaction ID.
    pub tx_id: [u8; 12],
    /// Server-reflexive mapped address from `XOR-MAPPED-ADDRESS` or `MAPPED-ADDRESS`.
    pub mapped_addr: Option<SocketAddr>,
}

/// Returns `true` if `packet` looks like a valid RFC 5389 STUN header.
#[must_use]
pub fn is_stun_packet(packet: &[u8]) -> bool {
    packet.len() >= 20
        && (packet[0] & 0xC0) == 0
        && u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]) == STUN_MAGIC_COOKIE
}

/// Parse an RFC 5389 STUN Binding Response, verifying FINGERPRINT if present.
pub fn parse_stun_binding_response(packet: &[u8]) -> VoipResult<StunBindingResponse> {
    if !is_stun_packet(packet) {
        return Err(VoipError::Stun("not a valid STUN packet".into()));
    }
    let msg_type = u16::from_be_bytes([packet[0], packet[1]]);
    if msg_type != STUN_BINDING_RESPONSE {
        return Err(VoipError::Stun(format!(
            "unexpected STUN message type 0x{msg_type:04x}"
        )));
    }
    let msg_len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    if 20 + msg_len > packet.len() {
        return Err(VoipError::Stun("truncated STUN body".into()));
    }
    let mut tx_id = [0u8; 12];
    tx_id.copy_from_slice(&packet[8..20]);

    let mut mapped_addr = None;
    let mut cursor = 20usize;
    let end = 20 + msg_len;
    while cursor + 4 <= end {
        let attr_type = u16::from_be_bytes([packet[cursor], packet[cursor + 1]]);
        let attr_len = u16::from_be_bytes([packet[cursor + 2], packet[cursor + 3]]) as usize;
        let val_start = cursor + 4;
        let val_end = val_start + attr_len;
        if val_end > end {
            return Err(VoipError::Stun("truncated STUN attribute".into()));
        }
        let val = &packet[val_start..val_end];
        match attr_type {
            stun_attr::XOR_MAPPED_ADDRESS => {
                mapped_addr = Some(decode_xor_address(val, &tx_id)?);
            }
            stun_attr::MAPPED_ADDRESS if mapped_addr.is_none() => {
                mapped_addr = Some(decode_mapped_address(val)?);
            }
            stun_attr::FINGERPRINT => {
                if val.len() != 4 {
                    return Err(VoipError::Stun("invalid FINGERPRINT length".into()));
                }
                let got = u32::from_be_bytes([val[0], val[1], val[2], val[3]]);
                let expected = crc32_ieee(&packet[..cursor]) ^ 0x5354_554E;
                if got != expected {
                    return Err(VoipError::Stun("STUN FINGERPRINT mismatch".into()));
                }
            }
            _ => {}
        }
        let padded = (attr_len + 3) & !3;
        cursor = val_start + padded;
    }

    Ok(StunBindingResponse { tx_id, mapped_addr })
}

fn append_stun_attr(buf: &mut Vec<u8>, attr_type: u16, value: &[u8]) {
    buf.extend_from_slice(&attr_type.to_be_bytes());
    buf.extend_from_slice(&(value.len() as u16).to_be_bytes());
    buf.extend_from_slice(value);
    let pad = (4 - (value.len() % 4)) % 4;
    for _ in 0..pad {
        buf.push(0);
    }
}

fn finish_stun_message(
    msg_type: u16,
    tx_id: [u8; 12],
    mut attrs: Vec<u8>,
    integrity_key: Option<&[u8]>,
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(20 + attrs.len() + 32);
    msg.extend_from_slice(&msg_type.to_be_bytes());
    msg.extend_from_slice(&0u16.to_be_bytes()); // placeholder length
    msg.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    msg.extend_from_slice(&tx_id);
    msg.append(&mut attrs);

    if let Some(key) = integrity_key {
        let len_with_mi = (msg.len() - 20 + 24) as u16;
        msg[2..4].copy_from_slice(&len_with_mi.to_be_bytes());
        if let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(key) {
            mac.update(&msg);
            let digest = mac.finalize().into_bytes();
            append_stun_attr(&mut msg, stun_attr::MESSAGE_INTEGRITY, &digest);
        }
    }

    let len_with_fp = (msg.len() - 20 + 8) as u16;
    msg[2..4].copy_from_slice(&len_with_fp.to_be_bytes());
    let fp = crc32_ieee(&msg) ^ 0x5354_554E;
    append_stun_attr(&mut msg, stun_attr::FINGERPRINT, &fp.to_be_bytes());
    msg
}

fn encode_xor_address(addr: SocketAddr, tx_id: &[u8; 12]) -> Vec<u8> {
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

fn decode_xor_address(val: &[u8], tx_id: &[u8; 12]) -> VoipResult<SocketAddr> {
    if val.len() < 8 {
        return Err(VoipError::Stun("XOR-MAPPED-ADDRESS too short".into()));
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
            "unsupported STUN address family 0x{family:02x}"
        ))),
    }
}

fn decode_mapped_address(val: &[u8]) -> VoipResult<SocketAddr> {
    if val.len() < 8 {
        return Err(VoipError::Stun("MAPPED-ADDRESS too short".into()));
    }
    let family = val[1];
    let port = u16::from_be_bytes([val[2], val[3]]);
    match family {
        0x01 => {
            let ip = Ipv4Addr::new(val[4], val[5], val[6], val[7]);
            Ok(SocketAddr::new(IpAddr::V4(ip), port))
        }
        0x02 if val.len() >= 20 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&val[4..20]);
            Ok(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(octets)), port))
        }
        _ => Err(VoipError::Stun(format!(
            "unsupported MAPPED-ADDRESS family 0x{family:02x}"
        ))),
    }
}

/// Standard IEEE 802.3 CRC-32 (for RFC 5389 STUN FINGERPRINT).
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

// ---------------------------------------------------------------------------
// Candidate & Route Classification from `ControllerDescriptor`
// ---------------------------------------------------------------------------

/// Classified transport candidate extracted from `ControllerDescriptor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportCandidate {
    /// Direct host/P2P UDP address (only advertised/used when `enable_p2p == true`).
    DirectP2p {
        /// Peer or local UDP socket address.
        addr: SocketAddr,
    },
    /// Telegram UDP reflector relay (`phoneConnection` / `RtcServer { login: "reflector" }`).
    TelegramReflector {
        /// Server connection ID (from `Endpoint::endpoint_id`, or `0` if only in `rtc_servers`).
        endpoint_id: i64,
        /// 1-based reflector ID used in `reflector-<id>-<tag>.reflector`.
        server_id: u8,
        /// Reflector UDP address.
        addr: SocketAddr,
        /// 16-byte raw server `peer_tag`.
        raw_peer_tag: [u8; 16],
        /// Whether this server is TCP-only.
        is_tcp: bool,
    },
    /// Standard WebRTC STUN server (`phoneConnectionWebrtc { stun: true }`).
    StunServer {
        /// STUN UDP address.
        addr: SocketAddr,
    },
    /// Standard WebRTC TURN server (`phoneConnectionWebrtc { turn: true }`).
    TurnServer {
        /// TURN UDP address.
        addr: SocketAddr,
        /// TURN username.
        username: String,
        /// TURN password.
        password: String,
    },
}

/// Extract normalized transport candidates from `tg_calls`' `endpoints` and `rtc_servers`.
///
/// Honors `enable_p2p`: when `false`, STUN server-reflexive probing and direct host
/// candidates are suppressed so the local IP is never leaked to the remote peer.
#[must_use]
pub fn collect_transport_candidates(
    endpoints: &[Endpoint],
    rtc_servers: &[RtcServer],
    enable_p2p: bool,
) -> Vec<TransportCandidate> {
    let mut out = Vec::new();

    // Map reflector `(host, port) -> endpoint_id` from legacy endpoints so `preferred_relay_id`
    // can be reported accurately in `phone.discardCall`.
    let mut endpoint_ids: HashMap<(String, u16), i64> = HashMap::new();
    for ep in endpoints {
        if !ep.host.ipv4.is_empty() {
            endpoint_ids.insert((ep.host.ipv4.clone(), ep.port), ep.endpoint_id);
        }
        if !ep.host.ipv6.is_empty() {
            endpoint_ids.insert((ep.host.ipv6.clone(), ep.port), ep.endpoint_id);
        }
    }

    // Parse `rtc_servers` (preferred v2 path).
    let mut seen_reflector = false;
    for srv in rtc_servers {
        let Ok(ip) = srv.host.parse::<IpAddr>() else {
            continue;
        };
        let port = if srv.port == 0 {
            DEFAULT_REFLECTOR_PORT
        } else {
            srv.port
        };
        let addr = SocketAddr::new(ip, port);

        if srv.is_turn && srv.login == "reflector" {
            if let Ok(raw_peer_tag) = decode_hex_16(&srv.password) {
                let endpoint_id = endpoint_ids
                    .get(&(srv.host.clone(), srv.port))
                    .copied()
                    .unwrap_or(i64::from(srv.id));
                out.push(TransportCandidate::TelegramReflector {
                    endpoint_id,
                    server_id: srv.id.max(1),
                    addr,
                    raw_peer_tag,
                    is_tcp: srv.is_tcp,
                });
                seen_reflector = true;
            }
        } else if srv.is_turn {
            out.push(TransportCandidate::TurnServer {
                addr,
                username: srv.login.clone(),
                password: srv.password.clone(),
            });
        } else if enable_p2p {
            out.push(TransportCandidate::StunServer { addr });
        }
    }

    // Fallback to legacy `endpoints` if `rtc_servers` didn't carry reflectors.
    if !seen_reflector {
        for (idx, ep) in endpoints.iter().enumerate() {
            let server_id = u8::try_from(idx + 1).unwrap_or(u8::MAX);
            for host in [&ep.host.ipv4, &ep.host.ipv6] {
                if let Ok(ip) = host.parse::<IpAddr>() {
                    out.push(TransportCandidate::TelegramReflector {
                        endpoint_id: ep.endpoint_id,
                        server_id,
                        addr: SocketAddr::new(ip, ep.port),
                        raw_peer_tag: ep.peer_tag,
                        is_tcp: false,
                    });
                }
            }
        }
    }

    out
}

/// Format an SDP ICE candidate attribute string for a Telegram reflector candidate.
#[must_use]
pub fn format_reflector_sdp_candidate(server_id: u8, local_tag: u32, port: u16) -> String {
    let host = format_reflector_hostname(server_id, local_tag);
    format!("candidate:reflector-{server_id} 1 udp 16777215 {host} {port} typ relay")
}

/// Format an SDP ICE candidate attribute string for a direct P2P host UDP candidate.
#[must_use]
pub fn format_host_sdp_candidate(addr: SocketAddr) -> String {
    format!(
        "candidate:host-1 1 udp 2122260223 {} {} typ host",
        addr.ip(),
        addr.port()
    )
}

/// Parse an SDP candidate string produced by `format_*_sdp_candidate` or `tgcalls`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedSdpCandidate {
    /// Synthetic Telegram reflector candidate (`reflector-<id>-<tag>.reflector:<port>`).
    Reflector {
        /// 1-based server ID.
        server_id: u8,
        /// Remote client's 4-byte `random_tag`.
        remote_tag: u32,
        /// Reflector port.
        port: u16,
    },
    /// Direct IPv4/IPv6 UDP candidate (`typ host` / `typ srflx` / `typ prflx`).
    DirectUdp {
        /// Socket address.
        addr: SocketAddr,
        /// Candidate type string (`"host"`, `"srflx"`, `"relay"`, etc.).
        cand_type: String,
    },
}

/// Parse an SDP candidate string (`candidate:... <host> <port> typ <type>`).
#[must_use]
pub fn parse_sdp_candidate(sdp: &str) -> Option<ParsedSdpCandidate> {
    let trimmed = sdp.trim().strip_prefix("a=").unwrap_or(sdp.trim());
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() < 8 {
        return None;
    }
    if !parts[2].eq_ignore_ascii_case("udp") {
        return None;
    }
    let host = parts[4];
    let port: u16 = parts[5].parse().ok()?;
    let cand_type = parts
        .windows(2)
        .find(|w| w[0] == "typ")
        .map(|w| w[1].to_string())
        .unwrap_or_else(|| "host".into());

    if let Some((server_id, remote_tag)) = parse_reflector_hostname(host) {
        return Some(ParsedSdpCandidate::Reflector {
            server_id,
            remote_tag,
            port,
        });
    }
    let ip: IpAddr = host.parse().ok()?;
    Some(ParsedSdpCandidate::DirectUdp {
        addr: SocketAddr::new(ip, port),
        cand_type,
    })
}

// ---------------------------------------------------------------------------
// Local UDP Reflector Stand-In (for integration / loopback testing)
// ---------------------------------------------------------------------------

/// Network impairment parameters for [`LoopbackReflector`] simulation.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReflectorImpairment {
    /// Deterministic drop pattern: drop every Nth data packet (`0` = no loss).
    pub drop_every_n: u64,
    /// Swap consecutive pairs of data packets every Nth packet (`0` = no reordering).
    pub reorder_every_n: u64,
}

/// Statistics recorded by a running [`LoopbackReflector`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoopbackReflectorStats {
    /// Total UDP hello packets processed.
    pub hellos_handled: u64,
    /// Total data packets forwarded to peers.
    pub packets_forwarded: u64,
    /// Total data packets dropped due to simulated loss.
    pub packets_dropped: u64,
    /// Total data packets reordered due to simulated reordering.
    pub packets_reordered: u64,
}

/// In-process local UDP Telegram reflector relay implementing the exact `ReflectorPort.cpp`
/// wire protocol, with optional deterministic packet loss and reordering for tests.
pub struct LoopbackReflector {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    hellos: Arc<AtomicU64>,
    forwarded: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    reordered: Arc<AtomicU64>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl LoopbackReflector {
    /// Bind a loopback UDP reflector on `127.0.0.1:0` with optional simulated impairments.
    pub async fn start(impairment: ReflectorImpairment) -> VoipResult<Self> {
        let socket = UdpSocket::bind("127.0.0.1:0").await?;
        let addr = socket.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let hellos = Arc::new(AtomicU64::new(0));
        let forwarded = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let reordered = Arc::new(AtomicU64::new(0));

        let stop_c = Arc::clone(&stop);
        let hellos_c = Arc::clone(&hellos);
        let forwarded_c = Arc::clone(&forwarded);
        let dropped_c = Arc::clone(&dropped);
        let reordered_c = Arc::clone(&reordered);

        let handle = tokio::spawn(async move {
            let routes: Mutex<HashMap<[u8; 16], SocketAddr>> = Mutex::new(HashMap::new());
            let mut held_for_reorder: Option<(SocketAddr, Vec<u8>)> = None;
            let mut data_counter = 0u64;
            let mut buf = vec![0u8; 2048];

            while !stop_c.load(Ordering::Acquire) {
                let recv = tokio::time::timeout(
                    std::time::Duration::from_millis(50),
                    socket.recv_from(&mut buf),
                )
                .await;
                let Ok(Ok((n, src))) = recv else {
                    continue;
                };
                if n < 16 {
                    continue;
                }
                let pkt = &buf[..n];
                let mut key16 = [0u8; 16];
                key16.copy_from_slice(&pkt[0..16]);

                let is_hello = n <= 24 || (n >= 28 && pkt[16..28] == REFLECTOR_SPECIAL_TAG);
                if is_hello {
                    routes.lock().await.insert(key16, src);
                    hellos_c.fetch_add(1, Ordering::Relaxed);
                    // Echo the hello back so `ReflectorPort` transitions to STATE_READY.
                    let _ = socket.send_to(pkt, src).await;
                    continue;
                }

                // Data packet: register sender's `(prefix[0..12] || sender_tag[16..20])` too.
                let mut sender_key = [0u8; 16];
                sender_key[..12].copy_from_slice(&pkt[0..12]);
                sender_key[12..16].copy_from_slice(&pkt[16..20]);
                let target_addr = {
                    let mut map = routes.lock().await;
                    map.insert(sender_key, src);
                    map.get(&key16).copied()
                };

                let Some(dst) = target_addr else {
                    continue;
                };

                data_counter += 1;
                if impairment.drop_every_n > 0
                    && data_counter.is_multiple_of(impairment.drop_every_n)
                {
                    dropped_c.fetch_add(1, Ordering::Relaxed);
                    continue;
                }

                if let Some((held_dst, held_pkt)) = held_for_reorder.take() {
                    // Send current packet FIRST, then the previously held packet (swapped order!).
                    let _ = socket.send_to(pkt, dst).await;
                    let _ = socket.send_to(&held_pkt, held_dst).await;
                    forwarded_c.fetch_add(2, Ordering::Relaxed);
                    reordered_c.fetch_add(1, Ordering::Relaxed);
                    continue;
                }

                if impairment.reorder_every_n > 0
                    && data_counter.is_multiple_of(impairment.reorder_every_n)
                {
                    held_for_reorder = Some((dst, pkt.to_vec()));
                    continue;
                }

                let _ = socket.send_to(pkt, dst).await;
                forwarded_c.fetch_add(1, Ordering::Relaxed);
            }
        });

        Ok(Self {
            addr,
            stop,
            hellos,
            forwarded,
            dropped,
            reordered,
            handle: Some(handle),
        })
    }

    /// Bound UDP socket address of the loopback reflector.
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Snapshot of reflector packet counters.
    #[must_use]
    pub fn stats(&self) -> LoopbackReflectorStats {
        LoopbackReflectorStats {
            hellos_handled: self.hellos.load(Ordering::Relaxed),
            packets_forwarded: self.forwarded.load(Ordering::Relaxed),
            packets_dropped: self.dropped.load(Ordering::Relaxed),
            packets_reordered: self.reordered.load(Ordering::Relaxed),
        }
    }

    /// Stop the reflector task and release the UDP socket.
    pub async fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.await;
        }
    }
}

impl Drop for LoopbackReflector {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflector_hello_and_data_wire_format() {
        let raw_tag = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0xDE, 0xAD,
            0xBE, 0xEF,
        ];
        let alice = ReflectorTag::new(&raw_tag, 0x1122_3344);
        let bob = ReflectorTag::new(&raw_tag, 0x5566_7788);

        // Hello packet checks
        let hello = encode_reflector_udp_hello(&alice);
        assert_eq!(hello.len(), 40);
        assert_eq!(&hello[0..12], &raw_tag[0..12]);
        assert_eq!(&hello[12..16], &0x1122_3344u32.to_le_bytes());
        assert_eq!(&hello[16..28], &[0xFF; 12]);
        assert_eq!(&hello[28..32], &[0xFE, 0xFF, 0xFF, 0xFF]);
        assert_eq!(&hello[32..40], &123u64.to_be_bytes());

        assert_eq!(
            decode_reflector_udp_packet(&alice.prefix, &hello).unwrap(),
            ReflectorIncoming::HelloAck {
                echoed_tag: 0x1122_3344
            }
        );

        // Data packet checks (including 4-byte padding)
        let payload = b"hello"; // 5 bytes -> 24+5 = 29 -> padded to 32 bytes
        let wire = encode_reflector_udp_data(&alice, bob.local_tag, payload);
        assert_eq!(wire.len(), 32);
        assert_eq!(&wire[0..12], &raw_tag[0..12]);
        assert_eq!(&wire[12..16], &bob.local_tag.to_le_bytes());
        assert_eq!(&wire[16..20], &alice.local_tag.to_le_bytes());
        assert_eq!(&wire[20..24], &5u32.to_be_bytes());
        assert_eq!(&wire[24..29], b"hello");
        assert_eq!(&wire[29..32], &[0, 0, 0]);

        assert_eq!(
            decode_reflector_udp_packet(&bob.prefix, &wire).unwrap(),
            ReflectorIncoming::Data {
                sender_tag: alice.local_tag,
                payload: payload.to_vec()
            }
        );
    }

    #[test]
    fn stun_binding_request_and_response_roundtrip() {
        let tx_id = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let req = build_stun_binding_request(tx_id);
        assert!(is_stun_packet(&req));
        assert_eq!(&req[0..2], &STUN_BINDING_REQUEST.to_be_bytes());

        let mapped: SocketAddr = "203.0.113.42:54321".parse().unwrap();
        let resp = build_stun_binding_response(tx_id, mapped);
        let parsed = parse_stun_binding_response(&resp).unwrap();
        assert_eq!(parsed.tx_id, tx_id);
        assert_eq!(parsed.mapped_addr, Some(mapped));
    }

    #[test]
    fn reflector_hostname_and_sdp_candidate_roundtrip() {
        let sdp = format_reflector_sdp_candidate(2, 987_654_321, 599);
        assert_eq!(
            parse_sdp_candidate(&sdp),
            Some(ParsedSdpCandidate::Reflector {
                server_id: 2,
                remote_tag: 987_654_321,
                port: 599,
            })
        );

        let host_addr: SocketAddr = "127.0.0.1:45678".parse().unwrap();
        let host_sdp = format_host_sdp_candidate(host_addr);
        assert_eq!(
            parse_sdp_candidate(&host_sdp),
            Some(ParsedSdpCandidate::DirectUdp {
                addr: host_addr,
                cand_type: "host".into(),
            })
        );
    }

    #[test]
    fn privacy_suppresses_stun_when_p2p_disabled() {
        let servers = vec![
            RtcServer {
                id: 1,
                host: "127.0.0.1".into(),
                port: 599,
                login: "reflector".into(),
                password: "0102030405060708090a0b0c0d0e0f10".into(),
                is_turn: true,
                is_tcp: false,
            },
            RtcServer {
                id: 0,
                host: "198.51.100.1".into(),
                port: 3478,
                login: String::new(),
                password: String::new(),
                is_turn: false,
                is_tcp: false,
            },
        ];

        let p2p_off = collect_transport_candidates(&[], &servers, false);
        assert_eq!(p2p_off.len(), 1);
        assert!(matches!(
            p2p_off[0],
            TransportCandidate::TelegramReflector { .. }
        ));

        let p2p_on = collect_transport_candidates(&[], &servers, true);
        assert_eq!(p2p_on.len(), 2);
    }
}
