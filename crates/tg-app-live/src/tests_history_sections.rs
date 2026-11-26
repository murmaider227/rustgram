//! Live extended history sections tests (`FakeRpcClient` only, synthetic data).
//!
//! Covers forum topics, scheduled messages, channel comments / replies threads,
//! admin log with filters & admin selection, saved messages sublists & pins,
//! polls & quizzes (`inputMediaPoll`), forwarding options, rich drafts, and all
//! matching real-time TL updates.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_api::{DcId, InvocationError, RpcClient};
use tg_chat::send_options::SCHEDULED_UNTIL_ONLINE_TIMESTAMP;
use tg_data::{
    Channel, ChannelFlags, ChannelId, Peer, PeerId as DataPeerId, User, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::{EntityInText, EntityType, TextWithEntities};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::{ForumTopicEdit, LiveForwardOptions, LivePollOptions, LiveSession};
use tg_app_core::core_types::dto::rich_compose::RichThreadDraft;
use tg_app_core::history::backend::{HistoryBackend, HistoryMedia, SendMessageExtended};
use tg_app_core::history::sections_model::{
    AdminLogFilter, AdminLogFilterFlags, CreatePollRequest, FORUM_TOPIC_ICON_COLORS,
    ForwardOptions, ReplyQuote, ScheduleTarget, ThreadKey,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

// =============================================================================
// Synthetic TL builders & FakeRpcClient
// =============================================================================

type RecordedCall = (u32, Vec<u8>);
type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedCall>>>,
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

    fn push_updates(&self, updates: tl_enums::Updates) {
        let _ = self.updates_tx.send(updates);
    }

    fn recorded_calls(&self) -> Vec<RecordedCall> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn last_request<R: Deserializable + Identifiable>(&self) -> R {
        let calls = self.recorded_calls();
        let (_, bytes) = calls
            .iter()
            .rev()
            .find(|(ctor, _)| *ctor == R::CONSTRUCTOR_ID)
            .expect("expected matching RPC call to be recorded");
        // `RemoteCall` deserialization reads the leading 4-byte constructor ID.
        R::from_bytes(bytes).expect("deserialize recorded TL request")
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes(bytes[..4].try_into().unwrap_or([0; 4]));
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));

        let next = self.responses.lock().expect("lock responses").pop_front();
        match next {
            Some((expected, res)) => {
                assert_eq!(
                    ctor, expected,
                    "unexpected RPC constructor: got 0x{ctor:08x}, expected 0x{expected:08x}"
                );
                let raw = res?;
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            None => Err(InvocationError::Dropped),
        }
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.invoke(req).await
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("lock dc") = dc_id;
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn build_live_session() -> (FakeRpcClient, LiveSession) {
    let rpc = FakeRpcClient::new();
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId(100),
            crate::ErasedRpcClient::new(rpc.clone()),
            cfg,
        )
        .expect("start SessionRuntime"),
    );

    // Seed synthetic peers into `tg_data::Session`.
    let session_arc = Arc::clone(runtime.session());
    runtime.tokio_handle().block_on(async move {
        let mut s = session_arc.write().await;

        let mut self_u = User::new(DataUserId(100), true);
        self_u.first_name = "Self".to_owned();
        self_u.common.name = "Self Tester".to_owned();
        self_u.common.loaded_status = tg_data::LoadedStatus::Full;
        self_u.access_hash = 100_007;
        s.upsert_user(self_u, tg_data::PeerUpdateFlags::NAME);

        let mut peer_u = User::new(DataUserId(200), false);
        peer_u.first_name = "Peer".to_owned();
        peer_u.common.name = "Peer Two".to_owned();
        peer_u.common.loaded_status = tg_data::LoadedStatus::Full;
        peer_u.access_hash = 200_007;
        s.upsert_user(peer_u, tg_data::PeerUpdateFlags::NAME);

        let mut peer_u3 = User::new(DataUserId(300), false);
        peer_u3.first_name = "Mod".to_owned();
        peer_u3.common.name = "Mod Three".to_owned();
        peer_u3.common.loaded_status = tg_data::LoadedStatus::Full;
        peer_u3.access_hash = 300_007;
        s.upsert_user(peer_u3, tg_data::PeerUpdateFlags::NAME);

        let mut forum = Channel::new(ChannelId(500));
        forum.common.name = "Forum Five".to_owned();
        forum.common.loaded_status = tg_data::LoadedStatus::Full;
        forum.access_hash = 500_007;
        forum.flags = ChannelFlags::MEGAGROUP | ChannelFlags::FORUM;
        s.upsert_channel(forum, tg_data::PeerUpdateFlags::NAME);

        let mut channel = Channel::new(ChannelId(600));
        channel.common.name = "Broadcast Six".to_owned();
        channel.common.loaded_status = tg_data::LoadedStatus::Full;
        channel.access_hash = 600_007;
        channel.flags = ChannelFlags::BROADCAST;
        s.upsert_channel(channel, tg_data::PeerUpdateFlags::NAME);

        let mut disc = Channel::new(ChannelId(601));
        disc.common.name = "Discussion Group".to_owned();
        disc.common.loaded_status = tg_data::LoadedStatus::Full;
        disc.access_hash = 601_007;
        disc.flags = ChannelFlags::MEGAGROUP;
        s.upsert_channel(disc, tg_data::PeerUpdateFlags::NAME);

        let _ = Peer::User(Box::new(User::new(DataUserId(100), true)));
    });

    let live = LiveSession::new(runtime, 1);
    (rpc, live)
}

fn synthetic_tl_user(id: i64, name: &str) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: id == 100,
        contact: false,
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
        stories_unavailable: false,
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
        access_hash: Some(id * 1000 + 7),
        first_name: Some(name.to_owned()),
        last_name: None,
        username: None,
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

#[allow(clippy::too_many_arguments)]
fn synthetic_tl_message(
    id: i32,
    peer: tl_enums::Peer,
    from_uid: i64,
    out: bool,
    date: i32,
    text: &str,
    top_msg_id: Option<i32>,
    media: Option<tl_enums::MessageMedia>,
) -> tl_enums::Message {
    let reply_to = top_msg_id.map(|top| {
        tl_enums::MessageReplyHeader::MessageReplyHeader(tl_types::MessageReplyHeader {
            reply_to_scheduled: false,
            forum_topic: true,
            quote: false,
            reply_to_ephemeral: false,
            reply_to_msg_id: Some(top),
            reply_to_peer_id: None,
            reply_from: None,
            reply_media: None,
            reply_to_top_id: Some(top),
            quote_text: None,
            quote_entities: None,
            quote_offset: None,
            todo_item_id: None,
            poll_option: None,
            quote_rich: None,
        })
    });
    tl_enums::Message::Message(tl_types::Message {
        out,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: false,
        from_scheduled: false,
        legacy: false,
        edit_hide: false,
        pinned: false,
        noforwards: false,
        invert_media: false,
        offline: false,
        video_processing_pending: false,
        paid_suggested_post_stars: false,
        paid_suggested_post_ton: false,
        id,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: from_uid,
        })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to,
        date,
        message: text.to_owned(),
        media,
        reply_markup: None,
        entities: None,
        views: None,
        forwards: None,
        replies: None,
        edit_date: None,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id: None,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message: None,
    })
}

fn default_notify_settings() -> tl_enums::PeerNotifySettings {
    tl_enums::PeerNotifySettings::PeerNotifySettings(tl_types::PeerNotifySettings {
        show_previews: Some(true),
        silent: Some(false),
        mute_until: Some(0),
        ios_sound: None,
        android_sound: None,
        other_sound: None,
        stories_muted: None,
        stories_hide_sender: None,
        stories_ios_sound: None,
        stories_android_sound: None,
        stories_other_sound: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn synthetic_tl_forum_topic(
    id: i32,
    channel_id: i64,
    title: &str,
    color: i32,
    pinned: bool,
    closed: bool,
    top_msg: i32,
    unread_count: i32,
) -> tl_enums::ForumTopic {
    tl_enums::ForumTopic::ForumTopic(tl_types::ForumTopic {
        my: false,
        closed,
        pinned,
        short: false,
        hidden: false,
        title_missing: false,
        id,
        date: 1_795_340_000,
        peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id }),
        title: title.to_owned(),
        icon_color: color,
        icon_emoji_id: None,
        top_message: top_msg,
        read_inbox_max_id: top_msg - unread_count,
        read_outbox_max_id: top_msg,
        unread_count,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
        from_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 }),
        notify_settings: default_notify_settings(),
        draft: None,
    })
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_600,
        seq: 0,
    })
}

fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    pred()
}

// =============================================================================
// 1. Forum topics tests
// =============================================================================

#[test]
fn forum_topics_crud_pin_reorder_delete_and_replies() {
    let (rpc, mut live) = build_live_session();
    let forum_peer = PeerId(DataPeerId::from_channel(ChannelId(500)).0);
    let tl_forum_peer = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 });

    // 1. messages.getForumTopics
    rpc.enqueue_ok(
        tl_fns::messages::GetForumTopics::CONSTRUCTOR_ID,
        &tl_enums::messages::ForumTopics::ForumTopics(tl_types::messages::ForumTopics {
            order_by_create_date: false,
            count: 2,
            topics: vec![
                synthetic_tl_forum_topic(1, 500, "General", 0x6FB9F0, true, false, 10, 2),
                synthetic_tl_forum_topic(201, 500, "Build Notes", 0x8EEE98, false, false, 205, 0),
            ],
            messages: vec![
                synthetic_tl_message(
                    10,
                    tl_forum_peer.clone(),
                    200,
                    false,
                    1_795_341_000,
                    "Welcome to General",
                    Some(1),
                    None,
                ),
                synthetic_tl_message(
                    205,
                    tl_forum_peer.clone(),
                    200,
                    false,
                    1_795_341_200,
                    "Build passed",
                    Some(201),
                    None,
                ),
            ],
            chats: Vec::new(),
            users: vec![synthetic_tl_user(200, "Peer Two")],
            pts: 12,
        }),
    );

    let topics = live.forum_topics(forum_peer);
    assert_eq!(topics.len(), 2);
    assert_eq!(topics[0].root_id, MsgId(1));
    assert!(topics[0].is_general);
    assert!(topics[0].is_pinned);
    assert_eq!(topics[0].last_preview, "Welcome to General");
    assert_eq!(topics[1].root_id, MsgId(201));
    assert_eq!(topics[1].title, "Build Notes");

    // 2. messages.createForumTopic
    let create_svc = tl_enums::Message::Service(tl_types::MessageService {
        out: true,
        mentioned: false,
        media_unread: false,
        reactions_are_possible: false,
        silent: false,
        post: false,
        legacy: false,
        id: 301,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 })),
        peer_id: tl_forum_peer.clone(),
        saved_peer_id: None,
        reply_to: None,
        date: 1_795_341_500,
        action: tl_enums::MessageAction::TopicCreate(tl_types::MessageActionTopicCreate {
            title_missing: false,
            title: "Design".to_owned(),
            icon_color: FORUM_TOPIC_ICON_COLORS[2].0 as i32,
            icon_emoji_id: Some(555001),
        }),
        reactions: None,
        ttl_period: None,
    });
    rpc.enqueue_ok(
        tl_fns::messages::CreateForumTopic::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewChannelMessage(
                tl_types::UpdateNewChannelMessage {
                    message: create_svc,
                    pts: 13,
                    pts_count: 1,
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_341_500,
            seq: 1,
        }),
    );

    let created = live
        .create_forum_topic(
            forum_peer,
            "Design".to_owned(),
            FORUM_TOPIC_ICON_COLORS[2].0,
            Some("555001".to_owned()),
        )
        .expect("create_forum_topic");
    assert_eq!(created.root_id, MsgId(301));
    assert_eq!(created.title, "Design");
    assert_eq!(created.custom_emoji.as_deref(), Some("555001"));

    let create_req: tl_fns::messages::CreateForumTopic = rpc.last_request();
    assert_eq!(create_req.title, "Design");
    assert_eq!(create_req.icon_emoji_id, Some(555001));

    // 3. messages.editForumTopic (rename + icon + close + hide)
    rpc.enqueue_ok(
        tl_fns::messages::EditForumTopic::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    let edited = live
        .edit_forum_topic(
            forum_peer,
            MsgId(301),
            ForumTopicEdit {
                title: Some("Design v2".to_owned()),
                icon_emoji_id: Some(777002),
                closed: Some(true),
                hidden: Some(true),
            },
        )
        .expect("edit_forum_topic");
    assert_eq!(edited.title, "Design v2");
    assert_eq!(edited.custom_emoji.as_deref(), Some("777002"));
    assert!(edited.is_closed);
    assert!(live.is_forum_topic_hidden(forum_peer, MsgId(301)));

    let edit_req: tl_fns::messages::EditForumTopic = rpc.last_request();
    assert_eq!(edit_req.topic_id, 301);
    assert_eq!(edit_req.title.as_deref(), Some("Design v2"));
    assert_eq!(edit_req.icon_emoji_id, Some(777002));
    assert_eq!(edit_req.closed, Some(true));
    assert_eq!(edit_req.hidden, Some(true));

    // 4. messages.updatePinnedForumTopic + reorderPinnedForumTopics
    rpc.enqueue_ok(
        tl_fns::messages::UpdatePinnedForumTopic::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    let now_pinned = live
        .toggle_forum_topic_pin(forum_peer, MsgId(201))
        .expect("toggle_forum_topic_pin");
    assert!(now_pinned);

    rpc.enqueue_ok(
        tl_fns::messages::ReorderPinnedForumTopics::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.reorder_pinned_forum_topics(forum_peer, &[MsgId(201), MsgId(1)], true)
        .expect("reorder_pinned_forum_topics");
    let reorder_req: tl_fns::messages::ReorderPinnedForumTopics = rpc.last_request();
    assert!(reorder_req.force);
    assert_eq!(reorder_req.order, vec![201, 1]);

    let ordered = live.forum_topics(forum_peer);
    assert_eq!(ordered[0].root_id, MsgId(201));
    assert_eq!(ordered[1].root_id, MsgId(1));

    // 5. Load topic messages through messages.getReplies
    rpc.enqueue_ok(
        tl_fns::messages::GetReplies::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 14,
            count: 2,
            offset_id_offset: None,
            messages: vec![
                synthetic_tl_message(
                    205,
                    tl_forum_peer.clone(),
                    200,
                    false,
                    1_795_341_200,
                    "Second msg",
                    Some(201),
                    None,
                ),
                synthetic_tl_message(
                    202,
                    tl_forum_peer.clone(),
                    100,
                    true,
                    1_795_341_100,
                    "First msg",
                    Some(201),
                    None,
                ),
            ],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let topic_msgs = live.load_topic_messages(forum_peer, MsgId(201));
    assert_eq!(topic_msgs.len(), 2);
    assert_eq!(topic_msgs[0].id, MsgId(202));
    assert_eq!(topic_msgs[1].id, MsgId(205));
    assert_eq!(topic_msgs[0].extra.thread_id, Some(MsgId(201)));
    let replies_req: tl_fns::messages::GetReplies = rpc.last_request();
    assert_eq!(replies_req.msg_id, 201);

    // 6. messages.deleteTopicHistory
    rpc.enqueue_ok(
        tl_fns::messages::DeleteTopicHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 15,
                pts_count: 2,
                offset: 0,
            },
        ),
    );
    let pts = live
        .delete_topic_history(forum_peer, MsgId(301))
        .expect("delete_topic_history");
    assert_eq!(pts, 15);
    assert!(
        live.forum_topics(forum_peer)
            .iter()
            .all(|t| t.root_id != MsgId(301))
    );
}

// =============================================================================
// 2. Scheduled messages tests
// =============================================================================

#[test]
fn scheduled_messages_list_send_now_reschedule_delete() {
    let (rpc, mut live) = build_live_session();
    let peer = PeerId(DataPeerId::from_user(DataUserId(200)).0);
    let tl_peer = tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 });

    // 1. messages.getScheduledHistory
    rpc.enqueue_ok(
        tl_fns::messages::GetScheduledHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![
                synthetic_tl_message(
                    901,
                    tl_peer.clone(),
                    100,
                    true,
                    1_795_400_000,
                    "Later note",
                    None,
                    None,
                ),
                synthetic_tl_message(
                    902,
                    tl_peer.clone(),
                    100,
                    true,
                    1_795_350_000,
                    "Sooner note",
                    Some(201),
                    None,
                ),
            ],
            chats: Vec::new(),
            users: Vec::new(),
            topics: Vec::new(),
        }),
    );

    let all_sched = live.scheduled_messages(peer, None);
    assert_eq!(all_sched.len(), 2);
    // Sorted ascending by schedule timestamp.
    assert_eq!(all_sched[0].id, MsgId(902));
    assert_eq!(all_sched[0].extra.scheduled_date, Some(1_795_350_000));
    assert_eq!(all_sched[1].id, MsgId(901));

    let topic_sched = live.scheduled_messages(peer, Some(MsgId(201)));
    assert_eq!(topic_sched.len(), 1);
    assert_eq!(topic_sched[0].id, MsgId(902));

    // 2. Reschedule via messages.editMessage (schedule_date = WhenOnline)
    rpc.enqueue_ok(
        tl_fns::messages::EditMessage::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    let rescheduled = live
        .reschedule_message(peer, MsgId(901), ScheduleTarget::WhenOnline)
        .expect("reschedule_message");
    assert_eq!(
        rescheduled.extra.scheduled_date,
        Some(SCHEDULED_UNTIL_ONLINE_TIMESTAMP)
    );
    let edit_req: tl_fns::messages::EditMessage = rpc.last_request();
    assert_eq!(edit_req.id, 901);
    assert_eq!(
        edit_req.schedule_date,
        Some(SCHEDULED_UNTIL_ONLINE_TIMESTAMP)
    );

    // 3. Send immediately via messages.sendScheduledMessages
    rpc.enqueue_ok(
        tl_fns::messages::SendScheduledMessages::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![
                tl_enums::Update::DeleteScheduledMessages(
                    tl_types::UpdateDeleteScheduledMessages {
                        peer: tl_peer.clone(),
                        messages: vec![902],
                        sent_messages: Some(vec![1500]),
                    },
                ),
                tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                    message: synthetic_tl_message(
                        1500,
                        tl_peer.clone(),
                        100,
                        true,
                        1_795_341_600,
                        "Sooner note",
                        None,
                        None,
                    ),
                    pts: 20,
                    pts_count: 1,
                }),
            ],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_341_600,
            seq: 0,
        }),
    );
    let sent_now = live
        .send_scheduled_now(peer, MsgId(902))
        .expect("send_scheduled_now");
    assert_eq!(sent_now.id, MsgId(1500));
    assert_eq!(live.scheduled_messages(peer, None).len(), 1);

    // 4. Delete via messages.deleteScheduledMessages
    rpc.enqueue_ok(
        tl_fns::messages::DeleteScheduledMessages::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.delete_scheduled_messages(peer, &[MsgId(901)])
        .expect("delete_scheduled_messages");
    assert!(live.scheduled_messages(peer, None).is_empty());
    let del_req: tl_fns::messages::DeleteScheduledMessages = rpc.last_request();
    assert_eq!(del_req.id, vec![901]);
}

