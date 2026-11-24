//! Integration tests for 1:1 video (H.264 RTP, Media Foundation encode->decode, rotation/PLI)
//! and group calls (DTLS 1.2 + SRTP UDP loopback, ICE-lite SFU + multi-SSRC mixing + speaking
//! levels, and chunked broadcast-stream playback).

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tg_calls::{
    ControllerDescriptor, Endpoint, EndpointHost, EndpointType, GroupCallSession, GroupJoinPayload,
    IceCandidateParam, InstanceMode, JoinResponseMediaSection, JoinResponseTransport,
    RtcJoinResponse, RtcServer, RtpHeaderExtensionParam, RtpPayloadTypeParam,
};
use tg_voip::{
    AUTH_KEY_SIZE, AudioBackendKind, BroadcastChunkLoader, BroadcastChunkRequest,
    BroadcastChunkResponse, BroadcastChunkScale, BroadcastChunkStatus, CallMediaEngine,
    DEFAULT_VOICE_BITRATE_BPS, DtlsIdentity, DtlsSrtpSession, GroupAudioMixer, GroupMediaConfig,
    GroupMediaEngine, GroupMediaEvent, H264RtpDepacketizer, H264RtpPacketizer, JitterBufferConfig,
    LoopbackReflector, MediaEngine, MediaEngineConfig, MediaEngineEvent, NAL_TYPE_FU_A,
    NAL_TYPE_IDR, NAL_TYPE_NON_IDR, NAL_TYPE_PPS, NAL_TYPE_SPS, NAL_TYPE_STAP_A, NetworkType,
    OPUS_RTP_PAYLOAD_TYPE, QualityController, ReflectorImpairment, RtpHeader, SyntheticVideoSource,
    TransportWireMode, VOIP_FRAME_SAMPLES, VOIP_SAMPLE_RATE, VideoCodecKind, VideoDecoder,
    VideoEncoder, VideoEncoderConfig, VideoFrame, VideoPixelFormat, VoiceDspConfig,
    VoiceOpusEncoder, WebRtcPacketKind, build_ice_binding_response, classify_webrtc_packet,
    create_h264_decoder, create_h264_encoder, encode_ogg_opus_chunk, join_annex_b_nalus,
    list_cameras, list_displays, nal_unit_type, verify_stun_integrity_and_fingerprint,
};
use tokio::net::UdpSocket;

fn goertzel_magnitude(samples: &[f32], target_hz: f32) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let n = samples.len() as f64;
    let omega = 2.0 * std::f64::consts::PI * f64::from(target_hz) / f64::from(VOIP_SAMPLE_RATE);
    let coeff = 2.0 * omega.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in samples {
        let s = f64::from(x) + coeff * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    (s2 * s2 + s1 * s1 - coeff * s1 * s2).max(0.0).sqrt() / n
}

fn encode_opus_sine_stream(freq_hz: f32, amplitude: f32, frames: usize) -> Vec<Vec<u8>> {
    let mut enc = VoiceOpusEncoder::new(DEFAULT_VOICE_BITRATE_BPS).unwrap();
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut pcm = [0.0f32; VOIP_FRAME_SAMPLES];
        for (i, s) in pcm.iter_mut().enumerate() {
            let t = (f * VOIP_FRAME_SAMPLES + i) as f32 / VOIP_SAMPLE_RATE as f32;
            *s = (2.0 * std::f32::consts::PI * freq_hz * t).sin() * amplitude;
        }
        out.push(enc.encode_frame(&pcm).unwrap());
    }
    out
}

// ---------------------------------------------------------------------------
// 1. H.264 RTP Packetize / Depacketize Round Trip (Single NAL, STAP-A, FU-A)
// ---------------------------------------------------------------------------

