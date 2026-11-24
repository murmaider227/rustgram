//! Soak and robustness regression tests for `tg-voip`:
//! 1. 50 start/stop cycles of a 1:1 `MediaEngine` (with real AEC3 DSP worker thread) verifying
//!    zero hangs and zero leaked OS threads or Tokio tasks.
//! 2. Synchronous `stop()` invoked from inside a `current_thread` Tokio runtime, inside a
//!    `multi_thread` Tokio runtime, from a plain OS thread (non-blocking), and twice in a row,
//!    for both `MediaEngine` and `GroupMediaEngine`.
//! 3. Two `GroupMediaEngine` instances exchanging real Opus audio frames over localhost UDP
//!    with DTLS 1.2 + SRTP (via an in-process localhost ICE-lite + DTLS-SRTP SFU bridge),
//!    verifying frames arrive, decrypt, and recover the transmitted tones.
//! 4. Switching audio input/output devices mid-call with `RUSTGRAM_HEADLESS_AUDIO=1`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use tg_audio::device::{DEFAULT_DEVICE_ID, DeviceType};
use tg_calls::{
    ControllerDescriptor, ControllerState, Endpoint, EndpointHost, EndpointType, IceCandidateParam,
    JoinResponseMediaSection, JoinResponseTransport, RtcJoinResponse, RtcServer,
    RtpHeaderExtensionParam, RtpPayloadTypeParam,
};
use tg_voip::{
    AUTH_KEY_SIZE, AudioBackendKind, CallMediaEngine, DtlsIdentity, DtlsSrtpSession,
    GroupMediaConfig, GroupMediaEngine, GroupMediaEvent, JitterBufferConfig, LoopbackReflector,
    MediaEngine, MediaEngineConfig, MediaEngineEvent, NetworkType, OPUS_RTP_PAYLOAD_TYPE,
    ReflectorImpairment, TransportWireMode, VOIP_FRAME_SAMPLES, VOIP_SAMPLE_RATE, VoiceDspConfig,
    WebRtcPacketKind, active_dsp_thread_count, active_group_media_task_count,
    active_media_task_count, build_ice_binding_response, classify_webrtc_packet, is_headless_audio,
    verify_stun_integrity_and_fingerprint,
};
use tokio::net::UdpSocket;

/// Serializes tests in this integration binary so process-wide active task/thread gauges
/// observe strictly isolated zero-leak assertions.
static SERIAL: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn make_descriptor(
    key: [u8; AUTH_KEY_SIZE],
    is_outgoing: bool,
    reflector_addr: Option<SocketAddr>,
) -> ControllerDescriptor {
    let peer_tag = [0x42u8; 16];
    let peer_tag_hex = "42424242424242424242424242424242".to_string();
    let (endpoints, rtc_servers) = match reflector_addr {
        Some(addr) => {
            let ip = addr.ip().to_string();
            let port = addr.port();
            (
                vec![Endpoint {
                    endpoint_id: 501,
                    host: EndpointHost {
                        ipv4: ip.clone(),
                        ipv6: String::new(),
                    },
                    port,
                    kind: EndpointType::UdpRelay,
                    peer_tag,
                }],
                vec![RtcServer {
                    id: 1,
                    host: ip,
                    port,
                    login: "reflector".into(),
                    password: peer_tag_hex,
                    is_turn: true,
                    is_tcp: false,
                }],
            )
        }
        None => (Vec::new(), Vec::new()),
    };

    ControllerDescriptor {
        version: "12.0.0".into(),
        initialization_timeout_ms: 15_000,
        receive_timeout_ms: 10_000,
        enable_p2p: reflector_addr.is_none(),
        max_api_layer: 92,
        encryption_key: key,
        is_outgoing,
        endpoints,
        rtc_servers,
        custom_parameters: None,
    }
}