// =============================================================================
// 3. Channel comments / replies threads tests
// =============================================================================

#[test]
fn channel_comments_get_discussion_message_and_replies_and_send() {
    let (rpc, mut live) = build_live_session();
    let chan_peer = PeerId(DataPeerId::from_channel(ChannelId(600)).0);
    let disc_peer = PeerId(DataPeerId::from_channel(ChannelId(601)).0);
    let tl_disc_peer = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 601 });

    // 1. messages.getDiscussionMessage
    rpc.enqueue_ok(
        tl_fns::messages::GetDiscussionMessage::CONSTRUCTOR_ID,
        &tl_enums::messages::DiscussionMessage::DiscussionMessage(
            tl_types::messages::DiscussionMessage {
                messages: vec![synthetic_tl_message(
                    4001,
                    tl_disc_peer.clone(),
                    200,
                    false,
                    1_795_340_500,
                    "Channel announcement post",
                    None,
                    None,
                )],
                max_id: Some(4010),
                read_inbox_max_id: Some(4005),
                read_outbox_max_id: Some(4009),
                unread_count: 1,
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );

    // 2. messages.getReplies against linked discussion supergroup (channel 601, root 4001)
    rpc.enqueue_ok(
        tl_fns::messages::GetReplies::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 30,
            count: 2,
            offset_id_offset: None,
            messages: vec![
                synthetic_tl_message(
                    4010,
                    tl_disc_peer.clone(),
                    200,
                    false,
                    1_795_340_900,
                    "Second comment",
                    Some(4001),
                    None,
                ),
                synthetic_tl_message(
                    4005,
                    tl_disc_peer.clone(),
                    100,
                    true,
                    1_795_340_700,
                    "First comment",
                    Some(4001),
                    None,
                ),
            ],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let (root, comments) = live
        .replies_root_and_comments(chan_peer, MsgId(50))
        .expect("replies_root_and_comments");
    assert_eq!(root.text.text, "Channel announcement post");
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0].id, MsgId(4005));
    assert!(comments[0].is_read);
    assert_eq!(comments[1].id, MsgId(4010));

    let read_state = live
        .discussion_read_state(chan_peer, MsgId(50))
        .expect("discussion_read_state");
    assert_eq!(read_state.discussion_peer_id, disc_peer);
    assert_eq!(read_state.discussion_root_msg_id, MsgId(4001));
    assert_eq!(read_state.read_inbox_max_id, Some(MsgId(4005)));
    assert_eq!(read_state.unread_count, 1);

    // 3. Sending a reply in this comments thread routes to discussion group with top_msg_id = 4001
    rpc.enqueue_ok(
        tl_fns::messages::SendMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewChannelMessage(
                tl_types::UpdateNewChannelMessage {
                    message: synthetic_tl_message(
                        4011,
                        tl_disc_peer,
                        100,
                        true,
                        1_795_341_100,
                        "My comment",
                        Some(4001),
                        None,
                    ),
                    pts: 31,
                    pts_count: 1,
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_341_100,
            seq: 3,
        }),
    );
    let sent = live
        .send_message_extended(
            chan_peer,
            SendMessageExtended {
                text: TextWithEntities::simple("My comment"),
                thread_key: Some(ThreadKey::Replies {
                    peer_id: chan_peer,
                    root_msg_id: MsgId(50),
                }),
                ..SendMessageExtended::default()
            },
        )
        .expect("send comment");
    assert_eq!(sent.id, MsgId(4011));
    let send_req: tl_fns::messages::SendMessage = rpc.last_request();
    match send_req.peer {
        tl_enums::InputPeer::Channel(c) => assert_eq!(c.channel_id, 601),
        other => panic!("expected discussion InputPeer::Channel(601), got {other:?}"),
    }
    match send_req.reply_to {
        Some(tl_enums::InputReplyTo::Message(r)) => {
            assert_eq!(r.top_msg_id, Some(4001));
        }
        other => panic!("expected InputReplyTo::Message with top_msg_id=4001, got {other:?}"),
    }
}

// =============================================================================
// 4. Admin log tests (filters + admin selection)
// =============================================================================

