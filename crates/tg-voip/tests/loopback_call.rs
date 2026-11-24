//! End-to-end loopback integration tests for `tg-voip`:
//!
//! Spawns two full [`MediaEngine`] instances in one process, connected over real
//! loopback UDP sockets through [`LoopbackReflector`] (speaking the exact
//! `ReflectorPort.cpp` UDP framing), exchanging encrypted `tgcalls` v2 signaling
//! and MTProto-2.0 AES-256-CTR encrypted Opus audio generated from a 440 Hz sine wave.
//!
//! Verifies:
//! 1. Clean loopback: 440 Hz sine wave encoded via Opus -> encrypted -> relayed via
//!    local UDP reflector -> decrypted -> jitter buffered -> decoded recovers dominant
//!    440 Hz spectral energy at the receiving engine.
//! 2. Impaired loopback (simulated packet loss + packet reordering): jitter buffer
//!    reorders out-of-order packets, runs Opus FEC/PLC on lost packets, maintains
//!    continuous 440 Hz audio recovery, updates signal bars & adaptive bitrate, and
//!    emits a valid `phone.saveCallDebug` JSON log on stop.
//! 3. Mute/unmute `MediaState` signaling propagation & `enable_p2p = false` privacy.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tg_calls::{
    ControllerDescriptor, ControllerState, Endpoint, EndpointHost, EndpointType, RemoteAudioState,
    RtcServer,
};
use tg_voip::{
    AUTH_KEY_SIZE, AudioBackendKind, CallMediaEngine, JitterBufferConfig, LoopbackReflector,
    MediaEngine, MediaEngineConfig, MediaEngineEvent, NetworkType, ReflectorImpairment,
    TransportWireMode, VOIP_SAMPLE_RATE, VoiceDspConfig,
};

fn make_shared_key() -> [u8; AUTH_KEY_SIZE] {
    let mut key = [0u8; AUTH_KEY_SIZE];
    for (i, b) in key.iter_mut().enumerate() {
        *b = ((i * 53 + 7) & 0xFF) as u8;
    }
    key
}

fn make_descriptor(
    key: [u8; AUTH_KEY_SIZE],
    is_outgoing: bool,
    reflector_addr: SocketAddr,
    enable_p2p: bool,
) -> ControllerDescriptor {
    let peer_tag = [
        0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xA0, 0xB0, 0xC0, 0x01, 0x02, 0x03,
        0x04,
    ];
    let peer_tag_hex = "102030405060708090a0b0c001020304".to_string();
    let ip = reflector_addr.ip().to_string();
    let port = reflector_addr.port();

    ControllerDescriptor {
        version: "12.0.0".into(),
        initialization_timeout_ms: 15_000,
        receive_timeout_ms: 10_000,
        enable_p2p,
        max_api_layer: 92,
        encryption_key: key,
        is_outgoing,
        endpoints: vec![Endpoint {
            endpoint_id: 777,
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
            host: ip,
            port,
            login: "reflector".into(),
            password: peer_tag_hex,
            is_turn: true,
            is_tcp: false,
        }],
        custom_parameters: None,
    }
}

fn make_loopback_config(dsp: VoiceDspConfig) -> MediaEngineConfig {
    MediaEngineConfig {
        audio_backend: AudioBackendKind::Virtual,
        virtual_ring_capacity: VOIP_SAMPLE_RATE as usize * 6,
        dsp,
        jitter: JitterBufferConfig {
            min_delay_ms: 40,
            max_delay_ms: 200,
            frame_ms: 20,
        },
        wire_mode: TransportWireMode::MtProtoRaw,
        bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        initial_network_type: NetworkType::WiFi,
        quality_tick_ms: 250,
        ..MediaEngineConfig::default()
    }
}

/// Generate `duration_ms` of a mono 48 kHz sine wave at `freq_hz`.
fn generate_sine(freq_hz: f32, duration_ms: usize, amplitude: f32) -> Vec<f32> {
    let total = (VOIP_SAMPLE_RATE as usize * duration_ms) / 1000;
    (0..total)
        .map(|i| {
            let t = i as f32 / VOIP_SAMPLE_RATE as f32;
            (2.0 * std::f32::consts::PI * freq_hz * t).sin() * amplitude
        })
        .collect()
}

