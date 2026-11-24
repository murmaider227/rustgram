//! Protocol conformance tests for Group 2 (Search, forum topics, saved dialogs,
//! scheduled messages, quick replies, and saved reaction tags) in `tg-app-live`
//! using an in-process `FakeRpcClient` (synthetic data only).
//!
//! Covers:
//! 5.  `history_sections.rs`: `messages.getScheduledHistory` sends
//!     `ScheduledMessagesStore::count_list_hash` and enforces the 60 s cooldown
//!     (`data/components/scheduled_messages.cpp:489-547,657-676`).
//! 6.  `history_sections.rs`: `messages.getSavedDialogs` calls `getPinnedSavedDialogs`
//!     first, sets `exclude_pinned: true`, paginates with `(offset_date, offset_id,
//!     offset_peer)`, stops on out-of-order dates, and records
//!     `SAVED_DIALOGS_UNSUPPORTED` (`data/data_saved_messages.cpp:298-372`).
//! 7.  `history_sections.rs`: `messages.getForumTopics` advances
//!     `(offset_date, offset_id, offset_topic)`, feeds `resp.pts` into `UpdatesHandler`,
//!     handles `forumTopicDeleted` (keeping General `id=1`), supports
//!     `messages.getForumTopicsByID`, and clears `ChannelFlags::FORUM` on
//!     `CHANNEL_FORUM_MISSING` (`data/data_forum.cpp:175-217,368-503`).
//! 8.  `history_sections.rs`: `messages.deleteTopicHistory` and
//!     `messages.deleteSavedHistory` feed every `messages.affectedHistory`
//!     `(pts, pts_count)` page into `UpdatesHandler` (`applyAffectedHistory`,
//!     `window/window_peer_menu.cpp:2325-2341`, `apiwrap.cpp:1610-1628`).
//! 9.  `history_sections.rs`: `messages.editForumTopic` omits `icon_emoji_id` for
//!     the General topic (`id == 1`) and treats `TOPIC_NOT_MODIFIED` as benign
//!     (`boxes/peers/edit_forum_topic_box.cpp:561-587`, `data/data_forum_topic.cpp:475-491`).
//! 10. `business.rs`: `messages.getQuickReplies` sends stored `shortcuts_hash`, and
//!     `messages.getQuickReplyMessages` enforces the 60 s cooldown and sends
//!     `ShortcutMessagesStore::count_list_hash` (`data/business/data_shortcut_messages.cpp:460-479,602-660`).
//! 11. `search_and_info.rs`: `messages.search` sends `saved_reaction` (both direct
//!     and via `SearchRequest::selected_tag`), applies `resp.topics` into the forum
//!     store, and feeds `ChannelMessages.pts` into `UpdatesHandler`
//!     (`api/api_messages_search.cpp:89-95,173-180`).
//! 12. `search_and_info.rs`: `channels.searchPosts` continues on `messages.messagesSlice`
//!     even when `next_rate` is `None` or unchanged until messages are empty
//!     (`dialogs/dialogs_search_posts.cpp:263-272`).
//! 13. `search_and_info.rs`: `load_global_media_page` (`messages.searchGlobal`) sends
//!     `folder_id: Some(0)` (`data/data_search_controller.cpp:75-81`).
//! 14. `compose_extras.rs`: `messages.getSavedReactionTags` resets cached hash to `0`
//!     on RPC error (`data/data_message_reactions.cpp:1053-1077`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::scheduled::ScheduledHistoryOutcome;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{
    Channel, ChannelFlags, ChannelId, PeerId as DataPeerId, PeerUpdateFlags, ReactionId, User,
    UserId as DataUserId, hash_finalize, hash_init, hash_update_i32, hash_update_i64,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::history_sections::ForumTopicEdit;
use crate::search_and_info::{GlobalSearchCursor, InChatSearchParams, SharedMediaFilter};
use tg_app_core::dialogs::backend::{ChatTypeFilter, DialogsBackend, SearchRequest, SearchTab};
use tg_app_core::history::sections_model::FORUM_GENERAL_TOPIC_ID;
use tg_app_core::info::backend::SharedMediaTab;
use tg_app_core::stand_in::types::{MsgId, PeerId};

