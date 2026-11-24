//! Async UDP transport for 1:1 calls: Telegram UDP reflectors, direct P2P candidates,
//! STUN binding probes, keepalives, and MTProto-2.0 CTR packet encryption.
//!
//! Port of `TelegramMessenger/tgcalls` (`development` branch):
//! - `tgcalls/v2/ReflectorPort.cpp`
//! - `tgcalls/v2/MtProtoDtlsTransport.cpp`
//! - `tgcalls/EncryptedConnection.cpp`
//!
//! # Notes on `TODO(tgcalls)` Interop Details
//! - `TODO(tgcalls)`: Live interoperability with official Telegram desktop/mobile clients
//!   on versions `7.0.0–12.0.0` uses ICE STUN connectivity checks (`USERNAME`/`PRIORITY`/
//!   `USE-CANDIDATE`/`ICE-CONTROLLING`) and DTLS-SRTP (or `MtProtoDtlsTransport` on v12/v13)
//!   over the reflector candidate before RTP media flows. Here we implement the verified
//!   `ReflectorPort` UDP framing, `TurnCustomizerImpl` STUN binding probes, and
//!   `EncryptedConnection` (`ChannelType::Transport`) AES-256-CTR framing directly; full
//!   ICE-agent STUN consent freshness and DTLS handshake (for legacy non-MTProto peers)
//!   remain follow-up items for live Telegram server testing.

use std::net::SocketAddr;

use rand::RngCore;
use tg_calls::{Endpoint, RtcServer};
use tokio::net::UdpSocket;

use crate::crypto::{AUTH_KEY_SIZE, EncryptedConnection, TransportMessage};
use crate::error::VoipResult;
use crate::quality::{ConnectionEndpointKind, NetworkType, TrafficStats};
use crate::reflector::{
    ParsedSdpCandidate, REFLECTOR_CONNECTING_PING_MS, REFLECTOR_READY_PING_MS, ReflectorIncoming,
    ReflectorTag, TransportCandidate, build_stun_binding_request, collect_transport_candidates,
    decode_reflector_udp_packet, encode_reflector_udp_data, encode_reflector_udp_hello,
    format_host_sdp_candidate, format_reflector_sdp_candidate, is_stun_packet, parse_sdp_candidate,
    parse_stun_binding_response,
};
use crate::rtp::VoipPacket;
use crate::signaling::SignalingIceCandidate;
use crate::turn::{
    TcpReflectorRoute, TurnAllocationClient, TurnIncomingOutcome, drain_tcp_reflector_frames,
    format_turn_relay_sdp_candidate, is_turn_channel_data, tcp_read_chunk,
};

/// Wire encapsulation mode inside [`EncryptedConnection`] (`ChannelType::Transport`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransportWireMode {
    /// `tgcalls` v12/v13 `MtProtoDtlsTransport` raw framing (`seq || 127 || len_be || payload`).
    #[default]
    MtProtoRaw,
    /// Legacy `tgcalls/Message.cpp` typed `AudioDataMessage` (`seq || 5 || payload`).
    LegacyMessage,
}

#[derive(Debug, Clone)]
struct ReflectorRoute {
    endpoint_id: i64,
    server_id: u8,
    addr: SocketAddr,
    tag: ReflectorTag,
    ready: bool,
    announced: bool,
    remote_tag: Option<u32>,
    last_hello_ms: u64,
    last_recv_ms: u64,
}

#[derive(Debug, Clone)]
struct DirectRoute {
    addr: SocketAddr,
    verified: bool,
    last_recv_ms: u64,
}

#[derive(Debug, Clone)]
struct StunProbe {
    addr: SocketAddr,
    tx_id: [u8; 12],
    resolved: Option<SocketAddr>,
}