#[test]
fn admin_log_filters_and_admin_selection() {
    let (rpc, live) = build_live_session();
    let chan_peer = PeerId(DataPeerId::from_channel(ChannelId(500)).0);
    let admin_peer = PeerId(DataPeerId::from_user(DataUserId(300)).0);

    // 1. channels.getParticipants(Admins)
    rpc.enqueue_ok(
        tl_fns::channels::GetParticipants::CONSTRUCTOR_ID,
        &tl_enums::channels::ChannelParticipants::ChannelParticipants(
            tl_types::channels::ChannelParticipants {
                count: 2,
                participants: vec![
                    tl_enums::ChannelParticipant::Creator(tl_types::ChannelParticipantCreator {
                        user_id: 100,
                        admin_rights: tl_enums::ChatAdminRights::ChatAdminRights(
                            tl_types::ChatAdminRights {
                                change_info: true,
                                post_messages: true,
                                edit_messages: true,
                                delete_messages: true,
                                ban_users: true,
                                invite_users: true,
                                pin_messages: true,
                                add_admins: true,
                                anonymous: false,
                                manage_call: true,
                                other: true,
                                manage_topics: true,
                                post_stories: true,
                                edit_stories: true,
                                delete_stories: true,
                                manage_direct_messages: true,
                                manage_ranks: true,
                                manage_linked_peers: true,
                                manage_welcome_messages: true,
                            },
                        ),
                        rank: Some("Founder".to_owned()),
                    }),
                    tl_enums::ChannelParticipant::Admin(tl_types::ChannelParticipantAdmin {
                        can_edit: true,
                        self_: false,
                        user_id: 300,
                        inviter_id: Some(100),
                        promoted_by: 100,
                        date: 1_795_300_000,
                        admin_rights: tl_enums::ChatAdminRights::ChatAdminRights(
                            tl_types::ChatAdminRights {
                                change_info: false,
                                post_messages: false,
                                edit_messages: false,
                                delete_messages: true,
                                ban_users: true,
                                invite_users: false,
                                pin_messages: true,
                                add_admins: false,
                                anonymous: false,
                                manage_call: false,
                                other: false,
                                manage_topics: true,
                                post_stories: false,
                                edit_stories: false,
                                delete_stories: false,
                                manage_direct_messages: false,
                                manage_ranks: false,
                                manage_linked_peers: false,
                                manage_welcome_messages: false,
                            },
                        ),
                        rank: Some("Moderator".to_owned()),
                    }),
                ],
                chats: Vec::new(),
                users: vec![synthetic_tl_user(300, "Mod Three")],
            },
        ),
    );

    let admins = live.admin_log_admins(chan_peer);
    assert_eq!(admins.len(), 2);
    assert_eq!(admins[0].role, "Founder");
    assert_eq!(admins[1].peer_id, admin_peer);
    assert_eq!(admins[1].role, "Moderator");

    // 2. channels.getAdminLog with event flags + admin filter + query
    let tl_chan_peer = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 });
    rpc.enqueue_ok(
        tl_fns::channels::GetAdminLog::CONSTRUCTOR_ID,
        &tl_enums::channels::AdminLogResults::AdminLogResults(
            tl_types::channels::AdminLogResults {
                events: vec![
                    tl_enums::ChannelAdminLogEvent::ChannelAdminLogEvent(
                        tl_types::ChannelAdminLogEvent {
                            id: 7001,
                            date: 1_795_341_000,
                            user_id: 300,
                            action: tl_enums::ChannelAdminLogEventAction::EditMessage(
                                tl_types::ChannelAdminLogEventActionEditMessage {
                                    prev_message: synthetic_tl_message(
                                        11,
                                        tl_chan_peer.clone(),
                                        300,
                                        false,
                                        1_795_340_900,
                                        "old text",
                                        None,
                                        None,
                                    ),
                                    new_message: synthetic_tl_message(
                                        11,
                                        tl_chan_peer,
                                        300,
                                        false,
                                        1_795_341_000,
                                        "new text",
                                        None,
                                        None,
                                    ),
                                },
                            ),
                        },
                    ),
                    tl_enums::ChannelAdminLogEvent::ChannelAdminLogEvent(
                        tl_types::ChannelAdminLogEvent {
                            id: 7002,
                            date: 1_795_341_100,
                            user_id: 300,
                            action: tl_enums::ChannelAdminLogEventAction::CreateTopic(
                                tl_types::ChannelAdminLogEventActionCreateTopic {
                                    topic: synthetic_tl_forum_topic(
                                        201,
                                        500,
                                        "Build Notes",
                                        0x8EEE98,
                                        false,
                                        false,
                                        201,
                                        0,
                                    ),
                                },
                            ),
                        },
                    ),
                ],
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );

    let filter = AdminLogFilter {
        flags: Some(AdminLogFilterFlags::EDIT.union(AdminLogFilterFlags::TOPICS)),
        admin_ids: Some(vec![admin_peer]),
        search_query: "Build".to_owned(),
    };
    let entries = live.admin_log_entries(chan_peer, &filter);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].flag, AdminLogFilterFlags::EDIT);
    assert_eq!(entries[0].before_text.as_deref(), Some("old text"));
    assert_eq!(entries[0].after_text.as_deref(), Some("new text"));
    assert_eq!(entries[1].flag, AdminLogFilterFlags::TOPICS);
    assert!(entries[1].summary.contains("Build Notes"));

    let log_req: tl_fns::channels::GetAdminLog = rpc.last_request();
    assert_eq!(log_req.q, "Build");
    let tl_enums::ChannelAdminLogEventsFilter::ChannelAdminLogEventsFilter(ef) =
        log_req.events_filter.expect("events_filter set");
    assert!(ef.edit);
    assert!(ef.forums);
    assert!(!ef.delete);
    assert_eq!(log_req.admins.as_ref().map(Vec::len), Some(1));
}

// =============================================================================
// 5. Saved Messages sublists tests
// =============================================================================

#[test]
fn saved_messages_sublists_history_delete_and_pins() {
    let (rpc, mut live) = build_live_session();
    let self_peer = PeerId(DataPeerId::from_user(DataUserId(100)).0);
    let sub_peer = PeerId(DataPeerId::from_user(DataUserId(200)).0);
    let tl_self_peer = tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 });
    let tl_sub_peer = tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 });

    // 1. messages.getPinnedSavedDialogs + messages.getSavedDialogs (exclude_pinned: true)
    rpc.enqueue_ok(
        tl_fns::messages::GetPinnedSavedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedDialogs::SavedDialogs(tl_types::messages::SavedDialogs {
            dialogs: vec![tl_enums::SavedDialog::SavedDialog(tl_types::SavedDialog {
                pinned: true,
                peer: tl_self_peer.clone(),
                top_message: 801,
            })],
            messages: vec![synthetic_tl_message(
                801,
                tl_self_peer.clone(),
                100,
                true,
                1_795_341_000,
                "Personal note",
                None,
                None,
            )],
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetSavedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedDialogs::SavedDialogs(tl_types::messages::SavedDialogs {
            dialogs: vec![tl_enums::SavedDialog::SavedDialog(tl_types::SavedDialog {
                pinned: false,
                peer: tl_sub_peer.clone(),
                top_message: 802,
            })],
            messages: vec![synthetic_tl_message(
                802,
                tl_self_peer.clone(),
                200,
                false,
                1_795_341_300,
                "Saved from peer two",
                None,
                None,
            )],
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let sublists = live.saved_sublists(self_peer);
    assert_eq!(sublists.len(), 2);
    assert!(sublists[0].is_my_notes);
    assert!(sublists[0].is_pinned);
    assert_eq!(sublists[0].title, "My Notes");
    assert_eq!(sublists[1].sublist_peer_id, sub_peer);
    assert_eq!(sublists[1].last_preview, "Saved from peer two");

    // 2. messages.getSavedHistory
    rpc.enqueue_ok(
        tl_fns::messages::GetSavedHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![
                synthetic_tl_message(
                    802,
                    tl_self_peer.clone(),
                    200,
                    false,
                    1_795_341_300,
                    "Saved from peer two",
                    None,
                    None,
                ),
                synthetic_tl_message(
                    800,
                    tl_self_peer,
                    200,
                    false,
                    1_795_340_900,
                    "Earlier saved note",
                    None,
                    None,
                ),
            ],
            chats: Vec::new(),
            users: Vec::new(),
            topics: Vec::new(),
        }),
    );

    let saved_msgs = live.load_saved_sublist_messages(self_peer, sub_peer);
    assert_eq!(saved_msgs.len(), 2);
    assert_eq!(saved_msgs[0].id, MsgId(800));
    assert_eq!(saved_msgs[1].id, MsgId(802));
    assert_eq!(saved_msgs[0].extra.saved_from_peer, Some(sub_peer));

    // 3. messages.toggleSavedDialogPin + reorderPinnedSavedDialogs
    rpc.enqueue_ok(
        tl_fns::messages::ToggleSavedDialogPin::CONSTRUCTOR_ID,
        &true,
    );
    assert!(
        live.toggle_saved_dialog_pin(self_peer, sub_peer, true)
            .expect("toggle_saved_dialog_pin")
    );
    rpc.enqueue_ok(
        tl_fns::messages::ReorderPinnedSavedDialogs::CONSTRUCTOR_ID,
        &true,
    );
    live.reorder_pinned_saved_dialogs(self_peer, &[sub_peer, self_peer], true)
        .expect("reorder_pinned_saved_dialogs");
    let reordered = live.saved_sublists(self_peer);
    assert_eq!(reordered[0].sublist_peer_id, sub_peer);
    assert_eq!(reordered[1].sublist_peer_id, self_peer);

    // 4. messages.deleteSavedHistory
    rpc.enqueue_ok(
        tl_fns::messages::DeleteSavedHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 42,
                pts_count: 2,
                offset: 0,
            },
        ),
    );
    let pts = live
        .delete_saved_history(self_peer, sub_peer, MsgId(0))
        .expect("delete_saved_history");
    assert_eq!(pts, 42);
    assert!(
        live.saved_sublists(self_peer)
            .iter()
            .all(|s| s.sublist_peer_id != sub_peer)
    );
}