type RecordedRpc = (u32, Vec<u8>);
type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedRpc>>>,
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

    fn recorded_calls(&self) -> Vec<RecordedRpc> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn clear_recorded(&self) {
        self.recorded.lock().expect("lock recorded").clear();
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
        *self.main_dc.lock().expect("lock main_dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("lock main_dc") = dc_id;
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn decode_req<R: Deserializable + Identifiable>(call: &RecordedRpc) -> R {
    assert_eq!(
        call.0,
        R::CONSTRUCTOR_ID,
        "expected ctor 0x{:08x}, got 0x{:08x}",
        R::CONSTRUCTOR_ID,
        call.0
    );
    R::from_bytes(&call.1).expect("decode recorded TL request")
}

fn rpc_error(code: i32, name: &str) -> InvocationError {
    InvocationError::Rpc(RpcError {
        code,
        name: name.to_owned(),
        value: None,
    })
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
    let session_arc = Arc::clone(runtime.session());
    runtime.tokio_handle().block_on(async move {
        let mut s = session_arc.write().await;

        let mut self_user = User::new(DataUserId(100), true);
        self_user.common.name = "Self User".to_owned();
        self_user.access_hash = 1001;
        s.upsert_user(self_user, PeerUpdateFlags::empty());

        let mut peer2 = User::new(DataUserId(200), false);
        peer2.common.name = "Peer Two".to_owned();
        peer2.access_hash = 2002;
        s.upsert_user(peer2, PeerUpdateFlags::empty());

        let mut peer3 = User::new(DataUserId(300), false);
        peer3.common.name = "Peer Three".to_owned();
        peer3.access_hash = 3003;
        s.upsert_user(peer3, PeerUpdateFlags::empty());

        let mut peer4 = User::new(DataUserId(400), false);
        peer4.common.name = "Peer Four".to_owned();
        peer4.access_hash = 4004;
        s.upsert_user(peer4, PeerUpdateFlags::empty());

        let mut forum = Channel::new(ChannelId(500));
        forum.common.name = "Dev Forum".to_owned();
        forum.access_hash = 5005;
        forum.flags = ChannelFlags::MEGAGROUP | ChannelFlags::FORUM;
        s.upsert_channel(forum, PeerUpdateFlags::empty());
    });
    let live = LiveSession::new(runtime, 0);
    (rpc, live)
}

#[allow(clippy::too_many_arguments)]
fn synthetic_tl_message(
    id: i32,
    peer: tl_enums::Peer,
    from_user_id: i64,
    out: bool,
    date: i32,
    edit_date: Option<i32>,
    text: &str,
    quick_reply_shortcut_id: Option<i32>,
) -> tl_enums::Message {
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
            user_id: from_user_id,
        })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        reply_to: None,
        date,
        message: text.to_owned(),
        media: None,
        reply_markup: None,
        entities: None,
        views: None,
        forwards: None,
        replies: None,
        edit_date,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message: None,
        guestchat_via_from: None,
    })
}

fn synthetic_tl_forum_topic(
    id: i32,
    title: &str,
    top_message: i32,
    date: i32,
    pinned: bool,
) -> tl_enums::ForumTopic {
    tl_enums::ForumTopic::ForumTopic(tl_types::ForumTopic {
        my: false,
        closed: false,
        pinned,
        short: false,
        hidden: false,
        title_missing: false,
        id,
        date,
        peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 }),
        title: title.to_owned(),
        icon_color: 0x6F_B9_F0,
        icon_emoji_id: None,
        top_message,
        read_inbox_max_id: 0,
        read_outbox_max_id: 0,
        unread_count: 0,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
        draft: None,
        from_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 }),
        notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
            tl_types::PeerNotifySettings {
                show_previews: None,
                silent: None,
                mute_until: None,
                ios_sound: None,
                android_sound: None,
                other_sound: None,
                stories_muted: None,
                stories_hide_sender: None,
                stories_ios_sound: None,
                stories_android_sound: None,
                stories_other_sound: None,
            },
        ),
    })
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_000,
        seq: 0,
    })
}

// =============================================================================
// Item 5: `messages.getScheduledHistory` hash + 60s cooldown
// (`ScheduledMessages::request` / `countListHash`, `scheduled_messages.cpp:489-547,657-676`)
// =============================================================================

#[test]
fn item5_scheduled_history_sends_count_list_hash_and_enforces_60s_cooldown() {
    let (rpc, live) = build_live_session();
    let peer = PeerId(DataPeerId::from_user(DataUserId(200)).0);
    let tl_peer = tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 });

    // First fetch at t=1_000_000 ms returns two scheduled messages.
    rpc.enqueue_ok(
        tl_fns::messages::GetScheduledHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![
                synthetic_tl_message(
                    11,
                    tl_peer.clone(),
                    100,
                    true,
                    1_800_000_100,
                    None,
                    "Earlier scheduled",
                    None,
                ),
                synthetic_tl_message(
                    12,
                    tl_peer.clone(),
                    100,
                    true,
                    1_800_000_200,
                    Some(1_795_340_500),
                    "Later scheduled",
                    None,
                ),
            ],
            chats: Vec::new(),
            users: Vec::new(),
            topics: Vec::new(),
        }),
    );

    let out1 = live
        .refresh_scheduled_messages(peer, 1_000_000, false)
        .expect("first scheduled fetch");
    assert_eq!(out1, ScheduledHistoryOutcome::Updated(2));

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let req1: tl_fns::messages::GetScheduledHistory = decode_req(&calls[0]);
    assert_eq!(req1.hash, 0, "initial fetch must send hash=0");

    // Second fetch within 60 s (`kRequestTimeLimit = 60_000 ms`) must be skipped without RPC.
    let out_skipped = live
        .refresh_scheduled_messages(peer, 1_030_000, false)
        .expect("cooldown skip");
    assert_eq!(out_skipped, ScheduledHistoryOutcome::Skipped);
    assert_eq!(
        rpc.recorded_calls().len(),
        1,
        "must not issue RPC inside 60s cooldown"
    );

    // Fetch at t=1_060_000 ms (>= 60 s later) must send exact `ScheduledMessages::countListHash`
    // (reverse sorted order: (12, edit_date=1_795_340_500, date=1_800_000_200), then (11, 0, 1_800_000_100)).
    let mut expected_hash = hash_init();
    hash_update_i64(&mut expected_hash, 12);
    hash_update_i32(&mut expected_hash, 1_795_340_500);
    hash_update_i32(&mut expected_hash, 1_800_000_200);
    hash_update_i64(&mut expected_hash, 11);
    hash_update_i32(&mut expected_hash, 0);
    hash_update_i32(&mut expected_hash, 1_800_000_100);
    let expected_hash = hash_finalize(expected_hash) as i64;
    assert_ne!(expected_hash, 0);

    rpc.enqueue_ok(
        tl_fns::messages::GetScheduledHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::NotModified(tl_types::messages::MessagesNotModified {
            count: 2,
        }),
    );

    let out2 = live
        .refresh_scheduled_messages(peer, 1_060_000, false)
        .expect("post-cooldown refresh");
    assert_eq!(out2, ScheduledHistoryOutcome::NotModified);

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 2);
    let req2: tl_fns::messages::GetScheduledHistory = decode_req(&calls[1]);
    assert_eq!(
        req2.hash, expected_hash,
        "post-cooldown request must send ScheduledMessages::countListHash"
    );
}

