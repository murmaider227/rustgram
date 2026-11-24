//! Fake-only integration tests for WP-48 platform services:
//! - caBLE BLE proximity scanner & FIDO2/Google EID advertisement parser (`cable_scanner`)
//! - Widget helper sandbox launcher & length-prefixed framed IPC (`widget_sandbox`)
//! - Windows StartupTask / Run key autostart registration & argument quoting (`autostart`)
//!
//! Never scans real Bluetooth, spawns real processes/named pipes, or modifies Windows startup
//! tasks or registry keys.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tg_platform::autostart::{
    AutostartEntry, AutostartProvider, FakeAutostartProvider, StartupTaskState,
    format_autostart_command, parse_command_line_args, quote_command_argument,
    quote_executable_path,
};
use tg_platform::cable_scanner::{
    BLE_AD_TYPE_SERVICE_DATA_16, BleAdvertisement, BleDataSection, CABLE_FIDO_UUID16,
    CABLE_GOOGLE_UUID16, CableBleScanner, CableServiceUuid, FakeCableBleScanner,
    build_cable_service_data_section, extract_cable_service_data, match_cable_advertisement,
    parse_cable_service_data_section, parse_raw_ble_ad_packet, start_cable_proximity_match,
};
use tg_platform::webauthn::{
    CABLE_ADVERT_LEN, CABLE_EID_KEY_LEN, CABLE_EID_LEN, CableDerivedValueType, cable_derive,
    cable_encrypt_advert,
};
use tg_platform::widget_sandbox::{
    ALL_APPLICATION_PACKAGES_OPT_OUT, APP_CONTAINER_NAME, AppContainerLevel,
    CHILD_PROCESS_RESTRICTED, ChannelError, DEFAULT_JOB_LIMIT_FLAGS, DEFAULT_JOB_UI_RESTRICTIONS,
    DEFAULT_MAX_MESSAGE_BYTES, FakeWidgetChannel, FakeWidgetSandboxLauncher, JobLimitsConfig,
    MITIGATION_HIGH_ENTROPY_ASLR_ALWAYS_ON, MITIGATION2_CET_USER_SHADOW_STACKS_ALWAYS_ON,
    WidgetHelperChannel, WidgetSandboxConfig, WidgetSandboxLauncher, compute_sandbox_policies,
    decode_frame_length, encode_length_prefixed_frame, format_widget_pipe_name,
    parse_inherited_handle_token, quote_windows_argument, verify_job_limits,
};

fn make_test_eid_advert(
    secret: &[u8; 16],
    nonce: [u8; 10],
    routing_id: [u8; 3],
    domain: u16,
) -> (
    [u8; CABLE_EID_KEY_LEN],
    [u8; CABLE_EID_LEN],
    [u8; CABLE_ADVERT_LEN],
) {
    // DerivedValueType::EidKey == 1 in cable_core.h
    let eid_key: [u8; CABLE_EID_KEY_LEN] = cable_derive(secret, &[], CableDerivedValueType::EidKey);
    let mut eid = [0u8; CABLE_EID_LEN];
    eid[0] = 0;
    eid[1..11].copy_from_slice(&nonce);
    eid[11..14].copy_from_slice(&routing_id);
    eid[14..16].copy_from_slice(&domain.to_le_bytes());
    let advert = cable_encrypt_advert(&eid, &eid_key);
    (eid_key, eid, advert)
}

// ============================================================================
// 1. caBLE BLE Proximity Scanner & EID Parser Tests
// ============================================================================

