use tg_calls::call::{Call, CallAction, CallSound, CallState, CallTimerKind, ControllerState};
use tg_calls::dh::{DH_GOOD_PRIME as GOOD_PRIME, DhConfig, RANDOM_POWER_SIZE};
use tg_calls::instance::{CallsInstance, InstanceAction};
use tg_calls::protocol::{
    CallProtocolConfig, DEFAULT_VERSION, EndpointType, HANGUP_TIMEOUT_MS, ServerCallConfig,
    collect_endpoints_and_servers,
};
use tg_calls::signal_bars::{SIGNAL_BAR_FINISHED, SIGNAL_BAR_STARTING, SignalBarsState};
use tg_calls::types::{
    CallId, DhConfigResponse, InputGroupCall, InputPhoneCall, PhoneCallAccepted, PhoneCallActive,
    PhoneCallDiscardReason, PhoneCallDiscarded, PhoneCallProtocol, PhoneCallRequested,
    PhoneCallUpdate, PhoneCallWaiting, PhoneConnection, UpdatePhoneCallSignalingData, UserId,
};

fn test_dh_config() -> DhConfig {
    DhConfig {
        version: 1,
        g: 3,
        p: GOOD_PRIME.to_vec(),
    }
}

fn test_protocol() -> PhoneCallProtocol {
    CallProtocolConfig::default().to_tl_protocol()
}