/// Output produced by [`VoipTransport::recv_next`].
#[derive(Debug)]
pub struct TransportRecvOutcome {
    /// Decrypted VoIP media/control packets extracted from the received datagram.
    pub packets: Vec<VoipPacket>,
    /// Newly discovered local ICE candidates to send via encrypted signaling (e.g. once
    /// a reflector replies to our hello or STUN returns a `srflx` address).
    pub new_local_candidates: Vec<SignalingIceCandidate>,
    /// Whether the preferred relay ID changed on this receive.
    pub preferred_relay_changed: Option<i64>,
}

/// Async UDP transport engine managing Telegram reflectors, P2P routes, STUN probes,
/// keepalives, and MTProto-2.0 AES-256-CTR transport encryption.
pub struct VoipTransport {
    socket: UdpSocket,
    local_addr: SocketAddr,
    enable_p2p: bool,
    wire_mode: TransportWireMode,
    crypto: EncryptedConnection,
    reflectors: Vec<ReflectorRoute>,
    tcp_reflectors: Vec<TcpReflectorRoute>,
    turn_clients: Vec<TurnAllocationClient>,
    turn_remote_peers: Vec<SocketAddr>,
    direct_routes: Vec<DirectRoute>,
    stun_probes: Vec<StunProbe>,
    active_relay_id: i64,
    active_endpoint_kind: ConnectionEndpointKind,
    traffic: TrafficStats,
    packets_sent: u64,
    packets_received: u64,
    packets_decrypt_failed: u64,
    ping_nonce: u32,
}

impl VoipTransport {
    /// Bind a UDP socket and initialize routes from `ControllerDescriptor` parameters.
    #[allow(clippy::too_many_arguments)]
    pub async fn bind(
        bind_addr: SocketAddr,
        auth_key: [u8; AUTH_KEY_SIZE],
        is_outgoing: bool,
        enable_p2p: bool,
        endpoints: &[Endpoint],
        rtc_servers: &[RtcServer],
        wire_mode: TransportWireMode,
    ) -> VoipResult<Self> {
        let socket = UdpSocket::bind(bind_addr).await?;
        let local_addr = socket.local_addr()?;

        let candidates = collect_transport_candidates(endpoints, rtc_servers, enable_p2p);
        let mut reflectors = Vec::new();
        let mut tcp_reflectors = Vec::new();
        let mut turn_clients = Vec::new();
        let mut stun_probes = Vec::new();

        for cand in candidates {
            match cand {
                TransportCandidate::TelegramReflector {
                    endpoint_id,
                    server_id,
                    addr,
                    raw_peer_tag,
                    is_tcp,
                } if !is_tcp => {
                    reflectors.push(ReflectorRoute {
                        endpoint_id,
                        server_id,
                        addr,
                        tag: ReflectorTag::with_random_tag(&raw_peer_tag),
                        ready: false,
                        announced: false,
                        remote_tag: None,
                        last_hello_ms: 0,
                        last_recv_ms: 0,
                    });
                }
                TransportCandidate::TelegramReflector {
                    endpoint_id,
                    server_id,
                    addr,
                    raw_peer_tag,
                    is_tcp: true,
                } => {
                    tcp_reflectors.push(TcpReflectorRoute::new(
                        endpoint_id,
                        server_id,
                        addr,
                        ReflectorTag::with_random_tag(&raw_peer_tag),
                    ));
                }
                TransportCandidate::TurnServer {
                    addr,
                    username,
                    password,
                } => {
                    turn_clients.push(TurnAllocationClient::new(addr, username, password));
                }
                TransportCandidate::StunServer { addr } if enable_p2p => {
                    let mut tx_id = [0u8; 12];
                    rand::rngs::OsRng.fill_bytes(&mut tx_id);
                    stun_probes.push(StunProbe {
                        addr,
                        tx_id,
                        resolved: None,
                    });
                }
                TransportCandidate::DirectP2p { addr } if enable_p2p => {
                    // Pre-configured direct peer address (unused in normal Telegram calls).
                    let _ = addr;
                }
                _ => {}
            }
        }

        let initial_relay_id = reflectors
            .first()
            .map(|r| r.endpoint_id)
            .or_else(|| tcp_reflectors.first().map(|r| r.endpoint_id))
            .unwrap_or(0);
        let initial_kind = if reflectors.is_empty()
            && tcp_reflectors.is_empty()
            && turn_clients.is_empty()
            && enable_p2p
        {
            ConnectionEndpointKind::P2p
        } else {
            ConnectionEndpointKind::TurnOrReflector
        };

        Ok(Self {
            socket,
            local_addr,
            enable_p2p,
            wire_mode,
            crypto: EncryptedConnection::new_transport(auth_key, is_outgoing),
            reflectors,
            tcp_reflectors,
            turn_clients,
            turn_remote_peers: Vec::new(),
            direct_routes: Vec::new(),
            stun_probes,
            active_relay_id: initial_relay_id,
            active_endpoint_kind: initial_kind,
            traffic: TrafficStats::default(),
            packets_sent: 0,
            packets_received: 0,
            packets_decrypt_failed: 0,
            ping_nonce: 0,
        })
    }

