//! Loopback integration tests for WP-40 `tg-voip` transport interop:
//! - Telegram TCP reflector (`RawTcpSocket` + `ReflectorPort` TCP mode)
//! - RFC 5766 TURN (401 auth challenge, `Allocate`, `CreatePermission`, `ChannelBind`,
//!   `SendIndication`/`DataIndication`, `ChannelData`)
//! - SCTP data-channel packet routing (`VoipPacket::Sctp` -> `MediaEngineEvent::DataChannelMessage`)
//!
//! All tests run strictly against local loopback fake servers (`127.0.0.1:0`).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tg_calls::{ControllerDescriptor, ControllerState, Endpoint, RtcServer};
use tg_voip::{
    AUTH_KEY_SIZE, AudioBackendKind, CallMediaEngine, LoopbackTcpReflector, LoopbackTurnServer,
    MediaEngine, MediaEngineConfig, MediaEngineEvent, NetworkType, SCTP_PPID_BINARY,
    SCTP_PPID_STRING, SctpDataChannelRouter, TransportWireMode, VoipPacket, VoipTransport,
    decode_turn_channel_data, drain_tcp_reflector_frames, encode_tcp_reflector_frame,
    encode_turn_channel_data, md5_digest, turn_long_term_key,
};

fn test_auth_key() -> [u8; AUTH_KEY_SIZE] {
    let mut k = [0u8; AUTH_KEY_SIZE];
    for (i, b) in k.iter_mut().enumerate() {
        *b = ((i as u8).wrapping_mul(37)).wrapping_add(11);
    }
    k
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

#[test]
fn rfc1321_md5_and_turn_framing_known_answers() {
    // RFC 1321 §A.5 test vectors.
    assert_eq!(
        hex_encode(&md5_digest(b"")),
        "d41d8cd98f00b204e9800998ecf8427e"
    );
    assert_eq!(
        hex_encode(&md5_digest(b"abc")),
        "900150983cd24fb0d6963f7d28e17f72"
    );
    assert_eq!(
        hex_encode(&md5_digest(b"message digest")),
        "f96b697d7cb7938d525a2f31aaf161d0"
    );

    // RFC 5389 §15.4 TURN long-term credential key: MD5("user:realm:pass").
    let key = turn_long_term_key("user", "realm", "pass");
    assert_eq!(key, md5_digest(b"user:realm:pass"));

    // ChannelData roundtrip.
    let wire = encode_turn_channel_data(0x4000, b"hello turn");
    let (ch, payload) = decode_turn_channel_data(&wire).expect("decode channel data");
    assert_eq!(ch, 0x4000);
    assert_eq!(payload, b"hello turn");

    // TCP reflector 2-byte big-endian length framing roundtrip.
    let mut stream_buf = Vec::new();
    stream_buf.extend_from_slice(&encode_tcp_reflector_frame(b"frame-one"));
    stream_buf.extend_from_slice(&encode_tcp_reflector_frame(b"frame-two"));
    let frames = drain_tcp_reflector_frames(&mut stream_buf);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0], b"frame-one");
    assert_eq!(frames[1], b"frame-two");
    assert!(stream_buf.is_empty());
}

#[test]
fn sctp_router_dedup_and_sack_tracking() {
    let mut sender = SctpDataChannelRouter::new();
    let mut receiver = SctpDataChannelRouter::new();

    let chunk1 = sender.prepare_send(0, "data", SCTP_PPID_STRING, b"{\"ping\":1}".to_vec());
    let out1 = receiver.handle_incoming(&chunk1).expect("rx chunk 1");
    let msg1 = out1.message.expect("delivered msg 1");
    assert_eq!(msg1.stream_id, 0);
    assert_eq!(msg1.label, "data");
    assert_eq!(msg1.ppid, SCTP_PPID_STRING);
    assert_eq!(msg1.payload, b"{\"ping\":1}");

    // Duplicate retransmission yields SACK but does not double-deliver.
    let dup = receiver.handle_incoming(&chunk1).expect("dup chunk 1");
    assert!(dup.message.is_none());
    assert!(dup.reply_sack.is_some());

    // Deliver SACK back to sender.
    let sack = out1.reply_sack.expect("sack");
    sender.handle_incoming(&sack).expect("sender handle sack");
    assert_eq!(sender.last_acked_tx_tsn(), 1);
    assert_eq!(sender.acks_received(), 1);
    assert_eq!(receiver.received_count(), 1);
}