// =============================================================================
// Item 6: `messages.getSavedDialogs` pinned fetch, `exclude_pinned: true`,
// pagination, out-of-order date guard, and `SAVED_DIALOGS_UNSUPPORTED`
// (`SavedMessages::sendLoadMore` / `loadPinned`, `data/data_saved_messages.cpp:298-372`)
// =============================================================================

#[test]
fn item6_saved_dialogs_pinned_pagination_out_of_order_and_unsupported() {
    let (rpc, live) = build_live_session();
    let self_peer = PeerId(DataPeerId::from_user(DataUserId(100)).0);
    let tl_self = tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 });
    let tl_p2 = tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 });
    let tl_p3 = tl_enums::Peer::User(tl_types::PeerUser { user_id: 300 });
    let tl_p4 = tl_enums::Peer::User(tl_types::PeerUser { user_id: 400 });

    // Page 1: getPinnedSavedDialogs + getSavedDialogs(exclude_pinned: true) returning Slice
    rpc.enqueue_ok(
        tl_fns::messages::GetPinnedSavedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedDialogs::SavedDialogs(tl_types::messages::SavedDialogs {
            dialogs: vec![tl_enums::SavedDialog::SavedDialog(tl_types::SavedDialog {
                pinned: true,
                peer: tl_self.clone(),
                top_message: 10,
            })],
            messages: vec![synthetic_tl_message(
                10,
                tl_self.clone(),
                100,
                true,
                1_795_340_500,
                None,
                "Pinned note",
                None,
            )],
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetSavedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedDialogs::Slice(tl_types::messages::SavedDialogsSlice {
            count: 3,
            dialogs: vec![tl_enums::SavedDialog::SavedDialog(tl_types::SavedDialog {
                pinned: false,
                peer: tl_p2.clone(),
                top_message: 20,
            })],
            messages: vec![synthetic_tl_message(
                20,
                tl_self.clone(),
                200,
                false,
                1_795_340_400,
                None,
                "Page 1 saved",
                None,
            )],
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let has_more_1 = live
        .load_more_saved_sublists(self_peer)
        .expect("saved page 1");
    assert!(has_more_1, "Slice with advancing cursor should have more");

    // Page 2: sends offset from Page 1 (date=1_795_340_400, id=20, peer=200),
    // returns out-of-order newer date (1_795_340_999 > 1_795_340_400) -> terminates pagination.
    rpc.enqueue_ok(
        tl_fns::messages::GetSavedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedDialogs::Slice(tl_types::messages::SavedDialogsSlice {
            count: 3,
            dialogs: vec![tl_enums::SavedDialog::SavedDialog(tl_types::SavedDialog {
                pinned: false,
                peer: tl_p3.clone(),
                top_message: 30,
            })],
            messages: vec![synthetic_tl_message(
                30,
                tl_self.clone(),
                300,
                false,
                1_795_340_999,
                None,
                "Out of order newer date",
                None,
            )],
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let has_more_2 = live
        .load_more_saved_sublists(self_peer)
        .expect("saved page 2");
    assert!(
        !has_more_2,
        "out-of-order date (applied.date > prev.date) must stop pagination"
    );

    // Calling load_more again must be a no-op (no extra RPC).
    let has_more_3 = live
        .load_more_saved_sublists(self_peer)
        .expect("saved page 3 noop");
    assert!(!has_more_3);

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 3);
    let _pinned_req: tl_fns::messages::GetPinnedSavedDialogs = decode_req(&calls[0]);
    let page1_req: tl_fns::messages::GetSavedDialogs = decode_req(&calls[1]);
    assert!(
        page1_req.exclude_pinned,
        "messages.getSavedDialogs must send exclude_pinned: true"
    );
    assert_eq!(page1_req.offset_date, 0);
    assert_eq!(page1_req.offset_id, 0);
    assert!(matches!(page1_req.offset_peer, tl_enums::InputPeer::Empty));

    let page2_req: tl_fns::messages::GetSavedDialogs = decode_req(&calls[2]);
    assert!(page2_req.exclude_pinned);
    assert_eq!(page2_req.offset_date, 1_795_340_400);
    assert_eq!(page2_req.offset_id, 20);
    assert!(matches!(
        page2_req.offset_peer,
        tl_enums::InputPeer::User(ref u) if u.user_id == 200
    ));

    // Separate session: SAVED_DIALOGS_UNSUPPORTED marks state unsupported and blocks further RPCs.
    let (rpc_unsup, live_unsup) = build_live_session();
    rpc_unsup.enqueue_err(
        tl_fns::messages::GetPinnedSavedDialogs::CONSTRUCTOR_ID,
        rpc_error(400, "SAVED_DIALOGS_UNSUPPORTED"),
    );
    let err = live_unsup
        .load_more_saved_sublists(self_peer)
        .expect_err("SAVED_DIALOGS_UNSUPPORTED should return error");
    assert!(err.contains("SAVED_DIALOGS_UNSUPPORTED"));
    assert!(
        live_unsup.is_saved_dialogs_unsupported(),
        "SAVED_DIALOGS_UNSUPPORTED must mark saved dialogs unsupported"
    );
    // Subsequent call short-circuits without sending another RPC.
    let _ = tl_p4;
    assert!(
        !live_unsup
            .load_more_saved_sublists(self_peer)
            .expect("unsupported subsequent call")
    );
    assert_eq!(rpc_unsup.recorded_calls().len(), 1);
}

// =============================================================================
// Item 7: `messages.getForumTopics` pagination, `pts`, `ForumTopic::Deleted`,
// `messages.getForumTopicsByID`, and `CHANNEL_FORUM_MISSING`
// (`Forum::requestTopics` / `applyReceivedTopics` / `requestSomeStale`,
// `data/data_forum.cpp:175-217,368-503`)
// =============================================================================

#[test]
fn item7_forum_topics_pagination_pts_deleted_by_id_and_forum_missing() {
    let (rpc, live) = build_live_session();
    let forum_peer = PeerId(DataPeerId::from_channel(ChannelId(500)).0);
    let tl_forum = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 });

    // Page 1: count=3, returns topics 1 and 201 with pts=42.
    rpc.enqueue_ok(
        tl_fns::messages::GetForumTopics::CONSTRUCTOR_ID,
        &tl_enums::messages::ForumTopics::ForumTopics(tl_types::messages::ForumTopics {
            order_by_create_date: false,
            count: 3,
            topics: vec![
                synthetic_tl_forum_topic(1, "General", 10, 1_795_340_100, true),
                synthetic_tl_forum_topic(201, "Alpha", 205, 1_795_340_200, false),
            ],
            messages: vec![
                synthetic_tl_message(
                    10,
                    tl_forum.clone(),
                    200,
                    false,
                    1_795_340_100,
                    None,
                    "General top",
                    None,
                ),
                synthetic_tl_message(
                    205,
                    tl_forum.clone(),
                    200,
                    false,
                    1_795_340_200,
                    None,
                    "Alpha top",
                    None,
                ),
            ],
            chats: Vec::new(),
            users: Vec::new(),
            pts: 42,
        }),
    );

    let more1 = live
        .load_more_forum_topics(forum_peer)
        .expect("forum page 1");
    assert!(more1, "more forum topics remain when topics.len() < count");
    assert_eq!(
        live.updates_channel_pts(ChannelId(500)),
        Some(42),
        "resp.pts must be fed into UpdatesHandler"
    );

    // Page 2: advances (offset_date=1_795_340_200, offset_id=205, offset_topic=201),
    // returns topic 202 plus Deleted(201) and Deleted(1) (General must be preserved!).
    rpc.enqueue_ok(
        tl_fns::messages::GetForumTopics::CONSTRUCTOR_ID,
        &tl_enums::messages::ForumTopics::ForumTopics(tl_types::messages::ForumTopics {
            order_by_create_date: false,
            count: 3,
            topics: vec![
                synthetic_tl_forum_topic(202, "Beta", 206, 1_795_340_050, false),
                tl_enums::ForumTopic::Deleted(tl_types::ForumTopicDeleted { id: 201 }),
                tl_enums::ForumTopic::Deleted(tl_types::ForumTopicDeleted { id: 1 }),
            ],
            messages: vec![synthetic_tl_message(
                206,
                tl_forum.clone(),
                200,
                false,
                1_795_340_050,
                None,
                "Beta top",
                None,
            )],
            chats: Vec::new(),
            users: Vec::new(),
            pts: 43,
        }),
    );

    let more2 = live
        .load_more_forum_topics(forum_peer)
        .expect("forum page 2");
    assert!(
        !more2,
        "count == resp.topics.len() marks forum fully loaded"
    );

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 2);
    let page2_req: tl_fns::messages::GetForumTopics = decode_req(&calls[1]);
    assert_eq!(page2_req.offset_date, 1_795_340_200);
    assert_eq!(page2_req.offset_id, 205);
    assert_eq!(page2_req.offset_topic, 201);

    let cached = live.get_forum_topics_live(forum_peer);
    let ids: Vec<i32> = cached.iter().map(|t| t.root_id.0).collect();
    assert!(ids.contains(&1), "General topic id=1 must never be deleted");
    assert!(ids.contains(&202), "new topic 202 must be present");
    assert!(
        !ids.contains(&201),
        "ForumTopic::Deleted(201) must remove non-General topic"
    );

    // `messages.getForumTopicsByID`
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::messages::GetForumTopicsByID::CONSTRUCTOR_ID,
        &tl_enums::messages::ForumTopics::ForumTopics(tl_types::messages::ForumTopics {
            order_by_create_date: false,
            count: 1,
            topics: vec![synthetic_tl_forum_topic(
                305,
                "Stale Topic",
                310,
                1_795_340_300,
                false,
            )],
            messages: vec![synthetic_tl_message(
                310,
                tl_forum.clone(),
                200,
                false,
                1_795_340_300,
                None,
                "Stale top",
                None,
            )],
            chats: Vec::new(),
            users: Vec::new(),
            pts: 44,
        }),
    );
    let fetched = live
        .load_forum_topics_by_id(forum_peer, &[MsgId(305)])
        .expect("load_forum_topics_by_id");
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].root_id, MsgId(305));
    assert_eq!(fetched[0].title, "Stale Topic");
    let by_id_calls = rpc.recorded_calls();
    assert_eq!(by_id_calls.len(), 1);
    let by_id_req: tl_fns::messages::GetForumTopicsByID = decode_req(&by_id_calls[0]);
    assert_eq!(by_id_req.topics, vec![305]);

    // `CHANNEL_FORUM_MISSING` clears `ChannelFlags::FORUM` on the channel.
    live.reload_forum_topics(forum_peer);
    rpc.enqueue_err(
        tl_fns::messages::GetForumTopics::CONSTRUCTOR_ID,
        rpc_error(400, "CHANNEL_FORUM_MISSING"),
    );
    let err = live
        .load_more_forum_topics(forum_peer)
        .expect_err("CHANNEL_FORUM_MISSING should error");
    assert!(err.contains("CHANNEL_FORUM_MISSING"));

    let is_still_forum = live.runtime().tokio_handle().block_on(async {
        let s = live.runtime().session().read().await;
        s.channel(ChannelId(500))
            .is_some_and(|ch| ch.flags.contains(ChannelFlags::FORUM))
    });
    assert!(
        !is_still_forum,
        "CHANNEL_FORUM_MISSING must clear ChannelFlags::FORUM"
    );
}