// =============================================================================
// 6. Send poll (quiz, multiple choice, close_period), forward options, rich draft
// =============================================================================

#[test]
fn send_poll_quiz_and_close_period() {
    let (rpc, mut live) = build_live_session();
    let peer = PeerId(DataPeerId::from_user(DataUserId(200)).0);
    let tl_peer = tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 });

    let server_poll_media = tl_enums::MessageMedia::Poll(tl_types::MessageMediaPoll {
        poll: tl_enums::Poll::Poll(tl_types::Poll {
            id: 99_001,
            closed: false,
            public_voters: false,
            multiple_choice: false,
            quiz: true,
            open_answers: false,
            revoting_disabled: false,
            shuffle_answers: false,
            hide_results_until_close: false,
            creator: true,
            subscribers_only: false,
            question: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: "2 + 2 = ?".to_owned(),
                entities: Vec::new(),
            }),
            answers: vec![
                tl_enums::PollAnswer::PollAnswer(tl_types::PollAnswer {
                    text: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "3".to_owned(),
                            entities: Vec::new(),
                        },
                    ),
                    option: vec![0],
                    media: None,
                    added_by: None,
                    date: None,
                }),
                tl_enums::PollAnswer::PollAnswer(tl_types::PollAnswer {
                    text: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "4".to_owned(),
                            entities: Vec::new(),
                        },
                    ),
                    option: vec![1],
                    media: None,
                    added_by: None,
                    date: None,
                }),
            ],
            close_period: Some(45),
            close_date: None,
            countries_iso2: None,
            hash: 0,
        }),
        results: Box::new(tl_enums::PollResults::PollResults(tl_types::PollResults {
            min: false,
            has_unread_votes: false,
            can_view_stats: false,
            results: None,
            total_voters: Some(0),
            recent_voters: None,
            solution: Some("Basic arithmetic".to_owned()),
            solution_entities: Some(Vec::new()),
            solution_media: None,
        })),
        attached_media: None,
    });

    rpc.enqueue_ok(
        tl_fns::messages::SendMedia::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message: synthetic_tl_message(
                    501,
                    tl_peer,
                    100,
                    true,
                    1_795_341_600,
                    "",
                    None,
                    Some(server_poll_media),
                ),
                pts: 50,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_341_600,
            seq: 4,
        }),
    );

    let msg = live
        .send_poll_with_options(
            peer,
            None,
            CreatePollRequest {
                question: "2 + 2 = ?".to_owned(),
                options: vec!["3".to_owned(), "4".to_owned()],
                is_anonymous: true,
                multiple_choice: false,
                is_quiz: true,
                correct_option: Some(1),
                explanation: Some("Basic arithmetic".to_owned()),
                silent: true,
                schedule_date: None,
            },
            LivePollOptions {
                close_period: Some(45),
                close_date: None,
            },
        )
        .expect("send_poll_with_options");

    assert_eq!(msg.id, MsgId(501));
    assert!(matches!(msg.media, Some(HistoryMedia::Poll(ref p)) if p.is_quiz));

    let send_req: tl_fns::messages::SendMedia = rpc.last_request();
    assert!(send_req.silent);
    match send_req.media {
        tl_enums::InputMedia::Poll(imp) => {
            assert_eq!(imp.correct_answers, Some(vec![1]));
            assert_eq!(imp.solution.as_deref(), Some("Basic arithmetic"));
            let tl_enums::Poll::Poll(p) = imp.poll;
            assert!(p.quiz);
            assert!(!p.public_voters);
            assert!(!p.multiple_choice);
            assert_eq!(p.close_period, Some(45));
        }
        other => panic!("expected InputMedia::Poll, got {other:?}"),
    }

    // Quiz + multiple_choice must be rejected locally without issuing an RPC.
    let before_calls = rpc.recorded_calls().len();
    let err = live
        .send_poll(
            peer,
            None,
            CreatePollRequest {
                question: "Invalid quiz".to_owned(),
                options: vec!["A".to_owned(), "B".to_owned()],
                is_anonymous: true,
                multiple_choice: true,
                is_quiz: true,
                correct_option: Some(0),
                explanation: None,
                silent: false,
                schedule_date: None,
            },
        )
        .expect_err("quiz + multiple_choice should fail validation");
    assert!(err.contains("multiple answers"));
    assert_eq!(rpc.recorded_calls().len(), before_calls);
}

#[test]
fn forward_messages_with_options_and_rich_draft_roundtrip() {
    let (rpc, mut live) = build_live_session();
    let from_peer = PeerId(DataPeerId::from_user(DataUserId(200)).0);
    let to_peer = PeerId(DataPeerId::from_channel(ChannelId(500)).0);
    let tl_to_peer = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 });

    // 1. forward_messages_with_options (drop_author + drop_media_captions)
    rpc.enqueue_ok(
        tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewChannelMessage(
                tl_types::UpdateNewChannelMessage {
                    message: synthetic_tl_message(
                        610,
                        tl_to_peer.clone(),
                        100,
                        true,
                        1_795_341_700,
                        "Forwarded copy",
                        None,
                        None,
                    ),
                    pts: 60,
                    pts_count: 1,
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_341_700,
            seq: 0,
        }),
    );

    let forwarded = live
        .forward_messages_with_options(
            from_peer,
            &[MsgId(42)],
            to_peer,
            ForwardOptions {
                drop_author: true,
                drop_media_captions: true,
            },
        )
        .expect("forward_messages_with_options");
    assert_eq!(forwarded.len(), 1);
    assert_eq!(forwarded[0].id, MsgId(610));

    let fwd_req: tl_fns::messages::ForwardMessages = rpc.last_request();
    assert!(fwd_req.drop_author);
    assert!(fwd_req.drop_media_captions);
    assert_eq!(fwd_req.id, vec![42]);

    // 2. forward_messages_extended (silent + schedule_date + top_msg_id)
    rpc.enqueue_ok(
        tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewScheduledMessage(
                tl_types::UpdateNewScheduledMessage {
                    message: synthetic_tl_message(
                        915,
                        tl_to_peer,
                        100,
                        true,
                        1_795_400_000,
                        "Scheduled forward",
                        Some(201),
                        None,
                    ),
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_341_710,
            seq: 6,
        }),
    );

    let sched_fwd = live
        .forward_messages_extended(
            from_peer,
            &[MsgId(43)],
            to_peer,
            LiveForwardOptions {
                drop_author: true,
                drop_media_captions: false,
                silent: true,
                schedule_date: Some(1_795_400_000),
                top_msg_id: Some(MsgId(201)),
                send_as: None,
            },
        )
        .expect("forward_messages_extended");
    assert_eq!(sched_fwd.len(), 1);
    assert_eq!(sched_fwd[0].id, MsgId(915));

    let ext_fwd_req: tl_fns::messages::ForwardMessages = rpc.last_request();
    assert!(ext_fwd_req.silent);
    assert!(ext_fwd_req.drop_author);
    assert_eq!(ext_fwd_req.schedule_date, Some(1_795_400_000));
    assert_eq!(ext_fwd_req.top_msg_id, Some(201));

    // 3. save_rich_draft & load_rich_draft (preserving UTF-16 bold entity + quote + top_msg_id)
    rpc.enqueue_ok(tl_fns::messages::SaveDraft::CONSTRUCTOR_ID, &true);
    let thread = ThreadKey::Topic {
        peer_id: to_peer,
        topic_root_id: MsgId(201),
    };
    let draft = RichThreadDraft {
        text: TextWithEntities {
            text: "Hello bold world".to_owned(),
            entities: vec![EntityInText::new(EntityType::Bold, 6, 4)],
        },
        reply_to: Some(MsgId(205)),
        reply_quote: Some(ReplyQuote {
            text: "Build".to_owned(),
            entities: Vec::new(),
            offset_utf16: 0,
        }),
        no_webpage: true,
    };
    live.save_rich_draft(thread, Some(draft.clone()));
    // `messages.saveDraft` goes out from the background queue.
    assert!(live.flush_background_rpcs(std::time::Duration::from_secs(10)));

    let draft_req: tl_fns::messages::SaveDraft = rpc.last_request();
    assert!(draft_req.no_webpage);
    assert_eq!(draft_req.message, "Hello bold world");
    let ents = draft_req.entities.expect("entities preserved in SaveDraft");
    assert_eq!(ents.len(), 1);
    assert!(
        matches!(ents[0], tl_enums::MessageEntity::Bold(ref b) if b.offset == 6 && b.length == 4)
    );
    match draft_req.reply_to {
        Some(tl_enums::InputReplyTo::Message(r)) => {
            assert_eq!(r.reply_to_msg_id, 205);
            assert_eq!(r.top_msg_id, Some(201));
            assert_eq!(r.quote_text.as_deref(), Some("Build"));
        }
        other => panic!("expected InputReplyTo::Message, got {other:?}"),
    }

    let loaded = live.load_rich_draft(thread).expect("load_rich_draft");
    assert_eq!(loaded, draft);
}