#[tokio::test]
async fn loopback_tcp_reflector_relays_encrypted_voip_packets() {
    let raw_peer_tag: [u8; 16] = [
        0x10, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0x01, 0x02, 0x03,
        0x04,
    ];
    let mut prefix = [0u8; 12];
    prefix.copy_from_slice(&raw_peer_tag[..12]);

    let tcp_reflector = LoopbackTcpReflector::start(prefix)
        .await
        .expect("start loopback TCP reflector");

    let rtc_servers = vec![RtcServer {
        id: 1,
        host: tcp_reflector.addr().ip().to_string(),
        port: tcp_reflector.addr().port(),
        login: "reflector".to_owned(),
        password: hex_encode(&raw_peer_tag),
        is_turn: true,
        is_tcp: true,
    }];

    let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let auth_key = test_auth_key();

    let mut alice = VoipTransport::bind(
        loopback,
        auth_key,
        true,
        false,
        &[],
        &rtc_servers,
        TransportWireMode::MtProtoRaw,
    )
    .await
    .expect("bind alice");
    let mut bob = VoipTransport::bind(
        loopback,
        auth_key,
        false,
        false,
        &[],
        &rtc_servers,
        TransportWireMode::MtProtoRaw,
    )
    .await
    .expect("bind bob");

    // Exchange reflector candidates.
    for c in alice.initial_local_candidates() {
        bob.add_remote_candidate(&c.sdp_string);
    }
    for c in bob.initial_local_candidates() {
        alice.add_remote_candidate(&c.sdp_string);
    }

    alice
        .send_initial_probes(100, NetworkType::WiFi)
        .await
        .expect("alice tcp hello");
    bob.send_initial_probes(100, NetworkType::WiFi)
        .await
        .expect("bob tcp hello");

    // Wait briefly for both TCP hellos to be registered on the fake reflector.
    for _ in 0..20 {
        if tcp_reflector.hellos_received() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(tcp_reflector.hellos_received() >= 2);
    assert!(alice.has_usable_route());
    assert!(bob.has_usable_route());

    // Alice sends an encrypted Ping over the TCP reflector -> Bob receives & decrypts it.
    let sent = alice
        .send_voip_packet(
            &VoipPacket::Ping {
                sent_ms: 4242,
                nonce: 99,
            },
            200,
            NetworkType::WiFi,
        )
        .await
        .expect("alice send over tcp reflector");
    assert!(sent);

    let mut buf = [0u8; 4096];
    let mut got_ping = false;
    for _ in 0..10 {
        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            bob.recv_next(&mut buf, 250, NetworkType::WiFi),
        )
        .await
        .expect("bob recv timeout")
        .expect("bob recv");
        if outcome.packets.iter().any(|p| {
            matches!(
                p,
                VoipPacket::Ping {
                    sent_ms: 4242,
                    nonce: 99
                }
            )
        }) {
            got_ping = true;
            break;
        }
    }
    assert!(
        got_ping,
        "Bob did not receive encrypted Ping via TCP reflector"
    );
    assert!(tcp_reflector.packets_forwarded() >= 1);
}