#[test]
fn h264_rtp_packetize_depacketize_roundtrip_all_modes() {
    let mut sps = vec![0x67];
    sps.extend((0..28u8).map(|i| i.wrapping_mul(3)));
    let mut pps = vec![0x68];
    pps.extend((0..10u8).map(|i| i.wrapping_mul(7)));
    let mut idr = vec![0x65];
    idr.extend((0..4200usize).map(|i| ((i * 19 + 5) & 0xFF) as u8));
    let mut p_frame = vec![0x41];
    p_frame.extend((0..300usize).map(|i| ((i * 11 + 3) & 0xFF) as u8));

    let kf_annex_b = join_annex_b_nalus(&[sps, pps, idr]);
    let pf_annex_b = join_annex_b_nalus(&[p_frame]);

    let mut packetizer = H264RtpPacketizer::new(0xCAFE_BABE).with_max_payload_size(600);
    let mut depacketizer = H264RtpDepacketizer::new();

    // Keyframe: SPS+PPS as STAP-A, IDR as FU-A.
    let kf_packets = packetizer.packetize_annex_b(&kf_annex_b, 90_000);
    assert!(kf_packets.len() >= 8);
    assert_eq!(nal_unit_type(kf_packets[0].payload[0]), NAL_TYPE_STAP_A);
    assert_eq!(nal_unit_type(kf_packets[1].payload[0]), NAL_TYPE_FU_A);
    assert!(kf_packets.last().unwrap().header.marker);

    let mut reassembled_kf = None;
    for pkt in &kf_packets {
        let out = depacketizer.push_packet(&pkt.header, &pkt.payload).unwrap();
        assert!(!out.needs_keyframe);
        if let Some(frame) = out.frame {
            reassembled_kf = Some(frame);
        }
    }
    let kf = reassembled_kf.expect("keyframe must reassemble");
    assert!(kf.is_keyframe);
    assert_eq!(kf.annex_b, kf_annex_b);

    // P-frame: Single NAL Unit.
    let pf_packets = packetizer.packetize_annex_b(&pf_annex_b, 93_000);
    assert_eq!(pf_packets.len(), 1);
    assert_eq!(nal_unit_type(pf_packets[0].payload[0]), NAL_TYPE_NON_IDR);
    let out = depacketizer
        .push_packet(&pf_packets[0].header, &pf_packets[0].payload)
        .unwrap();
    let pf = out.frame.expect("P-frame must reassemble");
    assert!(!pf.is_keyframe);
    assert_eq!(pf.annex_b, pf_annex_b);

    // Verify all NAL types are preserved inside the reassembled keyframe.
    let parsed_nalus = tg_voip::split_annex_b_nalus(&kf.annex_b);
    let types: Vec<u8> = parsed_nalus.iter().map(|n| nal_unit_type(n[0])).collect();
    assert_eq!(types, vec![NAL_TYPE_SPS, NAL_TYPE_PPS, NAL_TYPE_IDR]);
}

// ---------------------------------------------------------------------------
// 2. Windows Media Foundation H.264 Encode -> Decode of Synthetic Frames
// ---------------------------------------------------------------------------

