//! Call protocol constants, server timeout defaults, and connection descriptor builders.
//!
//! Port of `calls/calls_call.cpp` (`kMinLayer`, `kHangupTimeoutMs`, `kDefaultVersion`,
//! `CollectVersionsForApi`, `CollectEndpointIds`, `AppendEndpoint`, `AppendServer`)
//! and `mtproto/mtproto_config.h` (`call*TimeoutMs` defaults).

use std::collections::BTreeSet;

use crate::types::{PhoneCallProtocol, PhoneConnection};

/// Minimum VoIP protocol layer sent by tdesktop (`kMinLayer` in `calls_call.cpp:49`).
pub const MIN_LAYER: i32 = 65;

/// Maximum VoIP protocol layer (`tgcalls::Meta::MaxLayer()`).
///
/// Note: `Telegram/ThirdParty/tgcalls` is an unpopulated submodule in the reference checkout;
/// layer 92 is the standard `tgcalls` `InstanceImpl` / `InstanceV2Impl` max layer.
/// Callers may override this in [`CallProtocolConfig`].
pub const MAX_LAYER: i32 = 92;

/// Fallback `tgcalls` library version if `phoneCall.protocol.library_versions` is empty
/// (`kDefaultVersion` in `calls_call.cpp:71`).
pub const DEFAULT_VERSION: &str = "2.4.4";

/// Default ordered `tgcalls` library versions list aligned with `tgcalls::InstanceV2Impl`
/// (`MAX_LAYER = 92`).
///
/// Ordered newest-first to match `CollectVersionsForApi` (`calls/calls_call.cpp:212-214`),
/// which reverses `tgcalls::Meta::Versions()` before sending in `phoneCallProtocol`.
pub const DEFAULT_LIBRARY_VERSIONS: &[&str] = &["12.0.0", "9.0.0", "8.0.0", "7.0.0"];

/// Hangup timeout in milliseconds before forcing transition to `Ended`/`Failed`
/// (`kHangupTimeoutMs` in `calls_call.cpp:50`).
pub const HANGUP_TIMEOUT_MS: u64 = 5_000;

/// Interval in milliseconds between `phone.getCallConfig` refreshes (`24h`,
/// `kServerConfigUpdateTimeoutMs` in `calls_instance.cpp:52`).
pub const SERVER_CONFIG_UPDATE_TIMEOUT_MS: u64 = 24 * 3600 * 1_000;

/// Call timeout settings from `MTP::ConfigFields` (`mtproto/mtproto_config.h:34-37`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerCallConfig {
    /// Timeout waiting for `receive_date` after `phone.requestCall` (default 20 000 ms).
    pub call_receive_timeout_ms: u64,
    /// Timeout while ringing (incoming creation and outgoing `Ringing`, default 90 000 ms).
    pub call_ring_timeout_ms: u64,
    /// Media engine initialization timeout passed to controller (default 30 000 ms).
    pub call_connect_timeout_ms: u64,
    /// Media packet receive timeout passed to controller (default 10 000 ms).
    pub call_packet_timeout_ms: u64,
}

impl Default for ServerCallConfig {
    fn default() -> Self {
        Self {
            call_receive_timeout_ms: 20_000,
            call_ring_timeout_ms: 90_000,
            call_connect_timeout_ms: 30_000,
            call_packet_timeout_ms: 10_000,
        }
    }
}

/// Protocol capabilities advertised in `phone.requestCall`, `phone.acceptCall`, and
/// `phone.confirmCall`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallProtocolConfig {
    /// Minimum supported layer (default [`MIN_LAYER`] = 65).
    pub min_layer: i32,
    /// Maximum supported layer (default [`MAX_LAYER`] = 92).
    pub max_layer: i32,
    /// Supported library versions in priority order (newest first).
    pub library_versions: Vec<String>,
    /// Advertise UDP P2P support (default `true`).
    pub udp_p2p: bool,
    /// Advertise UDP reflector support (default `true`).
    pub udp_reflector: bool,
}

impl Default for CallProtocolConfig {
    fn default() -> Self {
        Self {
            min_layer: MIN_LAYER,
            max_layer: MAX_LAYER,
            library_versions: DEFAULT_LIBRARY_VERSIONS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            udp_p2p: true,
            udp_reflector: true,
        }
    }
}

impl CallProtocolConfig {
    /// Convert into the TL `PhoneCallProtocol` structure sent on the wire.
    pub fn to_tl_protocol(&self) -> PhoneCallProtocol {
        PhoneCallProtocol {
            udp_p2p: self.udp_p2p,
            udp_reflector: self.udp_reflector,
            min_layer: self.min_layer,
            max_layer: self.max_layer,
            library_versions: self.library_versions.clone(),
        }
    }
}