    /// Local bound UDP socket address.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Return initial local ICE candidates to advertise via encrypted signaling.
    ///
    /// When `enable_p2p == false`, host candidates are strictly omitted to preserve IP privacy.
    #[must_use]
    pub fn initial_local_candidates(&mut self) -> Vec<SignalingIceCandidate> {
        let mut out = Vec::new();
        if self.enable_p2p && !self.local_addr.ip().is_unspecified() {
            out.push(SignalingIceCandidate {
                sdp_string: format_host_sdp_candidate(self.local_addr),
            });
        }
        // Advertise reflector candidates immediately as well (and re-announce once ready if needed).
        for r in &mut self.reflectors {
            out.push(SignalingIceCandidate {
                sdp_string: format_reflector_sdp_candidate(
                    r.server_id,
                    r.tag.local_tag,
                    r.addr.port(),
                ),
            });
            r.announced = true;
        }
        for r in &mut self.tcp_reflectors {
            out.push(SignalingIceCandidate {
                sdp_string: format_reflector_sdp_candidate(
                    r.server_id,
                    r.tag.local_tag,
                    r.addr.port(),
                ),
            });
            r.announced = true;
        }
        out
    }

    /// Register a remote candidate received via encrypted `SignalingMessage::Candidates`.
    pub fn add_remote_candidate(&mut self, sdp_string: &str) {
        let Some(parsed) = parse_sdp_candidate(sdp_string) else {
            return;
        };
        match parsed {
            ParsedSdpCandidate::Reflector {
                server_id,
                remote_tag,
                port,
            } => {
                for r in &mut self.reflectors {
                    if r.server_id == server_id && r.addr.port() == port {
                        r.remote_tag = Some(remote_tag);
                    }
                }
                for r in &mut self.tcp_reflectors {
                    if r.server_id == server_id && r.addr.port() == port {
                        r.remote_tag = Some(remote_tag);
                    }
                }
            }
            ParsedSdpCandidate::DirectUdp { addr, cand_type } => {
                if !self.turn_clients.is_empty() && !self.turn_remote_peers.contains(&addr) {
                    self.turn_remote_peers.push(addr);
                }
                if self.enable_p2p
                    && cand_type != "relay"
                    && !self.direct_routes.iter().any(|d| d.addr == addr)
                {
                    self.direct_routes.push(DirectRoute {
                        addr,
                        verified: false,
                        last_recv_ms: 0,
                    });
                }
            }
        }
    }