#[test]
fn mf_h264_encode_decode_synthetic_frames_or_skip_gracefully() {
    let enc_cfg = VideoEncoderConfig {
        width: 320,
        height: 240,
        fps: 30,
        bitrate_bps: 500_000,
        gop_size: 30,
    };

    let mut encoder = match create_h264_encoder(enc_cfg) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("SKIPPING MF H.264 encode->decode test (no MF H.264 encoder): {err}");
            return;
        }
    };
    let mut decoder = match create_h264_decoder() {
        Ok(d) => d,
        Err(err) => {
            eprintln!("SKIPPING MF H.264 encode->decode test (no MF H.264 decoder): {err}");
            return;
        }
    };

    // Exercise dynamic bitrate adaptation via QualityController.
    let mut qc = QualityController::new(NetworkType::WiFi, tg_voip::DataSaving::Never);
    qc.observe_rtt(35.0);
    qc.on_quality_tick(0, 1_000);
    encoder.set_bitrate_bps(qc.video_bitrate_bps().min(600_000));

    let mut encoded_units = Vec::new();
    for idx in 0..12u64 {
        let frame = SyntheticVideoSource::generate_nv12_frame(320, 240, idx, 30);
        let bytes = encoder.encode(&frame, idx == 0 || idx == 6).unwrap();
        if !bytes.is_empty() {
            encoded_units.push(bytes);
        }
    }
    if let Ok(flushed) = encoder.flush() {
        encoded_units.extend(flushed);
    }

    assert!(
        !encoded_units.is_empty(),
        "MF H.264 encoder must emit Annex-B compressed units"
    );

    // Round-trip encoded Annex-B units through RFC 6184 RTP packetizer -> depacketizer -> MF decoder!
    let mut packetizer = H264RtpPacketizer::new(0x2233_4455);
    let mut depacketizer = H264RtpDepacketizer::new();
    let mut decoded_frames = Vec::new();

    for (idx, au) in encoded_units.iter().enumerate() {
        let ts = (idx as u32) * 3_000;
        let rtp_pkts = packetizer.packetize_annex_b(au, ts);
        for pkt in rtp_pkts {
            let outcome = depacketizer.push_packet(&pkt.header, &pkt.payload).unwrap();
            if let Some(assembled) = outcome.frame
                && let Some(decoded) = decoder.decode(&assembled.annex_b).unwrap()
            {
                decoded_frames.push(decoded);
            }
        }
    }
    if let Ok(flushed_frames) = decoder.flush() {
        decoded_frames.extend(flushed_frames);
    }

    assert!(
        !decoded_frames.is_empty(),
        "MF H.264 decoder must decode at least one synthetic frame"
    );
    let first = &decoded_frames[0];
    assert_eq!(first.width, 320);
    assert_eq!(first.height, 240);

    // Convert decoded frame to rotated RGBA for `tg-app`.
    let mut rotated = first.clone();
    rotated.rotation = 90;
    let rgba = rotated.to_rgba_rotated().unwrap();
    assert_eq!(rgba.width, 240);
    assert_eq!(rgba.height, 320);
    assert_eq!(rgba.rgba.len(), 240 * 320 * 4);
    // Decoded color bars must have non-constant pixel values.
    let min_r = rgba.rgba.iter().step_by(4).copied().min().unwrap_or(0);
    let max_r = rgba.rgba.iter().step_by(4).copied().max().unwrap_or(0);
    assert!(
        max_r.saturating_sub(min_r) > 30,
        "decoded color bar frame should preserve contrast (min_r={min_r}, max_r={max_r})"
    );
}

#[test]
fn windows_camera_and_display_enumeration_do_not_panic() {
    // Device listing must work safely even on headless CI machines with 0 cameras/monitors.
    let _ = list_cameras();
    let _ = list_displays();
}

// ---------------------------------------------------------------------------
// 3. 1:1 MediaEngine Video Loopback (RTP H.264, Rotation, PLI/FIR Keyframe)
// ---------------------------------------------------------------------------

struct MockH264Codec;

impl VideoEncoder for MockH264Codec {
    fn codec(&self) -> VideoCodecKind {
        VideoCodecKind::H264
    }

    fn encode(&mut self, frame: &VideoFrame, force_keyframe: bool) -> tg_voip::VoipResult<Vec<u8>> {
        let mut nal = Vec::with_capacity(1 + 8);
        nal.push(if force_keyframe { 0x65 } else { 0x41 });
        nal.extend_from_slice(&frame.width.to_be_bytes());
        nal.extend_from_slice(&frame.height.to_be_bytes());
        if force_keyframe {
            let sps = vec![0x67, 0x42, 0xE0, 0x1E];
            let pps = vec![0x68, 0xCE, 0x3C, 0x80];
            Ok(join_annex_b_nalus(&[sps, pps, nal]))
        } else {
            Ok(join_annex_b_nalus(&[nal]))
        }
    }

    fn set_bitrate_bps(&mut self, _bitrate_bps: u32) {}
}

impl VideoDecoder for MockH264Codec {
    fn codec(&self) -> VideoCodecKind {
        VideoCodecKind::H264
    }

