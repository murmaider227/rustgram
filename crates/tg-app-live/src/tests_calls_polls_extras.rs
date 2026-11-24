//! `FakeRpcClient` conformance tests for WP-57 Group-Call Invite/Scheduling, Call History
//! Clearing, and Poll Results/Stats RPCs (`tg-app-core` + `tg-app-live`).
//!
//! Covers all 8 RPCs in WP-57:
//! 1. `phone.exportGroupCallInvite` (`can_self_unmute: true` speaker + `false` listener)
//! 2. `phone.inviteToGroupCall` (10-user slice chunking per `kMaxInvitePerSlice` + partial failure)
//! 3. `phone.saveDefaultGroupCallJoinAs`
//! 4. `phone.startScheduledGroupCall`
//! 5. `phone.toggleGroupCallStartSubscription`
//! 6. `messages.deletePhoneCallHistory` (paginated `offset > 0` loop + `revoke` flag)
//! 7. `messages.getPollResults` (cached `poll_hash` + `Update::MessagePoll` with `min: true`)
//! 8. `stats.getPollStats` (direct `StatsGraph`, `StatsGraph::Async` -> `stats.loadAsyncGraph`,
//!    and `StatsGraph::Error`)

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_calls::types::CallId;
use tg_data::{
    FullMsgId as DataFullMsgId, Message, MessageContent, MessageMedia, MsgId as DataMsgId,
    PeerId as DataPeerId, PollAnswer as DataPollAnswer, PollAnswerVoters as DataPollAnswerVoters,
    PollData, PollId, PollResults as DataPollResults, TextWithEntities as DataText,
    UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use tg_app_core::calls::backend::CallsBackend;
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::stand_in::types::{MsgId, PeerId};

// =============================================================================
// Synthetic FakeRpcClient & fixtures
// =============================================================================

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
type RecordedCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: RecordedCalls,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn enqueue_err(&self, expected_ctor: u32, err: InvocationError) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Err(err)));
    }

    fn recorded_calls(&self) -> Vec<(u32, Vec<u8>)> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn recorded_for(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, bytes)| bytes.clone())
            .collect()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(self.main_dc_id(), req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));
        // Background `phone.getGroupCall` fired automatically on `join_group_call_with_id`
        // may arrive asynchronously; handle it cleanly if not explicitly queued at head.
        let popped = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(expected, _)| *expected == ctor) {
                q.remove(pos)
            } else {
                None
            }
        };
        if let Some((_, result)) = popped {
            let raw = result?;
            return R::Return::from_bytes(&raw)
                .map_err(|e| InvocationError::Deserialize(e.to_string()));
        }
        Err(InvocationError::Rpc(tg_api::rpc::RpcError {
            code: 400,
            name: format!("UNQUEUED_0x{ctor:08x}"),
            value: None,
        }))
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock main_dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("lock main_dc") = dc_id;
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn synthetic_user(id: i64, access_hash: i64, first: &str, is_self: bool) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: is_self,
        contact: !is_self,
        mutual_contact: false,
        deleted: false,
        bot: false,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: false,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: true,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(access_hash),
        first_name: Some(first.to_owned()),
        last_name: None,
        username: Some(format!("user_{id}")),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    })
}

fn synthetic_channel(id: i64, access_hash: i64, title: &str, megagroup: bool) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: true,
        left: false,
        broadcast: !megagroup,
        verified: false,
        megagroup,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: true,
        call_not_empty: true,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: true,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(access_hash),
        title: title.to_owned(),
        username: Some(format!("chan_{id}")),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_750_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(42),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    })
}