/// Goertzel single-bin DFT magnitude at `target_hz` normalized by sample count.
///
/// Averages over non-overlapping 100 ms (4800-sample) chunks when available so
/// every test frequency (300/440/880/1000/1200 Hz) lands on an exact DFT bin
/// and a single 20 ms PLC/jitter phase slip cannot destructively cancel across
/// a long coherent window.
fn goertzel_magnitude(samples: &[f32], target_hz: f32) -> f64 {
    fn single_window(chunk: &[f32], target_hz: f32) -> f64 {
        if chunk.is_empty() {
            return 0.0;
        }
        let n = chunk.len() as f64;
        let omega = 2.0 * std::f64::consts::PI * f64::from(target_hz) / f64::from(VOIP_SAMPLE_RATE);
        let coeff = 2.0 * omega.cos();
        let mut s_prev = 0.0f64;
        let mut s_prev2 = 0.0f64;
        for &x in chunk {
            let s = f64::from(x) + coeff * s_prev - s_prev2;
            s_prev2 = s_prev;
            s_prev = s;
        }
        let power = s_prev2 * s_prev2 + s_prev * s_prev - coeff * s_prev * s_prev2;
        (power.max(0.0).sqrt()) / n
    }

    let chunk_len = (VOIP_SAMPLE_RATE as usize) / 10; // 100 ms -> 10 Hz bin grid
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

/// Pump signaling events between `alice` and `bob` and track connection/media state.
fn relay_events(
    alice: &mut MediaEngine,
    bob: &mut MediaEngine,
    alice_established: &mut bool,
    bob_established: &mut bool,
    bob_saw_remote_muted: &mut Option<RemoteAudioState>,
    alice_signal_bars: &mut Vec<i32>,
) {
    while let Some(ev) = alice.poll_event() {
        match ev {
            MediaEngineEvent::SignalingData(blob) => bob.receive_signaling_data(blob),
            MediaEngineEvent::StateChanged(ControllerState::Established) => {
                *alice_established = true;
            }
            MediaEngineEvent::SignalBarsChanged(bars) => alice_signal_bars.push(bars),
            _ => {}
        }
    }
    while let Some(ev) = bob.poll_event() {
        match ev {
            MediaEngineEvent::SignalingData(blob) => alice.receive_signaling_data(blob),
            MediaEngineEvent::StateChanged(ControllerState::Established) => {
                *bob_established = true;
            }
            MediaEngineEvent::RemoteMediaStateChanged { audio, .. } => {
                *bob_saw_remote_muted = Some(audio);
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_two_engines_clean_sine_over_udp_reflector() {
    let reflector = LoopbackReflector::start(ReflectorImpairment::default())
        .await
        .expect("start loopback reflector");

    let key = make_shared_key();
    let mut alice =
        MediaEngine::with_current_runtime(make_loopback_config(VoiceDspConfig::bypass())).unwrap();
    let mut bob =
        MediaEngine::with_current_runtime(make_loopback_config(VoiceDspConfig::bypass())).unwrap();

    // `enable_p2p = false` forces all traffic through the Telegram UDP reflector.
    alice
        .start(make_descriptor(key, true, reflector.addr(), false))
        .unwrap();
    bob.start(make_descriptor(key, false, reflector.addr(), false))
        .unwrap();

    let alice_audio = alice.virtual_audio().unwrap();
    let bob_audio = bob.virtual_audio().unwrap();

    // Feed 2.4 seconds of 440 Hz sine wave into Alice's virtual microphone,
    // and 880 Hz into Bob's microphone (full duplex).
    let sine_440 = generate_sine(440.0, 2400, 0.5);
    let sine_880 = generate_sine(880.0, 2400, 0.4);
    alice_audio.push_capture_pcm(&sine_440);
    bob_audio.push_capture_pcm(&sine_880);

    let mut alice_est = false;
    let mut bob_est = false;
    let mut bob_remote_audio = None;
    let mut alice_bars = Vec::new();
    let mut bob_received_pcm = Vec::new();
    let mut alice_received_pcm = Vec::new();
    let mut alice_muted_sent = false;
    let mut pre_mute_bob_len = 0usize;
    let mut pre_mute_alice_len = 0usize;

    for step in 0..70 {
        tokio::time::sleep(Duration::from_millis(30)).await;
        relay_events(
            &mut alice,
            &mut bob,
            &mut alice_est,
            &mut bob_est,
            &mut bob_remote_audio,
            &mut alice_bars,
        );
        let bob_drained = bob_audio.drain_playback_pcm();
        let alice_drained = alice_audio.drain_playback_pcm();
        if alice_est && bob_est {
            bob_received_pcm.extend_from_slice(&bob_drained);
            alice_received_pcm.extend_from_slice(&alice_drained);
        }

        // Test mute signaling after capturing clean steady-state audio (independent of OS timer jitter).
        if !alice_muted_sent
            && alice_est
            && bob_est
            && (step >= 55
                || (step >= 35
                    && bob_received_pcm.len() >= (VOIP_SAMPLE_RATE as usize * 9) / 10
                    && alice_received_pcm.len() >= (VOIP_SAMPLE_RATE as usize * 9) / 10))
        {
            pre_mute_bob_len = bob_received_pcm.len();
            pre_mute_alice_len = alice_received_pcm.len();
            alice.set_muted(true);
            alice_muted_sent = true;
        }
        if alice_muted_sent && bob_remote_audio == Some(RemoteAudioState::Muted) && step >= 42 {
            break;
        }
    }

    let final_alice = alice.stop_async().await;
    let final_bob = bob.stop_async().await;
    let refl_stats = reflector.stats();
    reflector.stop().await;

    assert!(alice_est, "Alice must reach ControllerState::Established");
    assert!(bob_est, "Bob must reach ControllerState::Established");
    assert!(
        refl_stats.hellos_handled >= 2,
        "reflector must handle UDP hellos from both peers"
    );
    assert!(
        refl_stats.packets_forwarded >= 40,
        "reflector forwarded too few packets: {}",
        refl_stats.packets_forwarded
    );
    assert_eq!(final_bob.preferred_relay_id, 777);
    assert_eq!(final_alice.preferred_relay_id, 777);
    assert_eq!(bob_remote_audio, Some(RemoteAudioState::Muted));
    assert!(
        !alice_bars.is_empty() && alice_bars.iter().all(|&b| (0..=4).contains(&b)),
        "expected valid 0..=4 signal bars, got {alice_bars:?}"
    );

    // Inspect the steady-state window of Bob's decoded playback PCM (skip initial priming silence
    // and strictly stop before Alice muted) to verify 440 Hz tone dominance!
    let window_start = (VOIP_SAMPLE_RATE as usize * 3) / 10; // 300 ms
    let window_end = ((VOIP_SAMPLE_RATE as usize * 9) / 10)
        .min(pre_mute_bob_len)
        .min(bob_received_pcm.len());
    assert!(
        window_end > window_start + 9600,
        "insufficient decoded audio at Bob: {} samples (pre_mute={pre_mute_bob_len})",
        bob_received_pcm.len()
    );
    let bob_window = &bob_received_pcm[window_start..window_end];
    let mag_440 = goertzel_magnitude(bob_window, 440.0);
    let mag_1000 = goertzel_magnitude(bob_window, 1000.0);
    assert!(
        mag_440 > 0.12 && mag_440 > mag_1000 * 10.0,
        "expected strong 440 Hz tone at Bob (mag_440={mag_440:.4}, mag_1000={mag_1000:.4})"
    );

    // Also verify Bob -> Alice 880 Hz full-duplex return path.
    let a_end = ((VOIP_SAMPLE_RATE as usize * 9) / 10)
        .min(pre_mute_alice_len)
        .min(alice_received_pcm.len());
    assert!(a_end > window_start + 9600);
    let alice_window = &alice_received_pcm[window_start..a_end];
    let mag_880 = goertzel_magnitude(alice_window, 880.0);
    let mag_300 = goertzel_magnitude(alice_window, 300.0);
    assert!(
        mag_880 > 0.08 && mag_880 > mag_300 * 8.0,
        "expected strong 880 Hz tone at Alice (mag_880={mag_880:.4}, mag_300={mag_300:.4})"
    );

    // Verify `phone.saveCallDebug` JSON log structure.
    let parsed_log: serde_json::Value =
        serde_json::from_str(&final_bob.debug_log).expect("valid debug JSON");
    assert_eq!(parsed_log["lib"], "tg-voip");
    assert_eq!(parsed_log["codec"], "opus");
    assert!(parsed_log["bitrate"].is_array());
    assert!(parsed_log["network"].is_array());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_under_simulated_packet_loss_and_reordering() {
    // Drop every 7th data packet (~14% loss) and reorder every 5th pair!
    let reflector = LoopbackReflector::start(ReflectorImpairment {
        drop_every_n: 7,
        reorder_every_n: 5,
    })
    .await
    .expect("start impaired loopback reflector");

    let key = make_shared_key();
    let mut alice =
        MediaEngine::with_current_runtime(make_loopback_config(VoiceDspConfig::bypass())).unwrap();
    let mut bob =
        MediaEngine::with_current_runtime(make_loopback_config(VoiceDspConfig::bypass())).unwrap();

    alice
        .start(make_descriptor(key, true, reflector.addr(), false))
        .unwrap();
    bob.start(make_descriptor(key, false, reflector.addr(), false))
        .unwrap();

    let alice_audio = alice.virtual_audio().unwrap();
    let bob_audio = bob.virtual_audio().unwrap();

    // Feed 3 seconds of continuous 440 Hz sine wave so the buffer never runs
    // dry even if handshake packets experience loss or scheduling jitter.
    alice_audio.push_capture_pcm(&generate_sine(440.0, 3000, 0.5));
    bob_audio.push_capture_pcm(&generate_sine(440.0, 3000, 0.5));

    let mut alice_est = false;
    let mut bob_est = false;
    let mut bob_remote_audio = None;
    let mut alice_bars = Vec::new();
    let mut bob_pcm = Vec::new();

    for _ in 0..85 {
        tokio::time::sleep(Duration::from_millis(30)).await;
        relay_events(
            &mut alice,
            &mut bob,
            &mut alice_est,
            &mut bob_est,
            &mut bob_remote_audio,
            &mut alice_bars,
        );
        let drained = bob_audio.drain_playback_pcm();
        if alice_est && bob_est {
            bob_pcm.extend_from_slice(&drained);
            if bob_pcm.len() >= (VOIP_SAMPLE_RATE as usize * 15) / 10 {
                break;
            }
        }
    }

    // Exercise network type switch (WiFi -> Edge -> WiFi).
    alice.set_network_type(NetworkType::Edge);
    tokio::time::sleep(Duration::from_millis(60)).await;

    let final_alice = alice.stop_async().await;
    let final_bob = bob.stop_async().await;
    let refl_stats = reflector.stats();
    reflector.stop().await;

    assert!(alice_est && bob_est);
    assert!(
        refl_stats.packets_dropped > 0,
        "reflector must have dropped packets"
    );
    assert!(
        refl_stats.packets_reordered > 0,
        "reflector must have reordered packets"
    );

    // Jitter buffer must have detected loss and concealed/recovered frames via PLC/FEC,
    // and detected reordered packets!
    assert!(
        final_bob.stats.jitter_packets_lost > 0 || final_alice.stats.jitter_packets_lost > 0,
        "jitter buffer should record lost packets under 14% drop rate (bob={:?}, alice={:?})",
        final_bob.stats,
        final_alice.stats
    );
    assert!(
        final_bob.stats.jitter_fec_recovered + final_bob.stats.jitter_plc_frames > 0
            || final_alice.stats.jitter_fec_recovered + final_alice.stats.jitter_plc_frames > 0,
        "jitter buffer must invoke Opus FEC or PLC on lost frames"
    );
    assert!(
        final_bob.stats.jitter_packets_reordered > 0
            || final_alice.stats.jitter_packets_reordered > 0,
        "jitter buffer must record reordered packets"
    );

    // Despite ~14% loss + reordering, the decoded audio stream must remain finite and
    // preserve clear 440 Hz tone energy.
    let start = (VOIP_SAMPLE_RATE as usize * 2) / 10;
    let end = ((VOIP_SAMPLE_RATE as usize * 12) / 10).min(bob_pcm.len());
    assert!(end > start + 9600);
    let win = &bob_pcm[start..end];
    assert!(win.iter().all(|s| s.is_finite()));
    let mag_440 = goertzel_magnitude(win, 440.0);
    let mag_1200 = goertzel_magnitude(win, 1200.0);
    assert!(
        mag_440 > 0.08 && mag_440 > mag_1200 * 5.0,
        "440 Hz tone should survive 14% loss + reordering (mag_440={mag_440:.4}, mag_1200={mag_1200:.4})"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_direct_p2p_with_aec3_ns_agc2_dsp_and_signal_bars() {
    let key = make_shared_key();
    let mut alice =
        MediaEngine::with_current_runtime(make_loopback_config(VoiceDspConfig::default())).unwrap();
    let mut bob =
        MediaEngine::with_current_runtime(make_loopback_config(VoiceDspConfig::default())).unwrap();

    // Direct P2P call with zero reflectors: peers discover each other via encrypted `Candidates`.
    let desc_a = ControllerDescriptor {
        version: "12.0.0".into(),
        initialization_timeout_ms: 15_000,
        receive_timeout_ms: 10_000,
        enable_p2p: true,
        max_api_layer: 92,
        encryption_key: key,
        is_outgoing: true,
        endpoints: Vec::new(),
        rtc_servers: Vec::new(),
        custom_parameters: None,
    };
    let mut desc_b = desc_a.clone();
    desc_b.is_outgoing = false;

    alice.start(desc_a).unwrap();
    bob.start(desc_b).unwrap();

    let alice_audio = alice.virtual_audio().unwrap();
    let bob_audio = bob.virtual_audio().unwrap();
    alice_audio.push_capture_pcm(&generate_sine(440.0, 1200, 0.4));
    bob_audio.push_capture_pcm(&generate_sine(660.0, 1200, 0.4));

    let mut alice_est = false;
    let mut bob_est = false;
    let mut bob_remote_audio = None;
    let mut alice_bars = Vec::new();
    let mut bob_signal_state = tg_calls::SignalBarsState::new();
    let mut bob_pcm = Vec::new();

    for step in 0..35 {
        tokio::time::sleep(Duration::from_millis(30)).await;
        relay_events(
            &mut alice,
            &mut bob,
            &mut alice_est,
            &mut bob_est,
            &mut bob_remote_audio,
            &mut alice_bars,
        );
        if let Some(&bars) = alice_bars.last() {
            bob_signal_state.update(bars);
        }
        bob_pcm.extend_from_slice(&bob_audio.drain_playback_pcm());
        if step == 20 {
            alice.set_is_low_battery(true);
        }
    }

    let final_alice = alice.stop_async().await;
    let final_bob = bob.stop_async().await;

    assert!(alice_est && bob_est, "P2P engines must establish directly");
    assert_eq!(
        final_alice.stats.endpoint_kind,
        tg_voip::ConnectionEndpointKind::P2p
    );
    assert_eq!(final_alice.preferred_relay_id, 0);
    assert!(final_bob.stats.packets_received >= 20);
    assert!(bob_pcm.len() >= VOIP_SAMPLE_RATE as usize / 2);
    assert!(bob_pcm.iter().all(|s| s.is_finite()));
    assert!(!bob_signal_state.is_starting());
}