#[tokio::test]
async fn loopback_turn_server_allocates_binds_and_relays_encrypted_packets() {
    let turn_srv = LoopbackTurnServer::start("turnuser", "turnpass", "rustgram.test")
        .await
        .expect("start fake TURN server");

    let rtc_servers = vec![RtcServer {
        id: 2,
        host: turn_srv.addr().ip().to_string(),
        port: turn_srv.addr().port(),
        login: "turnuser".to_owned(),
        password: "turnpass".to_owned(),
        is_turn: true,
        is_tcp: false,
    }];

    let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let auth_key = test_auth_key();

    // enable_p2p = false so traffic MUST flow solely through the TURN relay!
    let mut alice = VoipTransport::bind(
        loopback,
        auth_key,
        true,
        false,
        &[],
        &rtc_servers,
        TransportWireMode::MtProtoRaw,
    )
    .await
    .expect("bind alice");
    let mut bob = VoipTransport::bind(
        loopback,
        auth_key,
        false,
        false,
        &[],
        &rtc_servers,
        TransportWireMode::MtProtoRaw,
    )
    .await
    .expect("bind bob");

    alice
        .send_initial_probes(100, NetworkType::WiFi)
        .await
        .expect("alice allocate");
    bob.send_initial_probes(100, NetworkType::WiFi)
        .await
        .expect("bob allocate");

    // Drive 401 challenge -> authenticated Allocate -> Allocated `typ relay` candidates for both.
    let mut buf = [0u8; 4096];
    let mut alice_relay_sdp = None;
    let mut bob_relay_sdp = None;
    for _ in 0..10 {
        if alice_relay_sdp.is_none() {
            let out = tokio::time::timeout(
                Duration::from_secs(2),
                alice.recv_next(&mut buf, 120, NetworkType::WiFi),
            )
            .await
            .expect("alice turn step timeout")
            .expect("alice turn step");
            if let Some(c) = out.new_local_candidates.into_iter().next() {
                alice_relay_sdp = Some(c.sdp_string);
            }
        }
        if bob_relay_sdp.is_none() {
            let out = tokio::time::timeout(
                Duration::from_secs(2),
                bob.recv_next(&mut buf, 120, NetworkType::WiFi),
            )
            .await
            .expect("bob turn step timeout")
            .expect("bob turn step");
            if let Some(c) = out.new_local_candidates.into_iter().next() {
                bob_relay_sdp = Some(c.sdp_string);
            }
        }
        if alice_relay_sdp.is_some() && bob_relay_sdp.is_some() {
            break;
        }
    }

    let alice_sdp = alice_relay_sdp.expect("alice got TURN relay candidate");
    let bob_sdp = bob_relay_sdp.expect("bob got TURN relay candidate");
    assert!(alice_sdp.contains("typ relay"));
    assert!(bob_sdp.contains("typ relay"));
    assert_eq!(turn_srv.allocations_created(), 2);

    // Exchange TURN `typ relay` candidates -> CreatePermission & ChannelBind on both sides.
    alice.add_remote_candidate(&bob_sdp);
    bob.add_remote_candidate(&alice_sdp);
    alice
        .tick_keepalives(200, NetworkType::WiFi)
        .await
        .expect("alice bind");
    bob.tick_keepalives(200, NetworkType::WiFi)
        .await
        .expect("bob bind");
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(alice.has_usable_route());
    assert!(bob.has_usable_route());

    // Alice sends encrypted SCTP packet through TURN -> Bob receives it via TURN ChannelData!
    let sent = alice
        .send_voip_packet(
            &VoipPacket::Sctp(b"hello-over-turn".to_vec()),
            300,
            NetworkType::WiFi,
        )
        .await
        .expect("alice send over TURN");
    assert!(sent);

    let mut delivered = false;
    for _ in 0..12 {
        let out = tokio::time::timeout(
            Duration::from_secs(2),
            bob.recv_next(&mut buf, 350, NetworkType::WiFi),
        )
        .await
        .expect("bob recv TURN timeout")
        .expect("bob recv TURN");
        if out
            .packets
            .iter()
            .any(|p| matches!(p, VoipPacket::Sctp(b) if b == b"hello-over-turn"))
        {
            delivered = true;
            break;
        }
    }
    assert!(
        delivered,
        "Bob did not receive packet relayed via RFC 5766 TURN"
    );
    assert!(turn_srv.packets_relayed() >= 2);
}