fn synthetic_group_call(
    id: i64,
    access_hash: i64,
    schedule_date: Option<i32>,
    schedule_start_subscribed: bool,
) -> tl_enums::GroupCall {
    tl_enums::GroupCall::GroupCall(tl_types::GroupCall {
        join_muted: false,
        can_change_join_muted: true,
        join_date_asc: false,
        schedule_start_subscribed,
        can_start_video: true,
        record_video_active: false,
        rtmp_stream: false,
        listeners_hidden: false,
        conference: false,
        creator: true,
        messages_enabled: true,
        can_change_messages_enabled: true,
        min: false,
        id,
        access_hash,
        participants_count: 1,
        title: Some("Voice Lounge".to_owned()),
        stream_dc_id: None,
        record_start_date: None,
        schedule_date,
        unmuted_video_count: Some(0),
        unmuted_video_limit: 30,
        version: 2,
        invite_link: None,
        send_paid_messages_stars: None,
        default_send_as: None,
    })
}

fn make_session() -> (LiveSession, FakeRpcClient, PeerId, PeerId, Vec<PeerId>) {
    let fake = FakeRpcClient::new();
    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId(10_001),
            crate::ErasedRpcClient::new(fake.clone()),
            SessionRuntimeConfig {
                init_updates_state: false,
                run_get_difference_on_start: false,
                load_initial_dialogs: false,
                fetch_self_user_on_start: false,
                ..SessionRuntimeConfig::default()
            },
        )
        .expect("start SessionRuntime"),
    );

    let mut users = vec![synthetic_user(10_001, 111_222, "SelfUser", true)];
    let mut invitees = Vec::new();
    for i in 0..13_i64 {
        let uid = 20_001 + i;
        users.push(synthetic_user(uid, uid * 101, &format!("Member{i}"), false));
        invitees.push(PeerId::from_user(uid as u64));
    }
    let group_id = 500_001_i64;
    let alias_channel_id = 500_002_i64;
    let chats = vec![
        synthetic_channel(group_id, 999_001, "Alpha Group", true),
        synthetic_channel(alias_channel_id, 999_002, "Alias Channel", false),
    ];

    let session_arc = runtime.session().clone();
    runtime.tokio_handle().block_on(async move {
        let mut s = session_arc.write().await;
        apply_tl_users_and_chats(&mut s, &users, &chats);
    });

    let live = LiveSession::new(runtime, 1);
    (
        live,
        fake,
        PeerId::from_channel(group_id as u64),
        PeerId::from_channel(alias_channel_id as u64),
        invitees,
    )
}

fn sample_chart_json() -> String {
    r#"{
        "columns": [["x", 1704067200000, 1704153600000], ["y0", 12, 27]],
        "types": {"x": "x", "y0": "line"},
        "names": {"y0": "Option A"},
        "colors": {"y0": "BLUE#2F80ED"}
    }"#
    .to_owned()
}

// =============================================================================
// 1. Group-Call Invite & Scheduling RPCs (`phone.*`)
// =============================================================================

#[test]
fn export_group_call_invite_speaker_and_listener() {
    let (mut live, fake, group_peer, _alias, _invitees) = make_session();
    let self_peer = PeerId::from_user(10_001);

    // Without an active group call, returns a descriptive error.
    assert!(live.export_group_call_invite(true).is_err());

    live.join_group_call_with_id(group_peer, self_peer, CallId(777_001), 888_002, true, 1_000);

    fake.enqueue_ok(
        tl_fns::phone::ExportGroupCallInvite::CONSTRUCTOR_ID,
        &tl_enums::phone::ExportedGroupCallInvite::ExportedGroupCallInvite(
            tl_types::phone::ExportedGroupCallInvite {
                link: "https://t.me/chan_500001?voicechat=speaker_tok".to_owned(),
            },
        ),
    );
    fake.enqueue_ok(
        tl_fns::phone::ExportGroupCallInvite::CONSTRUCTOR_ID,
        &tl_enums::phone::ExportedGroupCallInvite::ExportedGroupCallInvite(
            tl_types::phone::ExportedGroupCallInvite {
                link: "https://t.me/chan_500001?voicechat=listener_tok".to_owned(),
            },
        ),
    );

    let speaker_link = live
        .export_group_call_invite(true)
        .expect("speaker invite link");
    let listener_link = live
        .export_group_call_invite(false)
        .expect("listener invite link");

    assert_eq!(
        speaker_link,
        "https://t.me/chan_500001?voicechat=speaker_tok"
    );
    assert_eq!(
        listener_link,
        "https://t.me/chan_500001?voicechat=listener_tok"
    );

    let snap = live
        .active_group_call()
        .expect("active group call snapshot");
    assert_eq!(snap.invite_link_speaker, speaker_link);
    assert_eq!(snap.invite_link_listener, listener_link);

    let payloads = fake.recorded_for(tl_fns::phone::ExportGroupCallInvite::CONSTRUCTOR_ID);
    assert_eq!(payloads.len(), 2);
    let req_speaker =
        tl_fns::phone::ExportGroupCallInvite::from_bytes(&payloads[0]).expect("decode speaker req");
    let req_listener = tl_fns::phone::ExportGroupCallInvite::from_bytes(&payloads[1])
        .expect("decode listener req");
    assert!(req_speaker.can_self_unmute);
    assert!(!req_listener.can_self_unmute);
    assert_eq!(
        req_speaker.call,
        tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
            id: 777_001,
            access_hash: 888_002,
        })
    );
    assert_eq!(req_listener.call, req_speaker.call);
}