fn make_virtual_config(dsp: VoiceDspConfig) -> MediaEngineConfig {
    MediaEngineConfig {
        audio_backend: AudioBackendKind::Virtual,
        virtual_ring_capacity: VOIP_SAMPLE_RATE as usize * 4,
        dsp,
        jitter: JitterBufferConfig {
            min_delay_ms: 40,
            max_delay_ms: 200,
            frame_ms: 20,
        },
        wire_mode: TransportWireMode::MtProtoRaw,
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        initial_network_type: NetworkType::WiFi,
        quality_tick_ms: 200,
        ..MediaEngineConfig::default()
    }
}

fn generate_sine(freq_hz: f32, duration_ms: usize, amplitude: f32) -> Vec<f32> {
    let total = (VOIP_SAMPLE_RATE as usize * duration_ms) / 1000;
    (0..total)
        .map(|i| {
            let t = i as f32 / VOIP_SAMPLE_RATE as f32;
            (2.0 * std::f32::consts::PI * freq_hz * t).sin() * amplitude
        })
        .collect()
}

fn goertzel_magnitude(samples: &[f32], target_hz: f32) -> f64 {
    fn single_window(chunk: &[f32], target_hz: f32) -> f64 {
        if chunk.is_empty() {
            return 0.0;
        }
        let n = chunk.len() as f64;
        let omega = 2.0 * std::f64::consts::PI * f64::from(target_hz) / f64::from(VOIP_SAMPLE_RATE);
        let coeff = 2.0 * omega.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &x in chunk {
            let s = f64::from(x) + coeff * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        (s2 * s2 + s1 * s1 - coeff * s1 * s2).max(0.0).sqrt() / n
    }

    let chunk_len = (VOIP_SAMPLE_RATE as usize) / 10; // 100 ms -> exact 10 Hz bin grid
    if samples.len() < chunk_len {
        return single_window(samples, target_hz);
    }
    let mut sum = 0.0;
    let mut count = 0usize;
    for chunk in samples.chunks_exact(chunk_len) {
        sum += single_window(chunk, target_hz);
        count += 1;
    }
    sum / count.max(1) as f64
}

async fn wait_for_zero_leaks(timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if active_media_task_count() == 0
            && active_group_media_task_count() == 0
            && active_dsp_thread_count() == 0
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn soak_50_start_stop_cycles_no_hang_no_leaks() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build runtime");

    let start_all = Instant::now();
    rt.block_on(async {
        let key = [0x3Cu8; AUTH_KEY_SIZE];
        let mut engine =
            MediaEngine::with_current_runtime(make_virtual_config(VoiceDspConfig::default()))
                .expect("create engine");

        for cycle in 0..50usize {
            let desc = make_descriptor(key, cycle.is_multiple_of(2), None);
            engine.start(desc).expect("start cycle");

            // Give the media loop a brief moment to bind and emit initial signaling.
            tokio::task::yield_now().await;
            if cycle % 10 == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            if cycle % 2 == 0 {
                let final_state = engine.stop_async().await;
                assert!(
                    !final_state.debug_log.is_empty() || final_state.stats.version == "12.0.0",
                    "cycle {cycle}: expected initialized stats/debug_log on stop_async"
                );
            } else {
                // Synchronous stop inside Tokio multi-thread runtime must never panic or block.
                let t0 = Instant::now();
                let _ = engine.stop();
                assert!(
                    t0.elapsed() < Duration::from_millis(200),
                    "cycle {cycle}: sync stop blocked for {:?}",
                    t0.elapsed()
                );
            }
        }

        // Wait briefly for all background sync-stop teardown tasks and DSP threads to finish.
        wait_for_zero_leaks(Duration::from_secs(3)).await;
        assert_eq!(
            active_media_task_count(),
            0,
            "leaked MediaEngine background tasks after 50 cycles"
        );
        assert_eq!(
            active_dsp_thread_count(),
            0,
            "leaked tg-voip-dsp worker threads after 50 cycles"
        );
    });

    assert!(
        start_all.elapsed() < Duration::from_secs(20),
        "50 start/stop cycles took too long: {:?}",
        start_all.elapsed()
    );
}

#[test]
fn stop_inside_tokio_runtime_plain_thread_and_double_stop() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let key = [0x77u8; AUTH_KEY_SIZE];

    // 1. Inside a `current_thread` Tokio runtime (calling `block_on` here panics in Tokio!).
    let ct_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current_thread runtime");
    ct_rt.block_on(async {
        let mut engine =
            MediaEngine::with_current_runtime(make_virtual_config(VoiceDspConfig::default()))
                .expect("create engine in current_thread rt");
        engine
            .start(make_descriptor(key, true, None))
            .expect("start engine");
        tokio::time::sleep(Duration::from_millis(25)).await;

        let t0 = Instant::now();
        let first = engine.stop();
        assert!(
            t0.elapsed() < Duration::from_millis(200),
            "sync stop in current_thread runtime blocked for {:?}",
            t0.elapsed()
        );
        // Double stop must be safe, idempotent, and preserve the cached final state.
        let second = engine.stop();
        assert_eq!(first.debug_log, second.debug_log);
        assert_eq!(first.preferred_relay_id, second.preferred_relay_id);
        assert_eq!(first.is_rating_suggested, second.is_rating_suggested);

        // GroupMediaEngine sync stop + double stop inside current_thread runtime.
        let mut group = GroupMediaEngine::with_current_runtime(GroupMediaConfig {
            audio_backend: AudioBackendKind::Virtual,
            dsp: VoiceDspConfig::default(),
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            ..GroupMediaConfig::default()
        })
        .expect("create group engine");
        group.start().expect("start group engine");
        tokio::time::sleep(Duration::from_millis(25)).await;
        group.stop();
        group.stop();

        wait_for_zero_leaks(Duration::from_secs(3)).await;
        assert_eq!(active_media_task_count(), 0);
        assert_eq!(active_group_media_task_count(), 0);
        assert_eq!(active_dsp_thread_count(), 0);
    });

    // 2. From a plain OS thread (simulating egui UI thread outside any Tokio runtime).
    let mt_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build multi_thread runtime");
    let handle = mt_rt.handle().clone();

    let plain_thread = std::thread::spawn(move || {
        let mut engine = MediaEngine::new(
            make_virtual_config(VoiceDspConfig::default()),
            handle.clone(),
        );
        engine
            .start(make_descriptor(key, false, None))
            .expect("start from plain thread");
        std::thread::sleep(Duration::from_millis(40));

        let t0 = Instant::now();
        let s1 = engine.stop();
        assert!(
            t0.elapsed() < Duration::from_millis(200),
            "sync stop from plain/UI thread must not block (took {:?})",
            t0.elapsed()
        );
        let s2 = engine.stop();
        assert_eq!(s1.debug_log, s2.debug_log);
        assert_eq!(s1.preferred_relay_id, s2.preferred_relay_id);

        let mut group = GroupMediaEngine::new(
            GroupMediaConfig {
                audio_backend: AudioBackendKind::Virtual,
                dsp: VoiceDspConfig::default(),
                bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
                ..GroupMediaConfig::default()
            },
            handle,
        )
        .expect("create group engine from plain thread");
        group.start().expect("start group engine");
        std::thread::sleep(Duration::from_millis(30));
        let tg = Instant::now();
        group.stop();
        group.stop();
        assert!(
            tg.elapsed() < Duration::from_millis(200),
            "group sync stop from plain/UI thread must not block (took {:?})",
            tg.elapsed()
        );
    });
    plain_thread.join().expect("plain thread finished cleanly");

    mt_rt.block_on(async {
        wait_for_zero_leaks(Duration::from_secs(3)).await;
        assert_eq!(active_media_task_count(), 0);
        assert_eq!(active_group_media_task_count(), 0);
        assert_eq!(active_dsp_thread_count(), 0);
    });
}