// =============================================================================
// Item 8: `deleteTopicHistory` and `deleteSavedHistory` feed `affectedHistory`
// `(pts, pts_count)` into `UpdatesHandler` (`applyAffectedHistory`,
// `window/window_peer_menu.cpp:2325-2341`, `apiwrap.cpp:1610-1628`)
// =============================================================================

#[test]
fn item8_delete_topic_and_saved_history_feed_affected_history_pts() {
    let (rpc, mut live) = build_live_session();
    let forum_peer = PeerId(DataPeerId::from_channel(ChannelId(500)).0);
    let self_peer = PeerId(DataPeerId::from_user(DataUserId(100)).0);
    let sub_peer = PeerId(DataPeerId::from_user(DataUserId(200)).0);

    // Seed initial PTS watermarks (common=100, channel 500=50).
    live.seed_updates_pts(100, Some((ChannelId(500), 50)));

    // Multi-page deleteTopicHistory: page 1 (pts=52, pts_count=2, offset=10),
    // page 2 (pts=55, pts_count=3, offset=0).
    rpc.enqueue_ok(
        tl_fns::messages::DeleteTopicHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 52,
                pts_count: 2,
                offset: 10,
            },
        ),
    );
    rpc.enqueue_ok(
        tl_fns::messages::DeleteTopicHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 55,
                pts_count: 3,
                offset: 0,
            },
        ),
    );

    let last_topic_pts = live
        .delete_topic_history(forum_peer, MsgId(201))
        .expect("delete_topic_history");
    assert_eq!(last_topic_pts, 55);
    assert_eq!(
        live.updates_channel_pts(ChannelId(500)),
        Some(55),
        "deleteTopicHistory must advance channel PTS through every affectedHistory page"
    );

    // Multi-page deleteSavedHistory: page 1 (pts=102, pts_count=2, offset=5),
    // page 2 (pts=105, pts_count=3, offset=0).
    rpc.enqueue_ok(
        tl_fns::messages::DeleteSavedHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 102,
                pts_count: 2,
                offset: 5,
            },
        ),
    );
    rpc.enqueue_ok(
        tl_fns::messages::DeleteSavedHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 105,
                pts_count: 3,
                offset: 0,
            },
        ),
    );

    let last_saved_pts = live
        .delete_saved_history(self_peer, sub_peer, MsgId(0))
        .expect("delete_saved_history");
    assert_eq!(last_saved_pts, 105);
    assert_eq!(
        live.updates_common_pts(),
        105,
        "deleteSavedHistory must advance common PTS through every affectedHistory page"
    );

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 4);
    let del_t1: tl_fns::messages::DeleteTopicHistory = decode_req(&calls[0]);
    let del_t2: tl_fns::messages::DeleteTopicHistory = decode_req(&calls[1]);
    assert_eq!(del_t1.top_msg_id, 201);
    assert_eq!(del_t2.top_msg_id, 201);
    let del_s1: tl_fns::messages::DeleteSavedHistory = decode_req(&calls[2]);
    let del_s2: tl_fns::messages::DeleteSavedHistory = decode_req(&calls[3]);
    assert_eq!(del_s1.max_id, 0);
    assert_eq!(del_s2.max_id, 0);
}