#[test]
fn invite_to_group_call_batches_by_ten_and_handles_partial_failure() {
    let (mut live, fake, group_peer, _alias, invitees) = make_session();
    let self_peer = PeerId::from_user(10_001);
    live.join_group_call_with_id(group_peer, self_peer, CallId(777_010), 888_010, true, 1_000);

    let empty_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_010,
        seq: 1,
    });
    // 13 users -> 2 slices: 10 users + 3 users.
    fake.enqueue_ok(
        tl_fns::phone::InviteToGroupCall::CONSTRUCTOR_ID,
        &empty_updates,
    );
    fake.enqueue_ok(
        tl_fns::phone::InviteToGroupCall::CONSTRUCTOR_ID,
        &empty_updates,
    );

    let invited = live
        .invite_to_group_call(&invitees)
        .expect("invite 13 users across 2 slices");
    assert_eq!(invited.len(), 13);
    assert_eq!(invited, invitees);

    let payloads = fake.recorded_for(tl_fns::phone::InviteToGroupCall::CONSTRUCTOR_ID);
    assert_eq!(payloads.len(), 2);
    let slice1 = tl_fns::phone::InviteToGroupCall::from_bytes(&payloads[0]).expect("slice 1");
    let slice2 = tl_fns::phone::InviteToGroupCall::from_bytes(&payloads[1]).expect("slice 2");
    assert_eq!(slice1.users.len(), 10);
    assert_eq!(slice2.users.len(), 3);
    assert_eq!(
        slice1.call,
        tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
            id: 777_010,
            access_hash: 888_010,
        })
    );

    // Partial failure: first slice of 10 succeeds, second slice of 3 fails -> still returns 10.
    fake.enqueue_ok(
        tl_fns::phone::InviteToGroupCall::CONSTRUCTOR_ID,
        &empty_updates,
    );
    fake.enqueue_err(
        tl_fns::phone::InviteToGroupCall::CONSTRUCTOR_ID,
        InvocationError::Rpc(tg_api::rpc::RpcError {
            code: 400,
            name: "USER_PRIVACY_RESTRICTED".to_owned(),
            value: None,
        }),
    );
    let partial = live
        .invite_to_group_call(&invitees)
        .expect("partial invite succeeds with first slice");
    assert_eq!(partial.len(), 10);
    assert_eq!(partial, invitees[..10]);
}

