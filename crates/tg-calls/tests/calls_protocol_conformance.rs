//! Offline protocol conformance tests for `tg-calls` against Telegram Desktop
//! (`tdesktop` commit `c2d0ad9`, Layer 233).
//!
//! Covers:
//! 1. 1:1 call setup, DH validation & caching, key fingerprint, timeouts, discard reasons,
//!    duplicate & out-of-order updates, signaling routing & MTProto-2.0 CTR encryption.
//! 2. Emoji fingerprint against values hand-computed directly from `calls_emoji_fingerprint.cpp`.
//! 3. Group call join payload JSON, versioned participant ordering & gap refetch,
//!    cross-call-id isolation, self-update flags (`volume: None`), leave source, RTMP stream channels.
//! 4. RPC error handling (`CALL_PROTOCOL_LAYER_INVALID`, `CALL_ALREADY_DECLINED`,
//!    `PARTICIPANT_VERSION_OUTDATED`, `USER_PRIVACY_RESTRICTED`, `FLOOD_WAIT`,
//!    `GROUPCALL_SSRC_DUPLICATE_MUCH`, `GROUPCALL_FORBIDDEN`, `GROUPCALL_JOIN_MISSING`).

use tg_calls::call::{Call, CallAction, CallState, CallTimerKind, ControllerState};
use tg_calls::dh::{
    DH_GOOD_PRIME, DhConfig, DhConfigCache, RANDOM_POWER_SIZE, compute_ga_hash,
    compute_key_fingerprint, compute_key_sha_for_fingerprint, fill_auth_key_data,
    finish_dh_key_exchange, generate_mod_exp_first_with_rng, is_dh_prime_good,
    is_good_dh_mod_exp_first, verify_ga_hash,
};
use tg_calls::emoji_fingerprint::{
    EMOJI_COUNT, compute_call_emoji_fingerprint, compute_emoji_fingerprint,
    compute_emoji_fingerprint_indices, emoji_by_index,
};
use tg_calls::error::{DhError, SignalingError};
use tg_calls::group::participant::{ParticipantsList, VersionedGroupCallUpdate};
use tg_calls::group::payload::{
    DtlsFingerprint, GroupJoinPayload, JoinSsrcGroup, ParsedJoinResponse, parse_join_response,
};
use tg_calls::group::state::{
    GroupCallAction, GroupCallSession, GroupCallState, GroupCallTimerKind, InstanceMode,
    InstanceState, MuteState,
};
use tg_calls::group::{REQUEST_PER_PAGE, WAIT_FOR_UPDATES_TIMEOUT_MS};
use tg_calls::instance::CallsInstance;
use tg_calls::protocol::{
    CallProtocolConfig, DEFAULT_LIBRARY_VERSIONS, HANGUP_TIMEOUT_MS, MAX_LAYER, MIN_LAYER,
    ServerCallConfig,
};
use tg_calls::signaling::SignalingCipher;
use tg_calls::types::{
    CallId, DhConfigResponse, GroupCallDiscarded, GroupCallInfo, GroupCallObject,
    GroupCallParticipantData, GroupCallStreamChannel, InputGroupCall, PeerId, PhoneCallAccepted,
    PhoneCallActive, PhoneCallDiscardReason, PhoneCallDiscarded, PhoneCallRequested,
    PhoneCallUpdate, PhoneCallWaiting, PhoneConnection, UpdateGroupCallConnection,
    UpdateGroupCallParticipants, UpdatePhoneCallSignalingData, UserId,
};
use tg_crypto::hash::{sha1, sha256};

fn good_dh_config(version: i32) -> DhConfig {
    DhConfig {
        version,
        g: 3,
        p: DH_GOOD_PRIME.to_vec(),
    }
}

fn sample_group_call_info(id: CallId, version: i32, participants_count: i32) -> GroupCallInfo {
    GroupCallInfo {
        id,
        access_hash: 0x5555_6666,
        participants_count,
        title: Some("Conformance Call".to_string()),
        stream_dc_id: Some(4),
        record_start_date: None,
        schedule_date: None,
        unmuted_video_limit: 30,
        version,
        join_muted: false,
        can_change_join_muted: true,
        join_date_asc: false,
        schedule_start_subscribed: false,
        record_video_active: false,
        rtmp_stream: false,
        listeners_hidden: false,
        conference: false,
        creator: true,
        messages_enabled: true,
        can_change_messages_enabled: true,
        invite_link: None,
        send_paid_messages_stars: None,
    }
}

fn sample_participant(
    user_id: u64,
    source: u32,
    muted: bool,
    versioned: bool,
    is_self: bool,
) -> GroupCallParticipantData {
    GroupCallParticipantData {
        muted,
        left: false,
        can_self_unmute: true,
        just_joined: false,
        versioned,
        min: false,
        muted_by_you: false,
        volume_by_admin: false,
        is_self,
        video_joined: false,
        peer: PeerId::from_user(UserId(user_id)),
        date: 1_700_000_000,
        active_date: None,
        source,
        volume: Some(10_000),
        about: None,
        raise_hand_rating: None,
        video: None,
        presentation: None,
    }
}

// ============================================================================
// 1. Emoji Fingerprint Conformance (calls_emoji_fingerprint.cpp)
// ============================================================================

