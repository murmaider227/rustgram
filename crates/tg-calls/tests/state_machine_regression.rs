//! Regression tests verifying `tg-calls` 1:1 and group call state machines against
//! Telegram Desktop (`calls_call.cpp`, `calls_instance.cpp`, `calls_group_call.cpp`):
//! - ring, receive, connect, packet, and hangup timeouts and timer cancellations;
//! - discard reasons (`Missed`, `Busy`, `Hangup`, `Disconnect`, `MigrateConferenceCall`);
//! - `need_rating` / `need_debug` submission flags and deferred debug-log upload;
//! - repeated group call join/leave cycles and in-flight hangup / discard / duplicate leave edge cases.

use tg_calls::call::{Call, CallAction, CallState, CallTimerKind, ControllerState};
use tg_calls::dh::{DH_GOOD_PRIME, DhConfig};
use tg_calls::group::state::{
    GroupCallAction, GroupCallSession, GroupCallState, GroupCallTimerKind, MuteState,
};
use tg_calls::group::{CHECK_JOINED_TIMEOUT_MS, DEFAULT_VOLUME};
use tg_calls::instance::CallsInstance;
use tg_calls::protocol::{CallProtocolConfig, HANGUP_TIMEOUT_MS, ServerCallConfig};
use tg_calls::types::{
    CallId, ChannelId, GroupCallDiscarded, GroupCallInfo, GroupCallObject,
    GroupCallParticipantData, InputPhoneCall, PeerId, PhoneCallAccepted, PhoneCallActive,
    PhoneCallDiscardReason, PhoneCallDiscarded, PhoneCallProtocol, PhoneCallRequested,
    PhoneCallUpdate, PhoneCallWaiting, UpdateGroupCallConnection, UserId,
};

fn test_dh_config() -> DhConfig {
    DhConfig {
        version: 1,
        g: 3,
        p: DH_GOOD_PRIME.to_vec(),
    }
}

fn test_protocol() -> PhoneCallProtocol {
    CallProtocolConfig::default().to_tl_protocol()
}