#[test]
fn full_1on1_outgoing_and_incoming_handshake_and_hangup() {
    let alice_id = UserId(100);
    let bob_id = UserId(200);
    let server_cfg = ServerCallConfig::default();
    let proto_cfg = CallProtocolConfig::default();
    let dh = test_dh_config();

    // 1. Alice creates outgoing call -> WaitingUserConfirmation -> confirms -> Requesting.
    let mut alice = Call::new_outgoing(alice_id, bob_id, false, server_cfg, proto_cfg.clone());
    assert_eq!(alice.state(), CallState::WaitingUserConfirmation);
    alice.take_actions();

    alice.apply_user_confirmation();
    assert_eq!(alice.state(), CallState::Requesting);
    alice.take_actions();

    // 2. Alice starts DH and emits phone.requestCall with g_a_hash.
    alice.start_with_rng(dh.clone(), &[0x11; RANDOM_POWER_SIZE], 777, |b| {
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(7).wrapping_add(1);
        }
    });
    let alice_actions = alice.take_actions();
    let g_a_hash = alice_actions
        .iter()
        .find_map(|a| match a {
            CallAction::RequestCall { g_a_hash, .. } => Some(*g_a_hash),
            _ => None,
        })
        .expect("Alice must emit RequestCall");

    // 3. Server responds with phoneCallWaiting (receive_date = None).
    alice.handle_request_call_result(
        Ok(PhoneCallUpdate::Waiting(PhoneCallWaiting {
            video: false,
            id: CallId(9001),
            access_hash: 5555,
            date: 1_700_000_000,
            admin_id: alice_id,
            participant_id: bob_id,
            protocol: test_protocol(),
            receive_date: None,
        })),
        1000,
    );
    assert_eq!(alice.state(), CallState::Waiting);
    let actions = alice.take_actions();
    assert!(actions.contains(&CallAction::SetTimer {
        kind: CallTimerKind::DiscardByTimeout,
        duration_ms: server_cfg.call_receive_timeout_ms,
    }));

    // 4. Bob receives incoming phoneCallRequested.
    let mut bob = Call::new_incoming(bob_id, alice_id, false, server_cfg, proto_cfg);
    assert_eq!(bob.state(), CallState::Starting);
    assert!(bob.is_incoming_waiting());
    let bob_init_actions = bob.take_actions();
    assert!(bob_init_actions.contains(&CallAction::SetTimer {
        kind: CallTimerKind::DiscardByTimeout,
        duration_ms: server_cfg.call_ring_timeout_ms,
    }));
    assert!(bob_init_actions.contains(&CallAction::StartWaitingTrack { outgoing: false }));

    assert!(bob.handle_update(
        PhoneCallUpdate::Requested(PhoneCallRequested {
            video: false,
            id: CallId(9001),
            access_hash: 5555,
            date: 1_700_000_000,
            admin_id: alice_id,
            participant_id: bob_id,
            g_a_hash: g_a_hash.to_vec(),
            protocol: test_protocol(),
        }),
        1100,
    ));

    // 5. Bob runs DH and sends phone.receivedCall -> WaitingIncoming.
    bob.start_with_rng(dh, &[0x22; RANDOM_POWER_SIZE], 888, |b| {
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(11).wrapping_add(3);
        }
    });
    assert!(bob.take_actions().contains(&CallAction::ReceivedCall {
        peer: InputPhoneCall {
            id: CallId(9001),
            access_hash: 5555,
        },
    }));
    bob.handle_received_call_result(Ok(()), 1200);
    assert_eq!(bob.state(), CallState::WaitingIncoming);

    // 6. Alice receives phoneCallWaiting with receive_date -> Ringing + ring timeout.
    alice.handle_update(
        PhoneCallUpdate::Waiting(PhoneCallWaiting {
            video: false,
            id: CallId(9001),
            access_hash: 5555,
            date: 1_700_000_000,
            admin_id: alice_id,
            participant_id: bob_id,
            protocol: test_protocol(),
            receive_date: Some(1_700_000_001),
        }),
        1300,
    );
    assert_eq!(alice.state(), CallState::Ringing);
    let actions = alice.take_actions();
    assert!(actions.contains(&CallAction::SetTimer {
        kind: CallTimerKind::DiscardByTimeout,
        duration_ms: server_cfg.call_ring_timeout_ms,
    }));
    assert!(actions.contains(&CallAction::StartWaitingTrack { outgoing: true }));

    // 7. Bob answers -> ExchangingKeys + phone.acceptCall(g_b).
    bob.actually_answer();
    assert_eq!(bob.state(), CallState::ExchangingKeys);
    let g_b = bob
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            CallAction::AcceptCall { g_b, .. } => Some(g_b),
            _ => None,
        })
        .expect("Bob must emit AcceptCall");

    // 8. Alice receives phoneCallAccepted(g_b) -> ExchangingKeys + phone.confirmCall(g_a, fp).
    alice.handle_update(
        PhoneCallUpdate::Accepted(PhoneCallAccepted {
            video: false,
            id: CallId(9001),
            access_hash: 5555,
            date: 1_700_000_000,
            admin_id: alice_id,
            participant_id: bob_id,
            g_b,
            protocol: test_protocol(),
        }),
        1400,
    );
    assert_eq!(alice.state(), CallState::ExchangingKeys);
    let (g_a, key_fp) = alice
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            CallAction::ConfirmCall {
                g_a,
                key_fingerprint,
                ..
            } => Some((g_a, key_fingerprint)),
            _ => None,
        })
        .expect("Alice must emit ConfirmCall");

    let active_call = PhoneCallActive {
        p2p_allowed: true,
        video: false,
        conference_supported: true,
        id: CallId(9001),
        access_hash: 5555,
        date: 1_700_000_000,
        admin_id: alice_id,
        participant_id: bob_id,
        g_a_or_b: g_a,
        key_fingerprint: key_fp,
        protocol: PhoneCallProtocol {
            udp_p2p: true,
            udp_reflector: true,
            min_layer: 65,
            max_layer: 92,
            library_versions: vec!["11.0.0".to_string()],
        },
        connections: vec![PhoneConnection::Standard {
            tcp: false,
            id: 42,
            ip: "149.154.175.1".to_string(),
            ipv6: "::1".to_string(),
            port: 599,
            peer_tag: vec![0xAA; 16],
        }],
        start_date: 1_700_000_002,
        custom_parameters: None,
    };

    // 9. Alice gets phoneCall from confirmCall response; Bob gets phoneCall via update.
    alice.handle_confirm_call_result(Ok(PhoneCallUpdate::Active(active_call.clone())), 1500);
    bob.handle_update(PhoneCallUpdate::Active(active_call), 1500);

    let alice_desc = alice
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            CallAction::StartController(d) => Some(d),
            _ => None,
        })
        .expect("Alice starts controller");
    let bob_desc = bob
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            CallAction::StartController(d) => Some(d),
            _ => None,
        })
        .expect("Bob starts controller");

    assert_eq!(alice_desc.encryption_key, bob_desc.encryption_key);
    assert_eq!(alice_desc.version, "11.0.0");
    assert!(alice_desc.is_outgoing);
    assert!(!bob_desc.is_outgoing);
    assert_eq!(alice.emoji_fingerprint(), bob.emoji_fingerprint());
    assert!(alice.conference_supported());

    // 10. Controller transitions: WaitInit -> WaitInitAck -> Established.
    alice.handle_controller_state_change(ControllerState::WaitInit, 1600);
    assert_eq!(alice.state(), CallState::WaitingInit);
    alice.handle_controller_state_change(ControllerState::WaitInitAck, 1700);
    assert_eq!(alice.state(), CallState::WaitingInitAck);
    alice.handle_controller_state_change(ControllerState::Established, 2000);
    assert_eq!(alice.state(), CallState::Established);

    // Signaling forwarding works once controller is active.
    assert!(alice.handle_signaling_data(UpdatePhoneCallSignalingData {
        phone_call_id: CallId(9001),
        data: vec![1, 2, 3],
    }));
    assert!(
        alice
            .take_actions()
            .contains(&CallAction::ReceiveSignalingData(vec![1, 2, 3]))
    );

    // 11. Alice hangs up after 10 seconds -> HangingUp + DiscardCall(Hangup, duration=10).
    alice.set_preferred_relay_id(42);
    alice.hangup(None, 12_000, false);
    assert_eq!(alice.state(), CallState::HangingUp);
    let hangup_actions = alice.take_actions();
    assert!(hangup_actions.contains(&CallAction::DiscardCall {
        video: false,
        peer: InputPhoneCall {
            id: CallId(9001),
            access_hash: 5555,
        },
        duration: 10,
        reason: PhoneCallDiscardReason::Hangup,
        connection_id: 42,
    }));
    assert!(hangup_actions.contains(&CallAction::SetTimer {
        kind: CallTimerKind::FinishByTimeout,
        duration_ms: HANGUP_TIMEOUT_MS,
    }));

    alice.handle_discard_call_result(12_100);
    assert_eq!(alice.state(), CallState::Ended);
}