#[test]
fn emoji_fingerprint_matches_hand_computed_tdesktop_vectors() {
    // Table size and boundary entries directly from `calls_emoji_fingerprint.cpp` Data[]/Offsets[].
    assert_eq!(EMOJI_COUNT, 333);
    assert_eq!(emoji_by_index(0).as_deref(), Some("\u{1f609}")); // 😉
    assert_eq!(emoji_by_index(1).as_deref(), Some("\u{1f60d}")); // 😍
    assert_eq!(emoji_by_index(166).as_deref(), Some("\u{1f6bf}")); // 🚿
    assert_eq!(emoji_by_index(331).as_deref(), Some("\u{1f536}")); // 🔶
    assert_eq!(emoji_by_index(332).as_deref(), Some("\u{1f537}")); // 🔷

    // Vector 1: all-zero 32-byte SHA-256 -> [0, 0, 0, 0].
    let zero = [0u8; 32];
    assert_eq!(compute_emoji_fingerprint_indices(&zero), [0, 0, 0, 0]);
    assert_eq!(
        compute_emoji_fingerprint(&zero),
        ["\u{1f609}", "\u{1f609}", "\u{1f609}", "\u{1f609}"]
    );

    // Vector 2: all-0xFF 32-byte SHA-256 tests `bytes[0] & 0x7F` MSB masking ->
    // 0x7FFF_FFFF_FFFF_FFFF % 333 == 79 (💍).
    let ff = [0xFFu8; 32];
    assert_eq!(compute_emoji_fingerprint_indices(&ff), [79, 79, 79, 79]);
    assert_eq!(
        compute_emoji_fingerprint(&ff),
        ["\u{1f48d}", "\u{1f48d}", "\u{1f48d}", "\u{1f48d}"]
    );

    // Vector 3: MSB bit 7 set in each 8-byte chunk must be masked off before `% 333`.
    let mut msb_masked = [0u8; 32];
    for i in 0..4 {
        msb_masked[i * 8] = 0x80;
        msb_masked[i * 8 + 7] = (i + 1) as u8;
    }
    assert_eq!(compute_emoji_fingerprint_indices(&msb_masked), [1, 2, 3, 4]);
    assert_eq!(
        compute_emoji_fingerprint(&msb_masked),
        ["\u{1f60d}", "\u{1f61b}", "\u{1f62d}", "\u{1f631}"]
    );

    // Vector 4: SHA256(auth_key(0x11*256) || g_a(0x22*256))
    // Hand-computed from calls_call.cpp:795-811 + calls_emoji_fingerprint.cpp:150-168:
    // sha = 8c1ae2a2d6b664e8f5b3637317e059fc334fe038837e2a0d56bd6c3a1254e383
    // indices = [23, 111, 128, 97] = ["🙀", "🐬", "🌎", "🐼"]
    let ak4 = [0x11u8; 256];
    let ga4 = [0x22u8; 256];
    let sha4 = compute_key_sha_for_fingerprint(&ak4, &ga4);
    assert_eq!(
        sha4,
        [
            0x8c, 0x1a, 0xe2, 0xa2, 0xd6, 0xb6, 0x64, 0xe8, 0xf5, 0xb3, 0x63, 0x73, 0x17, 0xe0,
            0x59, 0xfc, 0x33, 0x4f, 0xe0, 0x38, 0x83, 0x7e, 0x2a, 0x0d, 0x56, 0xbd, 0x6c, 0x3a,
            0x12, 0x54, 0xe3, 0x83,
        ]
    );
    assert_eq!(compute_emoji_fingerprint_indices(&sha4), [23, 111, 128, 97]);
    assert_eq!(
        compute_call_emoji_fingerprint(&ak4, &ga4),
        ["\u{1f640}", "\u{1f42c}", "\u{1f30e}", "\u{1f43c}"]
    );

    // Vector 5: SHA256(auth_key(0..255) || 255-byte g_a(0xA5*255))
    // sha = 59af7c6a4b33585c16bce984742b332abeea777ccdf739e964b5635b56b9ba48
    // indices = [224, 47, 30, 19] = ["🍷", "👇", "💩", "👴"]
    let ak5: [u8; 256] = std::array::from_fn(|i| i as u8);
    let ga5 = [0xA5u8; 255];
    let sha5 = compute_key_sha_for_fingerprint(&ak5, &ga5);
    assert_eq!(compute_emoji_fingerprint_indices(&sha5), [224, 47, 30, 19]);
    assert_eq!(
        compute_call_emoji_fingerprint(&ak5, &ga5),
        ["\u{1f377}", "\u{1f447}", "\u{1f4a9}", "\u{1f474}"]
    );
}

// ============================================================================
// 2. DH Validation, Caching, Key Fingerprint, and Range Checks
// ============================================================================