#[test]
fn two_engines_exchange_audio_frames_over_localhost_dtls_srtp() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async {
        // Two independent localhost SFU UDP legs (one per GroupMediaEngine) that complete
        // ICE-lite + DTLS 1.2 + SRTP independently with Alice and Bob and forward decrypted
        // RTP Opus frames across the bridge, re-encrypted under each peer's SRTP keys.
        let sfu_a_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sfu_b_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sfu_a_addr = sfu_a_sock.local_addr().unwrap();
        let sfu_b_addr = sfu_b_sock.local_addr().unwrap();

        let sfu_a_id = DtlsIdentity::generate().unwrap();
        let sfu_b_id = DtlsIdentity::generate().unwrap();
        let sfu_pwd = "tg_local_sfu_pwd_12345678".to_string();

        let make_cfg = || GroupMediaConfig {
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
        };

        let mut alice = GroupMediaEngine::with_current_runtime(make_cfg()).unwrap();
        let mut bob = GroupMediaEngine::with_current_runtime(make_cfg()).unwrap();

        let alice_join = alice.emit_join_payload(Vec::new());
        let bob_join = bob.emit_join_payload(Vec::new());

        let make_join_resp = |addr: SocketAddr, id: &DtlsIdentity, pwd: &str| {
            serde_json::to_string(&RtcJoinResponse {
                transport: JoinResponseTransport {
                    ufrag: "tg_sfu".into(),
                    pwd: pwd.into(),
                    fingerprints: vec![id.to_group_fingerprint("passive")],
                    candidates: vec![IceCandidateParam {
                        port: addr.port().to_string(),
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
            })
            .unwrap()
        };

        let alice_resp_json = make_join_resp(sfu_a_addr, &sfu_a_id, &sfu_pwd);
        let bob_resp_json = make_join_resp(sfu_b_addr, &sfu_b_id, &sfu_pwd);

        let alice_fps = alice_join.fingerprints.clone();
        let bob_fps = bob_join.fingerprints.clone();
        let pwd_bytes = sfu_pwd.into_bytes();

        let mut srv_a =
            DtlsSrtpSession::new(&sfu_a_id, false, alice_fps, Instant::now()).unwrap();
        let mut srv_b =
            DtlsSrtpSession::new(&sfu_b_id, false, bob_fps, Instant::now()).unwrap();
        let (sfu_stop_tx, mut sfu_stop_rx) = tokio::sync::oneshot::channel::<()>();

        let sfu_task = tokio::spawn(async move {
            let mut buf_a = vec![0u8; 4096];
            let mut buf_b = vec![0u8; 4096];
            let mut addr_a: Option<SocketAddr> = None;
            let mut addr_b: Option<SocketAddr> = None;
            let mut timeout_a: Option<Instant> = None;
            let mut timeout_b: Option<Instant> = None;
            let mut forwarded_a_to_b = 0usize;
            let mut forwarded_b_to_a = 0usize;

            let mut dtls_tick = tokio::time::interval(Duration::from_millis(50));
            dtls_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    biased;
                    _ = &mut sfu_stop_rx => {
                        break;
                    }
                    res = sfu_a_sock.recv_from(&mut buf_a) => {
                        let Ok((n, src)) = res else { continue };
                        addr_a = Some(src);
                        let data = &buf_a[..n];
                        match classify_webrtc_packet(data) {
                            WebRtcPacketKind::Stun => {
                                if verify_stun_integrity_and_fingerprint(data, Some(&pwd_bytes)).is_ok() {
                                    let mut tx = [0u8; 12];
                                    tx.copy_from_slice(&data[8..20]);
                                    let resp = build_ice_binding_response(tx, src, Some(&pwd_bytes));
                                    let _ = sfu_a_sock.send_to(&resp, src).await;
                                }
                            }
                            WebRtcPacketKind::Dtls => {
                                if srv_a.handle_packet(data).is_ok()
                                    && let Ok(out) = srv_a.poll_outputs()
                                {
                                    timeout_a = out.next_timeout;
                                    for pkt in out.outgoing_packets {
                                        let _ = sfu_a_sock.send_to(&pkt, src).await;
                                    }
                                }
                            }
                            WebRtcPacketKind::SrtpOrSrtcp => {
                                if srv_a.is_ready()
                                    && srv_b.is_ready()
                                    && let Ok(plain_rtp) = srv_a.unprotect_rtp(data)
                                    && let Some(dst_b) = addr_b
                                    && let Ok(re_encrypted) = srv_b.protect_rtp(&plain_rtp)
                                {
                                    let _ = sfu_b_sock.send_to(&re_encrypted, dst_b).await;
                                    forwarded_a_to_b += 1;
                                }
                            }
                            WebRtcPacketKind::Unknown => {}
                        }
                    }
                    res = sfu_b_sock.recv_from(&mut buf_b) => {
                        let Ok((n, src)) = res else { continue };
                        addr_b = Some(src);
                        let data = &buf_b[..n];
                        match classify_webrtc_packet(data) {
                            WebRtcPacketKind::Stun => {
                                if verify_stun_integrity_and_fingerprint(data, Some(&pwd_bytes)).is_ok() {
                                    let mut tx = [0u8; 12];
                                    tx.copy_from_slice(&data[8..20]);
                                    let resp = build_ice_binding_response(tx, src, Some(&pwd_bytes));
                                    let _ = sfu_b_sock.send_to(&resp, src).await;
                                }
                            }
                            WebRtcPacketKind::Dtls => {
                                if srv_b.handle_packet(data).is_ok()
                                    && let Ok(out) = srv_b.poll_outputs()
                                {
                                    timeout_b = out.next_timeout;
                                    for pkt in out.outgoing_packets {
                                        let _ = sfu_b_sock.send_to(&pkt, src).await;
                                    }
                                }
                            }
                            WebRtcPacketKind::SrtpOrSrtcp => {
                                if srv_b.is_ready()
                                    && srv_a.is_ready()
                                    && let Ok(plain_rtp) = srv_b.unprotect_rtp(data)
                                    && let Some(dst_a) = addr_a
                                    && let Ok(re_encrypted) = srv_a.protect_rtp(&plain_rtp)
                                {
                                    let _ = sfu_a_sock.send_to(&re_encrypted, dst_a).await;
                                    forwarded_b_to_a += 1;
                                }
                            }
                            WebRtcPacketKind::Unknown => {}
                        }
                    }
                    _ = dtls_tick.tick() => {
                        let now = Instant::now();
                        if let (Some(dl), Some(dst)) = (timeout_a, addr_a)
                            && now >= dl
                            && srv_a.handle_timeout(now).is_ok()
                            && let Ok(out) = srv_a.poll_outputs()
                        {
                            timeout_a = out.next_timeout;
                            for pkt in out.outgoing_packets {
                                let _ = sfu_a_sock.send_to(&pkt, dst).await;
                            }
                        }
                        if let (Some(dl), Some(dst)) = (timeout_b, addr_b)
                            && now >= dl
                            && srv_b.handle_timeout(now).is_ok()
                            && let Ok(out) = srv_b.poll_outputs()
                        {
                            timeout_b = out.next_timeout;
                            for pkt in out.outgoing_packets {
                                let _ = sfu_b_sock.send_to(&pkt, dst).await;
                            }
                        }
                    }
                }
            }
            (forwarded_a_to_b, forwarded_b_to_a)
        });

        alice.set_join_response_payload(&alice_resp_json).unwrap();
        bob.set_join_response_payload(&bob_resp_json).unwrap();

        let alice_audio = alice.virtual_audio().unwrap();
        let bob_audio = bob.virtual_audio().unwrap();

        let mut alice_connected = false;
        let mut bob_connected = false;
        let mut audio_seeded = false;
        let mut alice_errors = Vec::new();
        let mut bob_errors = Vec::new();
        let mut alice_pcm = Vec::new();
        let mut bob_pcm = Vec::new();

        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
            while let Some(ev) = alice.poll_event() {
                match ev {
                    GroupMediaEvent::NetworkStateChanged {
                        connected: true, ..
                    } => {
                        alice_connected = true;
                    }
                    GroupMediaEvent::Error(e) => alice_errors.push(e),
                    _ => {}
                }
            }
            while let Some(ev) = bob.poll_event() {
                match ev {
                    GroupMediaEvent::NetworkStateChanged {
                        connected: true, ..
                    } => {
                        bob_connected = true;
                    }
                    GroupMediaEvent::Error(e) => bob_errors.push(e),
                    _ => {}
                }
            }
            let a_drained = alice_audio.drain_playback_pcm();
            let b_drained = bob_audio.drain_playback_pcm();
            if alice_connected && bob_connected {
                if !audio_seeded {
                    alice_audio.push_capture_pcm(&generate_sine(440.0, 2500, 0.45));
                    bob_audio.push_capture_pcm(&generate_sine(880.0, 2500, 0.45));
                    audio_seeded = true;
                } else {
                    alice_pcm.extend_from_slice(&a_drained);
                    bob_pcm.extend_from_slice(&b_drained);
                    if alice_pcm.len() >= VOIP_SAMPLE_RATE as usize
                        && bob_pcm.len() >= VOIP_SAMPLE_RATE as usize
                    {
                        break;
                    }
                }
            }
        }

        alice.stop_async().await;
        bob.stop_async().await;
        let _ = sfu_stop_tx.send(());
        let (a_to_b, b_to_a) = sfu_task.await.unwrap();

        assert!(
            alice_connected && bob_connected,
            "both DTLS-SRTP engines must connect over localhost (alice={alice_connected}, bob={bob_connected}, alice_err={alice_errors:?}, bob_err={bob_errors:?})"
        );
        assert!(
            a_to_b >= 20 && b_to_a >= 20,
            "expected bidirectional SRTP audio frames (a_to_b={a_to_b}, b_to_a={b_to_a})"
        );

        let start = VOIP_FRAME_SAMPLES * 10; // skip 200 ms jitter-buffer priming after connect
        let bob_end = bob_pcm.len().min(start + VOIP_FRAME_SAMPLES * 35);
        let alice_end = alice_pcm.len().min(start + VOIP_FRAME_SAMPLES * 35);
        assert!(
            bob_end > start + VOIP_FRAME_SAMPLES * 8 && alice_end > start + VOIP_FRAME_SAMPLES * 8,
            "insufficient decrypted playback PCM (bob={}, alice={})",
            bob_pcm.len(),
            alice_pcm.len()
        );

        let bob_m440 = goertzel_magnitude(&bob_pcm[start..bob_end], 440.0);
        let bob_m1200 = goertzel_magnitude(&bob_pcm[start..bob_end], 1200.0);
        assert!(
            bob_m440 > 0.06 && bob_m440 > bob_m1200 * 5.0,
            "Bob must decode Alice's 440 Hz tone over DTLS-SRTP (m440={bob_m440:.4}, m1200={bob_m1200:.4})"
        );

        let alice_m880 = goertzel_magnitude(&alice_pcm[start..alice_end], 880.0);
        let alice_m300 = goertzel_magnitude(&alice_pcm[start..alice_end], 300.0);
        assert!(
            alice_m880 > 0.06 && alice_m880 > alice_m300 * 5.0,
            "Alice must decode Bob's 880 Hz tone over DTLS-SRTP (m880={alice_m880:.4}, m300={alice_m300:.4})"
        );

        wait_for_zero_leaks(Duration::from_secs(3)).await;
        assert_eq!(active_group_media_task_count(), 0);
        assert_eq!(active_dsp_thread_count(), 0);
    });
}