#[test]
fn early_answer_waits_for_dh_config_before_sending_accept_call() {
    let mut bob = Call::new_incoming(
        UserId(2),
        UserId(1),
        false,
        ServerCallConfig::default(),
        CallProtocolConfig::default(),
    );
    bob.handle_update(
        PhoneCallUpdate::Requested(PhoneCallRequested {
            video: false,
            id: CallId(10),
            access_hash: 20,
            date: 100,
            admin_id: UserId(1),
            participant_id: UserId(2),
            g_a_hash: vec![0xCC; 32],
            protocol: test_protocol(),
        }),
        0,
    );
    bob.take_actions();

    // User clicks Answer before messages.getDhConfig has returned.
    bob.actually_answer();
    assert_eq!(bob.state(), CallState::ExchangingKeys);
    let actions = bob.take_actions();
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, CallAction::AcceptCall { .. }))
    );

    // Now DH config arrives -> automatically triggers AcceptCall.
    bob.start_with_rng(test_dh_config(), &[0x44; RANDOM_POWER_SIZE], 1, |b| {
        b.fill(0x55)
    });
    assert!(
        bob.take_actions()
            .iter()
            .any(|a| matches!(a, CallAction::AcceptCall { .. }))
    );
}

#[test]
fn discard_reasons_busy_redial_and_migration_security_guard() {
    let server_cfg = ServerCallConfig::default();
    let proto_cfg = CallProtocolConfig::default();

    // Outgoing hangup while Waiting -> Missed.
    let mut call = Call::new_outgoing(UserId(1), UserId(2), false, server_cfg, proto_cfg.clone());
    call.apply_user_confirmation();
    call.start_with_rng(test_dh_config(), &[0x11; 256], 1, |b| b.fill(0x22));
    call.handle_request_call_result(
        Ok(PhoneCallUpdate::Waiting(PhoneCallWaiting {
            video: false,
            id: CallId(50),
            access_hash: 60,
            date: 100,
            admin_id: UserId(1),
            participant_id: UserId(2),
            protocol: test_protocol(),
            receive_date: None,
        })),
        10,
    );
    call.take_actions();
    call.hangup(None, 20, false);
    assert!(call.take_actions().contains(&CallAction::DiscardCall {
        video: false,
        peer: InputPhoneCall {
            id: CallId(50),
            access_hash: 60,
        },
        duration: 0,
        reason: PhoneCallDiscardReason::Missed,
        connection_id: 0,
    }));

    // Busy -> redial() transitions back to Requesting.
    let mut call2 = Call::new_outgoing(UserId(1), UserId(2), false, server_cfg, proto_cfg.clone());
    call2.apply_user_confirmation();
    call2.start_with_rng(test_dh_config(), &[0x11; 256], 2, |b| b.fill(0x33));
    call2.handle_request_call_result(
        Ok(PhoneCallUpdate::Waiting(PhoneCallWaiting {
            video: false,
            id: CallId(51),
            access_hash: 61,
            date: 100,
            admin_id: UserId(1),
            participant_id: UserId(2),
            protocol: test_protocol(),
            receive_date: None,
        })),
        10,
    );
    call2.take_actions();
    call2.handle_update(
        PhoneCallUpdate::Discarded(PhoneCallDiscarded {
            need_rating: false,
            need_debug: false,
            video: false,
            id: CallId(51),
            reason: Some(PhoneCallDiscardReason::Busy),
            duration: None,
        }),
        30,
    );
    assert_eq!(call2.state(), CallState::Busy);
    let busy_actions = call2.take_actions();
    assert!(busy_actions.contains(&CallAction::PlaySound(CallSound::Busy)));
    assert!(busy_actions.contains(&CallAction::CancelTimer(CallTimerKind::DiscardByTimeout)));

    call2.redial();
    assert_eq!(call2.state(), CallState::Requesting);
    assert!(call2.take_actions().contains(&CallAction::NotifyRedial));

    // Security guard: unaccepted incoming call MUST NOT migrate to conference call!
    let mut unaccepted =
        Call::new_incoming(UserId(2), UserId(1), false, server_cfg, proto_cfg.clone());
    unaccepted.handle_update(
        PhoneCallUpdate::Requested(PhoneCallRequested {
            video: false,
            id: CallId(77),
            access_hash: 88,
            date: 100,
            admin_id: UserId(1),
            participant_id: UserId(2),
            g_a_hash: vec![0xAA; 32],
            protocol: test_protocol(),
        }),
        0,
    );
    unaccepted.take_actions();
    unaccepted.handle_update(
        PhoneCallUpdate::Discarded(PhoneCallDiscarded {
            need_rating: false,
            need_debug: false,
            video: false,
            id: CallId(77),
            reason: Some(PhoneCallDiscardReason::MigrateConferenceCall {
                slug: "secret-slug".to_string(),
            }),
            duration: None,
        }),
        10,
    );
    assert_eq!(unaccepted.state(), CallState::EndedByOtherDevice);
    assert!(
        !unaccepted
            .take_actions()
            .iter()
            .any(|a| matches!(a, CallAction::GetGroupCall { .. }))
    );

    // Once accepted (ExchangingKeys), migration IS followed.
    let mut accepted = Call::new_incoming(UserId(2), UserId(1), false, server_cfg, proto_cfg);
    accepted.handle_update(
        PhoneCallUpdate::Requested(PhoneCallRequested {
            video: false,
            id: CallId(78),
            access_hash: 89,
            date: 100,
            admin_id: UserId(1),
            participant_id: UserId(2),
            g_a_hash: vec![0xAA; 32],
            protocol: test_protocol(),
        }),
        0,
    );
    accepted.actually_answer();
    accepted.take_actions();
    accepted.handle_update(
        PhoneCallUpdate::Discarded(PhoneCallDiscarded {
            need_rating: true,
            need_debug: true,
            video: false,
            id: CallId(78),
            reason: Some(PhoneCallDiscardReason::MigrateConferenceCall {
                slug: "valid-slug".to_string(),
            }),
            duration: Some(45),
        }),
        10,
    );
    assert_eq!(accepted.state(), CallState::MigrationHangingUp);
    assert_eq!(accepted.discarded_duration(), 45);
    assert!(accepted.take_actions().contains(&CallAction::GetGroupCall {
        call: InputGroupCall::Slug {
            slug: "valid-slug".to_string(),
        },
        limit: 5,
    }));
}