    fn decode(&mut self, packet: &[u8]) -> tg_voip::VoipResult<Option<VideoFrame>> {
        let nalus = tg_voip::split_annex_b_nalus(packet);
        for nal in nalus {
            if nal.len() >= 9 && matches!(nal_unit_type(nal[0]), NAL_TYPE_IDR | NAL_TYPE_NON_IDR) {
                let w = u32::from_be_bytes([nal[1], nal[2], nal[3], nal[4]]);
                let h = u32::from_be_bytes([nal[5], nal[6], nal[7], nal[8]]);
                let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
                for px in rgba.as_chunks_mut::<4>().0 {
                    *px = [200, 100, 50, 255];
                }
                return Ok(Some(VideoFrame {
                    width: w,
                    height: h,
                    rotation: 0,
                    timestamp_us: 0,
                    format: VideoPixelFormat::Rgba,
                    data: rgba,
                }));
            }
        }
        Ok(None)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_to_one_video_loopback_rotation_and_pli_keyframe() {
    let reflector = LoopbackReflector::start(ReflectorImpairment::default())
        .await
        .unwrap();
    let key = [0x5Au8; AUTH_KEY_SIZE];
    let peer_tag = [0x22u8; 16];
    let peer_tag_hex = "22222222222222222222222222222222".to_string();
    let ip = reflector.addr().ip().to_string();
    let port = reflector.addr().port();

    let make_desc = |is_outgoing| ControllerDescriptor {
        version: "12.0.0".into(),
        initialization_timeout_ms: 15_000,
        receive_timeout_ms: 10_000,
        enable_p2p: false,
        max_api_layer: 92,
        encryption_key: key,
        is_outgoing,
        endpoints: vec![Endpoint {
            endpoint_id: 101,
            host: EndpointHost {
                ipv4: ip.clone(),
                ipv6: String::new(),
            },
            port,
            kind: EndpointType::UdpRelay,
            peer_tag,
        }],
        rtc_servers: vec![RtcServer {
            id: 1,
            host: ip.clone(),
            port,
            login: "reflector".into(),
            password: peer_tag_hex.clone(),
            is_turn: true,
            is_tcp: false,
        }],
        custom_parameters: None,
    };

    let cfg = MediaEngineConfig {
        audio_backend: AudioBackendKind::Virtual,
        dsp: VoiceDspConfig::bypass(),
        wire_mode: TransportWireMode::MtProtoRaw,
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        quality_tick_ms: 250,
        ..MediaEngineConfig::default()
    };

    let mut alice = MediaEngine::with_current_runtime(cfg.clone()).unwrap();
    let mut bob = MediaEngine::with_current_runtime(cfg).unwrap();
    alice.start(make_desc(true)).unwrap();
    bob.start(make_desc(false)).unwrap();

    bob.set_video_decoder(Box::new(MockH264Codec));
    alice.set_video_rotation(90);
    alice.start_video(
        Box::new(SyntheticVideoSource::new(64, 48, 15)),
        Box::new(MockH264Codec),
        false,
    );

    let mut bob_frames = Vec::new();
    let mut bob_rotation = 0u16;
    let mut alice_saw_pli = false;

    for step in 0..35 {
        tokio::time::sleep(Duration::from_millis(30)).await;
        if step == 15 {
            bob.request_remote_keyframe();
        }
        while let Some(ev) = alice.poll_event() {
            match ev {
                MediaEngineEvent::SignalingData(b) => bob.receive_signaling_data(b),
                MediaEngineEvent::KeyframeRequested => alice_saw_pli = true,
                _ => {}
            }
        }
        while let Some(ev) = bob.poll_event() {
            match ev {
                MediaEngineEvent::SignalingData(b) => alice.receive_signaling_data(b),
                MediaEngineEvent::RemoteVideoRotationChanged(r) => bob_rotation = r,
                MediaEngineEvent::RemoteVideoFrame(f) => bob_frames.push(f),
                _ => {}
            }
        }
    }

    alice.stop_async().await;
    bob.stop_async().await;
    reflector.stop().await;

    assert_eq!(bob_rotation, 90, "Bob must receive Alice's 90-deg rotation");
    assert!(
        alice_saw_pli,
        "Alice must receive Bob's PLI/FIR keyframe request"
    );
    assert!(
        !bob_frames.is_empty(),
        "Bob must decode RGBA video frames from Alice"
    );
    let rotated = bob_frames.iter().find(|f| f.source_rotation == 90);
    let frame = rotated.expect("at least one frame must have 90-deg rotation applied");
    assert_eq!(frame.width, 48);
    assert_eq!(frame.height, 64);
    assert_eq!(frame.rgba.len(), 48 * 64 * 4);
}

// ---------------------------------------------------------------------------
// 4. DTLS 1.2 + SRTP Loopback Between Two Local UDP Endpoints
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dtls_srtp_udp_loopback_between_two_local_endpoints() {
    let sock_a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sock_b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr_a = sock_a.local_addr().unwrap();
    let addr_b = sock_b.local_addr().unwrap();

    let id_a = DtlsIdentity::generate().unwrap();
    let id_b = DtlsIdentity::generate().unwrap();

    let now = Instant::now();
    let mut client =
        DtlsSrtpSession::new(&id_a, true, vec![id_b.to_group_fingerprint("passive")], now).unwrap();
    let mut server =
        DtlsSrtpSession::new(&id_b, false, vec![id_a.to_group_fingerprint("active")], now).unwrap();

    // Kick off client flight over real localhost UDP sockets.
    for pkt in client.poll_outputs().unwrap().outgoing_packets {
        sock_a.send_to(&pkt, addr_b).await.unwrap();
    }

    let mut buf_a = vec![0u8; 4096];
    let mut buf_b = vec![0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(client.is_ready() && server.is_ready()) && Instant::now() < deadline {
        tokio::select! {
            res = sock_b.recv_from(&mut buf_b) => {
                let (n, _) = res.unwrap();
                server.handle_packet(&buf_b[..n]).unwrap();
                for pkt in server.poll_outputs().unwrap().outgoing_packets {
                    sock_b.send_to(&pkt, addr_a).await.unwrap();
                }
            }
            res = sock_a.recv_from(&mut buf_a) => {
                let (n, _) = res.unwrap();
                client.handle_packet(&buf_a[..n]).unwrap();
                for pkt in client.poll_outputs().unwrap().outgoing_packets {
                    sock_a.send_to(&pkt, addr_b).await.unwrap();
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }

    assert!(
        client.is_ready() && server.is_ready(),
        "DTLS-SRTP UDP handshake must complete"
    );

    let mut buf = buf_a;

    // Exchange SRTP-encrypted RTP packets in both directions over UDP.
    for seq in 1..=5u16 {
        let hdr = RtpHeader {
            marker: seq == 1,
            payload_type: OPUS_RTP_PAYLOAD_TYPE,
            sequence_number: seq,
            timestamp: u32::from(seq) * 960,
            ssrc: 0x1020_3040,
        };
        let mut plain = hdr.to_bytes().to_vec();
        plain.extend_from_slice(&[0xF8, seq as u8, 0xAA, 0xBB]);

        let srtp = client.protect_rtp(&plain).unwrap();
        sock_a.send_to(&srtp, addr_b).await.unwrap();
        let (n, _) = sock_b.recv_from(&mut buf).await.unwrap();
        let decrypted = server.unprotect_rtp(&buf[..n]).unwrap();
        assert_eq!(decrypted, plain);

        // Echo back encrypted by server.
        let srtp_reply = server.protect_rtp(&plain).unwrap();
        sock_b.send_to(&srtp_reply, addr_a).await.unwrap();
        let (n2, _) = sock_a.recv_from(&mut buf).await.unwrap();
        let decrypted2 = client.unprotect_rtp(&buf[..n2]).unwrap();
        assert_eq!(decrypted2, plain);
    }
}

// ---------------------------------------------------------------------------
// 5. Group Call SFU Loopback: ICE-lite + DTLS-SRTP + Multi-SSRC Mixing + Speaking Levels
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn group_media_engine_sfu_loopback_multi_ssrc_mixing_and_speaking_levels() {
    // 1. Bind a local mock Telegram SFU UDP server (ICE-lite responder + DTLS 1.2 server + SRTP sender).
    let sfu_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sfu_addr = sfu_sock.local_addr().unwrap();
    let sfu_id = DtlsIdentity::generate().unwrap();
    let sfu_ufrag = "tg_sfu".to_string();
    let sfu_pwd = "tg_sfu_password_12345678".to_string();

    let mut engine = GroupMediaEngine::with_current_runtime(GroupMediaConfig {
        audio_backend: AudioBackendKind::Virtual,
        virtual_ring_capacity: VOIP_SAMPLE_RATE as usize * 6,
        dsp: VoiceDspConfig::bypass(),
        jitter: JitterBufferConfig {
            min_delay_ms: 40,
            max_delay_ms: 200,
            frame_ms: 20,
        },
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        initial_muted: false,
        ..GroupMediaConfig::default()
    })
    .unwrap();

    // Verify `GroupJoinPayload` compatibility with `tg_calls::GroupCallSession`.
    let (local_ssrc, join_json) = engine.emit_join_payload_json(Vec::new()).unwrap();
    let parsed_join = GroupJoinPayload::from_json(&join_json).unwrap();
    assert_eq!(parsed_join.ssrc, local_ssrc);
    let mut session = GroupCallSession::new(
        tg_calls::types::PeerId::from_chat(tg_calls::types::ChatId(100)),
        tg_calls::types::PeerId::from_user(tg_calls::types::UserId(1)),
        tg_calls::types::CallId(999),
        12345,
        true,
        0,
        false,
    );
    session.on_join_payload_ready(local_ssrc, join_json);

    let sfu_join_resp = RtcJoinResponse {
        transport: JoinResponseTransport {
            ufrag: sfu_ufrag.clone(),
            pwd: sfu_pwd.clone(),
            fingerprints: vec![sfu_id.to_group_fingerprint("passive")],
            candidates: vec![IceCandidateParam {
                port: sfu_addr.port().to_string(),
                protocol: "udp".into(),
                network: "0".into(),
                generation: "0".into(),
                id: "1".into(),
                component: "1".into(),
                foundation: "1".into(),
                priority: "2130706431".into(),
                ip: "127.0.0.1".into(),
                kind: "host".into(),
            }],
            rtcp_mux: true,
        },
        audio: JoinResponseMediaSection {
            endpoint: String::new(),
            payload_types: vec![RtpPayloadTypeParam {
                id: u32::from(OPUS_RTP_PAYLOAD_TYPE),
                name: "opus".into(),
                clockrate: 48_000,
                channels: Some(2),
                parameters: serde_json::Map::new(),
            }],
            rtp_hdrexts: vec![RtpHeaderExtensionParam {
                id: 1,
                uri: "urn:ietf:params:rtp-hdrext:ssrc-audio-level".into(),
            }],
        },
        video: JoinResponseMediaSection::default(),
    };
    let sfu_json = serde_json::to_string(&sfu_join_resp).unwrap();

    // Pre-encode 3 synthetic participant streams (440 Hz, 880 Hz, and muted 1320 Hz).
    let ssrc_1 = 5001u32;
    let ssrc_2 = 5002u32;
    let ssrc_3 = 5003u32;
    let stream_1 = encode_opus_sine_stream(440.0, 0.35, 35);
    let stream_2 = encode_opus_sine_stream(880.0, 0.35, 35);
    let stream_3 = encode_opus_sine_stream(1320.0, 0.35, 35);

    // Spawn mock SFU task handling ICE STUN -> DTLS 1.2 -> SRTP multi-SSRC forwarding.
    let client_fps = parsed_join.fingerprints.clone();
    let sfu_pwd_bytes = sfu_pwd.into_bytes();
    let sfu_task = tokio::spawn(async move {
        let mut server = DtlsSrtpSession::new(&sfu_id, false, client_fps, Instant::now()).unwrap();
        let mut buf = vec![0u8; 4096];
        let mut client_addr: Option<SocketAddr> = None;
        let mut sent_idx = 0usize;
        let mut received_client_srtp = 0usize;
        let mut send_tick = tokio::time::interval(Duration::from_millis(20));

        let stop_at = Instant::now() + Duration::from_secs(3);
        while Instant::now() < stop_at {
            tokio::select! {
                res = sfu_sock.recv_from(&mut buf) => {
                    let Ok((n, src)) = res else { continue };
                    client_addr = Some(src);
                    let data = &buf[..n];
                    match classify_webrtc_packet(data) {
                        WebRtcPacketKind::Stun => {
                            if verify_stun_integrity_and_fingerprint(data, Some(&sfu_pwd_bytes)).is_ok() {
                                let mut tx_id = [0u8; 12];
                                tx_id.copy_from_slice(&data[8..20]);
                                let resp = build_ice_binding_response(tx_id, src, Some(&sfu_pwd_bytes));
                                let _ = sfu_sock.send_to(&resp, src).await;
                            }
                        }
                        WebRtcPacketKind::Dtls => {
                            if server.handle_packet(data).is_ok()
                                && let Ok(out) = server.poll_outputs()
                            {
                                for pkt in out.outgoing_packets {
                                    let _ = sfu_sock.send_to(&pkt, src).await;
                                }
                            }
                        }
                        WebRtcPacketKind::SrtpOrSrtcp => {
                            if server.is_ready() && server.unprotect_rtp(data).is_ok() {
                                received_client_srtp += 1;
                            }
                        }
                        WebRtcPacketKind::Unknown => {}
                    }
                }
                _ = send_tick.tick() => {
                    if server.is_ready()
                        && sent_idx < stream_1.len()
                        && let Some(dst) = client_addr
                    {
                        for (ssrc, pkts) in [(ssrc_1, &stream_1), (ssrc_2, &stream_2), (ssrc_3, &stream_3)] {
                            let hdr = RtpHeader {
                                marker: sent_idx == 0,
                                payload_type: OPUS_RTP_PAYLOAD_TYPE,
                                sequence_number: sent_idx as u16,
                                timestamp: (sent_idx * VOIP_FRAME_SAMPLES) as u32,
                                ssrc,
                            };
                            let mut raw = hdr.to_bytes().to_vec();
                            raw.extend_from_slice(&pkts[sent_idx]);
                            if let Ok(srtp) = server.protect_rtp(&raw) {
                                let _ = sfu_sock.send_to(&srtp, dst).await;
                            }
                        }
                        sent_idx += 1;
                    }
                }
            }
        }
        received_client_srtp
    });

    // Mute SSRC 3 locally via volume = 0.0 so only 440 Hz and 880 Hz appear in speaker output!
    engine.set_join_response_payload(&sfu_json).unwrap();
    engine.set_ssrc_volume(ssrc_3, 0.0);

    let v_audio = engine.virtual_audio().unwrap();
    // Feed local mic sine wave so client also sends SRTP Opus packets to the SFU.
    let local_mic: Vec<f32> = (0..VOIP_SAMPLE_RATE as usize)
        .map(|i| {
            (2.0 * std::f32::consts::PI * 300.0 * (i as f32 / VOIP_SAMPLE_RATE as f32)).sin() * 0.4
        })
        .collect();
    v_audio.push_capture_pcm(&local_mic);

    let mut connected = false;
    let mut speaking_ssrcs = std::collections::HashSet::new();
    let mut speaker_pcm = Vec::new();

    for step in 0..45 {
        tokio::time::sleep(Duration::from_millis(30)).await;
        if step == 35 {
            engine.set_muted(true);
        }
        while let Some(ev) = engine.poll_event() {
            match ev {
                GroupMediaEvent::NetworkStateChanged {
                    connected: true, ..
                } => connected = true,
                GroupMediaEvent::AudioLevelsUpdated(levels) => {
                    for l in levels {
                        if l.voice {
                            speaking_ssrcs.insert(l.ssrc);
                        }
                    }
                }
                _ => {}
            }
        }
        speaker_pcm.extend_from_slice(&v_audio.drain_playback_pcm());
    }

    engine.stop_async().await;
    let client_srtp_received = sfu_task.await.unwrap();

    assert!(
        connected,
        "GroupMediaEngine must complete ICE + DTLS-SRTP connection"
    );
    assert!(
        client_srtp_received >= 5,
        "SFU must receive SRTP-encrypted local audio (got {client_srtp_received})"
    );
    assert!(
        speaking_ssrcs.contains(&ssrc_1) && speaking_ssrcs.contains(&ssrc_2),
        "speaking levels must detect active remote SSRCs (saw {speaking_ssrcs:?})"
    );

    // Mixed playback must contain 440 Hz (SSRC 1) and 880 Hz (SSRC 2), while locally muted
    // SSRC 3 (1320 Hz) must be suppressed!
    let start = VOIP_FRAME_SAMPLES * 8;
    let end = speaker_pcm.len().min(start + VOIP_FRAME_SAMPLES * 25);
    assert!(
        end > start + VOIP_FRAME_SAMPLES * 8,
        "insufficient mixed PCM: {}",
        speaker_pcm.len()
    );
    let win = &speaker_pcm[start..end];
    let m440 = goertzel_magnitude(win, 440.0);
    let m880 = goertzel_magnitude(win, 880.0);
    let m1320 = goertzel_magnitude(win, 1320.0);
    assert!(
        m440 > 0.05 && m880 > 0.05 && m1320 < m440 * 0.25,
        "expected 440+880 Hz mixed and 1320 Hz muted (m440={m440:.4}, m880={m880:.4}, m1320={m1320:.4})"
    );
}

// ---------------------------------------------------------------------------
// 6. Group Call Broadcast-Stream (Chunked RTMP-like) Playback Path
// ---------------------------------------------------------------------------

struct MockBroadcastLoader {
    ogg_chunk: Vec<u8>,
}

impl BroadcastChunkLoader for MockBroadcastLoader {
    fn fetch_chunk(
        &self,
        req: BroadcastChunkRequest,
    ) -> Pin<Box<dyn Future<Output = BroadcastChunkResponse> + Send + '_>> {
        let data = self.ogg_chunk.clone();
        Box::pin(async move {
            BroadcastChunkResponse {
                timestamp_ms: req.time_ms,
                response_timestamp_sec: (req.time_ms as f64) / 1000.0,
                status: BroadcastChunkStatus::Success,
                data,
            }
        })
    }

    fn request_current_time_ms(&self) -> Pin<Box<dyn Future<Output = Option<i64>> + Send + '_>> {
        Box::pin(async { Some(20_000) })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn group_media_engine_broadcast_stream_playback() {
    let opus_pkts = encode_opus_sine_stream(520.0, 0.4, 25);
    let ogg = encode_ogg_opus_chunk(&opus_pkts);
    let loader = Arc::new(MockBroadcastLoader { ogg_chunk: ogg });

    let mut engine = GroupMediaEngine::with_current_runtime(GroupMediaConfig {
        audio_backend: AudioBackendKind::Virtual,
        virtual_ring_capacity: VOIP_SAMPLE_RATE as usize * 4,
        dsp: VoiceDspConfig::bypass(),
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        broadcast_scale: BroadcastChunkScale::Ms500,
        ..GroupMediaConfig::default()
    })
    .unwrap();

    engine.set_broadcast_loader(loader);
    engine
        .set_join_response_payload(r#"{"stream": true, "rtmp": false}"#)
        .unwrap();

    let v_audio = engine.virtual_audio().unwrap();
    let mut saw_stream_mode = false;
    let mut saw_connected = false;
    let mut pcm = Vec::new();

    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(30)).await;
        while let Some(ev) = engine.poll_event() {
            match ev {
                GroupMediaEvent::ModeChanged(InstanceMode::Stream) => saw_stream_mode = true,
                GroupMediaEvent::NetworkStateChanged {
                    connected: true, ..
                } => saw_connected = true,
                _ => {}
            }
        }
        pcm.extend_from_slice(&v_audio.drain_playback_pcm());
    }

    engine.stop_async().await;

    assert!(saw_stream_mode, "must enter InstanceMode::Stream");
    assert!(saw_connected, "must connect once broadcast chunks decode");
    assert!(pcm.len() >= VOIP_FRAME_SAMPLES * 8);
    let m520 = goertzel_magnitude(&pcm, 520.0);
    let m1500 = goertzel_magnitude(&pcm, 1500.0);
    assert!(
        m520 > 0.08 && m520 > m1500 * 5.0,
        "broadcast stream should decode 520 Hz tone (m520={m520:.4}, m1500={m1500:.4})"
    );
}

#[test]
fn group_mixer_standalone_four_synthetic_ssrcs() {
    let mut mixer = GroupAudioMixer::new(JitterBufferConfig::default());
    let freqs = [330.0f32, 440.0, 550.0, 660.0];
    let streams: Vec<Vec<Vec<u8>>> = freqs
        .iter()
        .map(|&f| encode_opus_sine_stream(f, 0.25, 20))
        .collect();

    let mut out = Vec::new();
    for i in 0..20usize {
        let ts = (i * VOIP_FRAME_SAMPLES) as u32;
        let t_ms = (i * 20) as u64;
        for (idx, pkts) in streams.iter().enumerate() {
            mixer
                .push_opus_packet(100 + idx as u32, i as u16, ts, t_ms, pkts[i].clone(), None)
                .unwrap();
        }
        let mut frame = [0.0f32; VOIP_FRAME_SAMPLES];
        let _ = mixer.mix_frame(t_ms, &mut frame);
        out.extend_from_slice(&frame);
    }

    assert_eq!(mixer.active_stream_count(), 4);
    let steady = &out[VOIP_FRAME_SAMPLES * 4..];
    assert!(steady.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
    for &f in &freqs {
        let mag = goertzel_magnitude(steady, f);
        assert!(
            mag > 0.03,
            "expected tone {f} Hz in 4-SSRC mix, got {mag:.4}"
        );
    }
}