#[test]
fn save_default_group_call_join_as_encodes_peer_and_join_as() {
    let (mut live, fake, group_peer, alias_channel, _invitees) = make_session();

    fake.enqueue_ok(
        tl_fns::phone::SaveDefaultGroupCallJoinAs::CONSTRUCTOR_ID,
        &true,
    );

    let ok = live
        .save_default_group_call_join_as(group_peer, alias_channel)
        .expect("save default join_as");
    assert!(ok);

    let payloads = fake.recorded_for(tl_fns::phone::SaveDefaultGroupCallJoinAs::CONSTRUCTOR_ID);
    assert_eq!(payloads.len(), 1);
    let req = tl_fns::phone::SaveDefaultGroupCallJoinAs::from_bytes(&payloads[0])
        .expect("decode SaveDefaultGroupCallJoinAs");
    assert_eq!(
        req.peer,
        tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
            channel_id: 500_001,
            access_hash: 999_001,
        })
    );
    assert_eq!(
        req.join_as,
        tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
            channel_id: 500_002,
            access_hash: 999_002,
        })
    );
}

#[test]
fn scheduled_group_call_toggle_subscription_and_start_now() {
    let (mut live, fake, group_peer, _alias, _invitees) = make_session();
    let self_peer = PeerId::from_user(10_001);
    live.join_group_call_with_id(group_peer, self_peer, CallId(777_020), 888_020, true, 1_000);

    // 1. Toggle subscription ON -> server returns Update::GroupCall with schedule_date + subscribed=true.
    let sub_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::GroupCall(tl_types::UpdateGroupCall {
            live_story: false,
            peer: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                channel_id: 500_001,
            })),
            call: synthetic_group_call(777_020, 888_020, Some(1_850_000_000), true),
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_020,
        seq: 2,
    });
    fake.enqueue_ok(
        tl_fns::phone::ToggleGroupCallStartSubscription::CONSTRUCTOR_ID,
        &sub_updates,
    );

    live.toggle_group_call_start_subscription(true)
        .expect("toggle schedule start subscription");
    assert_eq!(live.active_group_call_schedule_date(), Some(1_850_000_000));
    assert!(live.active_group_call_schedule_subscribed());

    let sub_payloads =
        fake.recorded_for(tl_fns::phone::ToggleGroupCallStartSubscription::CONSTRUCTOR_ID);
    assert_eq!(sub_payloads.len(), 1);
    let sub_req = tl_fns::phone::ToggleGroupCallStartSubscription::from_bytes(&sub_payloads[0])
        .expect("decode ToggleGroupCallStartSubscription");
    assert!(sub_req.subscribed);
    assert_eq!(
        sub_req.call,
        tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
            id: 777_020,
            access_hash: 888_020,
        })
    );

    // 2. Start scheduled group call immediately -> server clears schedule_date (None).
    let start_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::GroupCall(tl_types::UpdateGroupCall {
            live_story: false,
            peer: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                channel_id: 500_001,
            })),
            call: synthetic_group_call(777_020, 888_020, None, false),
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_025,
        seq: 3,
    });
    fake.enqueue_ok(
        tl_fns::phone::StartScheduledGroupCall::CONSTRUCTOR_ID,
        &start_updates,
    );

    live.start_scheduled_group_call()
        .expect("start scheduled group call");
    assert_eq!(live.active_group_call_schedule_date(), None);

    let start_payloads = fake.recorded_for(tl_fns::phone::StartScheduledGroupCall::CONSTRUCTOR_ID);
    assert_eq!(start_payloads.len(), 1);
    let start_req = tl_fns::phone::StartScheduledGroupCall::from_bytes(&start_payloads[0])
        .expect("decode StartScheduledGroupCall");
    assert_eq!(start_req.call, sub_req.call);
}

// =============================================================================
// 2. Clear Call History (`messages.deletePhoneCallHistory`)
// =============================================================================

