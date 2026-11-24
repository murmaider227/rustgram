//! `FakeRpcClient` tests verifying that `tg-app-live` no longer keeps a private
//! `UpdatesHandler` and instead routes every PTS-producing call site (`load_more_forum_topics`,
//! `load_forum_topics_by_id`, `search_forum_topics`, `delete_topic_history`,
//! `delete_saved_history`, and `search_in_chat` `ChannelMessages`) through `SessionRuntime`'s
//! commands into `SessionActor`'s single `UpdatesHandler`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{
    Channel, ChannelFlags, ChannelId, FullMsgId as DataFullMsgId, LoadedStatus, MsgId as DataMsgId,
    PeerId as DataPeerId, PeerUpdateFlags, User, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::search_and_info::{InChatSearchParams, SharedMediaFilter};
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

    fn push_update(&self, updates: tl_enums::Updates) {
        let _ = self.updates_tx.send(updates);
    }

    fn recorded_ctors(&self) -> Vec<u32> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .map(|(ctor, _)| *ctor)
            .collect()
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

fn build_live_session() -> (FakeRpcClient, LiveSession) {
    let rpc = FakeRpcClient::new();
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        idle_ping_enabled: false,
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
        self_user.common.loaded_status = LoadedStatus::Full;
        self_user.access_hash = 1001;
        s.upsert_user(self_user, PeerUpdateFlags::empty());

        let mut peer2 = User::new(DataUserId(200), false);
        peer2.common.name = "Peer Two".to_owned();
        peer2.common.loaded_status = LoadedStatus::Full;
        peer2.access_hash = 2002;
        s.upsert_user(peer2, PeerUpdateFlags::empty());

        let mut forum = Channel::new(ChannelId(500));
        forum.common.name = "Dev Forum".to_owned();
        forum.common.loaded_status = LoadedStatus::Full;
        forum.access_hash = 5005;
        forum.flags = ChannelFlags::MEGAGROUP | ChannelFlags::FORUM;
        s.upsert_channel(forum, PeerUpdateFlags::empty());
    });
    let live = LiveSession::new(runtime, 0);
    (rpc, live)
}

fn synthetic_tl_message(
    id: i32,
    peer: tl_enums::Peer,
    from_user_id: i64,
    date: i32,
    text: &str,
) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out: false,
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
        guestchat_via_from: None,
    })
}