// =============================================================================
// Item 9: `messages.editForumTopic` omits `icon_emoji_id` for General topic
// (`id == 1`) and treats `TOPIC_NOT_MODIFIED` as benign
// (`edit_forum_topic_box.cpp:561-587`, `data_forum_topic.cpp:475-491`)
// =============================================================================

#[test]
fn item9_edit_forum_topic_omits_general_icon_and_ignores_topic_not_modified() {
    let (rpc, mut live) = build_live_session();
    let forum_peer = PeerId(DataPeerId::from_channel(ChannelId(500)).0);

    // 1. Editing General topic (id=1) with an icon_emoji_id must omit icon_emoji_id on the wire.
    rpc.enqueue_ok(
        tl_fns::messages::EditForumTopic::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    let edited = live
        .edit_forum_topic(
            forum_peer,
            FORUM_GENERAL_TOPIC_ID,
            ForumTopicEdit {
                title: Some("Main".to_owned()),
                icon_emoji_id: Some(999_888),
                closed: None,
                hidden: Some(true),
            },
        )
        .expect("edit General topic");
    assert_eq!(edited.title, "Main");
    assert_eq!(
        edited.custom_emoji, None,
        "General topic must not receive a custom icon locally"
    );

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let req1: tl_fns::messages::EditForumTopic = decode_req(&calls[0]);
    assert_eq!(req1.topic_id, 1);
    assert_eq!(req1.title.as_deref(), Some("Main"));
    assert_eq!(
        req1.icon_emoji_id, None,
        "editForumTopic must omit icon_emoji_id for General topic (id == 1)"
    );
    assert_eq!(req1.hidden, Some(true));

    // 2. `TOPIC_NOT_MODIFIED` must be treated as benign `Ok(...)`.
    rpc.enqueue_err(
        tl_fns::messages::EditForumTopic::CONSTRUCTOR_ID,
        rpc_error(400, "TOPIC_NOT_MODIFIED"),
    );
    let res = live.edit_forum_topic(
        forum_peer,
        MsgId(201),
        ForumTopicEdit {
            title: Some("Same Title".to_owned()),
            icon_emoji_id: None,
            closed: None,
            hidden: None,
        },
    );
    assert!(
        res.is_ok(),
        "TOPIC_NOT_MODIFIED must be treated as benign success, got {res:?}"
    );
}

// =============================================================================
// Item 10: `messages.getQuickReplies` stored hash + `messages.getQuickReplyMessages`
// `ShortcutMessages::countListHash` + 60s cooldown
// (`data/business/data_shortcut_messages.cpp:460-479,602-660`)
// =============================================================================

#[test]
fn item10_quick_replies_hash_and_quick_reply_messages_hash_and_cooldown() {
    let (rpc, live) = build_live_session();
    let tl_self = tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 });

    // First `getQuickReplies` sends hash=0 and returns shortcut 7 ("greeting", top_message=51, count=1)
    // along with its message (so `load_quick_replies` won't automatically fetch full messages).
    rpc.enqueue_ok(
        tl_fns::messages::GetQuickReplies::CONSTRUCTOR_ID,
        &tl_enums::messages::QuickReplies::QuickReplies(tl_types::messages::QuickReplies {
            quick_replies: vec![tl_enums::QuickReply::QuickReply(tl_types::QuickReply {
                shortcut_id: 7,
                shortcut: "greeting".to_owned(),
                top_message: 51,
                count: 1,
            })],
            messages: vec![synthetic_tl_message(
                51,
                tl_self.clone(),
                100,
                true,
                1_795_340_100,
                Some(1_795_340_150),
                "Hello!",
                Some(7),
            )],
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let map = live.load_quick_replies().expect("first getQuickReplies");
    assert_eq!(map.len(), 1);
    assert_eq!(map[&7].name, "greeting");

    // Second `getQuickReplies` must send the stored `shortcuts_hash` (not hardcoded 0).
    let expected_shortcuts_hash: i64 = 0x5A5A_1234_9876;
    live.runtime().tokio_handle().block_on(async {
        let mut s = live.runtime().session().write().await;
        s.shortcut_messages_mut()
            .set_shortcuts_hash(expected_shortcuts_hash);
    });

    rpc.enqueue_ok(
        tl_fns::messages::GetQuickReplies::CONSTRUCTOR_ID,
        &tl_enums::messages::QuickReplies::NotModified,
    );
    let _ = live.load_quick_replies().expect("second getQuickReplies");

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 2);
    let qr1: tl_fns::messages::GetQuickReplies = decode_req(&calls[0]);
    let qr2: tl_fns::messages::GetQuickReplies = decode_req(&calls[1]);
    assert_eq!(qr1.hash, 0);
    assert_eq!(
        qr2.hash, expected_shortcuts_hash,
        "second getQuickReplies must send stored shortcuts_hash"
    );

    // Now test `getQuickReplyMessages`: sends `ShortcutMessages::countListHash` (hashes `(id, edit_date_or_0)`
    // in reverse WITHOUT `date`, `data_shortcut_messages.cpp:693-708`) and enforces 60s cooldown.
    rpc.clear_recorded();
    let mut expected_msg_hash = hash_init();
    hash_update_i64(&mut expected_msg_hash, 51);
    hash_update_i32(&mut expected_msg_hash, 1_795_340_150);
    let expected_msg_hash = hash_finalize(expected_msg_hash) as i64;

    rpc.enqueue_ok(
        tl_fns::messages::GetQuickReplyMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![synthetic_tl_message(
                51,
                tl_self.clone(),
                100,
                true,
                1_795_340_100,
                Some(1_795_340_150),
                "Hello!",
                Some(7),
            )],
            chats: Vec::new(),
            users: Vec::new(),
            topics: Vec::new(),
        }),
    );

    let msgs1 = live
        .load_quick_reply_messages_with_clock(7, 2_000_000, false)
        .expect("first getQuickReplyMessages");
    assert_eq!(msgs1.len(), 1);
    assert_eq!(msgs1[0].id, 51);

    // Within 60 s: must skip RPC and return cached messages.
    let msgs_cached = live
        .load_quick_reply_messages_with_clock(7, 2_030_000, false)
        .expect("cooldown cached quick reply messages");
    assert_eq!(msgs_cached.len(), 1);
    assert_eq!(
        rpc.recorded_calls().len(),
        1,
        "getQuickReplyMessages must not issue RPC within 60s cooldown"
    );

    // After 60 s: sends exact `ShortcutMessages::countListHash`.
    rpc.enqueue_ok(
        tl_fns::messages::GetQuickReplyMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::NotModified(tl_types::messages::MessagesNotModified {
            count: 1,
        }),
    );
    let _ = live
        .load_quick_reply_messages_with_clock(7, 2_060_000, false)
        .expect("post-cooldown getQuickReplyMessages");

    let msg_calls = rpc.recorded_calls();
    assert_eq!(msg_calls.len(), 2);
    let qrm1: tl_fns::messages::GetQuickReplyMessages = decode_req(&msg_calls[0]);
    let qrm2: tl_fns::messages::GetQuickReplyMessages = decode_req(&msg_calls[1]);
    assert_eq!(qrm1.shortcut_id, 7);
    assert_eq!(qrm1.hash, expected_msg_hash);
    assert_eq!(qrm2.hash, expected_msg_hash);
}