#[test]
fn delete_phone_call_history_loops_until_offset_zero_and_clears_messages() {
    let (mut live, fake, _group_peer, _alias, invitees) = make_session();
    let remote_peer = DataPeerId::from_user(DataUserId(invitees[0].bare()));

    // Insert 3 non-channel call messages into session history.
    let session_arc = live.runtime().session().clone();
    live.runtime().tokio_handle().block_on(async move {
        let mut s = session_arc.write().await;
        for msg_id in [101_i64, 102, 103] {
            s.insert_message(Message::new_text(
                remote_peer,
                DataMsgId(msg_id),
                None,
                1_750_000_000 + msg_id as i32,
                DataText::plain("call"),
            ));
        }
    });

    // Page 1: offset = 10 -> must loop again.
    fake.enqueue_ok(
        tl_fns::messages::DeletePhoneCallHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedFoundMessages::AffectedFoundMessages(
            tl_types::messages::AffectedFoundMessages {
                pts: 51,
                pts_count: 2,
                offset: 10,
                messages: vec![101, 102],
            },
        ),
    );
    // Page 2: offset = 0 -> stops.
    fake.enqueue_ok(
        tl_fns::messages::DeletePhoneCallHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedFoundMessages::AffectedFoundMessages(
            tl_types::messages::AffectedFoundMessages {
                pts: 52,
                pts_count: 1,
                offset: 0,
                messages: vec![103],
            },
        ),
    );

    let deleted = live
        .delete_phone_call_history(true)
        .expect("delete phone call history");
    assert_eq!(deleted, 3);
    assert!(live.recent_calls().is_empty());

    // All 3 messages removed from Session.
    let session_arc = live.runtime().session().clone();
    live.runtime().tokio_handle().block_on(async move {
        let s = session_arc.read().await;
        for msg_id in [101_i64, 102, 103] {
            assert!(
                s.message(DataFullMsgId::new(remote_peer, DataMsgId(msg_id)))
                    .is_none(),
                "message {msg_id} should be removed"
            );
        }
    });

    let payloads = fake.recorded_for(tl_fns::messages::DeletePhoneCallHistory::CONSTRUCTOR_ID);
    assert_eq!(payloads.len(), 2);
    for raw in &payloads {
        let req = tl_fns::messages::DeletePhoneCallHistory::from_bytes(raw)
            .expect("decode DeletePhoneCallHistory");
        assert!(req.revoke);
    }
}

// =============================================================================
// 3. Poll Results Reload (`messages.getPollResults`) & Poll Stats (`stats.getPollStats`)
// =============================================================================