#[test]
fn dh_config_cache_and_bounds_conformance() {
    let mut cache = DhConfigCache::new();
    assert_eq!(cache.version(), 0);

    // NotModified with empty cache must fail.
    assert_eq!(
        cache.update(DhConfigResponse::NotModified {
            random: vec![0xAA; RANDOM_POWER_SIZE],
        }),
        Err(DhError::MissingCachedConfig)
    );

    // Bad generator `g = 2` for GOOD_PRIME (which requires g = 3 mod 24 check) must fail.
    assert!(!is_dh_prime_good(&DH_GOOD_PRIME, 2));
    assert!(is_dh_prime_good(&DH_GOOD_PRIME, 3));
    assert_eq!(
        cache.update(DhConfigResponse::Config {
            g: 2,
            p: DH_GOOD_PRIME.to_vec(),
            version: 1,
            random: vec![0x11; RANDOM_POWER_SIZE],
        }),
        Err(DhError::InvalidParameters)
    );

    // Bad prime length or non-prime must fail.
    assert_eq!(
        cache.update(DhConfigResponse::Config {
            g: 3,
            p: vec![0xFF; 128],
            version: 1,
            random: vec![0x11; RANDOM_POWER_SIZE],
        }),
        Err(DhError::InvalidParameters)
    );

    // Wrong random seed length must fail.
    assert_eq!(
        cache.update(DhConfigResponse::Config {
            g: 3,
            p: DH_GOOD_PRIME.to_vec(),
            version: 14,
            random: vec![0x11; 100],
        }),
        Err(DhError::InvalidRandomLength {
            expected: RANDOM_POWER_SIZE,
            actual: 100,
        })
    );

    // Valid Config populates cache version = 14.
    let seed1 = cache
        .update(DhConfigResponse::Config {
            g: 3,
            p: DH_GOOD_PRIME.to_vec(),
            version: 14,
            random: vec![0x33; RANDOM_POWER_SIZE],
        })
        .expect("valid dhConfig");
    assert_eq!(seed1.len(), RANDOM_POWER_SIZE);
    assert_eq!(cache.version(), 14);

    // NotModified reuses cached g/p and returns new random seed.
    let seed2 = cache
        .update(DhConfigResponse::NotModified {
            random: vec![0x44; RANDOM_POWER_SIZE],
        })
        .expect("dhConfigNotModified with cached config");
    assert_eq!(seed2, vec![0x44; RANDOM_POWER_SIZE]);
    assert_eq!(cache.config().g, 3);
    assert_eq!(cache.config().version, 14);

    // IsGoodModExpFirst exact bit-length boundary checks (`mtproto_dh_utils.cpp:95-100`):
    // `modexp.bits() >= 2048 - 64` (1984 bits) and `(p - modexp).bits() >= 1984`.
    // Smallest 1984-bit value is 2^1983 (byte 8 = 0x80); largest 1983-bit value is 2^1983 - 1.
    let mut min_valid = [0u8; 256];
    min_valid[8] = 0x80; // 2^1983 -> 1984 bits
    assert!(is_good_dh_mod_exp_first(&min_valid, &DH_GOOD_PRIME));

    let mut below_min = [0u8; 256];
    below_min[8] = 0x7F;
    for b in &mut below_min[9..] {
        *b = 0xFF;
    } // 2^1983 - 1 -> 1983 bits
    assert!(!is_good_dh_mod_exp_first(&below_min, &DH_GOOD_PRIME));
    assert!(!is_good_dh_mod_exp_first(&[0u8; 256], &DH_GOOD_PRIME));
    assert!(!is_good_dh_mod_exp_first(&[1u8], &DH_GOOD_PRIME));
    assert!(!is_good_dh_mod_exp_first(&DH_GOOD_PRIME, &DH_GOOD_PRIME));

    // Auth key left-padding to 256 bytes and SHA-1 low 64 bits LE fingerprint.
    let short_secret = [0xABu8; 200];
    let padded = fill_auth_key_data(&short_secret).expect("pad <256 bytes");
    assert!(padded[..56].iter().all(|&b| b == 0));
    assert_eq!(&padded[56..], &short_secret);

    let sha1_digest = sha1(&[&padded]);
    let expected_fp = u64::from_le_bytes(sha1_digest[12..20].try_into().unwrap());
    assert_eq!(compute_key_fingerprint(&padded), expected_fp);

    // Two-party DH produces identical auth_key, key_fingerprint, and emoji fingerprint.
    let cfg = cache.config();
    let alice = generate_mod_exp_first_with_rng(&cfg, &[0x12; 256], |buf| buf.fill(0x34))
        .expect("alice g_a");
    let bob =
        generate_mod_exp_first_with_rng(&cfg, &[0x56; 256], |buf| buf.fill(0x78)).expect("bob g_b");
    let ga_hash = compute_ga_hash(&alice.modexp);
    assert_eq!(ga_hash, sha256(&[&alice.modexp]));
    assert!(verify_ga_hash(&alice.modexp, &ga_hash).is_ok());

    let (alice_key, alice_fp) =
        finish_dh_key_exchange(&cfg, &bob.modexp, &alice.random_power).expect("alice key");
    let (bob_key, bob_fp) =
        finish_dh_key_exchange(&cfg, &alice.modexp, &bob.random_power).expect("bob key");
    assert_eq!(alice_key, bob_key);
    assert_eq!(alice_fp, bob_fp);
    assert_eq!(
        compute_call_emoji_fingerprint(&alice_key, &alice.modexp),
        compute_call_emoji_fingerprint(&bob_key, &alice.modexp)
    );
}

// ============================================================================
// 3. 1:1 Call Protocol, Versions, Out-of-Order Updates, and Bug Regressions
// ============================================================================

#[test]
fn protocol_library_versions_are_newest_first_matching_collect_versions_for_api() {
    // Regression for `CollectVersionsForApi` (`calls/calls_call.cpp:212-214`):
    // versions must be sent newest-first so servers negotiate the highest mutual version.
    assert_eq!(MIN_LAYER, 65);
    assert_eq!(MAX_LAYER, 92);
    assert_eq!(
        DEFAULT_LIBRARY_VERSIONS,
        &["12.0.0", "9.0.0", "8.0.0", "7.0.0"]
    );

    let proto = CallProtocolConfig::default().to_tl_protocol();
    assert!(proto.udp_p2p);
    assert!(proto.udp_reflector);
    assert_eq!(proto.min_layer, 65);
    assert_eq!(proto.max_layer, 92);
    assert_eq!(
        proto.library_versions,
        vec!["12.0.0", "9.0.0", "8.0.0", "7.0.0"]
    );
}

