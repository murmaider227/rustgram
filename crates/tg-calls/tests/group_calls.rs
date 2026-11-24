use tg_calls::group::extract_conference_slug;
use tg_calls::group::participant::{ParticipantsList, VersionedGroupCallUpdate};
use tg_calls::group::payload::{
    DtlsFingerprint, GroupJoinPayload, JoinSsrcGroup, ParsedJoinResponse, RtmpInfo,
    parse_join_response,
};
use tg_calls::group::state::{
    AudioLevelSample, GroupCallAction, GroupCallSession, GroupCallSound, GroupCallState,
    GroupCallTimerKind, InstanceMode, MuteState,
};
use tg_calls::group::video::{
    MediaSsrcGroup, RequestedVideoTrackInput, VideoEndpoint, VideoEndpointType, VideoQuality,
    compute_requested_video_channels,
};
use tg_calls::group::{DEFAULT_VOLUME, MAX_VOLUME, RTMP_ENDPOINT_ID};
use tg_calls::types::{
    CallId, ChannelId, GroupCallInfo, GroupCallObject, GroupCallParticipantData,
    GroupCallParticipantVideo, GroupCallParticipantVideoSourceGroup, PeerId,
    UpdateGroupCallConnection, UpdateGroupCallParticipants, UserId,
};

fn make_participant(peer: PeerId, ssrc: u32) -> GroupCallParticipantData {
    GroupCallParticipantData {
        muted: true,
        left: false,
        can_self_unmute: true,
        just_joined: false,
        versioned: false,
        min: false,
        muted_by_you: false,
        volume_by_admin: false,
        is_self: false,
        video_joined: true,
        peer,
        date: 1000,
        active_date: None,
        source: ssrc,
        volume: Some(DEFAULT_VOLUME),
        about: None,
        raise_hand_rating: None,
        video: None,
        presentation: None,
    }
}

fn make_group_call_info(id: u64, version: i32) -> GroupCallInfo {
    GroupCallInfo {
        id: CallId(id),
        access_hash: 999,
        participants_count: 1,
        title: Some("Standup".to_string()),
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
        invite_link: Some("https://t.me/+call?slug=abc123&foo=bar#frag".to_string()),
        send_paid_messages_stars: None,
    }
}

#[test]
fn join_payload_and_join_response_json_parsing() {
    let payload = GroupJoinPayload {
        ssrc: 123_456_789,
        ufrag: "abcd".to_string(),
        pwd: "secretpassword1234567890".to_string(),
        fingerprints: vec![DtlsFingerprint {
            hash: "sha-256".to_string(),
            setup: Some("active".to_string()),
            fingerprint: "AA:BB:CC".to_string(),
        }],
        ssrc_groups: vec![JoinSsrcGroup {
            semantics: "FID".to_string(),
            sources: vec![101, 102],
        }],
    };
    let json = payload.to_json().expect("serialize join payload");
    assert!(json.contains("\"ssrc-groups\""));
    let parsed_back = GroupJoinPayload::from_json(&json).expect("deserialize join payload");
    assert_eq!(parsed_back, payload);

    // RTC join response
    let rtc_json = r#"{
        "transport": {
            "ufrag": "srv_ufrag",
            "pwd": "srv_pwd",
            "rtcp-mux": true,
            "fingerprints": [{"hash": "sha-256", "setup": "passive", "fingerprint": "11:22"}],
            "candidates": [{"ip": "149.154.175.5", "port": "1400", "protocol": "udp", "type": "host"}]
        },
        "video": {
            "endpoint": "ep_camera_42",
            "payload-types": [{"id": 100, "name": "VP8", "clockrate": 90000}]
        }
    }"#;
    match parse_join_response(rtc_json).expect("rtc response") {
        ParsedJoinResponse::Rtc {
            video_endpoint,
            params,
        } => {
            assert_eq!(video_endpoint, "ep_camera_42");
            assert_eq!(params.transport.ufrag, "srv_ufrag");
            assert_eq!(params.transport.candidates.len(), 1);
            assert!(params.transport.rtcp_mux);
            assert_eq!(params.video.payload_types[0].name, "VP8");
        }
        other => panic!("expected Rtc, got {other:?}"),
    }

    // RTMP broadcast stream response
    let rtmp_json = r#"{
        "stream": true,
        "rtmp": true,
        "rtmp_stream_url": "rtmps://dc2-1.rtmp.t.me/s/",
        "rtmp_stream_key": "12345:secret"
    }"#;
    assert_eq!(
        parse_join_response(rtmp_json).expect("rtmp response"),
        ParsedJoinResponse::BroadcastStream {
            rtmp: true,
            rtmp_info: RtmpInfo {
                url: "rtmps://dc2-1.rtmp.t.me/s/".to_string(),
                key: "12345:secret".to_string(),
            },
        }
    );

    assert!(parse_join_response("not json").is_err());
    assert!(parse_join_response("[1, 2, 3]").is_err());
    assert_eq!(
        extract_conference_slug("tg://call?slug=my_slug_42&x=1#top").as_deref(),
        Some("my_slug_42")
    );
}