#[test]
fn switch_audio_devices_during_call_with_headless_audio() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async {
        let headless = is_headless_audio();
        let backend = if headless {
            // Prove `AudioBackendKind::Hardware` (the production default) cleanly routes to the
            // headless virtual audio layer when `RUSTGRAM_HEADLESS_AUDIO=1` is active!
            AudioBackendKind::Hardware
        } else {
            AudioBackendKind::Virtual
        };

        let capture_devices =
            MediaEngine::list_audio_devices(DeviceType::Capture).expect("list capture devices");
        let playback_devices =
            MediaEngine::list_audio_devices(DeviceType::Playback).expect("list playback devices");
        if headless {
            assert!(
                capture_devices.len() >= 2 && playback_devices.len() >= 2,
                "headless audio enumeration must return synthetic default + alternate devices"
            );
        }

        let alt_in = capture_devices
            .iter()
            .find(|d| d.id != DEFAULT_DEVICE_ID)
            .map_or_else(|| "headless-capture-2".to_string(), |d| d.id.clone());
        let alt_out = playback_devices
            .iter()
            .find(|d| d.id != DEFAULT_DEVICE_ID)
            .map_or_else(|| "headless-playback-2".to_string(), |d| d.id.clone());

        let reflector = LoopbackReflector::start(ReflectorImpairment::default())
            .await
            .expect("start loopback reflector");
        let key = [0x9Au8; AUTH_KEY_SIZE];

        let cfg = MediaEngineConfig {
            audio_backend: backend,
            dsp: VoiceDspConfig::default(),
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            quality_tick_ms: 200,
            ..MediaEngineConfig::default()
        };

        let mut alice = MediaEngine::with_current_runtime(cfg.clone()).unwrap();
        let mut bob = MediaEngine::with_current_runtime(cfg).unwrap();
        alice
            .start(make_descriptor(key, true, Some(reflector.addr())))
            .expect("start alice under headless audio");
        bob.start(make_descriptor(key, false, Some(reflector.addr())))
            .expect("start bob under headless audio");

        let mut alice_est = false;
        let mut bob_est = false;

        for step in 0..30 {
            tokio::time::sleep(Duration::from_millis(25)).await;
            while let Some(ev) = alice.poll_event() {
                match ev {
                    MediaEngineEvent::SignalingData(b) => bob.receive_signaling_data(b),
                    MediaEngineEvent::StateChanged(ControllerState::Established) => {
                        alice_est = true;
                    }
                    _ => {}
                }
            }
            while let Some(ev) = bob.poll_event() {
                match ev {
                    MediaEngineEvent::SignalingData(b) => alice.receive_signaling_data(b),
                    MediaEngineEvent::StateChanged(ControllerState::Established) => {
                        bob_est = true;
                    }
                    _ => {}
                }
            }

            // Hot-swap audio devices repeatedly while the call is live.
            if step == 5 {
                alice.set_audio_input_device(&alt_in);
                alice.set_audio_output_device(&alt_out);
            } else if step == 15 {
                alice.set_audio_devices(DEFAULT_DEVICE_ID, DEFAULT_DEVICE_ID);
            } else if step == 22 {
                alice.set_audio_devices(&alt_in, &alt_out);
            }
        }

        assert!(
            alice_est && bob_est,
            "engines must reach Established while hot-swapping headless audio devices"
        );
        assert_eq!(alice.input_device_id(), alt_in);
        assert_eq!(alice.output_device_id(), alt_out);

        // Also exercise mid-call audio device switching on GroupMediaEngine.
        let mut group = GroupMediaEngine::with_current_runtime(GroupMediaConfig {
            audio_backend: backend,
            dsp: VoiceDspConfig::default(),
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            ..GroupMediaConfig::default()
        })
        .expect("create group engine");
        group.start().expect("start group engine");
        group.set_audio_devices(&alt_in, &alt_out);
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(group.input_device_id(), alt_in);
        assert_eq!(group.output_device_id(), alt_out);
        group.set_audio_devices(DEFAULT_DEVICE_ID, DEFAULT_DEVICE_ID);
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(group.input_device_id(), DEFAULT_DEVICE_ID);
        assert_eq!(group.output_device_id(), DEFAULT_DEVICE_ID);

        group.stop_async().await;
        alice.stop_async().await;
        bob.stop_async().await;
        reflector.stop().await;

        wait_for_zero_leaks(Duration::from_secs(3)).await;
        assert_eq!(active_media_task_count(), 0);
        assert_eq!(active_group_media_task_count(), 0);
        assert_eq!(active_dsp_thread_count(), 0);
    });
}