#[test]
fn tampered_ga_hash_and_terminal_failed_state_stickiness() {
    let mut bob = Call::new_incoming(
        UserId(2),
        UserId(1),
        false,
        ServerCallConfig::default(),
        CallProtocolConfig::default(),
    );
    bob.handle_update(
        PhoneCallUpdate::Requested(PhoneCallRequested {
            video: false,
            id: CallId(123),
            access_hash: 456,
            date: 100,
            admin_id: UserId(1),
            participant_id: UserId(2),
            g_a_hash: vec![0xFF; 32], // does not match g_a below
            protocol: test_protocol(),
        }),
        0,
    );
    bob.start_with_rng(test_dh_config(), &[0x11; 256], 1, |b| b.fill(0x22));
    bob.actually_answer();
    bob.take_actions();

    bob.handle_update(
        PhoneCallUpdate::Active(PhoneCallActive {
            p2p_allowed: false,
            video: false,
            conference_supported: false,
            id: CallId(123),
            access_hash: 456,
            date: 100,
            admin_id: UserId(1),
            participant_id: UserId(2),
            g_a_or_b: vec![0x01; 256],
            key_fingerprint: 999,
            protocol: PhoneCallProtocol {
                udp_p2p: true,
                udp_reflector: true,
                min_layer: 65,
                max_layer: 92,
                library_versions: vec![],
            },
            connections: vec![],
            start_date: 101,
            custom_parameters: None,
        }),
        50,
    );
    assert_eq!(bob.state(), CallState::FailedHangingUp);
    // Timeout forces Failed.
    bob.on_timer(CallTimerKind::FinishByTimeout, 5_050);
    assert_eq!(bob.state(), CallState::Failed);

    // Sticky: cannot transition out of Failed even if Discarded arrives later.
    bob.handle_update(
        PhoneCallUpdate::Discarded(PhoneCallDiscarded {
            need_rating: false,
            need_debug: false,
            video: false,
            id: CallId(123),
            reason: Some(PhoneCallDiscardReason::Hangup),
            duration: None,
        }),
        6_000,
    );
    assert_eq!(bob.state(), CallState::Failed);
}