#[test]
fn participants_list_versioned_queue_min_updates_and_raised_hand() {
    let mut list = ParticipantsList::new(CallId(500));
    let peer1 = PeerId::from_user(UserId(1));
    let peer2 = PeerId::from_user(UserId(2));

    // Initial version = 1.
    let mut p1 = make_participant(peer1, 1001);
    p1.volume = Some(15_000);
    p1.muted_by_you = true;
    p1.raise_hand_rating = Some(10);
    list.process_full_call(GroupCallObject::Active(make_group_call_info(500, 1)), &[p1]);
    assert_eq!(list.version(), 1);
    assert_eq!(list.find_local_raised_hand_rating(), 11);

    // Out-of-order update for version 3 arrives before version 2 -> queued.
    let mut p2_v3 = make_participant(peer2, 2002);
    p2_v3.left = true;
    p2_v3.versioned = true;
    let ev_queued = list.enqueue_update(VersionedGroupCallUpdate::Participants(
        UpdateGroupCallParticipants {
            call_id: CallId(500),
            participants: vec![p2_v3],
            version: 3,
        },
    ));
    assert!(ev_queued.has_pending_queued_updates);
    assert_eq!(list.version(), 1);

    // Version 2 arrives -> applies v2 (peer2 joins) AND drains queued v3 (peer2 leaves)!
    let mut p2_v2 = make_participant(peer2, 2002);
    p2_v2.just_joined = true;
    p2_v2.versioned = true;
    let ev_drained = list.enqueue_update(VersionedGroupCallUpdate::Participants(
        UpdateGroupCallParticipants {
            call_id: CallId(500),
            participants: vec![p2_v2],
            version: 2,
        },
    ));
    assert!(!ev_drained.has_pending_queued_updates);
    assert_eq!(list.version(), 3);
    assert!(list.participant_by_peer(peer2).is_none());
    assert_eq!(ev_drained.participant_updates.len(), 2);

    // A `min` update for peer1 preserves local custom volume and muted_by_me.
    let mut p1_min = make_participant(peer1, 1001);
    p1_min.min = true;
    p1_min.volume = Some(DEFAULT_VOLUME);
    p1_min.muted_by_you = false;
    list.enqueue_update(VersionedGroupCallUpdate::Participants(
        UpdateGroupCallParticipants {
            call_id: CallId(500),
            participants: vec![p1_min],
            version: 3,
        },
    ));
    let stored = list.participant_by_peer(peer1).unwrap();
    assert_eq!(stored.volume, 15_000);
    assert!(stored.muted_by_me);
}