// =============================================================================
// Item 11: `messages.search` sends `saved_reaction`, applies `resp.topics`,
// and feeds `ChannelMessages.pts` into `UpdatesHandler`
// (`api/api_messages_search.cpp:89-95,173-180`)
// =============================================================================

#[test]
fn item11_messages_search_sends_saved_reaction_and_applies_topics_and_pts() {
    let (rpc, live) = build_live_session();
    let self_peer = PeerId(DataPeerId::from_user(DataUserId(100)).0);
    let forum_peer = PeerId(DataPeerId::from_channel(ChannelId(500)).0);
    let tl_self = tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 });
    let tl_forum = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 });

    // 1. Direct `search_in_chat` with empty query + `saved_reaction` tag filter.
    rpc.enqueue_ok(
        tl_fns::messages::Search::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![synthetic_tl_message(
                701,
                tl_self.clone(),
                100,
                true,
                1_795_340_100,
                None,
                "Tagged saved note",
                None,
            )],
            chats: Vec::new(),
            users: Vec::new(),
            topics: Vec::new(),
        }),
    );

    let mut params = InChatSearchParams::new(self_peer, "", SharedMediaFilter::Empty);
    params.saved_reaction = vec![ReactionId::Emoji("⭐".to_owned())];
    let page = live
        .search_in_chat(&params)
        .expect("search_in_chat with saved_reaction");
    assert_eq!(page.messages.len(), 1);

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let req1: tl_fns::messages::Search = decode_req(&calls[0]);
    assert_eq!(
        req1.saved_reaction,
        Some(vec![tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
            emoticon: "⭐".to_owned(),
        })]),
        "messages.search must forward saved_reaction"
    );

    // 2. Via `DialogsBackend::search_dialogs` with `SearchRequest::selected_tag` and empty query.
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::messages::Search::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![synthetic_tl_message(
                702,
                tl_self,
                100,
                true,
                1_795_340_200,
                None,
                "Tag filtered via SearchRequest",
                None,
            )],
            chats: Vec::new(),
            users: Vec::new(),
            topics: Vec::new(),
        }),
    );
    let search_res = live.search_dialogs(&SearchRequest {
        query: String::new(),
        tab: SearchTab::Chats,
        chat_type_filter: ChatTypeFilter::All,
        in_peer: Some(self_peer),
        in_archive: false,
        selected_tag: Some("🔥".to_owned()),
        messages_offset: 0,
        messages_limit: 20,
    });
    assert_eq!(search_res.messages.len(), 1);
    let tag_calls = rpc.recorded_calls();
    assert_eq!(tag_calls.len(), 1);
    let req_tag: tl_fns::messages::Search = decode_req(&tag_calls[0]);
    assert_eq!(
        req_tag.saved_reaction,
        Some(vec![tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
            emoticon: "🔥".to_owned(),
        })])
    );

    // 3. `ChannelMessages` response applies `resp.topics` into session forum store and `resp.pts` into UpdatesHandler.
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::messages::Search::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 88,
            count: 1,
            offset_id_offset: None,
            messages: vec![synthetic_tl_message(
                801,
                tl_forum,
                200,
                false,
                1_795_340_300,
                None,
                "Match inside topic",
                None,
            )],
            topics: vec![synthetic_tl_forum_topic(
                404,
                "Search Topic",
                801,
                1_795_340_300,
                false,
            )],
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let forum_params = InChatSearchParams::new(forum_peer, "Match", SharedMediaFilter::Empty);
    let forum_page = live
        .search_in_chat(&forum_params)
        .expect("forum search_in_chat");
    assert_eq!(forum_page.messages.len(), 1);
    assert_eq!(
        live.updates_channel_pts(ChannelId(500)),
        Some(88),
        "ChannelMessages.pts must be fed into UpdatesHandler"
    );

    let topic_title = live.runtime().tokio_handle().block_on(async {
        let s = live.runtime().session().read().await;
        s.forum_topic(
            DataPeerId::from_channel(ChannelId(500)),
            tg_data::MsgId(404),
        )
        .map(|t| t.title.clone())
    });
    assert_eq!(
        topic_title.as_deref(),
        Some("Search Topic"),
        "ChannelMessages.topics must be applied into Session::forum"
    );
}