// =============================================================================
// 7. Live real-time updates tests
// =============================================================================

#[test]
fn real_time_updates_for_scheduled_polls_topics_and_discussion_read() {
    let (rpc, mut live) = build_live_session();
    let user_peer = PeerId(DataPeerId::from_user(DataUserId(200)).0);
    let forum_peer = PeerId(DataPeerId::from_channel(ChannelId(500)).0);
    let chan_peer = PeerId(DataPeerId::from_channel(ChannelId(600)).0);
    let tl_user_peer = tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 });
    let tl_forum_peer = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 });
    let tl_disc_peer = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 601 });

    // Prime empty scheduled list, forum topics, a poll message, and a discussion thread.
    rpc.enqueue_ok(
        tl_fns::messages::GetScheduledHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            topics: Vec::new(),
        }),
    );
    assert!(live.scheduled_messages(user_peer, None).is_empty());

    rpc.enqueue_ok(
        tl_fns::messages::GetForumTopics::CONSTRUCTOR_ID,
        &tl_enums::messages::ForumTopics::ForumTopics(tl_types::messages::ForumTopics {
            order_by_create_date: false,
            count: 2,
            topics: vec![
                synthetic_tl_forum_topic(1, 500, "General", 0x6FB9F0, false, false, 10, 3),
                synthetic_tl_forum_topic(201, 500, "Topics Test", 0x8EEE98, false, false, 205, 2),
            ],
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            pts: 1,
        }),
    );
    assert_eq!(live.forum_topics(forum_peer).len(), 2);

    // Send a poll so it's stored in `Session` and indexed by `poll_id = 88_001`.
    let poll_media = tl_enums::MessageMedia::Poll(tl_types::MessageMediaPoll {
        poll: tl_enums::Poll::Poll(tl_types::Poll {
            id: 88_001,
            closed: false,
            public_voters: true,
            multiple_choice: false,
            quiz: false,
            open_answers: false,
            revoting_disabled: false,
            shuffle_answers: false,
            hide_results_until_close: false,
            creator: true,
            subscribers_only: false,
            question: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: "Favorite color?".to_owned(),
                entities: Vec::new(),
            }),
            answers: vec![
                tl_enums::PollAnswer::PollAnswer(tl_types::PollAnswer {
                    text: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "Blue".to_owned(),
                            entities: Vec::new(),
                        },
                    ),
                    option: vec![0],
                    media: None,
                    added_by: None,
                    date: None,
                }),
                tl_enums::PollAnswer::PollAnswer(tl_types::PollAnswer {
                    text: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "Green".to_owned(),
                            entities: Vec::new(),
                        },
                    ),
                    option: vec![1],
                    media: None,
                    added_by: None,
                    date: None,
                }),
            ],
            close_period: None,
            close_date: None,
            countries_iso2: None,
            hash: 0,
        }),
        results: Box::new(tl_enums::PollResults::PollResults(tl_types::PollResults {
            min: false,
            has_unread_votes: false,
            can_view_stats: false,
            results: Some(vec![tl_enums::PollAnswerVoters::PollAnswerVoters(
                tl_types::PollAnswerVoters {
                    chosen: true,
                    correct: false,
                    option: vec![0],
                    voters: Some(1),
                    recent_voters: None,
                },
            )]),
            total_voters: Some(1),
            recent_voters: None,
            solution: None,
            solution_entities: None,
            solution_media: None,
        })),
        attached_media: None,
    });
    rpc.enqueue_ok(
        tl_fns::messages::SendMedia::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message: synthetic_tl_message(
                    555,
                    tl_user_peer.clone(),
                    100,
                    true,
                    1_795_341_600,
                    "",
                    None,
                    Some(poll_media),
                ),
                pts: 5,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_341_600,
            seq: 0,
        }),
    );
    let _ = live
        .send_poll(
            user_peer,
            None,
            CreatePollRequest {
                question: "Favorite color?".to_owned(),
                options: vec!["Blue".to_owned(), "Green".to_owned()],
                is_anonymous: false,
                multiple_choice: false,
                is_quiz: false,
                correct_option: None,
                explanation: None,
                silent: false,
                schedule_date: None,
            },
        )
        .expect("send poll");

    // Prime a discussion thread so ReadChannelDiscussionInbox/Outbox updates apply to it.
    rpc.enqueue_ok(
        tl_fns::messages::GetDiscussionMessage::CONSTRUCTOR_ID,
        &tl_enums::messages::DiscussionMessage::DiscussionMessage(
            tl_types::messages::DiscussionMessage {
                messages: vec![synthetic_tl_message(
                    5001,
                    tl_disc_peer.clone(),
                    200,
                    false,
                    1_795_340_000,
                    "Post root",
                    None,
                    None,
                )],
                max_id: Some(5010),
                read_inbox_max_id: Some(5001),
                read_outbox_max_id: Some(5001),
                unread_count: 2,
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetReplies::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 10,
            count: 1,
            offset_id_offset: None,
            messages: vec![synthetic_tl_message(
                5010,
                tl_disc_peer,
                100,
                true,
                1_795_340_800,
                "My outgoing comment",
                Some(5001),
                None,
            )],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    assert!(
        live.replies_root_and_comments(chan_peer, MsgId(77))
            .is_some()
    );

    // Push real-time updates over broadcast stream!
    rpc.push_updates(tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![
            // a) updateNewScheduledMessage
            tl_enums::Update::NewScheduledMessage(tl_types::UpdateNewScheduledMessage {
                message: synthetic_tl_message(
                    950,
                    tl_user_peer.clone(),
                    100,
                    true,
                    1_795_500_000,
                    "Pushed scheduled msg",
                    None,
                    None,
                ),
            }),
            // b) updateMessagePoll (min: true must preserve chosen: true on option [0]!)
            tl_enums::Update::MessagePoll(tl_types::UpdateMessagePoll {
                peer: None,
                msg_id: None,
                top_msg_id: None,
                poll_id: 88_001,
                poll: None,
                results: tl_enums::PollResults::PollResults(tl_types::PollResults {
                    min: true,
                    has_unread_votes: false,
                    can_view_stats: false,
                    results: Some(vec![
                        tl_enums::PollAnswerVoters::PollAnswerVoters(tl_types::PollAnswerVoters {
                            chosen: false, // ignored because min = true
                            correct: false,
                            option: vec![0],
                            voters: Some(2),
                            recent_voters: None,
                        }),
                        tl_enums::PollAnswerVoters::PollAnswerVoters(tl_types::PollAnswerVoters {
                            chosen: false,
                            correct: false,
                            option: vec![1],
                            voters: Some(3),
                            recent_voters: None,
                        }),
                    ]),
                    total_voters: Some(5),
                    recent_voters: None,
                    solution: None,
                    solution_entities: None,
                    solution_media: None,
                }),
            }),
            // c) updateMessagePollVote
            tl_enums::Update::MessagePollVote(tl_types::UpdateMessagePollVote {
                poll_id: 88_001,
                peer: tl_user_peer.clone(),
                options: vec![vec![1]],
                positions: vec![1],
                qts: 1,
            }),
            // d) updatePinnedForumTopic + updatePinnedForumTopics
            tl_enums::Update::PinnedForumTopic(tl_types::UpdatePinnedForumTopic {
                pinned: true,
                peer: tl_forum_peer.clone(),
                topic_id: 201,
            }),
            tl_enums::Update::PinnedForumTopics(tl_types::UpdatePinnedForumTopics {
                peer: tl_forum_peer.clone(),
                order: Some(vec![201, 1]),
            }),
            // e) updateReadChannelDiscussionInbox (for both discussion group & forum topic)
            tl_enums::Update::ReadChannelDiscussionInbox(
                tl_types::UpdateReadChannelDiscussionInbox {
                    channel_id: 601,
                    top_msg_id: 5001,
                    read_max_id: 5010,
                    broadcast_id: Some(600),
                    broadcast_post: Some(77),
                },
            ),
            tl_enums::Update::ReadChannelDiscussionInbox(
                tl_types::UpdateReadChannelDiscussionInbox {
                    channel_id: 500,
                    top_msg_id: 201,
                    read_max_id: 205,
                    broadcast_id: None,
                    broadcast_post: None,
                },
            ),
            // f) updateReadChannelDiscussionOutbox
            tl_enums::Update::ReadChannelDiscussionOutbox(
                tl_types::UpdateReadChannelDiscussionOutbox {
                    channel_id: 601,
                    top_msg_id: 5001,
                    read_max_id: 5010,
                },
            ),
        ],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_800,
        seq: 2,
    }));

    let arrived = wait_until(Duration::from_secs(3), || {
        live.scheduled_messages(user_peer, None)
            .iter()
            .any(|m| m.id == MsgId(950))
            && live
                .discussion_read_state(chan_peer, MsgId(77))
                .is_some_and(|rs| rs.read_outbox_max_id == Some(MsgId(5010)))
    });
    assert!(
        arrived,
        "background updates listener did not process updates in time"
    );

    // Verify poll update merged total_voters=5 and preserved chosen=true under min=true!
    let slice = live.load_slice_around(user_peer, MsgId(555), 5, 5);
    let poll_msg = slice
        .messages
        .iter()
        .find(|m| m.id == MsgId(555))
        .expect("poll message in history");
    match &poll_msg.media {
        Some(HistoryMedia::Poll(p)) => {
            assert_eq!(p.total_voters, 5);
            assert_eq!(p.options[0].votes, 2);
            assert!(
                p.options[0].chosen,
                "min=true poll update must preserve chosen=true"
            );
            assert_eq!(p.options[1].votes, 3);
        }
        other => panic!("expected Poll media, got {other:?}"),
    }
    assert_eq!(live.poll_recent_voters(88_001, &[1]), vec![user_peer]);

    // Verify forum topic pin order and unread cleared by ReadChannelDiscussionInbox.
    let topics = live.forum_topics(forum_peer);
    assert_eq!(topics[0].root_id, MsgId(201));
    assert!(topics[0].is_pinned);
    assert_eq!(topics[0].unread_count, 0);

    // Verify discussion inbox + outbox read state and outgoing comment marked read.
    let rs = live
        .discussion_read_state(chan_peer, MsgId(77))
        .expect("discussion_read_state");
    assert_eq!(rs.read_inbox_max_id, Some(MsgId(5010)));
    assert_eq!(rs.unread_count, 0);
    assert_eq!(rs.read_outbox_max_id, Some(MsgId(5010)));
    let (_, comments) = live
        .replies_root_and_comments(chan_peer, MsgId(77))
        .expect("comments");
    assert!(comments[0].is_read);

    // Push updateDeleteScheduledMessages and verify removal!
    rpc.push_updates(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::DeleteScheduledMessages(
            tl_types::UpdateDeleteScheduledMessages {
                peer: tl_user_peer,
                messages: vec![950],
                sent_messages: None,
            },
        ),
        date: 1_795_341_900,
    }));
    let deleted = wait_until(Duration::from_secs(3), || {
        live.scheduled_messages(user_peer, None).is_empty()
    });
    assert!(
        deleted,
        "updateDeleteScheduledMessages did not remove scheduled msg"
    );

    // Push NewChannelMessage(TopicEdit) and verify live topic rename/close/hide.
    let topic_edit_svc = tl_enums::Message::Service(tl_types::MessageService {
        out: false,
        mentioned: false,
        media_unread: false,
        reactions_are_possible: false,
        silent: false,
        post: false,
        legacy: false,
        id: 210,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 })),
        peer_id: tl_forum_peer,
        saved_peer_id: None,
        reply_to: Some(tl_enums::MessageReplyHeader::MessageReplyHeader(
            tl_types::MessageReplyHeader {
                reply_to_scheduled: false,
                forum_topic: true,
                quote: false,
                reply_to_ephemeral: false,
                reply_to_msg_id: Some(201),
                reply_to_peer_id: None,
                reply_from: None,
                reply_media: None,
                reply_to_top_id: Some(201),
                quote_text: None,
                quote_entities: None,
                quote_offset: None,
                todo_item_id: None,
                poll_option: None,
                quote_rich: None,
            },
        )),
        date: 1_795_342_000,
        action: tl_enums::MessageAction::TopicEdit(tl_types::MessageActionTopicEdit {
            title: Some("Renamed Live".to_owned()),
            icon_emoji_id: Some(888001),
            closed: Some(true),
            hidden: Some(true),
        }),
        reactions: None,
        ttl_period: None,
    });
    rpc.push_updates(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::NewChannelMessage(tl_types::UpdateNewChannelMessage {
            message: topic_edit_svc,
            pts: 100,
            pts_count: 1,
        }),
        date: 1_795_342_000,
    }));
    let topic_updated = wait_until(Duration::from_secs(3), || {
        live.forum_topics(forum_peer)
            .iter()
            .any(|t| t.root_id == MsgId(201) && t.title == "Renamed Live" && t.is_closed)
    });
    assert!(
        topic_updated,
        "TopicEdit update did not update cached forum topic"
    );
    assert!(live.is_forum_topic_hidden(forum_peer, MsgId(201)));

    // Push updateEditForumTopic (Layer 233 dedicated update) and verify rename/unhide/reopen.
    rpc.push_updates(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::EditForumTopic(tl_types::UpdateEditForumTopic {
            peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 }),
            topic_id: 201,
            title: Some("Edited Via Update".to_owned()),
            icon_emoji_id: Some(999_001),
            closed: Some(false),
            hidden: Some(false),
        }),
        date: 1_795_342_050,
    }));
    let edit_upd = wait_until(Duration::from_secs(3), || {
        live.forum_topics(forum_peer).iter().any(|t| {
            t.root_id == MsgId(201)
                && t.title == "Edited Via Update"
                && !t.is_closed
                && t.custom_emoji.as_deref() == Some("999001")
        })
    });
    assert!(
        edit_upd,
        "updateEditForumTopic did not update cached forum topic"
    );
    assert!(!live.is_forum_topic_hidden(forum_peer, MsgId(201)));

    // Push updateDraftMessage for topic 201 and verify rich_thread_drafts & topic.draft_preview sync.
    rpc.push_updates(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::DraftMessage(tl_types::UpdateDraftMessage {
            peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 }),
            top_msg_id: Some(201),
            saved_peer_id: None,
            draft: tl_enums::DraftMessage::DraftMessage(tl_types::DraftMessage {
                no_webpage: true,
                invert_media: false,
                reply_to: None,
                message: "Remote draft".to_owned(),
                entities: Some(vec![tl_enums::MessageEntity::Italic(
                    tl_types::MessageEntityItalic {
                        offset: 0,
                        length: 6,
                    },
                )]),
                date: 1_795_342_100,
                media: None,
                effect: None,
                suggested_post: None,
                rich_message: None,
            }),
        }),
        date: 1_795_342_100,
    }));
    let topic_key = ThreadKey::Topic {
        peer_id: forum_peer,
        topic_root_id: MsgId(201),
    };
    let draft_synced = wait_until(Duration::from_secs(3), || {
        live.load_rich_draft(topic_key)
            .is_some_and(|d| d.text.text == "Remote draft" && d.no_webpage)
    });
    assert!(draft_synced, "updateDraftMessage did not sync topic draft");
    let loaded_remote = live.load_rich_draft(topic_key).expect("remote topic draft");
    assert_eq!(loaded_remote.text.entities.len(), 1);
    assert_eq!(
        loaded_remote.text.entities[0].entity_type(),
        EntityType::Italic
    );
    assert!(
        live.forum_topics(forum_peer)
            .iter()
            .any(|t| t.root_id == MsgId(201) && t.draft_preview.as_deref() == Some("Remote draft"))
    );
}