#[test]
fn group_call_session_join_mute_raise_hand_volume_and_speaking_indicators() {
    let chat_peer = PeerId::from_channel(ChannelId(99));
    let me = PeerId::from_user(UserId(1));
    let bob = PeerId::from_user(UserId(2));

    let mut session = GroupCallSession::new(chat_peer, me, CallId(500), 777, false, 0, false);
    assert_eq!(session.state(), GroupCallState::Joining);
    assert!(
        session
            .take_actions()
            .contains(&GroupCallAction::EmitJoinPayload)
    );

    // Provide join payload -> emits JoinGroupCall.
    session.on_join_payload_ready(1111, r#"{"ssrc":1111}"#.to_string());
    assert!(
        session
            .take_actions()
            .iter()
            .any(|a| matches!(a, GroupCallAction::JoinGroupCall { .. }))
    );

    // Simulate SSRC duplicate error -> auto rejoin!
    session.handle_join_fail("GROUPCALL_SSRC_DUPLICATE_MUCH");
    assert_eq!(session.state(), GroupCallState::Joining);
    assert!(
        session
            .take_actions()
            .contains(&GroupCallAction::EmitJoinPayload)
    );

    // Retry with new SSRC -> Connecting -> Connected -> Joined.
    session.on_join_payload_ready(2222, r#"{"ssrc":2222}"#.to_string());
    session.handle_join_done(MuteState::Muted, true);
    assert_eq!(session.state(), GroupCallState::Connecting);
    assert_eq!(session.my_ssrc(), 2222);

    session.handle_connection_update(&UpdateGroupCallConnection {
        presentation: false,
        params_json: r#"{"video":{"endpoint":"my_cam_ep"}}"#.to_string(),
    });
    assert_eq!(session.instance_mode(), InstanceMode::Rtc);
    assert_eq!(session.camera_endpoint(), "my_cam_ep");

    session.set_instance_connected(true, false);
    assert_eq!(session.state(), GroupCallState::Joined);
    assert!(
        session
            .take_actions()
            .contains(&GroupCallAction::PlaySound(GroupCallSound::Started))
    );

    // Populate participants (self + Bob).
    let mut me_p = make_participant(me, 2222);
    me_p.is_self = true;
    let mut bob_p = make_participant(bob, 3333);
    bob_p.muted = false;
    session.process_full_call(
        GroupCallObject::Active(make_group_call_info(500, 1)),
        &[me_p, bob_p],
    );
    session.take_actions();

    // Admin force-mutes us, we raise hand, then admin allows us to speak.
    let mut force_muted_me = make_participant(me, 2222);
    force_muted_me.is_self = true;
    force_muted_me.muted = true;
    force_muted_me.can_self_unmute = false;
    session.handle_versioned_update(VersionedGroupCallUpdate::Participants(
        UpdateGroupCallParticipants {
            call_id: CallId(500),
            participants: vec![force_muted_me],
            version: 1,
        },
    ));
    assert_eq!(session.muted(), MuteState::ForceMuted);
    assert!(session.muted_by_admin());
    session.take_actions();

    // Raise hand -> emits EditGroupCallParticipant(raise_hand: Some(true)).
    session.set_muted_and_update(MuteState::RaisedHand);
    assert_eq!(session.muted(), MuteState::RaisedHand);
    let rh_actions = session.take_actions();
    assert!(rh_actions.iter().any(|a| matches!(
        a,
        GroupCallAction::EditGroupCallParticipant {
            raise_hand: Some(true),
            ..
        }
    )));
    session.handle_self_update_done(None);

    // Admin un-force-mutes us -> transitions to Muted + AllowedToSpeak notification!
    let mut allowed_me = make_participant(me, 2222);
    allowed_me.is_self = true;
    allowed_me.muted = true;
    allowed_me.can_self_unmute = true;
    session.handle_versioned_update(VersionedGroupCallUpdate::Participants(
        UpdateGroupCallParticipants {
            call_id: CallId(500),
            participants: vec![allowed_me],
            version: 1,
        },
    ));
    assert_eq!(session.muted(), MuteState::Muted);
    assert!(
        session
            .take_actions()
            .contains(&GroupCallAction::AllowedToSpeak)
    );

    // Change Bob's volume above MAX_VOLUME -> clamped to 20000 (2.0x).
    session.change_participant_volume(bob, 99_999, false);
    let vol_actions = session.take_actions();
    assert!(vol_actions.iter().any(|a| matches!(
        a,
        GroupCallAction::EditGroupCallParticipant {
            participant,
            volume: Some(MAX_VOLUME),
            ..
        } if *participant == bob
    )));
    assert!(vol_actions.iter().any(|a| matches!(
        a,
        GroupCallAction::SetInstanceVolume { ssrc: 3333, volume } if (*volume - 2.0).abs() < 1e-6
    )));

    // Speaking indicators: Bob speaks above threshold (0.2) -> sounding & speaking = true.
    session.audio_levels_updated(
        &[AudioLevelSample {
            ssrc: 3333,
            level: 0.75,
            voice: true,
        }],
        10_000,
    );
    let bob_now = session
        .participants_list()
        .participant_by_peer(bob)
        .unwrap();
    assert!(bob_now.sounding);
    assert!(bob_now.speaking);
    session.take_actions();

    // After 1600 ms (> 1500 ms kSoundStatusKeptFor), speaking expires.
    session.on_timer(GroupCallTimerKind::CheckLastSpoke, 11_600);
    let bob_after = session
        .participants_list()
        .participant_by_peer(bob)
        .unwrap();
    assert!(!bob_after.sounding);
    assert!(!bob_after.speaking);

    // Unknown SSRC triggers ResolveUnknownSsrcs.
    session.audio_levels_updated(
        &[AudioLevelSample {
            ssrc: 8888,
            level: 0.9,
            voice: true,
        }],
        12_000,
    );
    assert!(session.take_actions().iter().any(|a| matches!(
        a,
        GroupCallAction::ResolveUnknownSsrcs { ssrcs, .. } if ssrcs == &[8888]
    )));

    // Leave cleanly: HangingUp -> LeaveGroupCall -> Ended.
    session.hangup();
    assert_eq!(session.state(), GroupCallState::HangingUp);
    assert!(
        session
            .take_actions()
            .iter()
            .any(|a| matches!(a, GroupCallAction::LeaveGroupCall { source: 2222, .. }))
    );
    session.handle_leave_done();
    assert_eq!(session.state(), GroupCallState::Ended);
}

#[test]
fn video_quality_budget_downgrades_exactly_like_tdesktop() {
    let groups = vec![MediaSsrcGroup {
        semantics: "SIM".to_string(),
        ssrcs: vec![1, 2, 3],
    }];

    // 3 Full cameras (3 * 4 = 12) + 1 Full screen (4) = 16 -> exact budget, no downgrade!
    let eps: Vec<VideoEndpoint> = (1..=4)
        .map(|i| {
            let kind = if i == 4 {
                VideoEndpointType::Screen
            } else {
                VideoEndpointType::Camera
            };
            VideoEndpoint::new(kind, PeerId::from_user(UserId(i)), format!("ep_{i}"))
        })
        .collect();
    let inputs: Vec<RequestedVideoTrackInput<'_>> = eps
        .iter()
        .enumerate()
        .map(|(idx, ep)| RequestedVideoTrackInput {
            endpoint: ep,
            desired_quality: VideoQuality::Full,
            audio_ssrc: 100 + idx as u32,
            user_id: (idx + 1) as i64,
            ssrc_groups: &groups,
        })
        .collect();

    let ch_exact = compute_requested_video_channels(&inputs, "", "");
    assert_eq!(ch_exact.len(), 4);
    assert!(ch_exact.iter().all(|c| c.max_quality == VideoQuality::Full));

    // Add 1 more Full camera -> 5 * 4 = 20 > 16 -> Full cameras downgrade to Medium, Full screen stays Full!
    let ep5 = VideoEndpoint::new(
        VideoEndpointType::Camera,
        PeerId::from_user(UserId(5)),
        "ep_5",
    );
    let mut inputs_over = inputs.clone();
    inputs_over.push(RequestedVideoTrackInput {
        endpoint: &ep5,
        desired_quality: VideoQuality::Full,
        audio_ssrc: 105,
        user_id: 5,
        ssrc_groups: &groups,
    });
    let ch_over = compute_requested_video_channels(&inputs_over, "", "");
    assert_eq!(ch_over[0].max_quality, VideoQuality::Medium); // camera downgraded
    assert_eq!(ch_over[3].max_quality, VideoQuality::Full); // screencast preserved
    assert_eq!(ch_over[3].min_quality, VideoQuality::Full);
    assert_eq!(ch_over[4].max_quality, VideoQuality::Medium);

    // 5 Full screencasts -> 5 * 4 = 20 > 16 -> downgradeAll downgrades screencasts to Thumbnail!
    let screen_eps: Vec<VideoEndpoint> = (1..=5)
        .map(|i| {
            VideoEndpoint::new(
                VideoEndpointType::Screen,
                PeerId::from_user(UserId(i)),
                format!("s_{i}"),
            )
        })
        .collect();
    let screen_inputs: Vec<RequestedVideoTrackInput<'_>> = screen_eps
        .iter()
        .enumerate()
        .map(|(idx, ep)| RequestedVideoTrackInput {
            endpoint: ep,
            desired_quality: VideoQuality::Full,
            audio_ssrc: 200 + idx as u32,
            user_id: (idx + 1) as i64,
            ssrc_groups: &groups,
        })
        .collect();
    let ch_screens = compute_requested_video_channels(&screen_inputs, "", "");
    assert!(
        ch_screens
            .iter()
            .all(|c| c.max_quality == VideoQuality::Thumbnail
                && c.min_quality == VideoQuality::Thumbnail)
    );

    // Self camera endpoint is skipped; RTMP endpoint is preserved with max_quality = Full.
    let rtmp_ep = VideoEndpoint::new(
        VideoEndpointType::Screen,
        PeerId::from_channel(ChannelId(1)),
        RTMP_ENDPOINT_ID,
    );
    let mixed = vec![
        RequestedVideoTrackInput {
            endpoint: &eps[0],
            desired_quality: VideoQuality::Full,
            audio_ssrc: 100,
            user_id: 1,
            ssrc_groups: &groups,
        },
        RequestedVideoTrackInput {
            endpoint: &rtmp_ep,
            desired_quality: VideoQuality::Thumbnail,
            audio_ssrc: 0,
            user_id: 0,
            ssrc_groups: &[],
        },
    ];
    let ch_mixed = compute_requested_video_channels(&mixed, "ep_1", "");
    assert_eq!(ch_mixed.len(), 1);
    assert_eq!(ch_mixed[0].endpoint_id, RTMP_ENDPOINT_ID);
    assert_eq!(ch_mixed[0].max_quality, VideoQuality::Full);
    assert_eq!(ch_mixed[0].min_quality, VideoQuality::Thumbnail);

    // Empty source_groups are dropped during video param parsing (calls_group_call.cpp:367-369).
    let vid = GroupCallParticipantVideo {
        paused: false,
        endpoint: "ep_x".to_string(),
        source_groups: vec![
            GroupCallParticipantVideoSourceGroup {
                semantics: "FID".to_string(),
                sources: vec![],
            },
            GroupCallParticipantVideoSourceGroup {
                semantics: "SIM".to_string(),
                sources: vec![7, 8],
            },
        ],
        audio_source: Some(999),
    };
    let parsed = tg_calls::group::video::ParticipantVideoParams::parse(Some(&vid), None).unwrap();
    assert_eq!(parsed.camera.ssrc_groups.len(), 1);
    assert_eq!(parsed.camera.ssrc_groups[0].ssrcs, vec![7, 8]);
}