    /// Send initial Telegram reflector UDP/TCP hellos, TURN Allocate requests, and (if P2P enabled)
    /// STUN binding requests.
    pub async fn send_initial_probes(
        &mut self,
        now_ms: u64,
        network: NetworkType,
    ) -> VoipResult<()> {
        for r in &mut self.reflectors {
            let hello = encode_reflector_udp_hello(&r.tag);
            let _ = self.socket.send_to(&hello, r.addr).await;
            r.last_hello_ms = now_ms;
            self.traffic.record_sent(hello.len() as u64, network);
        }
        for r in &mut self.tcp_reflectors {
            if r.stream.is_none()
                && let Ok(bytes_sent) = r.connect_and_hello().await
            {
                self.traffic.record_sent(bytes_sent as u64, network);
            }
        }
        for turn in &self.turn_clients {
            let req = turn.start_allocate();
            let _ = self.socket.send_to(&req, turn.server_addr).await;
            self.traffic.record_sent(req.len() as u64, network);
        }
        if self.enable_p2p {
            for probe in &self.stun_probes {
                let req = build_stun_binding_request(probe.tx_id);
                let _ = self.socket.send_to(&req, probe.addr).await;
                self.traffic.record_sent(req.len() as u64, network);
            }
        }
        Ok(())
    }

    /// Send periodic reflector keepalives, TURN refreshes/bindings, encrypted RTT pings, and
    /// pending ACK service packets.
    pub async fn tick_keepalives(&mut self, now_ms: u64, network: NetworkType) -> VoipResult<()> {
        // 1. Telegram reflector hello/keepalive packets (500 ms connecting, 10 s ready).
        for r in &mut self.reflectors {
            let interval = if r.ready {
                REFLECTOR_READY_PING_MS
            } else {
                REFLECTOR_CONNECTING_PING_MS
            };
            if now_ms.saturating_sub(r.last_hello_ms) >= interval {
                let hello = encode_reflector_udp_hello(&r.tag);
                let _ = self.socket.send_to(&hello, r.addr).await;
                r.last_hello_ms = now_ms;
                self.traffic.record_sent(hello.len() as u64, network);
            }
        }

        // 1b. Reconnect disconnected TCP reflectors.
        for r in &mut self.tcp_reflectors {
            if r.stream.is_none()
                && let Ok(bytes_sent) = r.connect_and_hello().await
            {
                self.traffic.record_sent(bytes_sent as u64, network);
            }
        }

        // 1c. Refresh TURN allocations & ensure CreatePermission/ChannelBind for known peers.
        for turn in &mut self.turn_clients {
            if let Some(refresh) = turn.maybe_refresh(now_ms) {
                let _ = self.socket.send_to(&refresh, turn.server_addr).await;
                self.traffic.record_sent(refresh.len() as u64, network);
            }
            if turn.relayed_addr().is_some() {
                for &peer_addr in &self.turn_remote_peers {
                    for ctrl in turn.ensure_peer_channel(peer_addr) {
                        let _ = self.socket.send_to(&ctrl, turn.server_addr).await;
                        self.traffic.record_sent(ctrl.len() as u64, network);
                    }
                }
            }
        }

        // 2. Flush any pending EncryptedConnection ACKs / reliable resends.
        if let Ok(Some(service)) = self.crypto.prepare_for_sending_service(now_ms) {
            let _ = self.send_encrypted_bytes(&service.bytes, network).await;
        }

        // 3. Send an encrypted RTT keepalive Ping over the best available route(s).
        self.ping_nonce = self.ping_nonce.wrapping_add(1);
        let ping = VoipPacket::Ping {
            sent_ms: now_ms,
            nonce: self.ping_nonce,
        };
        let _ = self.send_voip_packet(&ping, now_ms, network).await;

        Ok(())
    }

    /// Encrypt and transmit a [`VoipPacket`] over the active transport route.
    ///
    /// Returns `Ok(true)` if a destination route was available and the datagram was sent.
    pub async fn send_voip_packet(
        &mut self,
        packet: &VoipPacket,
        now_ms: u64,
        network: NetworkType,
    ) -> VoipResult<bool> {
        let raw_payload = packet.serialize();
        let encrypted = match self.wire_mode {
            TransportWireMode::MtProtoRaw => {
                self.crypto
                    .prepare_for_sending_raw_message(&raw_payload, false, now_ms)?
            }
            TransportWireMode::LegacyMessage => {
                let msg = TransportMessage::AudioData(raw_payload);
                self.crypto.prepare_for_sending(&msg, now_ms)?
            }
        };

        let sent = self.send_encrypted_bytes(&encrypted.bytes, network).await?;
        if sent {
            self.packets_sent += 1;
        }
        Ok(sent)
    }