fn synthetic_tl_forum_topic(
    id: i32,
    title: &str,
    top_message: i32,
    date: i32,
) -> tl_enums::ForumTopic {
    tl_enums::ForumTopic::ForumTopic(tl_types::ForumTopic {
        my: false,
        closed: false,
        pinned: false,
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

#[test]
fn forum_topics_and_search_feed_session_runtime_actor_pts() {
    let (rpc, live) = build_live_session();
    let ch_id = ChannelId(500);
    let forum_peer = PeerId(DataPeerId::from_channel(ch_id).0);
    let tl_forum = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 });

    // 1. `load_more_forum_topics` initializes channel 500 PTS directly on `SessionRuntime`'s actor.
    rpc.enqueue_ok(
        tl_fns::messages::GetForumTopics::CONSTRUCTOR_ID,
        &tl_enums::messages::ForumTopics::ForumTopics(tl_types::messages::ForumTopics {
            order_by_create_date: false,
            count: 1,
            topics: vec![synthetic_tl_forum_topic(1, "General", 10, 1_795_340_100)],
            messages: vec![synthetic_tl_message(
                10,
                tl_forum.clone(),
                200,
                1_795_340_100,
                "General top",
            )],
            chats: Vec::new(),
            users: Vec::new(),
            pts: 42,
        }),
    );
    let _ = live
        .load_more_forum_topics(forum_peer)
        .expect("load_more_forum_topics");
    let state = live
        .runtime()
        .persisted_updates_state_blocking()
        .expect("actor persisted_updates_state");
    assert_eq!(
        state.channel_pts.get(&ch_id).copied(),
        Some(42),
        "load_more_forum_topics must feed SessionActor's UpdatesHandler"
    );

    // 2. `load_forum_topics_by_id` on a fresh session feeds `SessionActor`.
    let (rpc2, live2) = build_live_session();
    rpc2.enqueue_ok(
        tl_fns::messages::GetForumTopicsByID::CONSTRUCTOR_ID,
        &tl_enums::messages::ForumTopics::ForumTopics(tl_types::messages::ForumTopics {
            order_by_create_date: false,
            count: 1,
            topics: vec![synthetic_tl_forum_topic(201, "Alpha", 210, 1_795_340_200)],
            messages: vec![synthetic_tl_message(
                210,
                tl_forum.clone(),
                200,
                1_795_340_200,
                "Alpha top",
            )],
            chats: Vec::new(),
            users: Vec::new(),
            pts: 64,
        }),
    );
    let _ = live2
        .load_forum_topics_by_id(forum_peer, &[MsgId(201)])
        .expect("load_forum_topics_by_id");
    assert_eq!(
        live2
            .runtime()
            .persisted_updates_state_blocking()
            .unwrap()
            .channel_pts
            .get(&ch_id)
            .copied(),
        Some(64),
        "load_forum_topics_by_id must feed SessionActor's UpdatesHandler"
    );

    // 3. `search_forum_topics` (`fetch_forum_topics_inner`) feeds `SessionActor`.
    let (rpc3, live3) = build_live_session();
    rpc3.enqueue_ok(
        tl_fns::messages::GetForumTopics::CONSTRUCTOR_ID,
        &tl_enums::messages::ForumTopics::ForumTopics(tl_types::messages::ForumTopics {
            order_by_create_date: false,
            count: 1,
            topics: vec![synthetic_tl_forum_topic(301, "Search", 310, 1_795_340_300)],
            messages: vec![synthetic_tl_message(
                310,
                tl_forum.clone(),
                200,
                1_795_340_300,
                "Search top",
            )],
            chats: Vec::new(),
            users: Vec::new(),
            pts: 73,
        }),
    );
    let _ = live3
        .search_forum_topics(forum_peer, Some("Search"), 0, 0, 0, 20)
        .expect("search_forum_topics");
    assert_eq!(
        live3
            .runtime()
            .persisted_updates_state_blocking()
            .unwrap()
            .channel_pts
            .get(&ch_id)
            .copied(),
        Some(73),
        "search_forum_topics must feed SessionActor's UpdatesHandler"
    );

    // 4. `search_in_chat` (`messages.search` `ChannelMessages.pts`) feeds `SessionActor`.
    let (rpc4, live4) = build_live_session();
    rpc4.enqueue_ok(
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
                1_795_340_300,
                "Search hit",
            )],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let params = InChatSearchParams::new(forum_peer, "hit", SharedMediaFilter::Empty);
    let _ = live4.search_in_chat(&params).expect("search_in_chat");
    assert_eq!(
        live4
            .runtime()
            .persisted_updates_state_blocking()
            .unwrap()
            .channel_pts
            .get(&ch_id)
            .copied(),
        Some(88),
        "search_in_chat ChannelMessages.pts must feed SessionActor's UpdatesHandler"
    );
}

