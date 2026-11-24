//! Integration tests for conference E2E frame encryption hooked into `tg-voip::GroupMediaEngine`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use tg_calls::types::UserId;
use tg_calls::{
    E2eCall, E2eKeyPair, IceCandidateParam, JoinResponseMediaSection, JoinResponseTransport,
    RtcJoinResponse, RtpHeaderExtensionParam, RtpPayloadTypeParam,
};
use tg_voip::{
    AudioBackendKind, DEFAULT_VOICE_BITRATE_BPS, DtlsIdentity, DtlsSrtpSession, GroupMediaConfig,
    GroupMediaEngine, GroupMediaEvent, JitterBufferConfig, OPUS_RTP_PAYLOAD_TYPE, RtpHeader,
    VOIP_FRAME_SAMPLES, VOIP_SAMPLE_RATE, VoiceDspConfig, VoiceOpusEncoder, WebRtcPacketKind,
    build_ice_binding_response, classify_webrtc_packet, transform_incoming_e2e_frame,
    transform_outgoing_e2e_frame, verify_stun_integrity_and_fingerprint,
};
use tokio::net::UdpSocket;

fn setup_two_party_e2e() -> (E2eCall, E2eCall) {
    let mut alice = E2eCall::with_key_pair(UserId(1001), E2eKeyPair::from_seed([0xA1; 32]));
    let mut bob = E2eCall::with_key_pair(UserId(2002), E2eKeyPair::from_seed([0xB2; 32]));

    let b0 = alice.make_join_block().expect("zero block");
    alice
        .apply_blocks(0, 1, std::slice::from_ref(&b0), true)
        .expect("alice apply b0");
    bob.refresh_last_block0(Some(b0.clone()));
    let b1 = bob.make_join_block().expect("self-add block");
    alice
        .apply_blocks(0, 2, std::slice::from_ref(&b1), false)
        .expect("alice apply b1");
    bob.apply_blocks(0, 2, &[b0, b1], true)
        .expect("bob apply b0..b1");
    (alice, bob)
}

#[tokio::test]
async fn group_engine_video_frame_e2e_flag_off_vs_on() {
    let (alice, bob) = setup_two_party_e2e();
    let rt = tokio::runtime::Handle::current();

    // Flag OFF -> pass-through even without handler.
    let mut cfg = GroupMediaConfig {
        audio_backend: AudioBackendKind::Virtual,
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        e2e_frame_encryption: false,
        ..GroupMediaConfig::default()
    };
    let engine_off = GroupMediaEngine::new(cfg.clone(), rt.clone()).unwrap();
    let sample_nal = vec![0x65, 0x10, 0x20, 0x30, 0x40, 0x50];
    assert_eq!(
        engine_off.encrypt_video_frame(&sample_nal).unwrap(),
        sample_nal
    );
    assert_eq!(
        engine_off.decrypt_video_frame(555, &sample_nal).unwrap(),
        sample_nal
    );

    // Flag ON -> encrypts video payload while preserving leading 1-byte NAL header.
    cfg.e2e_frame_encryption = true;
    let mut alice_engine = GroupMediaEngine::new(cfg.clone(), rt.clone()).unwrap();
    let mut bob_engine = GroupMediaEngine::new(cfg, rt).unwrap();

    // Without handler installed, flag ON fails closed (never leaks plaintext).
    assert!(alice_engine.encrypt_video_frame(&sample_nal).is_err());

    alice_engine.set_e2e_encrypt_decrypt(Some(alice.encrypt_decrypt()));
    bob_engine.set_e2e_encrypt_decrypt(Some(bob.encrypt_decrypt()));
    bob_engine.register_ssrc_user(777, UserId(1001));

    let encrypted = alice_engine.encrypt_video_frame(&sample_nal).unwrap();
    assert_eq!(encrypted[0], 0x65, "NAL header byte stays unencrypted");
    assert_ne!(encrypted, sample_nal);

    let decrypted = bob_engine.decrypt_video_frame(777, &encrypted).unwrap();
    assert_eq!(decrypted, sample_nal);

    // Unknown SSRC fails decryption.
    assert!(bob_engine.decrypt_video_frame(999, &encrypted).is_err());

    // Direct helper check for audio frames (prefix = 0).
    let opus = vec![0xF8, 0xAA, 0xBB, 0xCC];
    let enc_opus =
        transform_outgoing_e2e_frame(true, Some(&alice.encrypt_decrypt()), 0, &opus, 0).unwrap();
    assert_ne!(enc_opus, opus);
    let dec_opus = transform_incoming_e2e_frame(
        true,
        Some(&bob.encrypt_decrypt()),
        Some(UserId(1001)),
        0,
        &enc_opus,
    )
    .unwrap();
    assert_eq!(dec_opus, opus);
}