    /// Receive and process the next UDP datagram or TCP reflector frame.
    pub async fn recv_next(
        &mut self,
        buf: &mut [u8],
        now_ms: u64,
        network: NetworkType,
    ) -> VoipResult<TransportRecvOutcome> {
        let mut outcome = TransportRecvOutcome {
            packets: Vec::new(),
            new_local_candidates: Vec::new(),
            preferred_relay_changed: None,
        };

        // Drain any already-buffered TCP reflector frames before awaiting sockets.
        if self.process_buffered_tcp_frames(&mut outcome) {
            return Ok(outcome);
        }

        // Await either UDP datagram or connected primary TCP reflector chunk.
        let tcp_idx = self.tcp_reflectors.iter().position(|r| r.stream.is_some());
        let udp_received: Option<(usize, SocketAddr)> = if let Some(idx) = tcp_idx {
            let socket = &self.socket;
            let tcp_route = &mut self.tcp_reflectors[idx];
            let stream_ref = tcp_route.stream.as_ref();
            if let Some(stream) = stream_ref {
                tokio::select! {
                    res = socket.recv_from(buf) => Some(res?),
                    res = tcp_read_chunk(stream, &mut tcp_route.read_buf) => {
                        match res {
                            Ok(0) | Err(_) => {
                                tcp_route.stream = None;
                                tcp_route.ready = false;
                            }
                            Ok(n) => {
                                self.traffic.record_received(n as u64, network);
                            }
                        }
                        None
                    }
                }
            } else {
                Some(socket.recv_from(buf).await?)
            }
        } else {
            Some(self.socket.recv_from(buf).await?)
        };

        let Some((n, src)) = udp_received else {
            self.process_buffered_tcp_frames(&mut outcome);
            return Ok(outcome);
        };

        let data = &buf[..n];
        self.traffic.record_received(n as u64, network);

        // 0. Check if datagram came from a configured RFC 5766 TURN server.
        if let Some(turn_idx) = self.turn_clients.iter().position(|t| t.server_addr == src)
            && (is_stun_packet(data) || is_turn_channel_data(data))
        {
            let server_addr = self.turn_clients[turn_idx].server_addr;
            if let Ok(turn_event) = self.turn_clients[turn_idx].handle_incoming(data, now_ms) {
                match turn_event {
                    TurnIncomingOutcome::SendControl(ctrl_pkts) => {
                        for pkt in ctrl_pkts {
                            let _ = self.socket.send_to(&pkt, server_addr).await;
                            self.traffic.record_sent(pkt.len() as u64, network);
                        }
                    }
                    TurnIncomingOutcome::Allocated { relayed_addr } => {
                        if !self.turn_clients[turn_idx].announced {
                            self.turn_clients[turn_idx].announced = true;
                            outcome.new_local_candidates.push(SignalingIceCandidate {
                                sdp_string: format_turn_relay_sdp_candidate(
                                    relayed_addr,
                                    self.local_addr,
                                ),
                            });
                        }
                        for &peer_addr in &self.turn_remote_peers {
                            for ctrl in self.turn_clients[turn_idx].ensure_peer_channel(peer_addr) {
                                let _ = self.socket.send_to(&ctrl, server_addr).await;
                                self.traffic.record_sent(ctrl.len() as u64, network);
                            }
                        }
                    }
                    TurnIncomingOutcome::RelayedData { peer_addr, payload } => {
                        if !self.turn_remote_peers.contains(&peer_addr) {
                            self.turn_remote_peers.push(peer_addr);
                        }
                        for ctrl in self.turn_clients[turn_idx].ensure_peer_channel(peer_addr) {
                            let _ = self.socket.send_to(&ctrl, server_addr).await;
                            self.traffic.record_sent(ctrl.len() as u64, network);
                        }
                        if self.decrypt_into(&payload, &mut outcome.packets)
                            && self.active_endpoint_kind != ConnectionEndpointKind::TurnOrReflector
                        {
                            self.active_endpoint_kind = ConnectionEndpointKind::TurnOrReflector;
                            outcome.preferred_relay_changed = Some(self.active_relay_id);
                        }
                    }
                    TurnIncomingOutcome::ControlAck => {}
                }
            }
            return Ok(outcome);
        }

        // 1. STUN Binding Response?
        if is_stun_packet(data) {
            if let Ok(resp) = parse_stun_binding_response(data)
                && let Some(mapped) = resp.mapped_addr
            {
                for probe in &mut self.stun_probes {
                    if probe.tx_id == resp.tx_id && probe.resolved.is_none() {
                        probe.resolved = Some(mapped);
                        if self.enable_p2p {
                            outcome.new_local_candidates.push(SignalingIceCandidate {
                                sdp_string: format!(
                                    "candidate:srflx-1 1 udp 1694498815 {} {} typ srflx raddr {} rport {}",
                                    mapped.ip(),
                                    mapped.port(),
                                    self.local_addr.ip(),
                                    self.local_addr.port()
                                ),
                            });
                        }
                    }
                }
            }
            return Ok(outcome);
        }

        // 2. Check if datagram came from one of our configured Telegram reflectors.
        if let Some(idx) = self.reflectors.iter().position(|r| r.addr == src) {
            let prefix = self.reflectors[idx].tag.prefix;
            match decode_reflector_udp_packet(&prefix, data) {
                Ok(ReflectorIncoming::HelloAck { .. }) => {
                    let route = &mut self.reflectors[idx];
                    let newly_ready = !route.ready;
                    route.ready = true;
                    route.last_recv_ms = now_ms;
                    if newly_ready && !route.announced {
                        route.announced = true;
                        outcome.new_local_candidates.push(SignalingIceCandidate {
                            sdp_string: format_reflector_sdp_candidate(
                                route.server_id,
                                route.tag.local_tag,
                                route.addr.port(),
                            ),
                        });
                    }
                }
                Ok(ReflectorIncoming::Data {
                    sender_tag,
                    payload,
                }) => {
                    let endpoint_id = {
                        let route = &mut self.reflectors[idx];
                        route.ready = true;
                        route.last_recv_ms = now_ms;
                        if route.remote_tag.is_none() && sender_tag != 0 {
                            route.remote_tag = Some(sender_tag);
                        }
                        route.endpoint_id
                    };

                    if self.decrypt_into(&payload, &mut outcome.packets)
                        && (self.active_relay_id != endpoint_id
                            || self.active_endpoint_kind != ConnectionEndpointKind::TurnOrReflector)
                    {
                        self.active_relay_id = endpoint_id;
                        self.active_endpoint_kind = ConnectionEndpointKind::TurnOrReflector;
                        outcome.preferred_relay_changed = Some(endpoint_id);
                    }
                }
                Err(_) => {
                    self.packets_decrypt_failed += 1;
                }
            }
            return Ok(outcome);
        }

        // 3. Direct P2P UDP packet (only accepted when `enable_p2p == true`).
        if self.enable_p2p && self.decrypt_into(data, &mut outcome.packets) {
            if let Some(d) = self.direct_routes.iter_mut().find(|d| d.addr == src) {
                d.verified = true;
                d.last_recv_ms = now_ms;
            } else {
                self.direct_routes.push(DirectRoute {
                    addr: src,
                    verified: true,
                    last_recv_ms: now_ms,
                });
            }
            if self.active_endpoint_kind != ConnectionEndpointKind::P2p || self.active_relay_id != 0
            {
                self.active_endpoint_kind = ConnectionEndpointKind::P2p;
                self.active_relay_id = 0;
                outcome.preferred_relay_changed = Some(0);
            }
        }

        Ok(outcome)
    }