#[tokio::test]
async fn media_engine_routes_sctp_data_channel_messages_over_tcp_reflector() {
    let raw_peer_tag: [u8; 16] = [
        0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
        0x88,
    ];
    let mut prefix = [0u8; 12];
    prefix.copy_from_slice(&raw_peer_tag[..12]);
    let tcp_reflector = LoopbackTcpReflector::start(prefix)
        .await
        .expect("start tcp reflector");

    let auth_key = test_auth_key();
    let rtc_servers = vec![RtcServer {
        id: 1,
        host: tcp_reflector.addr().ip().to_string(),
        port: tcp_reflector.addr().port(),
        login: "reflector".to_owned(),
        password: hex_encode(&raw_peer_tag),
        is_turn: true,
        is_tcp: true,
    }];

    let make_desc = |is_outgoing: bool| ControllerDescriptor {
        version: "12.0.0".to_owned(),
        initialization_timeout_ms: 15_000,
        receive_timeout_ms: 10_000,
        enable_p2p: false,
        max_api_layer: 92,
        encryption_key: auth_key,
        is_outgoing,
        endpoints: Vec::<Endpoint>::new(),
        rtc_servers: rtc_servers.clone(),
        custom_parameters: None,
    };

    let make_cfg = || MediaEngineConfig {
        audio_backend: AudioBackendKind::Virtual,
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        quality_tick_ms: 100,
        ..MediaEngineConfig::default()
    };

    let rt = tokio::runtime::Handle::current();
    let mut alice = MediaEngine::new(make_cfg(), rt.clone());
    let mut bob = MediaEngine::new(make_cfg(), rt);
    alice.start(make_desc(true)).expect("start alice");
    bob.start(make_desc(false)).expect("start bob");

    let mut alice_established = false;
    let mut bob_established = false;
    let mut sent_dc = false;
    let mut bob_got_text = false;
    let mut alice_got_binary = false;

    for _ in 0..80 {
        while let Some(ev) = alice.poll_event() {
            match ev {
                MediaEngineEvent::SignalingData(blob) => bob.receive_signaling_data(blob),
                MediaEngineEvent::StateChanged(ControllerState::Established) => {
                    alice_established = true;
                }
                MediaEngineEvent::DataChannelMessage {
                    stream_id,
                    label,
                    ppid,
                    payload,
                } if stream_id == 3
                    && label == "sync"
                    && ppid == SCTP_PPID_BINARY
                    && payload == [1, 2, 3, 4] =>
                {
                    alice_got_binary = true;
                }
                _ => {}
            }
        }
        while let Some(ev) = bob.poll_event() {
            match ev {
                MediaEngineEvent::SignalingData(blob) => alice.receive_signaling_data(blob),
                MediaEngineEvent::StateChanged(ControllerState::Established) => {
                    bob_established = true;
                }
                MediaEngineEvent::DataChannelMessage {
                    stream_id,
                    label,
                    ppid,
                    payload,
                } if stream_id == 0
                    && label == "data"
                    && ppid == SCTP_PPID_STRING
                    && payload == b"{\"hello\":\"sctp\"}" =>
                {
                    bob_got_text = true;
                    bob.send_data_channel_bytes(3, "sync", vec![1, 2, 3, 4]);
                }
                _ => {}
            }
        }

        if alice_established && bob_established && !sent_dc {
            alice.send_data_channel_text("data", "{\"hello\":\"sctp\"}");
            sent_dc = true;
        }
        if bob_got_text && alice_got_binary {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let _ = alice.stop_async().await;
    let _ = bob.stop_async().await;

    assert!(
        alice_established && bob_established,
        "TCP reflector call failed to establish"
    );
    assert!(
        bob_got_text,
        "Bob did not receive SCTP text data channel message"
    );
    assert!(
        alice_got_binary,
        "Alice did not receive SCTP binary data channel reply"
    );
}