#[test]
fn cable_eid_parser_extracts_and_matches_fido_and_google_service_data() {
    let (eid_key, eid, advert) = make_test_eid_advert(
        &[0x11; 16],
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
        [0x10, 0x20, 0x30],
        0, // "cable.ua5v.com"
    );

    // Build raw BLE GAP packet with Flags + unrelated Manufacturer Data + FIDO 0xFFF9 Service Data
    let mut raw_packet = Vec::new();
    raw_packet.extend_from_slice(&[2, 0x01, 0x06]); // Flags
    raw_packet.extend_from_slice(&[5, 0xFF, 0x4C, 0x00, 0x02, 0x15]); // Unrelated manufacturer data
    raw_packet.push((1 + 2 + CABLE_ADVERT_LEN) as u8);
    raw_packet.push(BLE_AD_TYPE_SERVICE_DATA_16);
    raw_packet.extend_from_slice(&CABLE_FIDO_UUID16.to_le_bytes());
    raw_packet.extend_from_slice(&advert);

    let sections = parse_raw_ble_ad_packet(&raw_packet);
    assert_eq!(sections.len(), 3);

    let adv = BleAdvertisement {
        sections,
        bluetooth_address: Some(0x1122_3344_5566),
        rssi: Some(-48),
    };
    let extracted = extract_cable_service_data(&adv);
    assert_eq!(extracted.len(), 1);
    assert_eq!(extracted[0].uuid, CableServiceUuid::Fido);
    assert_eq!(extracted[0].advert_bytes(), Some(&advert));

    let matched = match_cable_advertisement(&adv, &eid_key).expect("FIDO EID should match");
    assert_eq!(matched.uuid, Some(CableServiceUuid::Fido));
    assert_eq!(matched.decrypted_eid, eid);
    assert_eq!(matched.components.routing_id, [0x10, 0x20, 0x30]);
    assert_eq!(matched.components.nonce, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    assert_eq!(matched.tunnel_domain, "cable.ua5v.com");

    // Also verify Google 0xFDE2 UUID with domain 1 ("cable.auth.com")
    let (g_key, g_eid, g_advert) = make_test_eid_advert(
        &[0x22; 16],
        [9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
        [0xFE, 0xED, 0x01],
        1, // "cable.auth.com"
    );
    let google_adv = BleAdvertisement {
        sections: vec![build_cable_service_data_section(
            CableServiceUuid::Google,
            &g_advert,
        )],
        ..Default::default()
    };
    let g_matched =
        match_cable_advertisement(&google_adv, &g_key).expect("Google EID should match");
    assert_eq!(g_matched.uuid, Some(CableServiceUuid::Google));
    assert_eq!(g_matched.decrypted_eid, g_eid);
    assert_eq!(g_matched.tunnel_domain, "cable.auth.com");
}

#[test]
fn cable_eid_parser_ignores_malformed_truncated_and_wrong_key_adverts() {
    let (eid_key, _eid, advert) = make_test_eid_advert(&[0x33; 16], [0xAA; 10], [1, 2, 3], 0);
    let (other_key, _other_eid, other_advert) =
        make_test_eid_advert(&[0x44; 16], [0xBB; 10], [4, 5, 6], 0);

    // 1. Truncated payload (< 22 bytes total)
    let mut short_sec = Vec::new();
    short_sec.extend_from_slice(&CABLE_FIDO_UUID16.to_le_bytes());
    short_sec.extend_from_slice(&advert[..19]);
    assert!(parse_cable_service_data_section(BLE_AD_TYPE_SERVICE_DATA_16, &short_sec).is_none());

    // 2. Unrelated 16-bit UUID (0x180D Heart Rate)
    let mut unrelated_uuid = Vec::new();
    unrelated_uuid.extend_from_slice(&0x180Du16.to_le_bytes());
    unrelated_uuid.extend_from_slice(&advert);
    assert!(
        parse_cable_service_data_section(BLE_AD_TYPE_SERVICE_DATA_16, &unrelated_uuid).is_none()
    );

    // 3. Wrong AD type (0xFF Manufacturer Specific instead of 0x16 Service Data)
    let mut valid_body = Vec::new();
    valid_body.extend_from_slice(&CABLE_GOOGLE_UUID16.to_le_bytes());
    valid_body.extend_from_slice(&advert);
    assert!(parse_cable_service_data_section(0xFF, &valid_body).is_none());

    // 4. Tampered HMAC tag fails decryption
    let mut tampered = advert;
    tampered[19] ^= 0x01;
    let tampered_adv = BleAdvertisement {
        sections: vec![build_cable_service_data_section(
            CableServiceUuid::Fido,
            &tampered,
        )],
        ..Default::default()
    };
    assert!(match_cable_advertisement(&tampered_adv, &eid_key).is_none());

    // 5. Valid advert encrypted under another ceremony's key does not match
    let other_adv = BleAdvertisement {
        sections: vec![build_cable_service_data_section(
            CableServiceUuid::Fido,
            &other_advert,
        )],
        ..Default::default()
    };
    assert!(match_cable_advertisement(&other_adv, &eid_key).is_none());
    assert!(match_cable_advertisement(&other_adv, &other_key).is_some());

    // 6. Malformed raw BLE packet with zero terminator and truncated tail never panics
    assert!(parse_raw_ble_ad_packet(&[]).is_empty());
    assert!(parse_raw_ble_ad_packet(&[0x00, 0x05, 0x16]).is_empty());
    assert!(parse_raw_ble_ad_packet(&[0x20, 0x16, 0xF9, 0xFF]).is_empty());
}

#[test]
fn fake_cable_scanner_delivers_matches_handles_no_bluetooth_and_errors() {
    let (eid_key, eid, advert) = make_test_eid_advert(&[0x55; 16], [5; 10], [0xDE, 0xAD, 0xBE], 0);
    let (_other_key, _other_eid, decoy_advert) =
        make_test_eid_advert(&[0x66; 16], [6; 10], [0x01, 0x02, 0x03], 0);

    // Unavailable radio returns Ok(false)
    let no_bt = FakeCableBleScanner::new(false);
    let no_bt_dyn: Arc<dyn CableBleScanner> = Arc::new(no_bt.clone());
    let started = start_cable_proximity_match(
        &no_bt_dyn,
        eid_key,
        true,
        |_| panic!("should not match without Bluetooth"),
        Arc::new(|_| {}),
    )
    .unwrap();
    assert!(!started);
    assert!(!no_bt.is_running());

    // Injected error propagates cleanly
    no_bt.set_error(Some("radio access denied".to_owned()));
    assert!(
        no_bt
            .start(Arc::new(|_| {}), Arc::new(|_| {}))
            .unwrap_err()
            .to_string()
            .contains("radio access denied")
    );

    // Active scanner filters decoys, deduplicates repeats, and delivers matching EID
    let fake = FakeCableBleScanner::new(true);
    let scanner: Arc<dyn CableBleScanner> = Arc::new(fake.clone());
    let matched_list = Arc::new(Mutex::new(Vec::new()));
    let matched_clone = Arc::clone(&matched_list);
    let availability = Arc::new(Mutex::new(Vec::new()));
    let avail_clone = Arc::clone(&availability);

    assert!(
        start_cable_proximity_match(
            &scanner,
            eid_key,
            true,
            move |m| matched_clone.lock().unwrap().push(m),
            Arc::new(move |ok| avail_clone.lock().unwrap().push(ok)),
        )
        .unwrap()
    );
    assert_eq!(*availability.lock().unwrap(), vec![true]);

    // Decoy caBLE advert is forwarded once (deduped on repeat) and rejected by HMAC check
    let decoy_adv = BleAdvertisement {
        sections: vec![build_cable_service_data_section(
            CableServiceUuid::Google,
            &decoy_advert,
        )],
        ..Default::default()
    };
    assert_eq!(fake.emit_advertisement(&decoy_adv), 1);
    assert_eq!(fake.emit_advertisement(&decoy_adv), 0); // deduplicated
    assert!(matched_list.lock().unwrap().is_empty());

    // Noise section ignored completely
    let noise_adv = BleAdvertisement {
        sections: vec![BleDataSection {
            data_type: 0x08,
            data: b"Keyboard".to_vec(),
        }],
        ..Default::default()
    };
    assert_eq!(fake.emit_advertisement(&noise_adv), 0);

    // Real matching advert arrives as raw BLE GAP packet -> triggers match & auto-stops scanner
    let mut raw_match = vec![
        (1 + 2 + CABLE_ADVERT_LEN) as u8,
        BLE_AD_TYPE_SERVICE_DATA_16,
    ];
    raw_match.extend_from_slice(&CABLE_FIDO_UUID16.to_le_bytes());
    raw_match.extend_from_slice(&advert);
    assert_eq!(fake.emit_raw_ble_packet(&raw_match), 1);
    assert!(!fake.is_running());

    let matches = matched_list.lock().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].decrypted_eid, eid);
    assert_eq!(matches[0].components.routing_id, [0xDE, 0xAD, 0xBE]);
    assert_eq!(matches[0].tunnel_domain, "cable.ua5v.com");
}