/// Host IPv4/IPv6 pair for a UDP relay endpoint (`tgcalls::EndpointHost`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointHost {
    /// IPv4 address string.
    pub ipv4: String,
    /// IPv6 address string.
    pub ipv6: String,
}

/// Endpoint transport type (`tgcalls::EndpointType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointType {
    /// Legacy Telegram UDP reflector (`EndpointType::UdpRelay`).
    UdpRelay,
}

/// Legacy UDP reflector endpoint passed to the media controller (`tgcalls::Endpoint`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Server-assigned connection ID.
    pub endpoint_id: i64,
    /// IPv4 / IPv6 addresses.
    pub host: EndpointHost,
    /// UDP port.
    pub port: u16,
    /// Endpoint type.
    pub kind: EndpointType,
    /// 16-byte reflector authentication tag.
    pub peer_tag: [u8; 16],
}

/// WebRTC STUN or TURN server passed to the media controller (`tgcalls::RtcServer`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtcServer {
    /// 1-based reflector ID for Telegram reflectors (0 for STUN/TURN WebRTC servers).
    pub id: u8,
    /// Server IP or hostname.
    pub host: String,
    /// Server port.
    pub port: u16,
    /// TURN username (empty for STUN, `"reflector"` for Telegram reflectors).
    pub login: String,
    /// TURN password (empty for STUN, lowercase hex of `peer_tag` for Telegram reflectors).
    pub password: String,
    /// `true` for TURN, `false` for STUN.
    pub is_turn: bool,
    /// `true` for TCP TURN reflector.
    pub is_tcp: bool,
}

/// Convert TL `PhoneConnection` list into legacy UDP relay [`Endpoint`]s and WebRTC [`RtcServer`]s.
///
/// Exact port of `CollectEndpointIds`, `AppendEndpoint`, and `AppendServer` in
/// `calls/calls_call.cpp:77-186`.
pub fn collect_endpoints_and_servers(
    connections: &[PhoneConnection],
) -> (Vec<Endpoint>, Vec<RtcServer>) {
    // Collect unique sorted IDs of standard connections (`base::flat_set<int64>`).
    let ids: BTreeSet<i64> = connections
        .iter()
        .filter_map(|conn| match conn {
            PhoneConnection::Standard { id, .. } => Some(*id),
            PhoneConnection::Webrtc { .. } => None,
        })
        .collect();

    let mut endpoints = Vec::new();
    let mut rtc_servers = Vec::new();

    for conn in connections {
        if let PhoneConnection::Standard {
            tcp,
            id,
            ip,
            ipv6,
            port,
            peer_tag,
        } = conn
            && peer_tag.len() == 16
            && !*tcp
        {
            let mut tag = [0u8; 16];
            tag.copy_from_slice(peer_tag);
            endpoints.push(Endpoint {
                endpoint_id: *id,
                host: EndpointHost {
                    ipv4: ip.clone(),
                    ipv6: ipv6.clone(),
                },
                port: *port,
                kind: EndpointType::UdpRelay,
                peer_tag: tag,
            });
        }
    }

    for conn in connections {
        match conn {
            PhoneConnection::Standard {
                tcp,
                id,
                ip,
                ipv6,
                port,
                peer_tag,
            } => {
                let password = encode_lower_hex(peer_tag);
                // 1-based index into sorted unique IDs (matches `uint8_t((i - begin(ids)) + 1)`).
                let rank = ids.iter().position(|x| x == id).unwrap_or(0) + 1;
                let server_id = rank as u8;
                for host in [ip, ipv6] {
                    rtc_servers.push(RtcServer {
                        id: server_id,
                        host: host.clone(),
                        port: *port,
                        login: "reflector".to_string(),
                        password: password.clone(),
                        is_turn: true,
                        is_tcp: *tcp,
                    });
                }
            }
            PhoneConnection::Webrtc {
                turn,
                stun,
                ip,
                ipv6,
                port,
                username,
                password,
                ..
            } => {
                if *stun {
                    for host in [ip, ipv6] {
                        if !host.is_empty() {
                            rtc_servers.push(RtcServer {
                                id: 0,
                                host: host.clone(),
                                port: *port,
                                login: String::new(),
                                password: String::new(),
                                is_turn: false,
                                is_tcp: false,
                            });
                        }
                    }
                }
                if *turn && !username.is_empty() && !password.is_empty() {
                    for host in [ip, ipv6] {
                        rtc_servers.push(RtcServer {
                            id: 0,
                            host: host.clone(),
                            port: *port,
                            login: username.clone(),
                            password: password.clone(),
                            is_turn: true,
                            is_tcp: false,
                        });
                    }
                }
            }
        }
    }

    (endpoints, rtc_servers)
}

fn encode_lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}