    /// Returns `true` if at least one route can currently transmit to the remote peer.
    #[must_use]
    pub fn has_usable_route(&self) -> bool {
        let has_direct = self.enable_p2p && !self.direct_routes.is_empty();
        let has_reflector = self.reflectors.iter().any(|r| r.remote_tag.is_some());
        let has_tcp_reflector = self
            .tcp_reflectors
            .iter()
            .any(|r| r.ready && r.remote_tag.is_some());
        let has_turn = self.turn_clients.iter().any(|t| t.relayed_addr().is_some())
            && !self.turn_remote_peers.is_empty();
        has_direct || has_reflector || has_tcp_reflector || has_turn
    }

    /// Returns all currently allocated RFC 5766 TURN relay addresses.
    #[must_use]
    pub fn turn_relayed_addrs(&self) -> Vec<SocketAddr> {
        self.turn_clients
            .iter()
            .filter_map(TurnAllocationClient::relayed_addr)
            .collect()
    }

    /// Active preferred relay `endpoint_id` (`0` if direct P2P or none).
    #[must_use]
    pub const fn preferred_relay_id(&self) -> i64 {
        self.active_relay_id
    }

    /// Active endpoint route classification.
    #[must_use]
    pub const fn endpoint_kind(&self) -> ConnectionEndpointKind {
        self.active_endpoint_kind
    }