#[test]
fn reload_poll_results_applies_min_update_and_caches_poll_hash() {
    let (mut live, fake, group_peer, _alias, _invitees) = make_session();
    let data_peer = DataPeerId::from_channel(tg_data::ChannelId(group_peer.bare()));
    let msg_id = MsgId(555);
    let poll_id = 990_001_i64;

    // Seed session with a poll message where option 0 is already chosen.
    let session_arc = live.runtime().session().clone();
    live.runtime().tokio_handle().block_on(async move {
        let mut s = session_arc.write().await;
        let mut msg = Message::new_text(
            data_peer,
            DataMsgId(i64::from(msg_id.0)),
            None,
            1_750_000_100,
            DataText::plain(""),
        );
        msg.content = MessageContent::Regular {
            text: DataText::plain(""),
            media: MessageMedia::Poll(Box::new(PollData {
                id: PollId::from_tl(poll_id),
                question: DataText::plain("Best transport?"),
                answers: vec![
                    DataPollAnswer {
                        text: DataText::plain("Abridged"),
                        option: b"0".to_vec(),
                    },
                    DataPollAnswer {
                        text: DataText::plain("Padded"),
                        option: b"1".to_vec(),
                    },
                ],
                closed: false,
                public_voters: true,
                multiple_choice: false,
                quiz: false,
                close_period: None,
                close_date: None,
                results: DataPollResults {
                    results: vec![
                        DataPollAnswerVoters {
                            option: b"0".to_vec(),
                            voters: 1,
                            chosen: true,
                            correct: false,
                        },
                        DataPollAnswerVoters {
                            option: b"1".to_vec(),
                            voters: 0,
                            chosen: false,
                            correct: false,
                        },
                    ],
                    total_voters: Some(1),
                    recent_voters: Vec::new(),
                    solution: None,
                },
            })),
        };
        s.insert_message(msg);
    });

    // Server replies to first `messages.getPollResults` with `peer: None, msg_id: None`,
    // `poll.hash = 0x55AA_1234`, and `results.min = true` (which omits `chosen` flags).
    let poll_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::MessagePoll(tl_types::UpdateMessagePoll {
            peer: None,
            msg_id: None,
            top_msg_id: None,
            poll_id,
            poll: Some(tl_enums::Poll::Poll(tl_types::Poll {
                id: poll_id,
                closed: false,
                public_voters: true,
                multiple_choice: false,
                quiz: false,
                open_answers: false,
                revoting_disabled: false,
                shuffle_answers: false,
                hide_results_until_close: false,
                creator: false,
                subscribers_only: false,
                question: tl_enums::TextWithEntities::TextWithEntities(
                    tl_types::TextWithEntities {
                        text: "Best transport?".to_owned(),
                        entities: Vec::new(),
                    },
                ),
                answers: vec![
                    tl_enums::PollAnswer::PollAnswer(tl_types::PollAnswer {
                        text: tl_enums::TextWithEntities::TextWithEntities(
                            tl_types::TextWithEntities {
                                text: "Abridged".to_owned(),
                                entities: Vec::new(),
                            },
                        ),
                        option: b"0".to_vec(),
                        media: None,
                        added_by: None,
                        date: None,
                    }),
                    tl_enums::PollAnswer::PollAnswer(tl_types::PollAnswer {
                        text: tl_enums::TextWithEntities::TextWithEntities(
                            tl_types::TextWithEntities {
                                text: "Padded".to_owned(),
                                entities: Vec::new(),
                            },
                        ),
                        option: b"1".to_vec(),
                        media: None,
                        added_by: None,
                        date: None,
                    }),
                ],
                close_period: None,
                close_date: None,
                hash: 0x55AA_1234,
                countries_iso2: None,
            })),
            results: tl_enums::PollResults::PollResults(tl_types::PollResults {
                min: true,
                has_unread_votes: false,
                can_view_stats: true,
                results: Some(vec![
                    tl_enums::PollAnswerVoters::PollAnswerVoters(tl_types::PollAnswerVoters {
                        chosen: false,
                        correct: false,
                        option: b"0".to_vec(),
                        voters: Some(4),
                        recent_voters: Some(Vec::new()),
                    }),
                    tl_enums::PollAnswerVoters::PollAnswerVoters(tl_types::PollAnswerVoters {
                        chosen: false,
                        correct: false,
                        option: b"1".to_vec(),
                        voters: Some(2),
                        recent_voters: Some(Vec::new()),
                    }),
                ]),
                total_voters: Some(6),
                recent_voters: None,
                solution: None,
                solution_entities: None,
                solution_media: None,
            }),
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_120,
        seq: 4,
    });
    fake.enqueue_ok(
        tl_fns::messages::GetPollResults::CONSTRUCTOR_ID,
        &poll_updates,
    );

    let updated = live
        .reload_poll_results(group_peer, msg_id)
        .expect("reload_poll_results")
        .expect("poll attachment present");
    assert_eq!(updated.total_voters, 6);
    assert_eq!(updated.options[0].votes, 4);
    assert!(
        updated.options[0].chosen,
        "min: true poll results must preserve local chosen=true flag"
    );
    assert_eq!(updated.options[1].votes, 2);
    assert!(!updated.options[1].chosen);

    // Second reload must send the cached `poll_hash == 0x55AA_1234`.
    fake.enqueue_ok(
        tl_fns::messages::GetPollResults::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_125,
            seq: 5,
        }),
    );
    let _ = live
        .reload_poll_results(group_peer, msg_id)
        .expect("second reload_poll_results");

    let payloads = fake.recorded_for(tl_fns::messages::GetPollResults::CONSTRUCTOR_ID);
    assert_eq!(payloads.len(), 2);
    let req1 = tl_fns::messages::GetPollResults::from_bytes(&payloads[0]).expect("req 1");
    let req2 = tl_fns::messages::GetPollResults::from_bytes(&payloads[1]).expect("req 2");
    assert_eq!(req1.msg_id, 555);
    assert_eq!(req1.poll_hash, 0);
    assert_eq!(req2.poll_hash, 0x55AA_1234);
}