fn make_group_info(id: u64, version: i32) -> GroupCallInfo {
    GroupCallInfo {
        id: CallId(id),
        access_hash: 9001,
        participants_count: 1,
        title: Some("Sync".into()),
        stream_dc_id: Some(2),
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

fn make_participant(peer: PeerId, ssrc: u32, is_self: bool) -> GroupCallParticipantData {
    GroupCallParticipantData {
        muted: true,
        left: false,
        can_self_unmute: true,
        just_joined: false,
        versioned: false,
        min: false,
        muted_by_you: false,
        volume_by_admin: false,
        is_self,
        video_joined: false,
        peer,
        date: 1_700_000_000,
        active_date: None,
        source: ssrc,
        volume: Some(DEFAULT_VOLUME),
        about: None,
        raise_hand_rating: None,
        video: None,
        presentation: None,
    }
}

#[test]
fn one_on_one_timeouts_discard_reasons_and_rating_debug_flags() {
    let cfg = ServerCallConfig::default();
    assert_eq!(cfg.call_receive_timeout_ms, 20_000);
    assert_eq!(cfg.call_ring_timeout_ms, 90_000);
    assert_eq!(cfg.call_connect_timeout_ms, 30_000);
    assert_eq!(cfg.call_packet_timeout_ms, 10_000);
    assert_eq!(HANGUP_TIMEOUT_MS, 5_000);

    let proto = CallProtocolConfig::default();

    // 1. Outgoing receive timeout (Waiting) -> DiscardCall(Missed) + CancelTimer(DiscardByTimeout).
    let mut out_wait = Call::new_outgoing(UserId(1), UserId(2), false, cfg, proto.clone());
    out_wait.apply_user_confirmation();
    out_wait.start_with_rng(test_dh_config(), &[0x11; 256], 1, |b| b.fill(0x22));
    out_wait.handle_request_call_result(
        Ok(PhoneCallUpdate::Waiting(PhoneCallWaiting {
            video: false,
            id: CallId(100),
            access_hash: 200,
            date: 1000,
            admin_id: UserId(1),
            participant_id: UserId(2),
            protocol: test_protocol(),
            receive_date: None,
        })),
        1_000,
    );
    let wait_actions = out_wait.take_actions();
    assert!(wait_actions.contains(&CallAction::SetTimer {
        kind: CallTimerKind::DiscardByTimeout,
        duration_ms: 20_000,
    }));
    out_wait.on_timer(CallTimerKind::DiscardByTimeout, 21_000);
    assert_eq!(out_wait.state(), CallState::HangingUp);
    let timeout_actions = out_wait.take_actions();
    assert!(timeout_actions.contains(&CallAction::CancelTimer(CallTimerKind::DiscardByTimeout)));
    assert!(timeout_actions.contains(&CallAction::DiscardCall {
        video: false,
        peer: InputPhoneCall {
            id: CallId(100),
            access_hash: 200,
        },
        duration: 0,
        reason: PhoneCallDiscardReason::Missed,
        connection_id: 0,
    }));
    out_wait.handle_discard_call_result(21_100);
    assert_eq!(out_wait.state(), CallState::Ended);
    assert!(
        out_wait
            .take_actions()
            .contains(&CallAction::CancelTimer(CallTimerKind::FinishByTimeout))
    );

    // 2. Incoming ring timeout -> DiscardCall(Busy) (declined, matching calls_call.cpp:722-729).
    let mut inc_ring = Call::new_incoming(UserId(2), UserId(1), false, cfg, proto.clone());
    let inc_init = inc_ring.take_actions();
    assert!(inc_init.contains(&CallAction::SetTimer {
        kind: CallTimerKind::DiscardByTimeout,
        duration_ms: 90_000,
    }));
    inc_ring.handle_update(
        PhoneCallUpdate::Requested(PhoneCallRequested {
            video: false,
            id: CallId(101),
            access_hash: 201,
            date: 1000,
            admin_id: UserId(1),
            participant_id: UserId(2),
            g_a_hash: vec![0xAA; 32],
            protocol: test_protocol(),
        }),
        1_000,
    );
    inc_ring.take_actions();
    inc_ring.on_timer(CallTimerKind::DiscardByTimeout, 91_000);
    assert_eq!(inc_ring.state(), CallState::HangingUp);
    assert!(inc_ring.take_actions().contains(&CallAction::DiscardCall {
        video: false,
        peer: InputPhoneCall {
            id: CallId(101),
            access_hash: 201,
        },
        duration: 0,
        reason: PhoneCallDiscardReason::Busy,
        connection_id: 0,
    }));

    // 3. Connect timeout / media ERROR_TIMEOUT -> Disconnect + FailedHangingUp -> Failed,
    //    plus deferred `need_debug` upload and `need_rating` panel flow (without double submit).
    let mut alice = Call::new_outgoing(UserId(1), UserId(2), false, cfg, proto.clone());
    let mut bob = Call::new_incoming(UserId(2), UserId(1), false, cfg, proto.clone());
    alice.apply_user_confirmation();
    alice.start_with_rng(test_dh_config(), &[0x11; 256], 7, |b| b.fill(0x31));
    let g_a_hash = alice
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            CallAction::RequestCall { g_a_hash, .. } => Some(g_a_hash),
            _ => None,
        })
        .unwrap();
    alice.handle_request_call_result(
        Ok(PhoneCallUpdate::Waiting(PhoneCallWaiting {
            video: false,
            id: CallId(500),
            access_hash: 600,
            date: 1000,
            admin_id: UserId(1),
            participant_id: UserId(2),
            protocol: test_protocol(),
            receive_date: Some(1001),
        })),
        1_000,
    );
    bob.handle_update(
        PhoneCallUpdate::Requested(PhoneCallRequested {
            video: false,
            id: CallId(500),
            access_hash: 600,
            date: 1000,
            admin_id: UserId(1),
            participant_id: UserId(2),
            g_a_hash: g_a_hash.to_vec(),
            protocol: test_protocol(),
        }),
        1_000,
    );
    bob.start_with_rng(test_dh_config(), &[0x22; 256], 8, |b| b.fill(0x41));
    bob.handle_received_call_result(Ok(()), 1_050);
    bob.take_actions();
    bob.actually_answer();
    let g_b = bob
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            CallAction::AcceptCall { g_b, .. } => Some(g_b),
            _ => None,
        })
        .unwrap();
    alice.take_actions();
    alice.handle_update(
        PhoneCallUpdate::Accepted(PhoneCallAccepted {
            video: false,
            id: CallId(500),
            access_hash: 600,
            date: 1000,
            admin_id: UserId(1),
            participant_id: UserId(2),
            g_b,
            protocol: test_protocol(),
        }),
        1_100,
    );
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
        .unwrap();
    let active = PhoneCallActive {
        p2p_allowed: true,
        video: false,
        conference_supported: false,
        id: CallId(500),
        access_hash: 600,
        date: 1000,
        admin_id: UserId(1),
        participant_id: UserId(2),
        g_a_or_b: g_a,
        key_fingerprint: key_fp,
        protocol: test_protocol(),
        connections: vec![],
        start_date: 1002,
        custom_parameters: None,
    };
    alice.handle_confirm_call_result(Ok(PhoneCallUpdate::Active(active.clone())), 1_200);
    bob.handle_update(PhoneCallUpdate::Active(active), 1_200);
    let alice_desc = alice
        .take_actions()
        .into_iter()
        .find_map(|a| match a {
            CallAction::StartController(d) => Some(d),
            _ => None,
        })
        .unwrap();
    assert_eq!(alice_desc.initialization_timeout_ms, 30_000);
    assert_eq!(alice_desc.receive_timeout_ms, 10_000);
    bob.take_actions();

    // Bob's media controller reports ERROR_TIMEOUT -> Disconnect + FailedHangingUp (no ShowError).
    bob.handle_controller_state_change(ControllerState::Failed("ERROR_TIMEOUT".into()), 31_200);
    assert_eq!(bob.state(), CallState::FailedHangingUp);
    let bob_fail_actions = bob.take_actions();
    assert!(
        !bob_fail_actions
            .iter()
            .any(|a| matches!(a, CallAction::ShowError(_))),
        "ERROR_TIMEOUT must not emit ShowError (matching calls_call.cpp:1682-1696)"
    );
    assert!(bob_fail_actions.contains(&CallAction::DiscardCall {
        video: false,
        peer: InputPhoneCall {
            id: CallId(500),
            access_hash: 600,
        },
        duration: 0,
        reason: PhoneCallDiscardReason::Disconnect,
        connection_id: 0,
    }));

    // Alice reaches Established, then receives phoneCallDiscarded with BOTH need_debug & need_rating
    // BEFORE `set_debug_log` is populated (e.g. caller stops engine after DestroyController).
    alice.handle_controller_state_change(ControllerState::Established, 1_500);
    alice.take_actions();
    alice.handle_update(
        PhoneCallUpdate::Discarded(PhoneCallDiscarded {
            need_rating: true,
            need_debug: true,
            video: false,
            id: CallId(500),
            reason: Some(PhoneCallDiscardReason::Hangup),
            duration: Some(12),
        }),
        13_500,
    );
    assert_eq!(alice.state(), CallState::Ended);
    assert!(
        alice.rating_requested(),
        "rating_requested() must remain true after Discarded so take_rating_to_panel() works"
    );
    assert!(
        alice.debug_requested(),
        "debug_requested() must be true while waiting for deferred set_debug_log()"
    );
    let disc_actions = alice.take_actions();
    assert!(disc_actions.contains(&CallAction::ShowRatingBox));
    assert!(disc_actions.contains(&CallAction::DestroyController));
    assert!(
        !disc_actions
            .iter()
            .any(|a| matches!(a, CallAction::SaveCallDebug { .. })),
        "must not upload empty debug JSON"
    );

    // Caller stops media engine and supplies debug log -> automatically emits SaveCallDebug once!
    alice.set_debug_log(r#"{"lib":"tg-voip"}"#.to_string());
    assert!(!alice.debug_requested());
    assert_eq!(
        alice.take_actions(),
        vec![CallAction::SaveCallDebug {
            peer: InputPhoneCall {
                id: CallId(500),
                access_hash: 600,
            },
            debug_json: r#"{"lib":"tg-voip"}"#.to_string(),
        }]
    );
    // Calling set_debug_log again must not double-upload.
    alice.set_debug_log(r#"{"lib":"tg-voip","v":2}"#.to_string());
    assert!(alice.take_actions().is_empty());

    // `take_rating_to_panel()` now works, and `submit_rating` + `finish_rating` sends only ONE
    // `SetCallRating` with `user_initiative: false` (matching calls_call.cpp:662, 695).
    alice.take_rating_to_panel();
    assert!(alice.rating_in_panel());
    alice.set_rating(4);
    alice.submit_rating(5, "clear audio".to_string());
    let rate_actions = alice.take_actions();
    assert_eq!(
        rate_actions,
        vec![CallAction::SetCallRating {
            user_initiative: false,
            peer: InputPhoneCall {
                id: CallId(500),
                access_hash: 600,
            },
            rating: 5,
            comment: "clear audio".to_string(),
        }]
    );
    alice.finish_rating();
    let finish_rate_actions = alice.take_actions();
    assert_eq!(finish_rate_actions, vec![CallAction::NotifyFinished]);

    // Terminal `Ended` state is sticky: late updates cannot revert it to Busy or Established.
    alice.handle_controller_state_change(ControllerState::Established, 14_000);
    alice.handle_update(
        PhoneCallUpdate::Discarded(PhoneCallDiscarded {
            need_rating: true,
            need_debug: true,
            video: false,
            id: CallId(500),
            reason: Some(PhoneCallDiscardReason::Busy),
            duration: Some(12),
        }),
        14_100,
    );
    assert_eq!(alice.state(), CallState::Ended);
    assert!(
        alice.take_actions().is_empty(),
        "duplicate Discarded on Ended call must not re-emit ShowRatingBox or SaveCallDebug"
    );

    // 4. `CallsInstance::is_quit_prevent` returns false when unconfirmed call ends immediately.
    let mut inst = CallsInstance::new(UserId(1), cfg, proto);
    assert!(inst.start_outgoing_call(UserId(2), false, false, 100));
    inst.take_actions();
    assert!(
        !inst.is_quit_prevent(200),
        "unconfirmed call with id == 0 ends immediately and must not block app quit"
    );
    assert!(inst.current_call().is_none());
}

#[test]
fn group_call_join_leave_cycles_and_edge_cases() {
    let chat = PeerId::from_channel(ChannelId(42));
    let me = PeerId::from_user(UserId(10));

    // 1. 20 full join -> connect -> joined -> hangup -> leave_done -> ended cycles.
    for cycle in 0..20u64 {
        let call_id = CallId(1_000 + cycle);
        let ssrc = 10_000 + cycle as u32;
        let mut session = GroupCallSession::new(chat, me, call_id, 777, true, 0, false);
        assert_eq!(session.state(), GroupCallState::Joining);
        let a0 = session.take_actions();
        assert!(a0.contains(&GroupCallAction::EmitJoinPayload));

        session.on_join_payload_ready(ssrc, format!(r#"{{"ssrc":{ssrc}}}"#));
        let a1 = session.take_actions();
        assert!(
            a1.iter()
                .any(|a| matches!(a, GroupCallAction::JoinGroupCall { muted: true, .. }))
        );

        session.handle_join_done(MuteState::Muted, true);
        assert_eq!(session.state(), GroupCallState::Connecting);
        let a2 = session.take_actions();
        assert!(a2.contains(&GroupCallAction::SetTimer {
            kind: GroupCallTimerKind::CheckJoined,
            duration_ms: CHECK_JOINED_TIMEOUT_MS,
            repeating: false,
        }));

        session.handle_connection_update(&UpdateGroupCallConnection {
            presentation: false,
            params_json: r#"{"transport":{"ufrag":"u","pwd":"p"}}"#.into(),
        });
        session.set_instance_connected(true, false);
        assert_eq!(session.state(), GroupCallState::Joined);
        let a3 = session.take_actions();
        assert!(
            a3.contains(&GroupCallAction::CancelTimer(
                GroupCallTimerKind::CheckJoined
            )),
            "entering Joined must cancel CheckJoined timer"
        );

        // Hangup -> HangingUp -> LeaveGroupCall(ssrc) -> handle_leave_done -> Ended.
        session.hangup();
        assert_eq!(session.state(), GroupCallState::HangingUp);
        let a4 = session.take_actions();
        assert!(a4.iter().any(|a| matches!(
            a,
            GroupCallAction::LeaveGroupCall { source, .. } if *source == ssrc
        )));

        session.handle_leave_done();
        assert_eq!(session.state(), GroupCallState::Ended);
        let a5 = session.take_actions();
        assert!(a5.contains(&GroupCallAction::NotifyFinished));

        // Duplicate handle_leave_done must NOT corrupt Ended -> Failed!
        session.handle_leave_done();
        assert_eq!(session.state(), GroupCallState::Ended);
        assert!(session.take_actions().is_empty());
    }

    // 2. Hangup while `phone.joinGroupCall` is in flight followed by `handle_join_fail`
    //    must finish cleanly to `Ended` instead of getting stuck in `HangingUp`.
    let mut s_fail = GroupCallSession::new(chat, me, CallId(2001), 123, false, 0, false);
    s_fail.take_actions();
    s_fail.on_join_payload_ready(5555, r#"{"ssrc":5555}"#.into());
    s_fail.take_actions();
    s_fail.hangup();
    assert_eq!(s_fail.state(), GroupCallState::HangingUp);
    s_fail.take_actions();
    s_fail.handle_join_fail("NETWORK_ERROR");
    assert_eq!(s_fail.state(), GroupCallState::Ended);
    assert!(
        s_fail
            .take_actions()
            .contains(&GroupCallAction::NotifyFinished)
    );

    // 3. Hangup while `phone.joinGroupCall` is in flight followed by `handle_join_done`
    //    must immediately issue `LeaveGroupCall` for the newly assigned SSRC.
    let mut s_race = GroupCallSession::new(chat, me, CallId(2002), 124, false, 0, false);
    s_race.take_actions();
    s_race.on_join_payload_ready(6666, r#"{"ssrc":6666}"#.into());
    s_race.take_actions();
    s_race.hangup();
    assert_eq!(s_race.state(), GroupCallState::HangingUp);
    s_race.take_actions();
    s_race.handle_join_done(MuteState::Muted, true);
    assert_eq!(s_race.state(), GroupCallState::HangingUp);
    assert!(
        s_race
            .take_actions()
            .iter()
            .any(|a| matches!(a, GroupCallAction::LeaveGroupCall { source: 6666, .. }))
    );
    s_race.handle_leave_done();
    assert_eq!(s_race.state(), GroupCallState::Ended);

    // 4. `process_full_call` returning `GroupCallObject::Discarded` ends the session cleanly.
    let mut s_disc = GroupCallSession::new(chat, me, CallId(2003), 125, false, 0, false);
    s_disc.on_join_payload_ready(7777, r#"{"ssrc":7777}"#.into());
    s_disc.handle_join_done(MuteState::Muted, true);
    s_disc.set_instance_connected(true, false);
    s_disc.process_full_call(
        GroupCallObject::Active(make_group_info(2003, 1)),
        &[make_participant(me, 7777, true)],
    );
    s_disc.take_actions();
    s_disc.process_full_call(
        GroupCallObject::Discarded(GroupCallDiscarded {
            id: CallId(2003),
            access_hash: 125,
            duration: 60,
        }),
        &[],
    );
    assert_eq!(s_disc.state(), GroupCallState::Ended);
    assert!(
        s_disc
            .take_actions()
            .contains(&GroupCallAction::NotifyFinished)
    );

    // 5. Admin `discard()` -> `DiscardGroupCall` -> `handle_discard_done()` -> `LeaveGroupCall` -> `Ended`.
    let mut s_admin = GroupCallSession::new(chat, me, CallId(2004), 126, true, 0, false);
    s_admin.on_join_payload_ready(8888, r#"{"ssrc":8888}"#.into());
    s_admin.handle_join_done(MuteState::Muted, true);
    s_admin.set_instance_connected(true, false);
    s_admin.take_actions();
    s_admin.discard();
    assert!(
        s_admin
            .take_actions()
            .iter()
            .any(|a| matches!(a, GroupCallAction::DiscardGroupCall { .. }))
    );
    s_admin.handle_discard_done();
    assert_eq!(s_admin.state(), GroupCallState::HangingUp);
    s_admin.handle_leave_done();
    assert_eq!(s_admin.state(), GroupCallState::Ended);
}