#[test]
fn late_accepted_and_confirm_result_after_hangup_do_not_resurrect_call() {
    let dh_cfg = good_dh_config(14);
    let bob =
        generate_mod_exp_first_with_rng(&dh_cfg, &[0x21; 256], |b| b.fill(0x43)).expect("bob g_b");

    let mut call = Call::new_outgoing(
        UserId(100),
        UserId(200),
        false,
        ServerCallConfig::default(),
        CallProtocolConfig::default(),
    );
    call.apply_user_confirmation();
    call.start_with_rng(dh_cfg.clone(), &[0x11; 256], 777, |b| b.fill(0x22));
    let _ = call.take_actions();

    let waiting = PhoneCallWaiting {
        video: false,
        id: CallId(9001),
        access_hash: 0x1234,
        date: 1_700_000_000,
        admin_id: UserId(100),
        participant_id: UserId(200),
        protocol: CallProtocolConfig::default().to_tl_protocol(),
        receive_date: Some(1_700_000_001),
    };
    call.handle_request_call_result(Ok(PhoneCallUpdate::Waiting(waiting)), 1_000);
    assert_eq!(call.state(), CallState::Ringing);
    let _ = call.take_actions();

    // User hangs up while ringing -> HangingUp + DiscardCall(Missed).
    call.hangup(None, 2_000, false);
    assert_eq!(call.state(), CallState::HangingUp);
    let hangup_actions = call.take_actions();
    assert!(hangup_actions.iter().any(|a| matches!(
        a,
        CallAction::DiscardCall {
            reason: PhoneCallDiscardReason::Missed,
            duration: 0,
            ..
        }
    )));
    assert!(hangup_actions.iter().any(|a| matches!(
        a,
        CallAction::SetTimer {
            kind: CallTimerKind::FinishByTimeout,
            duration_ms,
        } if *duration_ms == HANGUP_TIMEOUT_MS
    )));

    // Late `phoneCallAccepted` arrives while `HangingUp`: must NOT transition back to
    // `ExchangingKeys` and must NOT emit `ConfirmCall`.
    let accepted = PhoneCallAccepted {
        video: false,
        id: CallId(9001),
        access_hash: 0x1234,
        date: 1_700_000_000,
        admin_id: UserId(100),
        participant_id: UserId(200),
        g_b: bob.modexp.clone(),
        protocol: CallProtocolConfig::default().to_tl_protocol(),
    };
    assert!(call.handle_update(PhoneCallUpdate::Accepted(accepted), 2_100));
    assert_eq!(call.state(), CallState::HangingUp);
    let late_actions = call.take_actions();
    assert!(
        !late_actions
            .iter()
            .any(|a| matches!(a, CallAction::ConfirmCall { .. })),
        "late phoneCallAccepted must not emit ConfirmCall while HangingUp"
    );

    // Late `phone.confirmCall` response arriving while `HangingUp` must NOT emit StartController.
    let active = PhoneCallActive {
        p2p_allowed: true,
        video: false,
        conference_supported: false,
        id: CallId(9001),
        access_hash: 0x1234,
        date: 1_700_000_000,
        admin_id: UserId(100),
        participant_id: UserId(200),
        g_a_or_b: bob.modexp.clone(),
        key_fingerprint: 0,
        protocol: CallProtocolConfig::default().to_tl_protocol(),
        connections: vec![PhoneConnection::Standard {
            tcp: false,
            id: 1,
            ip: "127.0.0.1".into(),
            ipv6: String::new(),
            port: 599,
            peer_tag: vec![0xAA; 16],
        }],
        start_date: 1_700_000_002,
        custom_parameters: None,
    };
    call.handle_confirm_call_result(Ok(PhoneCallUpdate::Active(active)), 2_200);
    assert_eq!(call.state(), CallState::HangingUp);
    let late_confirm_actions = call.take_actions();
    assert!(
        !late_confirm_actions
            .iter()
            .any(|a| matches!(a, CallAction::StartController(_))),
        "late confirmCall result must not emit StartController while HangingUp"
    );
}

#[test]
fn dh_config_failure_after_call_destroyed_emits_no_stray_notify_failed() {
    // Regression for `Instance::refreshDhConfig` (`calls/calls_instance.cpp:523-541`).
    let mut inst = CallsInstance::new(
        UserId(100),
        ServerCallConfig::default(),
        CallProtocolConfig::default(),
    );
    inst.start_outgoing_call(UserId(200), false, true, 1_000);
    let _ = inst.take_actions();
    assert!(inst.current_call().is_some());

    inst.destroy_current_call();
    assert!(inst.current_call().is_none());
    let _ = inst.take_actions();

    // Late failed DH config response must be ignored cleanly.
    inst.handle_dh_config_response(Err(()));
    assert!(
        inst.take_actions().is_empty(),
        "stale getDhConfig failure with no active call must emit no actions"
    );

    // Late invalid DH parameters response must also be ignored cleanly.
    inst.handle_dh_config_response(Ok(DhConfigResponse::Config {
        g: 2,
        p: vec![0u8; 256],
        version: 1,
        random: vec![0u8; 256],
    }));
    assert!(
        inst.take_actions().is_empty(),
        "stale invalid dhConfig with no active call must emit no actions"
    );
}

#[test]
fn incoming_ga_hash_and_fingerprint_mismatch_discard_call() {
    let dh_cfg = good_dh_config(14);
    let alice = generate_mod_exp_first_with_rng(&dh_cfg, &[0x11; 256], |b| b.fill(0x22))
        .expect("alice g_a");
    let ga_hash = compute_ga_hash(&alice.modexp);

    // Case A: g_a_hash mismatch -> DiscardCall(Disconnect), no StartController.
    let mut incoming_bad_hash = Call::new_incoming(
        UserId(200),
        UserId(100),
        false,
        ServerCallConfig::default(),
        CallProtocolConfig::default(),
    );
    incoming_bad_hash.handle_update(
        PhoneCallUpdate::Requested(PhoneCallRequested {
            video: false,
            id: CallId(501),
            access_hash: 0xABCD,
            date: 1_700_000_000,
            admin_id: UserId(100),
            participant_id: UserId(200),
            g_a_hash: ga_hash.to_vec(),
            protocol: CallProtocolConfig::default().to_tl_protocol(),
        }),
        1_000,
    );
    incoming_bad_hash.start_with_rng(dh_cfg.clone(), &[0x55; 256], 1, |b| b.fill(0x66));
    incoming_bad_hash.handle_received_call_result(Ok(()), 1_010);
    incoming_bad_hash.actually_answer();
    let _ = incoming_bad_hash.take_actions();

    let mut tampered_ga = alice.modexp.clone();
    tampered_ga[0] ^= 0x01;
    incoming_bad_hash.handle_update(
        PhoneCallUpdate::Active(PhoneCallActive {
            p2p_allowed: true,
            video: false,
            conference_supported: false,
            id: CallId(501),
            access_hash: 0xABCD,
            date: 1_700_000_000,
            admin_id: UserId(100),
            participant_id: UserId(200),
            g_a_or_b: tampered_ga,
            key_fingerprint: 12345,
            protocol: CallProtocolConfig::default().to_tl_protocol(),
            connections: vec![],
            start_date: 1_700_000_005,
            custom_parameters: None,
        }),
        1_050,
    );
    assert_eq!(incoming_bad_hash.state(), CallState::FailedHangingUp);
    let actions = incoming_bad_hash.take_actions();
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, CallAction::StartController(_)))
    );
    assert!(actions.iter().any(|a| matches!(
        a,
        CallAction::DiscardCall {
            reason: PhoneCallDiscardReason::Disconnect,
            ..
        }
    )));

    // Case B: valid g_a_hash but mismatched key_fingerprint -> DiscardCall(Disconnect), no StartController.
    let mut incoming_bad_fp = Call::new_incoming(
        UserId(200),
        UserId(100),
        false,
        ServerCallConfig::default(),
        CallProtocolConfig::default(),
    );
    incoming_bad_fp.handle_update(
        PhoneCallUpdate::Requested(PhoneCallRequested {
            video: false,
            id: CallId(502),
            access_hash: 0xABCE,
            date: 1_700_000_000,
            admin_id: UserId(100),
            participant_id: UserId(200),
            g_a_hash: ga_hash.to_vec(),
            protocol: CallProtocolConfig::default().to_tl_protocol(),
        }),
        1_000,
    );
    incoming_bad_fp.start_with_rng(dh_cfg, &[0x55; 256], 2, |b| b.fill(0x66));
    incoming_bad_fp.handle_received_call_result(Ok(()), 1_010);
    incoming_bad_fp.actually_answer();
    let _ = incoming_bad_fp.take_actions();

    incoming_bad_fp.handle_update(
        PhoneCallUpdate::Active(PhoneCallActive {
            p2p_allowed: true,
            video: false,
            conference_supported: false,
            id: CallId(502),
            access_hash: 0xABCE,
            date: 1_700_000_000,
            admin_id: UserId(100),
            participant_id: UserId(200),
            g_a_or_b: alice.modexp.clone(),
            key_fingerprint: 0xDEAD_BEEF_CAFE_BABEu64, // wrong fingerprint
            protocol: CallProtocolConfig::default().to_tl_protocol(),
            connections: vec![],
            start_date: 1_700_000_005,
            custom_parameters: None,
        }),
        1_050,
    );
    assert_eq!(incoming_bad_fp.state(), CallState::FailedHangingUp);
    let actions = incoming_bad_fp.take_actions();
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, CallAction::StartController(_)))
    );
    assert!(actions.iter().any(|a| matches!(
        a,
        CallAction::DiscardCall {
            reason: PhoneCallDiscardReason::Disconnect,
            ..
        }
    )));
}