#[test]
fn send_multiple_choice_poll_and_admin_log_empty_admins_short_circuit() {
    let (rpc, mut live) = build_live_session();
    let forum_peer = PeerId(DataPeerId::from_channel(ChannelId(500)).0);
    let tl_forum_peer = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 });

    // Empty admin_ids filter must return empty locally without invoking channels.getAdminLog.
    let before = rpc.recorded_calls().len();
    let empty_entries = live.admin_log_entries(
        forum_peer,
        &AdminLogFilter {
            flags: None,
            admin_ids: Some(Vec::new()),
            search_query: String::new(),
        },
    );
    assert!(empty_entries.is_empty());
    assert_eq!(rpc.recorded_calls().len(), before);

    // Send a multiple-choice non-anonymous poll inside a forum topic (top_msg_id = 201).
    rpc.enqueue_ok(
        tl_fns::messages::SendMedia::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewChannelMessage(
                tl_types::UpdateNewChannelMessage {
                    message: synthetic_tl_message(
                        701,
                        tl_forum_peer,
                        100,
                        true,
                        1_795_342_200,
                        "",
                        Some(201),
                        None,
                    ),
                    pts: 70,
                    pts_count: 1,
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_342_200,
            seq: 1,
        }),
    );

    let msg = live
        .send_poll(
            forum_peer,
            Some(ThreadKey::Topic {
                peer_id: forum_peer,
                topic_root_id: MsgId(201),
            }),
            CreatePollRequest {
                question: "Pick days".to_owned(),
                options: vec!["Mon".to_owned(), "Tue".to_owned(), "Wed".to_owned()],
                is_anonymous: false,
                multiple_choice: true,
                is_quiz: false,
                correct_option: None,
                explanation: None,
                silent: false,
                schedule_date: None,
            },
        )
        .expect("send multiple-choice poll");
    assert_eq!(msg.id, MsgId(701));

    let send_req: tl_fns::messages::SendMedia = rpc.last_request();
    match send_req.reply_to {
        Some(tl_enums::InputReplyTo::Message(r)) => assert_eq!(r.top_msg_id, Some(201)),
        other => panic!("expected topic InputReplyTo::Message, got {other:?}"),
    }
    match send_req.media {
        tl_enums::InputMedia::Poll(imp) => {
            assert_eq!(imp.correct_answers, None);
            let tl_enums::Poll::Poll(p) = imp.poll;
            assert!(!p.quiz);
            assert!(p.public_voters);
            assert!(p.multiple_choice);
            assert_eq!(p.answers.len(), 3);
        }
        other => panic!("expected InputMedia::Poll, got {other:?}"),
    }
}