    /// Snapshot of cumulative traffic byte counters.
    #[must_use]
    pub const fn traffic_stats(&self) -> TrafficStats {
        self.traffic
    }

    /// Total encrypted media/control packets transmitted.
    #[must_use]
    pub const fn packets_sent(&self) -> u64 {
        self.packets_sent
    }

    /// Total encrypted media/control packets decrypted successfully.
    #[must_use]
    pub const fn packets_received(&self) -> u64 {
        self.packets_received
    }

    /// Total incoming packets rejected by auth, framing, or replay checks.
    #[must_use]
    pub const fn packets_decrypt_failed(&self) -> u64 {
        self.packets_decrypt_failed
    }

    async fn send_encrypted_bytes(
        &mut self,
        encrypted: &[u8],
        network: NetworkType,
    ) -> VoipResult<bool> {
        // Prefer a verified direct P2P route when P2P is enabled.
        if self.enable_p2p
            && let Some(direct) = self
                .direct_routes
                .iter()
                .find(|d| d.verified)
                .or_else(|| self.direct_routes.first())
        {
            self.socket.send_to(encrypted, direct.addr).await?;
            self.traffic.record_sent(encrypted.len() as u64, network);
            return Ok(true);
        }

        // Otherwise route through the best ready Telegram UDP reflector.
        if let Some(route) = self
            .reflectors
            .iter()
            .find(|r| r.ready && r.remote_tag.is_some())
            .or_else(|| self.reflectors.iter().find(|r| r.remote_tag.is_some()))
            && let Some(target_tag) = route.remote_tag
        {
            let wire = encode_reflector_udp_data(&route.tag, target_tag, encrypted);
            self.socket.send_to(&wire, route.addr).await?;
            self.traffic.record_sent(wire.len() as u64, network);
            return Ok(true);
        }

        // Fallback to a connected Telegram TCP reflector (`RawTcpSocket.cpp`).
        if let Some(route) = self
            .tcp_reflectors
            .iter_mut()
            .find(|r| r.ready && r.stream.is_some() && r.remote_tag.is_some())
            && let Some(target_tag) = route.remote_tag
            && let Ok(bytes_sent) = route.send_data(target_tag, encrypted).await
        {
            self.traffic.record_sent(bytes_sent as u64, network);
            return Ok(true);
        }

        // Fallback to an allocated RFC 5766 TURN server (ChannelData or SendIndication).
        if let Some(peer_addr) = self.turn_remote_peers.first().copied()
            && let Some(turn) = self
                .turn_clients
                .iter_mut()
                .find(|t| t.relayed_addr().is_some())
        {
            for ctrl in turn.ensure_peer_channel(peer_addr) {
                let _ = self.socket.send_to(&ctrl, turn.server_addr).await;
                self.traffic.record_sent(ctrl.len() as u64, network);
            }
            let wire = turn.wrap_outgoing(peer_addr, encrypted);
            self.socket.send_to(&wire, turn.server_addr).await?;
            self.traffic.record_sent(wire.len() as u64, network);
            return Ok(true);
        }

        Ok(false)
    }