// ============================================================================
// 2. Widget Helper Sandbox Launcher & Framed IPC Tests
// ============================================================================

#[test]
fn fake_widget_channel_round_trips_bidirectional_frames_across_threads() {
    let (host, helper) = FakeWidgetChannel::pair(4096);
    let host = Arc::new(host);
    let helper = Arc::new(helper);

    let helper_thread = {
        let helper = Arc::clone(&helper);
        thread::spawn(move || {
            for expected_idx in 0u32..5 {
                let req = helper.recv_frame().expect("helper recv");
                let mut reply = b"ack:".to_vec();
                reply.extend_from_slice(&req);
                assert_eq!(req, format!("msg-{expected_idx}").as_bytes());
                helper.send_frame(&reply).expect("helper send");
            }
            helper.shutdown();
        })
    };

    for idx in 0u32..5 {
        let msg = format!("msg-{idx}");
        host.send_frame(msg.as_bytes()).expect("host send");
        let reply = host.recv_frame().expect("host recv");
        assert_eq!(reply, format!("ack:{msg}").as_bytes());
    }
    helper_thread.join().expect("helper thread finished");

    // Once helper shuts down, host gets a clean PeerDisconnected error.
    assert_eq!(host.recv_frame(), Err(ChannelError::PeerDisconnected));
    assert_eq!(
        host.send_frame(b"late"),
        Err(ChannelError::PeerDisconnected)
    );
}