#[tokio::test]
async fn group_engine_rtc_e2e_encrypted_opus_udp_roundtrip() {
    let (alice_e2e, bob_e2e) = setup_two_party_e2e();
    let bob_ed = bob_e2e.encrypt_decrypt();

    // Mock SFU socket + DTLS server identity.
    let sfu_sock = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .await
        .unwrap();
    let sfu_addr = sfu_sock.local_addr().unwrap();
    let sfu_identity = DtlsIdentity::generate().unwrap();
    let sfu_ufrag = "sfuUfrag1";
    let sfu_pwd = "sfuPasswordSecret123456";
    let sfu_pwd_bytes = sfu_pwd.as_bytes().to_vec();

    let cfg = GroupMediaConfig {
        audio_backend: AudioBackendKind::Virtual,
        dsp: VoiceDspConfig {
            enable_aec: false,
            enable_ns: false,
            enable_agc: false,
            enable_hpf: false,
            initial_delay_ms: 0,
        },
        jitter: JitterBufferConfig::default(),
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        initial_muted: false,
        e2e_frame_encryption: true,
        e2e_channel_id: 0,
        e2e_audio_unencrypted_prefix: 0,
        ..GroupMediaConfig::default()
    };

    let mut engine = GroupMediaEngine::with_current_runtime(cfg).unwrap();
    engine.set_e2e_encrypt_decrypt(Some(alice_e2e.encrypt_decrypt()));
    let remote_bob_ssrc: u32 = 0xBEEF_2002;
    engine.register_ssrc_user(remote_bob_ssrc, UserId(2002));

    let payload = engine.emit_join_payload(vec![]);
    let local_alice_ssrc = payload.ssrc;

    let join_resp = RtcJoinResponse {
        transport: JoinResponseTransport {
            ufrag: sfu_ufrag.into(),
            pwd: sfu_pwd.into(),
            fingerprints: vec![sfu_identity.to_group_fingerprint("passive")],
            candidates: vec![IceCandidateParam {
                ip: sfu_addr.ip().to_string(),
                port: sfu_addr.port().to_string(),
                protocol: "udp".into(),
                kind: "host".into(),
                network: "0".into(),
                generation: "0".into(),
                id: "1".into(),
                component: "1".into(),
                foundation: "1".into(),
                priority: "2130706431".into(),
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
    let resp_json = serde_json::to_string(&join_resp).unwrap();
    engine.set_join_response_payload(&resp_json).unwrap();

    let v_audio = engine.virtual_audio().expect("virtual audio handle");

    // Feed local capture PCM so Alice transmits E2E-encrypted Opus frames once connected.
    let local_mic: Vec<f32> = (0..VOIP_SAMPLE_RATE as usize)
        .map(|i| {
            let t = i as f32 / VOIP_SAMPLE_RATE as f32;
            (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.4
        })
        .collect();
    v_audio.push_capture_pcm(&local_mic);

    let mut tone_frame = [0.0f32; VOIP_FRAME_SAMPLES];
    tone_frame.copy_from_slice(&local_mic[..VOIP_FRAME_SAMPLES]);

    let mut sfu_dtls = DtlsSrtpSession::new(
        &sfu_identity,
        false,
        payload.fingerprints.clone(),
        Instant::now(),
    )
    .unwrap();
    let mut bob_opus_enc = VoiceOpusEncoder::new(DEFAULT_VOICE_BITRATE_BPS).unwrap();

    let mut client_addr: Option<SocketAddr> = None;
    let mut received_valid_e2e_from_alice = false;
    let mut sent_bob_frames = 0u16;
    let mut buf = vec![0u8; 4096];

    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline && (!received_valid_e2e_from_alice || sent_bob_frames < 12) {
        if let Ok(Ok((n, src))) =
            tokio::time::timeout(Duration::from_millis(20), sfu_sock.recv_from(&mut buf)).await
        {
            client_addr = Some(src);
            let pkt = &buf[..n];
            match classify_webrtc_packet(pkt) {
                WebRtcPacketKind::Stun => {
                    if verify_stun_integrity_and_fingerprint(pkt, Some(&sfu_pwd_bytes)).is_ok() {
                        let mut tx_id = [0u8; 12];
                        tx_id.copy_from_slice(&pkt[8..20]);
                        let resp = build_ice_binding_response(tx_id, src, Some(&sfu_pwd_bytes));
                        let _ = sfu_sock.send_to(&resp, src).await;
                    }
                }
                WebRtcPacketKind::Dtls => {
                    let _ = sfu_dtls.handle_packet(pkt);
                    if let Ok(out) = sfu_dtls.poll_outputs() {
                        for d in out.outgoing_packets {
                            let _ = sfu_sock.send_to(&d, src).await;
                        }
                    }
                }
                WebRtcPacketKind::SrtpOrSrtcp => {
                    if sfu_dtls.is_ready()
                        && let Ok(rtp_plain) = sfu_dtls.unprotect_rtp(pkt)
                        && let Ok((hdr, off)) = RtpHeader::parse(&rtp_plain)
                        && hdr.ssrc == local_alice_ssrc
                    {
                        let wire_payload = &rtp_plain[off..];
                        // Decrypt with Bob's E2E key: proves Alice encrypted the RTP payload!
                        let dec_opus = bob_ed
                            .decrypt_frame(UserId(1001), 0, wire_payload)
                            .expect("Bob must decrypt Alice's E2E RTP audio frame");
                        assert!(!dec_opus.is_empty());
                        received_valid_e2e_from_alice = true;
                    }
                }
                WebRtcPacketKind::Unknown => {}
            }
        }

        // Send E2E-encrypted Opus frames from Bob -> Alice over SRTP!
        if sfu_dtls.is_ready()
            && let Some(addr) = client_addr
            && sent_bob_frames < 15
        {
            let opus = bob_opus_enc.encode_frame(&tone_frame).unwrap();
            let enc = bob_ed.encrypt_frame(0, &opus, 0).unwrap();
            let hdr = RtpHeader {
                marker: sent_bob_frames == 0,
                payload_type: OPUS_RTP_PAYLOAD_TYPE,
                sequence_number: sent_bob_frames,
                timestamp: u32::from(sent_bob_frames) * (VOIP_FRAME_SAMPLES as u32),
                ssrc: remote_bob_ssrc,
            };
            let mut rtp = hdr.to_bytes().to_vec();
            rtp.extend_from_slice(&enc);
            if let Ok(srtp) = sfu_dtls.protect_rtp(&rtp) {
                let _ = sfu_sock.send_to(&srtp, addr).await;
                sent_bob_frames += 1;
            }
        }
    }

    assert!(
        received_valid_e2e_from_alice,
        "SFU peer must receive and E2E-decrypt Opus frame from GroupMediaEngine"
    );

    // Wait briefly for mixer playout of Bob's decrypted frames and verify non-silent audio arrived.
    tokio::time::sleep(Duration::from_millis(160)).await;
    let speaker_pcm = v_audio.drain_playback_pcm();
    let max_amp = speaker_pcm
        .iter()
        .copied()
        .map(f32::abs)
        .fold(0.0f32, f32::max);
    assert!(
        max_amp > 0.01,
        "Alice must play out Bob's E2E-decrypted Opus audio (max_amp={max_amp})"
    );

    let mut saw_connected = false;
    while let Some(ev) = engine.poll_event() {
        if matches!(
            ev,
            GroupMediaEvent::NetworkStateChanged {
                connected: true,
                ..
            }
        ) {
            saw_connected = true;
        }
    }
    assert!(saw_connected);

    engine.stop_async().await;
}