    fn process_buffered_tcp_frames(&mut self, outcome: &mut TransportRecvOutcome) -> bool {
        let mut drained_any = false;
        let mut to_decrypt: Vec<(i64, Vec<u8>)> = Vec::new();
        for route in &mut self.tcp_reflectors {
            let frames = drain_tcp_reflector_frames(&mut route.read_buf);
            if frames.is_empty() {
                continue;
            }
            drained_any = true;
            let prefix = route.tag.prefix;
            for frame in frames {
                match decode_reflector_udp_packet(&prefix, &frame) {
                    Ok(ReflectorIncoming::HelloAck { .. }) => {
                        let newly_ready = !route.ready;
                        route.ready = true;
                        if newly_ready && !route.announced {
                            route.announced = true;
                            outcome.new_local_candidates.push(SignalingIceCandidate {
                                sdp_string: format_reflector_sdp_candidate(
                                    route.server_id,
                                    route.tag.local_tag,
                                    route.addr.port(),
                                ),
                            });
                        }
                    }
                    Ok(ReflectorIncoming::Data {
                        sender_tag,
                        payload,
                    }) => {
                        route.ready = true;
                        if route.remote_tag.is_none() && sender_tag != 0 {
                            route.remote_tag = Some(sender_tag);
                        }
                        to_decrypt.push((route.endpoint_id, payload));
                    }
                    Err(_) => {
                        self.packets_decrypt_failed += 1;
                    }
                }
            }
        }
        for (endpoint_id, payload) in to_decrypt {
            if self.decrypt_into(&payload, &mut outcome.packets)
                && (self.active_relay_id != endpoint_id
                    || self.active_endpoint_kind != ConnectionEndpointKind::TurnOrReflector)
            {
                self.active_relay_id = endpoint_id;
                self.active_endpoint_kind = ConnectionEndpointKind::TurnOrReflector;
                outcome.preferred_relay_changed = Some(endpoint_id);
            }
        }
        drained_any
    }

    fn decrypt_into(&mut self, encrypted: &[u8], out: &mut Vec<VoipPacket>) -> bool {
        let payloads: Vec<Vec<u8>> = match self.wire_mode {
            TransportWireMode::MtProtoRaw => {
                match self.crypto.handle_incoming_raw_packet(encrypted) {
                    Ok(msgs) => msgs.into_iter().map(|m| m.payload).collect(),
                    Err(_) => {
                        self.packets_decrypt_failed += 1;
                        return false;
                    }
                }
            }
            TransportWireMode::LegacyMessage => {
                match self.crypto.handle_incoming_packet(encrypted) {
                    Ok(msgs) => msgs
                        .into_iter()
                        .filter_map(|m| match m.message {
                            TransportMessage::AudioData(b)
                            | TransportMessage::UnstructuredData(b) => Some(b),
                            _ => None,
                        })
                        .collect(),
                    Err(_) => {
                        self.packets_decrypt_failed += 1;
                        return false;
                    }
                }
            }
        };

        let mut any = false;
        for raw in payloads {
            match VoipPacket::parse(&raw) {
                Ok(pkt) => {
                    out.push(pkt);
                    any = true;
                }
                Err(_) => {
                    self.packets_decrypt_failed += 1;
                }
            }
        }
        if any {
            self.packets_received += 1;
        }
        any
    }
}