#[test]
fn calls_instance_rejects_busy_ignores_stale_and_deduplicates_incoming() {
    let mut inst = CallsInstance::new(
        UserId(2),
        ServerCallConfig::default(),
        CallProtocolConfig::default(),
    );

    let make_req = |id: u64, date| {
        PhoneCallUpdate::Requested(PhoneCallRequested {
            video: true,
            id: CallId(id),
            access_hash: (id as i64) * 10,
            date,
            admin_id: UserId(1),
            participant_id: UserId(2),
            g_a_hash: vec![0x11; 32],
            protocol: test_protocol(),
        })
    };

    // Too old (> 90s ring timeout) is silently ignored.
    inst.handle_call_update(make_req(1, 1000), 1091, 5000);
    assert!(inst.current_call().is_none());
    assert!(inst.take_actions().is_empty());

    // Fresh call creates incoming call and asks for call config + DH config.
    inst.handle_call_update(make_req(2, 1050), 1091, 5000);
    assert!(inst.in_call());
    let actions = inst.take_actions();
    assert!(actions.contains(&InstanceAction::GetCallConfig));
    assert!(actions.contains(&InstanceAction::GetDhConfig {
        version: 0,
        random_length: 256,
    }));

    // Duplicate phoneCallRequested with same id is ignored.
    inst.handle_call_update(make_req(2, 1050), 1092, 6000);
    assert!(inst.take_actions().is_empty());

    // Second concurrent incoming call gets immediately rejected as Busy.
    inst.handle_call_update(make_req(3, 1090), 1092, 6000);
    assert_eq!(
        inst.take_actions(),
        vec![InstanceAction::DiscardIncomingBusy {
            video: true,
            peer: InputPhoneCall {
                id: CallId(3),
                access_hash: 30,
            },
        }]
    );

    // Valid DH config starts the incoming call.
    inst.handle_dh_config_response_with_rng(
        Ok(DhConfigResponse::Config {
            g: 3,
            p: GOOD_PRIME.to_vec(),
            version: 1,
            random: vec![0x55; 256],
        }),
        1,
        |b| b.fill(0x66),
    );
    assert!(
        inst.take_actions()
            .contains(&InstanceAction::Call(CallAction::ReceivedCall {
                peer: InputPhoneCall {
                    id: CallId(2),
                    access_hash: 20,
                },
            }))
    );
}