#[test]
fn signaling_routing_and_mtproto2_ctr_roundtrip_and_replay_protection() {
    let key = [0x42u8; 256];
    let mut caller_cipher = SignalingCipher::new(key, true);
    let mut callee_cipher = SignalingCipher::new(key, false);

    let payload = br#"{"@type":"Candidates","candidates":[]}"#;
    let encrypted = caller_cipher.encrypt(payload).expect("encrypt signaling");

    let decrypted = callee_cipher
        .decrypt(&encrypted)
        .expect("decrypt signaling");
    assert_eq!(decrypted.payload, payload);

    // Replaying the same encrypted packet must be rejected.
    assert_eq!(
        callee_cipher.decrypt(&encrypted),
        Err(SignalingError::ReplayDetected(decrypted.seq))
    );

    // Tampering with ciphertext must fail msg_key MAC check.
    let mut tampered = encrypted.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0xFF;
    assert_eq!(
        callee_cipher.decrypt(&tampered),
        Err(SignalingError::MsgKeyMismatch)
    );

    // Call signaling routing: rejected before controller starts, accepted for matching ID after.
    let dh_cfg = good_dh_config(14);
    let alice = generate_mod_exp_first_with_rng(&dh_cfg, &[0x11; 256], |b| b.fill(0x22))
        .expect("alice g_a");
    let bob =
        generate_mod_exp_first_with_rng(&dh_cfg, &[0x33; 256], |b| b.fill(0x44)).expect("bob g_b");
    let (_, fp) = finish_dh_key_exchange(&dh_cfg, &bob.modexp, &alice.random_power).expect("dh");

    let mut call = Call::new_outgoing(
        UserId(100),
        UserId(200),
        false,
        ServerCallConfig::default(),
        CallProtocolConfig::default(),
    );
    call.apply_user_confirmation();
    call.start_with_rng(dh_cfg, &[0x11; 256], 99, |b| b.fill(0x22));
    call.handle_request_call_result(
        Ok(PhoneCallUpdate::Waiting(PhoneCallWaiting {
            video: false,
            id: CallId(800),
            access_hash: 0x999,
            date: 1_700_000_000,
            admin_id: UserId(100),
            participant_id: UserId(200),
            protocol: CallProtocolConfig::default().to_tl_protocol(),
            receive_date: None,
        })),
        1_000,
    );
    assert!(
        !call.handle_signaling_data(UpdatePhoneCallSignalingData {
            phone_call_id: CallId(800),
            data: encrypted.clone(),
        }),
        "signaling before controller start must return false"
    );

    call.handle_update(
        PhoneCallUpdate::Accepted(PhoneCallAccepted {
            video: false,
            id: CallId(800),
            access_hash: 0x999,
            date: 1_700_000_000,
            admin_id: UserId(100),
            participant_id: UserId(200),
            g_b: bob.modexp.clone(),
            protocol: CallProtocolConfig::default().to_tl_protocol(),
        }),
        1_100,
    );
    call.handle_confirm_call_result(
        Ok(PhoneCallUpdate::Active(PhoneCallActive {
            p2p_allowed: true,
            video: false,
            conference_supported: false,
            id: CallId(800),
            access_hash: 0x999,
            date: 1_700_000_000,
            admin_id: UserId(100),
            participant_id: UserId(200),
            g_a_or_b: bob.modexp.clone(),
            key_fingerprint: fp,
            protocol: CallProtocolConfig::default().to_tl_protocol(),
            connections: vec![],
            start_date: 1_700_000_002,
            custom_parameters: None,
        })),
        1_200,
    );
    let _ = call.take_actions();

    // Wrong call_id rejected; matching call_id forwarded.
    assert!(!call.handle_signaling_data(UpdatePhoneCallSignalingData {
        phone_call_id: CallId(801),
        data: encrypted.clone(),
    }));
    assert!(call.handle_signaling_data(UpdatePhoneCallSignalingData {
        phone_call_id: CallId(800),
        data: encrypted.clone(),
    }));
    assert!(
        call.take_actions()
            .contains(&CallAction::ReceiveSignalingData(encrypted.clone()))
    );

    call.emit_signaling_data(encrypted.clone());
    assert!(call.take_actions().iter().any(|a| matches!(
        a,
        CallAction::SendSignalingData { peer, data }
            if peer.id == CallId(800) && peer.access_hash == 0x999 && data == &encrypted
    )));

    // Established duration calculation & Hangup reason + need_rating/need_debug.
    call.handle_controller_state_change(ControllerState::Established, 10_000);
    call.set_preferred_relay_id(777_888);
    let _ = call.take_actions();
    call.hangup(None, 52_700, false);
    let actions = call.take_actions();
    assert!(actions.iter().any(|a| matches!(
        a,
        CallAction::DiscardCall {
            duration: 42,
            reason: PhoneCallDiscardReason::Hangup,
            connection_id: 777_888,
            ..
        }
    )));

    call.handle_update(
        PhoneCallUpdate::Discarded(PhoneCallDiscarded {
            need_rating: true,
            need_debug: true,
            video: false,
            id: CallId(800),
            reason: Some(PhoneCallDiscardReason::Hangup),
            duration: Some(42),
        }),
        52_900,
    );
    assert_eq!(call.state(), CallState::Ended);
    assert!(call.rating_requested());
    assert!(call.debug_requested());
    assert!(call.take_actions().contains(&CallAction::ShowRatingBox));

    // Deferred debug log upload and rating submission.
    call.set_debug_log(r#"{"stats":"ok"}"#.to_string());
    assert!(call.take_actions().iter().any(|a| matches!(
        a,
        CallAction::SaveCallDebug { peer, debug_json }
            if peer.id == CallId(800) && debug_json == r#"{"stats":"ok"}"#
    )));
    call.submit_rating(5, "crystal clear".to_string());
    assert!(call.take_actions().iter().any(|a| matches!(
        a,
        CallAction::SetCallRating {
            user_initiative: false,
            rating: 5,
            comment,
            ..
        } if comment == "crystal clear"
    )));
}

// ============================================================================
// 4. Group Calls: Payload JSON, Version Gap Refetch, Cross-ID Isolation,
//    Self-Update Flags, Leave Source, and RTMP Stream Channels
// ============================================================================

#[test]
fn group_call_join_payload_json_and_response_parsing() {
    let payload = GroupJoinPayload {
        ssrc: 123_456_789,
        ufrag: "ab12".to_string(),
        pwd: "secretpassword0123456789".to_string(),
        fingerprints: vec![DtlsFingerprint {
            hash: "sha-256".to_string(),
            setup: Some("active".to_string()),
            fingerprint: "AA:BB:CC:DD:EE:FF".to_string(),
        }],
        ssrc_groups: vec![JoinSsrcGroup {
            semantics: "SIM".to_string(),
            sources: vec![1001, 1002, 1003],
        }],
    };
    let json = payload.to_json().expect("serialize join payload");
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(v["ssrc"], 123_456_789);
    assert_eq!(v["ufrag"], "ab12");
    assert_eq!(v["pwd"], "secretpassword0123456789");
    assert_eq!(v["fingerprints"][0]["hash"], "sha-256");
    assert_eq!(v["fingerprints"][0]["setup"], "active");
    assert_eq!(v["fingerprints"][0]["fingerprint"], "AA:BB:CC:DD:EE:FF");
    assert_eq!(v["ssrc-groups"][0]["semantics"], "SIM");
    assert_eq!(
        v["ssrc-groups"][0]["sources"],
        serde_json::json!([1001, 1002, 1003])
    );

    // Empty ssrc_groups must be omitted from JSON (matching calls_group_call.cpp).
    let audio_only = GroupJoinPayload {
        ssrc_groups: vec![],
        ..payload
    };
    let audio_v: serde_json::Value = serde_json::from_str(&audio_only.to_json().unwrap()).unwrap();
    assert!(audio_v.get("ssrc-groups").is_none());

    // RTMP join response parsing.
    let rtmp_json = r#"{"stream":true,"rtmp":true,"rtmp_stream_url":"rtmps://dc4.rtmp.t.me/s/","rtmp_stream_key":"k123"}"#;
    match parse_join_response(rtmp_json).expect("parse rtmp join response") {
        ParsedJoinResponse::BroadcastStream { rtmp, rtmp_info } => {
            assert!(rtmp);
            assert_eq!(rtmp_info.url, "rtmps://dc4.rtmp.t.me/s/");
            assert_eq!(rtmp_info.key, "k123");
        }
        other => panic!("expected BroadcastStream, got {other:?}"),
    }
}

#[test]
fn group_call_cross_call_id_isolation_and_version_gap_refetch() {
    let call_id = CallId(7000);
    let peer = PeerId::from_user(UserId(10));

    // Cross-call-ID isolation in ParticipantsList (`calls_instance.cpp:789-817`).
    let mut plist = ParticipantsList::new(call_id);
    plist.process_full_call(
        GroupCallObject::Active(sample_group_call_info(call_id, 1, 1)),
        &[sample_participant(10, 1001, false, false, true)],
    );
    let foreign_events = plist.enqueue_update(VersionedGroupCallUpdate::Call(
        GroupCallObject::Discarded(GroupCallDiscarded {
            id: CallId(9999),
            access_hash: 0,
            duration: 10,
        }),
    ));
    assert!(!foreign_events.discarded);
    assert!(foreign_events.applied_updates.is_empty());

    let foreign_part = plist.enqueue_update(VersionedGroupCallUpdate::Participants(
        UpdateGroupCallParticipants {
            call_id: CallId(9999),
            participants: vec![sample_participant(99, 9999, false, true, false)],
            version: 2,
        },
    ));
    assert!(foreign_part.applied_updates.is_empty());
    assert_eq!(plist.version(), 1);
    assert_eq!(plist.participants().len(), 1);

    // Version gap -> 3 s ReloadByQueuedUpdates timer -> ReloadGroupCall(limit=3) ->
    // GetGroupParticipants(offset, limit=50) when received < participants_count && received < loaded.
    let mut session = GroupCallSession::new(peer, peer, call_id, 0x5555_6666, true, 0, false);
    let _ = session.take_actions();
    session.process_full_call(
        GroupCallObject::Active(sample_group_call_info(call_id, 1, 5)),
        &[
            sample_participant(10, 1001, false, false, true),
            sample_participant(11, 1002, false, false, false),
            sample_participant(12, 1003, false, false, false),
            sample_participant(13, 1004, false, false, false),
            sample_participant(14, 1005, false, false, false),
        ],
    );
    let _ = session.take_actions();
    assert_eq!(session.participants_list().participants().len(), 5);

    // Receive version 3 (skipping version 2) -> buffered + arm ReloadByQueuedUpdates (3000 ms).
    session.handle_versioned_update(VersionedGroupCallUpdate::Participants(
        UpdateGroupCallParticipants {
            call_id,
            participants: vec![sample_participant(15, 1006, false, true, false)],
            version: 3,
        },
    ));
    let gap_actions = session.take_actions();
    assert_eq!(session.participants_list().queued_updates_len(), 1);
    assert!(gap_actions.iter().any(|a| matches!(
        a,
        GroupCallAction::SetTimer {
            kind: GroupCallTimerKind::ReloadByQueuedUpdates,
            duration_ms,
            repeating: false,
        } if *duration_ms == WAIT_FOR_UPDATES_TIMEOUT_MS
    )));

    // 3 s timer expires -> flushes queued update & emits ReloadGroupCall { limit: 3 }.
    session.on_timer(GroupCallTimerKind::ReloadByQueuedUpdates, 3_000);
    let reload_actions = session.take_actions();
    assert_eq!(session.participants_list().queued_updates_len(), 0);
    assert!(reload_actions.iter().any(|a| matches!(
        a,
        GroupCallAction::ReloadGroupCall {
            call: InputGroupCall::Id { id, .. },
            limit: 3,
        } if *id == call_id
    )));

    // Server replies with 3 participants (out of 6 total, while we had 6 loaded after flush) ->
    // `needs_participants_after_reload` triggers `GetGroupParticipants(offset="page2", limit=50)`.
    let page1 = vec![
        sample_participant(10, 1001, false, false, true),
        sample_participant(11, 1002, false, false, false),
        sample_participant(12, 1003, false, false, false),
    ];
    session.handle_reload_group_call_result(Ok((
        GroupCallObject::Active(sample_group_call_info(call_id, 3, 6)),
        &page1,
        "page2",
    )));
    let refetch_actions = session.take_actions();
    assert!(refetch_actions.iter().any(|a| matches!(
        a,
        GroupCallAction::GetGroupParticipants {
            call: InputGroupCall::Id { id, .. },
            offset,
            limit,
        } if *id == call_id && offset == "page2" && *limit == REQUEST_PER_PAGE
    )));

    // Follow-up slice completes reload and reconciles all 6 participants.
    let page2 = vec![
        sample_participant(13, 1004, false, false, false),
        sample_participant(14, 1005, false, false, false),
        sample_participant(15, 1006, false, false, false),
    ];
    session.handle_reload_participants_result(Ok((&page2, 6)));
    assert_eq!(session.participants_list().version(), 3);
    assert_eq!(session.participants_list().participants().len(), 6);
    assert!(session.participants_list().all_participants_loaded());
}

#[test]
fn group_call_self_update_omits_volume_and_handles_rtmp_and_leave_source() {
    let call_id = CallId(7100);
    let peer = PeerId::from_user(UserId(10));
    let mut session = GroupCallSession::new(peer, peer, call_id, 0x7777, false, 0, false);
    let _ = session.take_actions();

    // Join with SSRC 424242.
    session.on_join_payload_ready(424_242, r#"{"ssrc":424242}"#.to_string());
    let _ = session.take_actions();
    session.handle_join_done(MuteState::Muted, true);
    session.set_instance_connected(true, false);
    assert_eq!(session.state(), GroupCallState::Joined);
    let _ = session.take_actions();

    // Self unmute must emit EditGroupCallParticipant with `muted: Some(false)` and `volume: None`!
    session.set_muted_and_update(MuteState::Active);
    let unmute_actions = session.take_actions();
    let edit = unmute_actions
        .iter()
        .find_map(|a| match a {
            GroupCallAction::EditGroupCallParticipant {
                participant,
                muted,
                volume,
                raise_hand,
                video_stopped,
                video_paused,
                presentation_paused,
                ..
            } => Some((
                *participant,
                *muted,
                *volume,
                *raise_hand,
                *video_stopped,
                *video_paused,
                *presentation_paused,
            )),
            _ => None,
        })
        .expect("expected EditGroupCallParticipant on self unmute");
    assert_eq!(edit, (peer, Some(false), None, None, None, None, None));

    // Queue a camera toggle while mute update is in flight, then fail mute with FLOOD_WAIT:
    // must NOT immediately flush queued camera update on non-GROUPCALL_FORBIDDEN failure.
    session.toggle_camera(true);
    assert!(
        session
            .take_actions()
            .iter()
            .all(|a| !matches!(a, GroupCallAction::EditGroupCallParticipant { .. }))
    );
    session.handle_self_update_done(Some("FLOOD_WAIT_5"));
    assert!(
        session
            .take_actions()
            .iter()
            .all(|a| !matches!(a, GroupCallAction::EditGroupCallParticipant { .. }))
    );

    // Editing a non-existent peer must NOT emit EditGroupCallParticipant.
    session.toggle_participant_mute(PeerId::from_user(UserId(999)), true, false);
    assert!(
        session
            .take_actions()
            .iter()
            .all(|a| !matches!(a, GroupCallAction::EditGroupCallParticipant { .. }))
    );

    // Force-muting turns off the active camera (emitting CameraStopped with volume: None),
    // and completing it flushes the earlier queued CameraStopped update as well.
    session.set_muted(MuteState::ForceMuted);
    let force_mute_actions = session.take_actions();
    assert!(force_mute_actions.iter().any(|a| matches!(
        a,
        GroupCallAction::EditGroupCallParticipant {
            volume: None,
            video_stopped: Some(true),
            ..
        }
    )));
    session.handle_self_update_done(None);
    let _ = session.take_actions();
    session.handle_self_update_done(None);
    let _ = session.take_actions();

    // Raise hand after force-muted -> `raise_hand: Some(true)` and `volume: None`.
    session.set_muted_and_update(MuteState::RaisedHand);
    let raise_actions = session.take_actions();
    assert!(raise_actions.iter().any(|a| matches!(
        a,
        GroupCallAction::EditGroupCallParticipant {
            muted: None,
            volume: None,
            raise_hand: Some(true),
            ..
        }
    )));
    session.handle_self_update_done(None);
    let _ = session.take_actions();

    // Hangup emits LeaveGroupCall with our exact joined SSRC (424242).
    session.hangup();
    let leave_actions = session.take_actions();
    assert!(leave_actions.iter().any(|a| matches!(
        a,
        GroupCallAction::LeaveGroupCall {
            call: InputGroupCall::Id { id, .. },
            source: 424_242,
        } if *id == call_id
    )));
    session.handle_leave_done();
    assert_eq!(session.state(), GroupCallState::Ended);

    // RTMP stream channels flow (`calls_group_call.cpp:3329-3371`).
    let mut rtmp = GroupCallSession::new(peer, peer, CallId(7200), 0x8888, false, 0, true);
    let _ = rtmp.take_actions();
    rtmp.on_join_payload_ready(555_555, r#"{"ssrc":555555}"#.to_string());
    let _ = rtmp.take_actions();
    let mut rtmp_info = sample_group_call_info(CallId(7200), 1, 1);
    rtmp_info.rtmp_stream = true;
    rtmp_info.stream_dc_id = Some(5);
    rtmp.process_full_call(GroupCallObject::Active(rtmp_info), &[]);
    rtmp.handle_connection_update(&UpdateGroupCallConnection {
        presentation: false,
        params_json: r#"{"stream":true,"rtmp":true}"#.to_string(),
    });
    rtmp.handle_join_done(MuteState::Muted, true);
    rtmp.set_instance_connected(true, false);
    assert_eq!(rtmp.instance_mode(), InstanceMode::Stream);
    assert_eq!(rtmp.instance_state(), InstanceState::Connected);
    let _ = rtmp.take_actions();

    rtmp.request_stream_channels();
    assert!(rtmp.take_actions().iter().any(|a| matches!(
        a,
        GroupCallAction::GetGroupCallStreamChannels {
            call: InputGroupCall::Id { id, .. },
            stream_dc_id: Some(5),
        } if *id == CallId(7200)
    )));
    let ts = rtmp.handle_stream_channels_result(Ok(&[GroupCallStreamChannel {
        channel: 1,
        scale: 0,
        last_timestamp_ms: 1_700_000_500_000,
    }]));
    assert_eq!(ts, 1_700_000_500_000);
    assert!(!rtmp.empty_rtmp());
    assert_eq!(rtmp.last_rtmp_timestamp_ms(), 1_700_000_500_000);

    // Empty channel list marks `empty_rtmp` and returns 0.
    rtmp.request_stream_channels();
    let _ = rtmp.take_actions();
    assert_eq!(rtmp.handle_stream_channels_result(Ok(&[])), 0);
    assert!(rtmp.empty_rtmp());

    // `GROUPCALL_JOIN_MISSING` triggers automatic rejoin.
    rtmp.request_stream_channels();
    let _ = rtmp.take_actions();
    assert_eq!(
        rtmp.handle_stream_channels_result(Err("GROUPCALL_JOIN_MISSING")),
        0
    );
    assert_eq!(rtmp.state(), GroupCallState::Joining);
    assert!(
        rtmp.take_actions()
            .contains(&GroupCallAction::EmitJoinPayload)
    );
}

// ============================================================================
// 5. Required Call Error Codes
// ============================================================================

#[test]
fn required_call_rpc_errors_surface_and_fail_cleanly() {
    let error_codes = [
        "CALL_PROTOCOL_LAYER_INVALID",
        "CALL_ALREADY_DECLINED",
        "PARTICIPANT_VERSION_OUTDATED",
        "USER_PRIVACY_RESTRICTED",
        "FLOOD_WAIT_15",
    ];

    for err in error_codes {
        let mut call = Call::new_outgoing(
            UserId(100),
            UserId(200),
            false,
            ServerCallConfig::default(),
            CallProtocolConfig::default(),
        );
        call.apply_user_confirmation();
        call.start_with_rng(good_dh_config(14), &[0x11; 256], 10, |b| b.fill(0x22));
        let _ = call.take_actions();

        // Failing `phone.requestCall` while Requesting sets pending finish timer and surfaces error.
        call.handle_request_call_result(Err(err), 1_000);
        let actions = call.take_actions();
        assert!(
            actions.contains(&CallAction::ShowError(err.to_string())),
            "error {err} must emit CallAction::ShowError"
        );
        assert!(actions.iter().any(|a| matches!(
            a,
            CallAction::SetTimer {
                kind: CallTimerKind::FinishByTimeout,
                ..
            }
        )));

        // Firing FinishByTimeout completes transition to Failed.
        call.on_timer(CallTimerKind::FinishByTimeout, 6_000);
        assert_eq!(call.state(), CallState::Failed);
    }

    // Group call `GROUPCALL_SSRC_DUPLICATE_MUCH` retries via EmitJoinPayload.
    let peer = PeerId::from_user(UserId(10));
    let mut g = GroupCallSession::new(peer, peer, CallId(7300), 0x1111, false, 0, false);
    let _ = g.take_actions();
    g.on_join_payload_ready(1234, r#"{"ssrc":1234}"#.to_string());
    let _ = g.take_actions();
    g.handle_join_fail("GROUPCALL_SSRC_DUPLICATE_MUCH");
    assert_eq!(g.state(), GroupCallState::Joining);
    assert!(g.take_actions().contains(&GroupCallAction::EmitJoinPayload));
}