#[test]
fn poll_stats_direct_async_and_error_graphs() {
    let (live, fake, group_peer, _alias, _invitees) = make_session();
    let msg_id = MsgId(556);

    // 1. Direct `StatsGraph` with `dark = true`.
    fake.enqueue_ok(
        tl_fns::stats::GetPollStats::CONSTRUCTOR_ID,
        &tl_enums::stats::PollStats::PollStats(tl_types::stats::PollStats {
            votes_graph: tl_enums::StatsGraph::StatsGraph(tl_types::StatsGraph {
                json: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                    data: sample_chart_json(),
                }),
                zoom_token: Some("zoom_direct".to_owned()),
            }),
        }),
    );

    let direct = live
        .poll_stats(group_peer, msg_id, true)
        .expect("direct poll stats");
    assert!(!direct.chart.is_empty());
    assert_eq!(direct.zoom_token, "zoom_direct");
    assert!(direct.error.is_empty());

    // 2. Async `StatsGraph::Async` with `dark = false` -> auto-fetches `stats.loadAsyncGraph`.
    fake.enqueue_ok(
        tl_fns::stats::GetPollStats::CONSTRUCTOR_ID,
        &tl_enums::stats::PollStats::PollStats(tl_types::stats::PollStats {
            votes_graph: tl_enums::StatsGraph::Async(tl_types::StatsGraphAsync {
                token: "async_poll_tok_42".to_owned(),
            }),
        }),
    );
    fake.enqueue_ok(
        tl_fns::stats::LoadAsyncGraph::CONSTRUCTOR_ID,
        &tl_enums::StatsGraph::StatsGraph(tl_types::StatsGraph {
            json: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                data: sample_chart_json(),
            }),
            zoom_token: None,
        }),
    );

    let hydrated = live
        .poll_stats(group_peer, msg_id, false)
        .expect("async hydrated poll stats");
    assert!(!hydrated.chart.is_empty());
    assert!(hydrated.error.is_empty());

    // 3. `StatsGraph::Error` -> returned without invoking `loadAsyncGraph`.
    fake.enqueue_ok(
        tl_fns::stats::GetPollStats::CONSTRUCTOR_ID,
        &tl_enums::stats::PollStats::PollStats(tl_types::stats::PollStats {
            votes_graph: tl_enums::StatsGraph::Error(tl_types::StatsGraphError {
                error: "GRAPH_NOT_READY".to_owned(),
            }),
        }),
    );
    let err_graph = live
        .poll_stats(group_peer, msg_id, false)
        .expect("error graph returned");
    assert_eq!(err_graph.error, "GRAPH_NOT_READY");

    // Verify exact wire encoding of all calls.
    let stats_payloads = fake.recorded_for(tl_fns::stats::GetPollStats::CONSTRUCTOR_ID);
    assert_eq!(stats_payloads.len(), 3);
    let s1 = tl_fns::stats::GetPollStats::from_bytes(&stats_payloads[0]).expect("s1");
    let s2 = tl_fns::stats::GetPollStats::from_bytes(&stats_payloads[1]).expect("s2");
    assert!(s1.dark);
    assert!(!s2.dark);
    assert_eq!(s1.msg_id, 556);
    assert_eq!(
        s1.peer,
        tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
            channel_id: 500_001,
            access_hash: 999_001,
        })
    );

    let async_payloads = fake.recorded_for(tl_fns::stats::LoadAsyncGraph::CONSTRUCTOR_ID);
    assert_eq!(async_payloads.len(), 1);
    let ag = tl_fns::stats::LoadAsyncGraph::from_bytes(&async_payloads[0]).expect("loadAsyncGraph");
    assert_eq!(ag.token, "async_poll_tok_42");
    assert_eq!(ag.x, None);

    // Sanity check we did not accidentally drop or skip calls.
    assert!(fake.recorded_calls().len() >= 4);
}