#[test]
fn fake_widget_channel_enforces_max_size_and_handles_framing_errors() {
    let max_bytes = 32;
    let (host, helper) = FakeWidgetChannel::pair(max_bytes);
    assert_eq!(host.max_message_bytes(), max_bytes);

    // Boundary check: exact max size succeeds
    let exact = vec![0x5Au8; max_bytes];
    host.send_frame(&exact).unwrap();
    assert_eq!(helper.recv_frame().unwrap(), exact);

    // One byte over max size fails on send
    let over = vec![0x5Au8; max_bytes + 1];
    assert_eq!(
        host.send_frame(&over),
        Err(ChannelError::MessageTooLarge {
            size: max_bytes + 1,
            max: max_bytes,
        })
    );

    // Pure framing helpers match channel behavior
    let encoded = encode_length_prefixed_frame(b"abc", 16).unwrap();
    assert_eq!(encoded, vec![3, 0, 0, 0, b'a', b'b', b'c']);
    assert_eq!(decode_frame_length([3, 0, 0, 0], 16).unwrap(), 3);
    assert_eq!(
        decode_frame_length([17, 0, 0, 0], 16),
        Err(ChannelError::MessageTooLarge { size: 17, max: 16 })
    );

    // Incoming oversized header rejected on recv without reading huge allocation
    helper
        .inject_raw_bytes(&1_000_000u32.to_le_bytes())
        .unwrap();
    assert_eq!(
        host.recv_frame(),
        Err(ChannelError::MessageTooLarge {
            size: 1_000_000,
            max: max_bytes,
        })
    );

    // Local shutdown yields ChannelError::Closed
    host.shutdown();
    assert!(host.is_closed());
    assert_eq!(host.send_frame(b"x"), Err(ChannelError::Closed));
    assert_eq!(host.recv_frame(), Err(ChannelError::Closed));
}