#[test]
fn delete_topic_and_saved_history_close_interleaved_pushed_gaps_on_session_actor() {
    let (rpc, mut live) = build_live_session();
    let ch_id = ChannelId(500);
    let ch_data_peer = DataPeerId::from_channel(ch_id);
    let user_data_peer = DataPeerId::from_user(DataUserId(200));
    let forum_peer = PeerId(ch_data_peer.0);
    let self_peer = PeerId(DataPeerId::from_user(DataUserId(100)).0);
    let sub_peer = PeerId(user_data_peer.0);
    let tl_forum = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 500 });
    let tl_user = tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 });

    // Seed SessionActor's single UpdatesHandler: common pts=100, channel 500 pts=50.
    live.seed_updates_pts(100, Some((ch_id, 50)));

    // Push an out-of-order channel message (pts=53, pts_count=1) BEFORE `delete_topic_history`
    // returns `affectedHistory(pts=52, pts_count=2)`.
    rpc.push_update(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::NewChannelMessage(tl_types::UpdateNewChannelMessage {
            message: synthetic_tl_message(950, tl_forum, 200, 1_795_340_500, "Interleaved channel"),
            pts: 53,
            pts_count: 1,
        }),
        date: 1_795_340_500,
    }));
    std::thread::sleep(Duration::from_millis(60));

    rpc.enqueue_ok(
        tl_fns::messages::DeleteTopicHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 52,
                pts_count: 2,
                offset: 0,
            },
        ),
    );
    let last_pts = live
        .delete_topic_history(forum_peer, MsgId(201))
        .expect("delete_topic_history");
    assert_eq!(last_pts, 52);

    // SessionActor's unified tracker must have closed the gap (50 -> 52 -> 53) and drained msg 950!
    assert_eq!(live.updates_channel_pts(ch_id), Some(53));
    let has_channel_msg = live.runtime().tokio_handle().block_on(async {
        live.runtime()
            .session()
            .read()
            .await
            .message(DataFullMsgId::new(ch_data_peer, DataMsgId(950)))
            .is_some()
    });
    assert!(
        has_channel_msg,
        "delete_topic_history must route affectedHistory to SessionActor so buffered channel msg 950 drains"
    );

    // Push an out-of-order common message (pts=103, pts_count=1) BEFORE `delete_saved_history`
    // returns `affectedHistory(pts=102, pts_count=2)`.
    rpc.push_update(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message: synthetic_tl_message(960, tl_user, 200, 1_795_340_600, "Interleaved common"),
            pts: 103,
            pts_count: 1,
        }),
        date: 1_795_340_600,
    }));
    std::thread::sleep(Duration::from_millis(60));

    rpc.enqueue_ok(
        tl_fns::messages::DeleteSavedHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 102,
                pts_count: 2,
                offset: 0,
            },
        ),
    );
    let last_saved_pts = live
        .delete_saved_history(self_peer, sub_peer, MsgId(0))
        .expect("delete_saved_history");
    assert_eq!(last_saved_pts, 102);

    // SessionActor's unified tracker must have closed the common gap (100 -> 102 -> 103) and drained msg 960!
    assert_eq!(live.updates_common_pts(), 103);
    let has_common_msg = live.runtime().tokio_handle().block_on(async {
        live.runtime()
            .session()
            .read()
            .await
            .message(DataFullMsgId::new(user_data_peer, DataMsgId(960)))
            .is_some()
    });
    assert!(
        has_common_msg,
        "delete_saved_history must route affectedHistory to SessionActor so buffered common msg 960 drains"
    );

    // Direct LiveSession plumbing helpers route to SessionActor too.
    live.init_channel_pts(ch_id, 200).expect("init_channel_pts");
    assert_eq!(live.updates_channel_pts(ch_id), Some(200));
    live.apply_channel_pts(ch_id, 202, 2)
        .expect("apply_channel_pts");
    assert_eq!(live.updates_channel_pts(ch_id), Some(202));
    live.apply_affected_history(None, 105, 2)
        .expect("apply_affected_history");
    assert_eq!(live.updates_common_pts(), 105);

    // Confirm neither `getDifference` nor `getChannelDifference` was sent spuriously.
    let ctors = rpc.recorded_ctors();
    assert!(!ctors.contains(&tl_fns::updates::GetDifference::CONSTRUCTOR_ID));
    assert!(!ctors.contains(&tl_fns::updates::GetChannelDifference::CONSTRUCTOR_ID));
}