#[test]
fn saved_sublist_realtime_updates_and_delete_messages() {
    let (rpc, live) = build_live_session();
    let self_peer = PeerId(DataPeerId::from_user(DataUserId(100)).0);
    let sub_peer = PeerId(DataPeerId::from_user(DataUserId(200)).0);
    let tl_self_peer = tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 });
    let tl_sub_peer = tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 });

    // Prime saved sublists and sublist history for "My Notes" (self_peer).
    rpc.enqueue_ok(
        tl_fns::messages::GetPinnedSavedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedDialogs::SavedDialogs(tl_types::messages::SavedDialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetSavedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedDialogs::SavedDialogs(tl_types::messages::SavedDialogs {
            dialogs: vec![
                tl_enums::SavedDialog::SavedDialog(tl_types::SavedDialog {
                    pinned: false,
                    peer: tl_self_peer.clone(),
                    top_message: 901,
                }),
                tl_enums::SavedDialog::SavedDialog(tl_types::SavedDialog {
                    pinned: false,
                    peer: tl_sub_peer.clone(),
                    top_message: 902,
                }),
            ],
            messages: vec![
                synthetic_tl_message(
                    901,
                    tl_self_peer.clone(),
                    100,
                    true,
                    1_795_341_000,
                    "Note one",
                    None,
                    None,
                ),
                synthetic_tl_message(
                    902,
                    tl_self_peer.clone(),
                    200,
                    false,
                    1_795_341_100,
                    "Saved peer two",
                    None,
                    None,
                ),
            ],
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    assert_eq!(live.saved_sublists(self_peer).len(), 2);

    rpc.enqueue_ok(
        tl_fns::messages::GetSavedHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![synthetic_tl_message(
                901,
                tl_self_peer.clone(),
                100,
                true,
                1_795_341_000,
                "Note one",
                None,
                None,
            )],
            chats: Vec::new(),
            users: Vec::new(),
            topics: Vec::new(),
        }),
    );
    assert_eq!(
        live.load_saved_sublist_messages(self_peer, self_peer).len(),
        1
    );

    // Push UpdateSavedDialogPinned + UpdateNewMessage (into My Notes) + UpdateDeleteMessages.
    rpc.push_updates(tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![
            tl_enums::Update::SavedDialogPinned(tl_types::UpdateSavedDialogPinned {
                pinned: true,
                peer: tl_enums::DialogPeer::DialogPeer(tl_types::DialogPeer { peer: tl_sub_peer }),
            }),
            tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message: synthetic_tl_message(
                    905,
                    tl_self_peer,
                    100,
                    true,
                    1_795_341_500,
                    "Live note two",
                    None,
                    None,
                ),
                pts: 80,
                pts_count: 1,
            }),
        ],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_500,
        seq: 1,
    }));

    let arrived = wait_until(Duration::from_secs(3), || {
        live.load_saved_sublist_messages(self_peer, self_peer)
            .iter()
            .any(|m| m.id == MsgId(905))
            && live
                .saved_sublists(self_peer)
                .first()
                .is_some_and(|s| s.sublist_peer_id == sub_peer && s.is_pinned)
    });
    assert!(arrived, "saved sublist updates did not apply in time");

    rpc.push_updates(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::DeleteMessages(tl_types::UpdateDeleteMessages {
            messages: vec![901],
            pts: 81,
            pts_count: 1,
        }),
        date: 1_795_341_600,
    }));
    let pruned = wait_until(Duration::from_secs(3), || {
        let msgs = live.load_saved_sublist_messages(self_peer, self_peer);
        msgs.len() == 1 && msgs[0].id == MsgId(905)
    });
    assert!(
        pruned,
        "UpdateDeleteMessages did not prune cached saved sublist messages"
    );
}