// =============================================================================
// Item 12: `channels.searchPosts` continues on `MessagesSlice` when `next_rate`
// is `None` or unchanged until messages are empty
// (`dialogs/dialogs_search_posts.cpp:263-272`)
// =============================================================================

#[test]
fn item12_search_public_posts_continues_without_next_rate_until_empty() {
    let (rpc, live) = build_live_session();
    let tl_forum = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 });

    // Page 1: MessagesSlice with next_rate: None and 1 message -> has_more must be true!
    rpc.enqueue_ok(
        tl_fns::channels::SearchPosts::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
            inexact: false,
            count: 5,
            next_rate: None,
            offset_id_offset: None,
            search_flood: None,
            messages: vec![synthetic_tl_message(
                901,
                tl_forum.clone(),
                200,
                false,
                1_795_340_500,
                None,
                "Public post 1",
                None,
            )],
            chats: Vec::new(),
            users: Vec::new(),
            topics: Vec::new(),
        }),
    );

    let p1 = live
        .search_public_posts_page("rust", GlobalSearchCursor::default(), 20, None)
        .expect("public posts page 1");
    assert_eq!(p1.messages.len(), 1);
    assert!(
        p1.has_more,
        "MessagesSlice with non-empty messages must keep has_more=true even when next_rate is None"
    );

    // Page 2: MessagesSlice with unchanged next_rate: Some(0) and 1 message -> has_more still true!
    rpc.enqueue_ok(
        tl_fns::channels::SearchPosts::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
            inexact: false,
            count: 5,
            next_rate: Some(0),
            offset_id_offset: None,
            search_flood: None,
            messages: vec![synthetic_tl_message(
                900,
                tl_forum,
                200,
                false,
                1_795_340_400,
                None,
                "Public post 2",
                None,
            )],
            chats: Vec::new(),
            users: Vec::new(),
            topics: Vec::new(),
        }),
    );

    let p2 = live
        .search_public_posts_page("rust", p1.next_cursor, 20, None)
        .expect("public posts page 2");
    assert!(
        p2.has_more,
        "MessagesSlice with unchanged next_rate and non-empty messages must keep has_more=true"
    );

    // Page 3: empty MessagesSlice -> has_more becomes false.
    rpc.enqueue_ok(
        tl_fns::channels::SearchPosts::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
            inexact: false,
            count: 5,
            next_rate: None,
            offset_id_offset: None,
            search_flood: None,
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            topics: Vec::new(),
        }),
    );

    let p3 = live
        .search_public_posts_page("rust", p2.next_cursor, 20, None)
        .expect("public posts page 3");
    assert!(!p3.has_more, "empty MessagesSlice must end pagination");

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 3);
    let r2: tl_fns::channels::SearchPosts = decode_req(&calls[1]);
    assert_eq!(r2.offset_id, 901);
    let r3: tl_fns::channels::SearchPosts = decode_req(&calls[2]);
    assert_eq!(r3.offset_id, 900);
}