#[test]
fn fake_widget_sandbox_launcher_lifecycle_and_policy_checks() {
    let launcher = FakeWidgetSandboxLauncher::new();
    assert!(launcher.is_supported());
    assert_eq!(launcher.unsupported_reason(), None);

    let mut cfg = WidgetSandboxConfig::for_executable(PathBuf::from(
        r"C:\Program Files\Rustgram\rustgram-widget-helper.exe",
    ));
    cfg.arguments = vec!["--widget".to_owned(), "clock".to_owned()];
    cfg.max_message_bytes = 2048;
    assert_eq!(cfg.app_container_name, APP_CONTAINER_NAME);
    assert_eq!(cfg.app_container_level, AppContainerLevel::LessPrivileged);

    let proc = launcher.launch(&cfg).expect("launch succeeds");
    let pid = proc.pid();
    assert!(pid >= 10_000);
    let peer = launcher
        .take_peer_channel(pid)
        .expect("peer channel available");

    // Bidirectional IPC through launched process handle
    proc.channel().send_frame(b"open:2.0").unwrap();
    assert_eq!(peer.recv_frame().unwrap(), b"open:2.0");
    peer.send_frame(b"{\"type\":\"ready\"}").unwrap();
    assert_eq!(
        proc.channel().recv_frame().unwrap(),
        b"{\"type\":\"ready\"}"
    );

    // Process has not exited yet
    assert_eq!(proc.wait_exit(Duration::from_millis(1)).unwrap(), None);

    // Terminating closes the IPC channel and reports clean completion
    let completion = proc.terminate().unwrap();
    assert!(completion.reaped);
    assert!(completion.terminated);
    assert_eq!(peer.recv_frame(), Err(ChannelError::PeerDisconnected));

    // Verify sandbox policies, job checks, pipe naming, quoting, and handle parsing
    let policies = compute_sandbox_policies(true, true);
    assert_eq!(policies.creation_size, 16);
    assert_ne!(
        policies.creation[0] & MITIGATION_HIGH_ENTROPY_ASLR_ALWAYS_ON,
        0
    );
    assert_ne!(
        policies.creation[1] & MITIGATION2_CET_USER_SHADOW_STACKS_ALWAYS_ON,
        0
    );
    assert_eq!(policies.child_process, CHILD_PROCESS_RESTRICTED);
    assert_eq!(
        policies.all_application_packages,
        ALL_APPLICATION_PACKAGES_OPT_OUT
    );
    assert!(
        verify_job_limits(
            true,
            DEFAULT_JOB_LIMIT_FLAGS,
            1,
            DEFAULT_JOB_UI_RESTRICTIONS,
            &JobLimitsConfig::default()
        )
        .is_ok()
    );
    assert!(format_widget_pipe_name(pid, 1, 2).starts_with(r"\\.\pipe\telegram-widget-helper-"));
    assert_eq!(
        quote_windows_argument(r#"a "quoted" arg"#),
        r#""a \"quoted\" arg""#
    );
    assert_eq!(parse_inherited_handle_token("256"), Some(256));
    assert_eq!(parse_inherited_handle_token("0"), None);
    assert_eq!(DEFAULT_MAX_MESSAGE_BYTES, 32 * 1024 * 1024);

    // Unsupported launcher rejects launches with clear reason
    let unsupported = FakeWidgetSandboxLauncher::unsupported("Windows 10 1809 required");
    assert!(!unsupported.is_supported());
    assert_eq!(
        unsupported.unsupported_reason().as_deref(),
        Some("Windows 10 1809 required")
    );
    assert!(unsupported.launch(&cfg).is_err());
}

// ============================================================================
// 3. Windows StartupTask / Run Key Autostart Provider Tests
// ============================================================================

#[test]
fn fake_autostart_provider_state_transitions_and_policy_guards() {
    let entry =
        AutostartEntry::for_executable(PathBuf::from(r"C:\Program Files\Rustgram\rustgram.exe"))
            .with_workdir(Path::new(r"C:\Users\TestUser\AppData\Roaming\Rustgram\"));

    let provider = FakeAutostartProvider::new(entry);

    // Initial state: Disabled
    assert_eq!(provider.query_state().unwrap(), StartupTaskState::Disabled);
    assert!(!provider.is_enabled().unwrap());
    assert_eq!(provider.registered_command().unwrap(), None);

    // Enable -> Enabled with properly quoted command line
    assert_eq!(
        provider.set_enabled(true).unwrap(),
        StartupTaskState::Enabled
    );
    assert!(provider.is_enabled().unwrap());
    let cmd = provider
        .registered_command()
        .unwrap()
        .expect("command registered on enable");
    assert_eq!(
        parse_command_line_args(&cmd),
        vec![
            r"C:\Program Files\Rustgram\rustgram.exe",
            "-autostart",
            "-workdir",
            r"C:\Users\TestUser\AppData\Roaming\Rustgram\",
        ]
    );

    // Idempotent enable keeps Enabled
    assert_eq!(
        provider.set_enabled(true).unwrap(),
        StartupTaskState::Enabled
    );

    // Disable -> Disabled and clears registered command
    assert_eq!(
        provider.set_enabled(false).unwrap(),
        StartupTaskState::Disabled
    );
    assert!(!provider.is_enabled().unwrap());
    assert_eq!(provider.registered_command().unwrap(), None);

    // DisabledByUser: app cannot re-enable; must direct user to Windows Settings
    provider.set_state(StartupTaskState::DisabledByUser);
    assert_eq!(
        provider.set_enabled(true).unwrap(),
        StartupTaskState::DisabledByUser
    );
    assert!(!provider.is_enabled().unwrap());
    assert!(
        provider
            .query_state()
            .unwrap()
            .is_blocked_by_user_or_policy()
    );
    provider.open_startup_settings().unwrap();
    assert_eq!(provider.settings_opened_count(), 1);

    // Group Policy states cannot be toggled by the app
    provider.set_state(StartupTaskState::DisabledByPolicy);
    assert!(!StartupTaskState::DisabledByPolicy.can_toggle());
    assert_eq!(
        provider.set_enabled(true).unwrap(),
        StartupTaskState::DisabledByPolicy
    );

    provider.set_state(StartupTaskState::EnabledByPolicy);
    assert!(!StartupTaskState::EnabledByPolicy.can_toggle());
    assert!(provider.is_enabled().unwrap());
    assert_eq!(
        provider.set_enabled(false).unwrap(),
        StartupTaskState::EnabledByPolicy
    );

    // Error injection propagates cleanly
    provider.set_error(Some("StartupTask service unavailable".to_owned()));
    assert!(
        provider
            .query_state()
            .unwrap_err()
            .to_string()
            .contains("StartupTask service unavailable")
    );
    assert!(provider.toggle_history().len() >= 6);
}

#[test]
fn autostart_executable_and_argument_quoting_handles_spaces_quotes_and_backslashes() {
    let exe = Path::new(r"C:\Program Files (x86)\My App\rustgram.exe");
    assert_eq!(
        quote_executable_path(exe).unwrap(),
        r#""C:\Program Files (x86)\My App\rustgram.exe""#
    );

    assert_eq!(quote_command_argument("-autostart").unwrap(), "-autostart");
    assert_eq!(quote_command_argument("").unwrap(), "\"\"");
    assert_eq!(
        quote_command_argument(r"C:\My Folder\Sub Folder\").unwrap(),
        r#""C:\My Folder\Sub Folder\\""#
    );
    assert_eq!(
        quote_command_argument(r#"say "hi""#).unwrap(),
        r#""say \"hi\"""#
    );

    let args = vec![
        "-autostart".to_owned(),
        "-workdir".to_owned(),
        r"D:\Data Dir\Profile 1\".to_owned(),
        r#"--label=work "main""#.to_owned(),
    ];
    let formatted = format_autostart_command(exe, &args).unwrap();
    let round_tripped = parse_command_line_args(&formatted);
    assert_eq!(
        round_tripped[0],
        r"C:\Program Files (x86)\My App\rustgram.exe"
    );
    assert_eq!(&round_tripped[1..], args.as_slice());

    // Reject dangerous/invalid inputs
    assert!(quote_executable_path(Path::new("")).is_err());
    assert!(quote_executable_path(Path::new("   ")).is_err());
    assert!(quote_executable_path(Path::new("C:\\app\r\n.exe")).is_err());
    assert!(quote_command_argument("bad\0null").is_err());
}