#[test]
fn signal_bars_and_endpoint_server_conversion() {
    let mut bars = SignalBarsState::new();
    assert_eq!(bars.raw_count(), SIGNAL_BAR_STARTING);
    assert!(bars.is_starting());
    assert!(!bars.is_bar_active(0));

    assert!(bars.update(3));
    assert!(bars.is_bar_active(0));
    assert!(bars.is_bar_active(2));
    assert!(!bars.is_bar_active(3));
    assert!(!bars.update(3)); // unchanged

    assert!(bars.update(SIGNAL_BAR_FINISHED));
    assert!(bars.is_finished());
    // Further updates after Finished are ignored.
    assert!(!bars.update(4));
    assert_eq!(bars.raw_count(), SIGNAL_BAR_FINISHED);

    // Endpoint / RTC server conversion matches calls_call.cpp:77-186.
    let conns = vec![
        PhoneConnection::Standard {
            tcp: false,
            id: 200,
            ip: "1.2.3.4".to_string(),
            ipv6: "2001:db8::1".to_string(),
            port: 599,
            peer_tag: vec![
                0x01, 0xAB, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
                0x0E, 0x0F,
            ],
        },
        PhoneConnection::Standard {
            tcp: true,
            id: 100,
            ip: "5.6.7.8".to_string(),
            ipv6: "".to_string(),
            port: 443,
            peer_tag: vec![0xFF; 16],
        },
        PhoneConnection::Webrtc {
            turn: true,
            stun: true,
            id: 300,
            ip: "9.9.9.9".to_string(),
            ipv6: "".to_string(),
            port: 3478,
            username: "u".to_string(),
            password: "p".to_string(),
        },
    ];
    let (endpoints, servers) = collect_endpoints_and_servers(&conns);
    // Only non-TCP 16-byte peer_tag standard connection becomes a legacy UDP relay endpoint.
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0].endpoint_id, 200);
    assert_eq!(endpoints[0].kind, EndpointType::UdpRelay);

    // IDs sorted: {100 -> rank 1, 200 -> rank 2}.
    assert_eq!(servers[0].id, 2);
    assert_eq!(servers[0].login, "reflector");
    assert_eq!(servers[0].password, "01ab02030405060708090a0b0c0d0e0f");
    assert!(servers[0].is_turn);
    assert!(!servers[0].is_tcp);

    assert_eq!(servers[2].id, 1);
    assert!(servers[2].is_tcp);

    // WebRTC STUN skips empty ipv6, TURN adds ipv4 + ipv6.
    let stun_servers: Vec<_> = servers.iter().filter(|s| !s.is_turn).collect();
    assert_eq!(stun_servers.len(), 1);
    assert_eq!(stun_servers[0].host, "9.9.9.9");
    assert_eq!(DEFAULT_VERSION, "2.4.4");
}