// =============================================================================
// Item 13: `load_global_media_page` (`messages.searchGlobal`) sends `folder_id: Some(0)`
// (`data/data_search_controller.cpp:75-81`)
// =============================================================================

#[test]
fn item13_global_media_search_sends_folder_id_zero() {
    let (rpc, live) = build_live_session();

    rpc.enqueue_ok(
        tl_fns::messages::SearchGlobal::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            topics: Vec::new(),
        }),
    );

    let _ = live
        .load_global_media_page(SharedMediaTab::Voice, "", GlobalSearchCursor::default(), 20)
        .expect("load_global_media_page");

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let req: tl_fns::messages::SearchGlobal = decode_req(&calls[0]);
    assert_eq!(
        req.folder_id,
        Some(0),
        "messages.searchGlobal for global media must send folder_id: Some(0)"
    );
}

// =============================================================================
// Item 14: `messages.getSavedReactionTags` resets cached hash to `0` on RPC error
// (`Reactions::requestMyTags`, `data/data_message_reactions.cpp:1053-1077`)
// =============================================================================

#[test]
fn item14_saved_reaction_tags_resets_hash_to_zero_on_error() {
    let (rpc, live) = build_live_session();

    // 1. First fetch succeeds and stores non-zero hash `0x1234_5678`.
    rpc.enqueue_ok(
        tl_fns::messages::GetSavedReactionTags::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedReactionTags::SavedReactionTags(
            tl_types::messages::SavedReactionTags {
                tags: vec![tl_enums::SavedReactionTag::SavedReactionTag(
                    tl_types::SavedReactionTag {
                        reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                            emoticon: "⭐".to_owned(),
                        }),
                        title: Some("Important".to_owned()),
                        count: 3,
                    },
                )],
                hash: 0x1234_5678,
            },
        ),
    );
    let tags1 = live.saved_reaction_tags_live(None);
    assert_eq!(tags1.len(), 1);
    assert_eq!(tags1[0].emoji, "⭐");

    // 2. Second fetch sends stored hash `0x1234_5678` and fails with an RPC error -> resets hash to 0.
    rpc.enqueue_err(
        tl_fns::messages::GetSavedReactionTags::CONSTRUCTOR_ID,
        rpc_error(500, "INTERNAL"),
    );
    let _ = live.saved_reaction_tags_live(None);

    // 3. Third fetch must send `hash == 0` because error reset the cached hash.
    rpc.enqueue_ok(
        tl_fns::messages::GetSavedReactionTags::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedReactionTags::NotModified,
    );
    let _ = live.saved_reaction_tags_live(None);

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 3);
    let r1: tl_fns::messages::GetSavedReactionTags = decode_req(&calls[0]);
    let r2: tl_fns::messages::GetSavedReactionTags = decode_req(&calls[1]);
    let r3: tl_fns::messages::GetSavedReactionTags = decode_req(&calls[2]);
    assert_eq!(r1.hash, 0);
    assert_eq!(r2.hash, 0x1234_5678);
    assert_eq!(
        r3.hash, 0,
        "getSavedReactionTags must reset cached hash to 0 on RPC error"
    );
}
