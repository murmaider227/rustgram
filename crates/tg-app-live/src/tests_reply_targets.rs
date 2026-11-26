//! Reply targets outside the loaded history (`FakeRpcClient` only, synthetic data).
//!
//! A reply to a message that is not in the loaded slice shows "Loading...", the missing targets
//! of one slice go out as one `channels.getMessages` / `messages.getMessages` per message box,
//! and the open history gets a `MessageEdited` with the real author and text (or "Deleted
//! message") when the answer arrives (`ApiWrap::requestMessageData` / `resolveMessageDatas`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_api::convert::message_from_tl;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{
    Channel, ChannelFlags, ChannelId, MsgRange, PeerId as DataPeerId, SERVER_MAX_MSG_ID, User,
    UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use tg_app_core::history::backend::{HistoryBackend, HistoryLiveUpdate, HistoryMessage};
use tg_app_core::stand_in::types::{MsgId, PeerId};

type Handler = dyn Fn(u32, &[u8]) -> Option<Vec<u8>> + Send + Sync;

type Recorded = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
pub(super) struct FakeRpcClient {
    recorded: Recorded,
    handler: Arc<Mutex<Option<Arc<Handler>>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(16);
        Self {
            recorded: Arc::new(Mutex::new(Vec::new())),
            handler: Arc::new(Mutex::new(None)),
            updates_tx,
        }
    }

    pub(super) fn set_handler(
        &self,
        h: impl Fn(u32, &[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) {
        *self.handler.lock().expect("handler") = Some(Arc::new(h));
    }

    fn push_updates(&self, updates: tl_enums::Updates) {
        let _ = self.updates_tx.send(updates);
    }

    pub(super) fn calls<R: Deserializable + Identifiable>(&self) -> Vec<R> {
        self.recorded
            .lock()
            .expect("recorded")
            .iter()
            .filter(|(ctor, _)| *ctor == R::CONSTRUCTOR_ID)
            .map(|(_, bytes)| R::from_bytes(bytes).expect("decode recorded request"))
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
            .expect("recorded")
            .push((ctor, bytes.clone()));
        let handler = self.handler.lock().expect("handler").clone();
        match handler.and_then(|h| h(ctor, &bytes)) {
            Some(raw) => {
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
        2
    }

    fn set_main_dc_id(&self, _dc_id: DcId) {}

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

pub(super) const SELF_ID: i64 = 100;
pub(super) const PEER_ID: i64 = 200;
const OLD_AUTHOR_ID: i64 = 300;
const GROUP_ID: i64 = 700;
const BROADCAST_ID: i64 = 800;

fn group_peer() -> PeerId {
    PeerId(DataPeerId::from_channel(ChannelId(GROUP_ID as u64)).0)
}

fn user_peer() -> PeerId {
    PeerId(DataPeerId::from_user(DataUserId(PEER_ID as u64)).0)
}

fn tl_group_peer() -> tl_enums::Peer {
    tl_enums::Peer::Channel(tl_types::PeerChannel {
        channel_id: GROUP_ID,
    })
}

pub(super) fn tl_user_peer(id: i64) -> tl_enums::Peer {
    tl_enums::Peer::User(tl_types::PeerUser { user_id: id })
}

#[derive(Default, Clone)]
pub(super) struct Reply {
    pub(super) to: i32,
    pub(super) peer: Option<tl_enums::Peer>,
    pub(super) quote: Option<&'static str>,
    pub(super) from_name: Option<&'static str>,
    pub(super) top: Option<i32>,
}

pub(super) fn tl_message(
    id: i32,
    peer: tl_enums::Peer,
    from: i64,
    text: &str,
    reply: Option<Reply>,
) -> tl_enums::Message {
    let reply_to = reply.map(|r| {
        tl_enums::MessageReplyHeader::MessageReplyHeader(tl_types::MessageReplyHeader {
            reply_to_scheduled: false,
            forum_topic: false,
            quote: r.quote.is_some(),
            reply_to_ephemeral: false,
            reply_to_msg_id: Some(r.to),
            reply_to_peer_id: r.peer,
            reply_from: r.from_name.map(|name| {
                tl_enums::MessageFwdHeader::MessageFwdHeader(tl_types::MessageFwdHeader {
                    imported: false,
                    saved_out: false,
                    from_id: None,
                    from_name: Some(name.to_owned()),
                    date: 1_795_000_000,
                    channel_post: None,
                    post_author: None,
                    saved_from_peer: None,
                    saved_from_msg_id: None,
                    saved_from_id: None,
                    saved_from_name: None,
                    saved_date: None,
                    psa_type: None,
                })
            }),
            reply_media: None,
            reply_to_top_id: r.top,
            quote_text: r.quote.map(str::to_owned),
            quote_entities: None,
            quote_offset: None,
            todo_item_id: None,
            poll_option: None,
            quote_rich: None,
        })
    });
    tl_enums::Message::Message(tl_types::Message {
        out: from == SELF_ID,
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
        from_id: Some(tl_user_peer(from)),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to,
        date: 1_795_340_000 + id,
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
    })
}

fn tl_empty(id: i32, peer: tl_enums::Peer) -> tl_enums::Message {
    tl_enums::Message::Empty(tl_types::MessageEmpty {
        id,
        peer_id: Some(peer),
    })
}

pub(super) fn tl_user(id: i64, name: &str) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: id == SELF_ID,
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

fn channel_messages(messages: Vec<tl_enums::Message>, users: Vec<tl_enums::User>) -> Vec<u8> {
    tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
        inexact: false,
        pts: 1,
        count: messages.len() as i32,
        offset_id_offset: None,
        messages,
        topics: Vec::new(),
        chats: Vec::new(),
        users,
    })
    .to_bytes()
}

pub(super) fn plain_messages(
    messages: Vec<tl_enums::Message>,
    users: Vec<tl_enums::User>,
) -> Vec<u8> {
    tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
        messages,
        topics: Vec::new(),
        chats: Vec::new(),
        users,
    })
    .to_bytes()
}

/// Session with self, a user chat and a megagroup; `history` is loaded as the bottom slice of
/// `peer` (so opening it makes no `getHistory` request).
pub(super) fn build_live(
    peer: DataPeerId,
    history: Vec<tl_enums::Message>,
) -> (FakeRpcClient, LiveSession) {
    build_live_with(peer, history, test_runtime_config())
}

/// The runtime configuration [`build_live`] uses: no startup sync of any kind.
pub(super) fn test_runtime_config() -> SessionRuntimeConfig {
    SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    }
}

/// [`build_live`] with a caller-chosen runtime configuration.
pub(super) fn build_live_with(
    peer: DataPeerId,
    history: Vec<tl_enums::Message>,
    cfg: SessionRuntimeConfig,
) -> (FakeRpcClient, LiveSession) {
    let rpc = FakeRpcClient::new();
    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId(SELF_ID as u64),
            crate::ErasedRpcClient::new(rpc.clone()),
            cfg,
        )
        .expect("start SessionRuntime"),
    );
    let session_arc = Arc::clone(runtime.session());
    runtime.tokio_handle().block_on(async move {
        let mut s = session_arc.write().await;
        for (id, name, is_self) in [(SELF_ID, "Self Tester", true), (PEER_ID, "Peer Two", false)] {
            let mut u = User::new(DataUserId(id as u64), is_self);
            u.first_name = name.to_owned();
            u.common.name = name.to_owned();
            u.common.loaded_status = tg_data::LoadedStatus::Full;
            u.access_hash = (id * 1000 + 7) as u64;
            s.upsert_user(u, tg_data::PeerUpdateFlags::NAME);
        }
        let mut group = Channel::new(ChannelId(GROUP_ID as u64));
        group.common.name = "Group Seven".to_owned();
        group.common.loaded_status = tg_data::LoadedStatus::Full;
        group.access_hash = (GROUP_ID * 1000 + 7) as u64;
        group.flags = ChannelFlags::MEGAGROUP;
        s.upsert_channel(group, tg_data::PeerUpdateFlags::NAME);
        let mut broadcast = Channel::new(ChannelId(BROADCAST_ID as u64));
        broadcast.common.name = "Broadcast Eight".to_owned();
        broadcast.common.loaded_status = tg_data::LoadedStatus::Full;
        broadcast.access_hash = (BROADCAST_ID * 1000 + 7) as u64;
        broadcast.flags = ChannelFlags::BROADCAST;
        s.upsert_channel(broadcast, tg_data::PeerUpdateFlags::NAME);

        let self_id = s.self_user_id();
        let msgs: Vec<_> = history
            .iter()
            .filter_map(|m| message_from_tl(m, self_id))
            .collect();
        let from = msgs.iter().map(|m| m.id).min().expect("history");
        s.add_history_slice(peer, msgs, MsgRange::new(from, SERVER_MAX_MSG_ID), None);
    });
    let live = LiveSession::new(runtime, 1);
    (rpc, live)
}

pub(super) fn wait_until(mut pred: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    pred()
}

/// Drains history updates until every id in `ids` came back as `MessageEdited`.
fn wait_for_edits(live: &mut LiveSession, peer: PeerId, ids: &[i32]) -> Vec<HistoryMessage> {
    let mut edited: Vec<HistoryMessage> = Vec::new();
    let done = wait_until(|| {
        for update in live.drain_history_updates(peer) {
            if let HistoryLiveUpdate::MessageEdited(m) = update {
                edited.retain(|e| e.id != m.id);
                edited.push(*m);
            }
        }
        ids.iter().all(|id| edited.iter().any(|m| m.id.0 == *id))
    });
    assert!(
        done,
        "expected repaint of {ids:?}, got {:?}",
        edited.iter().map(|m| m.id).collect::<Vec<_>>()
    );
    edited
}

fn by_id(messages: &[HistoryMessage], id: i32) -> &HistoryMessage {
    messages
        .iter()
        .find(|m| m.id.0 == id)
        .unwrap_or_else(|| panic!("message {id} missing"))
}

fn reply_of(messages: &[HistoryMessage], id: i32) -> (String, String, Option<String>) {
    let r = by_id(messages, id).reply.clone().expect("reply header");
    (r.author_name, r.preview_text, r.quote_text)
}

fn ids_of(req: &[tl_enums::InputMessage]) -> Vec<i32> {
    let mut ids: Vec<i32> = req
        .iter()
        .map(|m| match m {
            tl_enums::InputMessage::ID(m) => m.id,
            other => panic!("expected inputMessageID, got {other:?}"),
        })
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn missing_replies_in_a_group_load_in_one_channel_request_and_repaint() {
    let peer = group_peer();
    let tl_peer = tl_group_peer();
    let reply = |to| {
        Some(Reply {
            to,
            ..Reply::default()
        })
    };
    let (rpc, mut live) = build_live(
        DataPeerId(peer.0),
        vec![
            tl_message(1000, tl_peer.clone(), PEER_ID, "loaded target", None),
            tl_message(1001, tl_peer.clone(), PEER_ID, "re old one", reply(50)),
            tl_message(1002, tl_peer.clone(), SELF_ID, "re old two", reply(60)),
            tl_message(1003, tl_peer.clone(), PEER_ID, "re loaded", reply(1000)),
            tl_message(1004, tl_peer.clone(), PEER_ID, "re deleted", reply(70)),
            tl_message(
                1005,
                tl_peer.clone(),
                PEER_ID,
                "re old one again",
                reply(50),
            ),
        ],
    );
    let answer_peer = tl_peer.clone();
    rpc.set_handler(move |ctor, _| {
        (ctor == tl_fns::channels::GetMessages::CONSTRUCTOR_ID).then(|| {
            channel_messages(
                vec![
                    tl_message(
                        50,
                        answer_peer.clone(),
                        OLD_AUTHOR_ID,
                        "the old question",
                        None,
                    ),
                    tl_message(60, answer_peer.clone(), PEER_ID, "second old message", None),
                    tl_empty(70, answer_peer.clone()),
                ],
                vec![tl_user(OLD_AUTHOR_ID, "Old Author")],
            )
        })
    });

    let slice = live.load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0);
    let loading = tg_lang::tr::lng_profile_loading();
    assert_eq!(
        reply_of(&slice.messages, 1001),
        (String::new(), loading.clone(), None)
    );
    assert_eq!(
        reply_of(&slice.messages, 1004),
        (String::new(), loading, None)
    );
    assert_eq!(
        reply_of(&slice.messages, 1003),
        ("Peer Two".to_owned(), "loaded target".to_owned(), None)
    );

    let edited = wait_for_edits(&mut live, peer, &[1001, 1002, 1004, 1005]);
    assert_eq!(
        reply_of(&edited, 1001),
        ("Old Author".to_owned(), "the old question".to_owned(), None)
    );
    assert_eq!(reply_of(&edited, 1005), reply_of(&edited, 1001));
    assert_eq!(
        reply_of(&edited, 1002),
        ("Peer Two".to_owned(), "second old message".to_owned(), None)
    );
    assert_eq!(
        reply_of(&edited, 1004),
        (String::new(), tg_lang::tr::lng_deleted_message(), None)
    );

    let requests = rpc.calls::<tl_fns::channels::GetMessages>();
    assert_eq!(requests.len(), 1, "one batched channels.getMessages");
    assert_eq!(ids_of(&requests[0].id), vec![50, 60, 70]);
    match &requests[0].channel {
        tl_enums::InputChannel::InputChannel(c) => assert_eq!(c.channel_id, GROUP_ID),
        other => panic!("expected the group's input channel, got {other:?}"),
    }
    assert!(rpc.calls::<tl_fns::messages::GetMessages>().is_empty());

    // Reopening the chat uses the cache: no new request, headers resolved at once.
    let again = live.load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0);
    assert_eq!(reply_of(&again.messages, 1001).0, "Old Author");
    assert_eq!(
        reply_of(&again.messages, 1004).1,
        tg_lang::tr::lng_deleted_message()
    );
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(rpc.calls::<tl_fns::channels::GetMessages>().len(), 1);
}

#[test]
fn replies_split_by_message_box_and_quotes_follow_tdesktop() {
    let peer = user_peer();
    let tl_peer = tl_user_peer(PEER_ID);
    let (rpc, mut live) = build_live(
        DataPeerId(peer.0),
        vec![
            tl_message(500, tl_peer.clone(), PEER_ID, "hello", None),
            tl_message(
                501,
                tl_peer.clone(),
                PEER_ID,
                "re common box",
                Some(Reply {
                    to: 40,
                    ..Reply::default()
                }),
            ),
            tl_message(
                502,
                tl_peer.clone(),
                SELF_ID,
                "re other chat",
                Some(Reply {
                    to: 80,
                    peer: Some(tl_group_peer()),
                    ..Reply::default()
                }),
            ),
            tl_message(
                503,
                tl_peer.clone(),
                PEER_ID,
                "re quoted, deleted",
                Some(Reply {
                    to: 41,
                    quote: Some("quoted words"),
                    from_name: Some("Hidden Sender"),
                    ..Reply::default()
                }),
            ),
            tl_message(
                504,
                tl_peer.clone(),
                PEER_ID,
                "re quoted, alive",
                Some(Reply {
                    to: 42,
                    quote: Some("a quoted part"),
                    ..Reply::default()
                }),
            ),
        ],
    );
    let common_peer = tl_peer.clone();
    rpc.set_handler(move |ctor, _| {
        if ctor == tl_fns::messages::GetMessages::CONSTRUCTOR_ID {
            Some(plain_messages(
                vec![
                    tl_message(40, common_peer.clone(), SELF_ID, "my older words", None),
                    tl_empty(41, common_peer.clone()),
                    tl_message(
                        42,
                        common_peer.clone(),
                        PEER_ID,
                        "a quoted part of it",
                        None,
                    ),
                ],
                Vec::new(),
            ))
        } else if ctor == tl_fns::channels::GetMessages::CONSTRUCTOR_ID {
            Some(channel_messages(
                vec![tl_message(80, tl_group_peer(), PEER_ID, "group post", None)],
                Vec::new(),
            ))
        } else {
            None
        }
    });

    let slice = live.load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0);
    // A quoted reply still shows "Loading..." while its target has a message id to fetch.
    assert_eq!(
        reply_of(&slice.messages, 503),
        (String::new(), tg_lang::tr::lng_profile_loading(), None)
    );

    let edited = wait_for_edits(&mut live, peer, &[501, 502, 503, 504]);
    assert_eq!(
        reply_of(&edited, 501),
        ("Self Tester".to_owned(), "my older words".to_owned(), None)
    );
    assert_eq!(
        reply_of(&edited, 502),
        ("Peer Two".to_owned(), "group post".to_owned(), None)
    );
    // Deleted target with a quote: the quote stays under the external sender name.
    assert_eq!(
        reply_of(&edited, 503),
        (
            "Hidden Sender".to_owned(),
            "quoted words".to_owned(),
            Some("quoted words".to_owned())
        )
    );
    assert_eq!(
        reply_of(&edited, 504),
        (
            "Peer Two".to_owned(),
            "a quoted part of it".to_owned(),
            Some("a quoted part".to_owned())
        )
    );

    let common = rpc.calls::<tl_fns::messages::GetMessages>();
    assert_eq!(
        common.len(),
        1,
        "one messages.getMessages for the common box"
    );
    assert_eq!(ids_of(&common[0].id), vec![40, 41, 42]);
    let channel = rpc.calls::<tl_fns::channels::GetMessages>();
    assert_eq!(
        channel.len(),
        1,
        "one channels.getMessages for the other chat"
    );
    assert_eq!(ids_of(&channel[0].id), vec![80]);
}

#[test]
fn new_message_replying_to_an_old_one_repaints_when_the_target_arrives() {
    let peer = group_peer();
    let tl_peer = tl_group_peer();
    let (rpc, mut live) = build_live(
        DataPeerId(peer.0),
        vec![tl_message(1000, tl_peer.clone(), PEER_ID, "recent", None)],
    );
    let answer_peer = tl_peer.clone();
    rpc.set_handler(move |ctor, _| {
        (ctor == tl_fns::channels::GetMessages::CONSTRUCTOR_ID).then(|| {
            channel_messages(
                vec![tl_message(5, answer_peer.clone(), PEER_ID, "ancient", None)],
                Vec::new(),
            )
        })
    });
    let _ = live.load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0);

    // A new incoming message (as an update would insert it) replying to message 5.
    let incoming = tl_message(
        1001,
        tl_peer.clone(),
        PEER_ID,
        "re ancient",
        Some(Reply {
            to: 5,
            ..Reply::default()
        }),
    );
    live.write_session(|s| {
        let self_id = s.self_user_id();
        if let Some(m) = message_from_tl(&incoming, self_id) {
            s.insert_message(m);
        }
    });
    live.bridge_state()
        .record_message_added(DataPeerId(peer.0), tg_data::MsgId(1001));

    let mut added_header = None;
    assert!(wait_until(|| {
        for update in live.drain_history_updates(peer) {
            if let HistoryLiveUpdate::MessageAdded(m) = update {
                added_header = m.reply.clone();
            }
        }
        added_header.is_some()
    }));
    assert_eq!(
        added_header.map(|r| r.preview_text),
        Some(tg_lang::tr::lng_profile_loading())
    );
    let edited = wait_for_edits(&mut live, peer, &[1001]);
    assert_eq!(
        reply_of(&edited, 1001),
        ("Peer Two".to_owned(), "ancient".to_owned(), None)
    );
    assert_eq!(rpc.calls::<tl_fns::channels::GetMessages>().len(), 1);
}

#[test]
fn transient_failure_is_retried_instead_of_showing_deleted() {
    let peer = group_peer();
    let tl_peer = tl_group_peer();
    let (rpc, live) = build_live(
        DataPeerId(peer.0),
        vec![
            tl_message(1000, tl_peer.clone(), PEER_ID, "recent", None),
            tl_message(
                1001,
                tl_peer.clone(),
                PEER_ID,
                "re old",
                Some(Reply {
                    to: 7,
                    ..Reply::default()
                }),
            ),
        ],
    );
    // No handler: every request fails with `Dropped` (connection lost).
    let slice = live.load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0);
    assert_eq!(
        reply_of(&slice.messages, 1001).1,
        tg_lang::tr::lng_profile_loading()
    );
    assert!(wait_until(|| rpc
        .calls::<tl_fns::channels::GetMessages>()
        .len()
        == 1));
    std::thread::sleep(Duration::from_millis(50));

    let answer_peer = tl_peer.clone();
    rpc.set_handler(move |ctor, _| {
        (ctor == tl_fns::channels::GetMessages::CONSTRUCTOR_ID).then(|| {
            channel_messages(
                vec![tl_message(
                    7,
                    answer_peer.clone(),
                    PEER_ID,
                    "found later",
                    None,
                )],
                Vec::new(),
            )
        })
    });
    let again = live.load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0);
    assert_eq!(
        reply_of(&again.messages, 1001).1,
        tg_lang::tr::lng_profile_loading()
    );
    assert!(wait_until(|| rpc
        .calls::<tl_fns::channels::GetMessages>()
        .len()
        == 2));
    assert!(wait_until(|| {
        let slice = live.load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0);
        reply_of(&slice.messages, 1001).1 == "found later"
    }));
}

#[test]
fn comment_thread_replies_resolve_inside_the_page_and_fetch_older_comments() {
    let broadcast = PeerId(DataPeerId::from_channel(ChannelId(BROADCAST_ID as u64)).0);
    let tl_broadcast = tl_enums::Peer::Channel(tl_types::PeerChannel {
        channel_id: BROADCAST_ID,
    });
    let disc = tl_group_peer();
    let (rpc, live) = build_live(
        DataPeerId(broadcast.0),
        vec![tl_message(90, tl_broadcast, PEER_ID, "post", None)],
    );
    let comment = |to| {
        Some(Reply {
            to,
            top: Some(4001),
            ..Reply::default()
        })
    };
    let d = disc.clone();
    rpc.set_handler(move |ctor, _| {
        if ctor == tl_fns::messages::GetDiscussionMessage::CONSTRUCTOR_ID {
            Some(
                tl_enums::messages::DiscussionMessage::DiscussionMessage(
                    tl_types::messages::DiscussionMessage {
                        messages: vec![tl_message(4001, d.clone(), PEER_ID, "post copy", None)],
                        max_id: Some(4011),
                        read_inbox_max_id: Some(4011),
                        read_outbox_max_id: Some(4011),
                        unread_count: 0,
                        chats: Vec::new(),
                        users: Vec::new(),
                    },
                )
                .to_bytes(),
            )
        } else if ctor == tl_fns::messages::GetReplies::CONSTRUCTOR_ID {
            // Newest first, as the server sends them.
            Some(channel_messages(
                vec![
                    tl_message(4011, d.clone(), PEER_ID, "re an old comment", comment(3000)),
                    tl_message(4010, d.clone(), SELF_ID, "re first comment", comment(4005)),
                    tl_message(4005, d.clone(), PEER_ID, "first comment", comment(4001)),
                ],
                Vec::new(),
            ))
        } else if ctor == tl_fns::channels::GetMessages::CONSTRUCTOR_ID {
            Some(channel_messages(
                vec![tl_message(
                    3000,
                    d.clone(),
                    OLD_AUTHOR_ID,
                    "old comment",
                    None,
                )],
                vec![tl_user(OLD_AUTHOR_ID, "Old Author")],
            ))
        } else {
            None
        }
    });

    let (_, comments) = live
        .replies_root_and_comments(broadcast, MsgId(90))
        .expect("comments");
    assert_eq!(
        reply_of(&comments, 4010),
        ("Peer Two".to_owned(), "first comment".to_owned(), None)
    );
    assert_eq!(
        reply_of(&comments, 4011),
        (String::new(), tg_lang::tr::lng_profile_loading(), None)
    );
    assert!(wait_until(|| {
        live.replies_root_and_comments(broadcast, MsgId(90))
            .is_some_and(|(_, c)| reply_of(&c, 4011).0 == "Old Author")
    }));
    let requests = rpc.calls::<tl_fns::channels::GetMessages>();
    assert_eq!(requests.len(), 1);
    assert_eq!(ids_of(&requests[0].id), vec![3000]);
}

fn story_reply(
    id: i32,
    peer: tl_enums::Peer,
    story_owner: i64,
    story_id: i32,
) -> tl_enums::Message {
    let mut m = tl_message(id, peer, SELF_ID, "re story", None);
    if let tl_enums::Message::Message(ref mut inner) = m {
        inner.reply_to = Some(tl_enums::MessageReplyHeader::MessageReplyStoryHeader(
            tl_types::MessageReplyStoryHeader {
                peer: tl_user_peer(story_owner),
                story_id,
            },
        ));
    }
    m
}

fn tl_story(id: i32, caption: &str) -> tl_enums::StoryItem {
    tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
        pinned: false,
        public: true,
        close_friends: false,
        min: false,
        noforwards: false,
        edited: false,
        contacts: false,
        selected_contacts: false,
        out: false,
        id,
        date: 1_795_300_000,
        from_id: None,
        fwd_from: None,
        expire_date: 1_795_386_400,
        caption: Some(caption.to_owned()),
        entities: None,
        media: Box::new(tl_enums::MessageMedia::Empty),
        media_areas: None,
        privacy: None,
        views: None,
        sent_reaction: None,
        albums: None,
        music: None,
    })
}

#[test]
fn story_replies_load_per_owner_and_show_story_or_deleted_story() {
    let peer = user_peer();
    let tl_peer = tl_user_peer(PEER_ID);
    let (rpc, mut live) = build_live(
        DataPeerId(peer.0),
        vec![
            tl_message(600, tl_peer.clone(), PEER_ID, "hi", None),
            story_reply(601, tl_peer.clone(), PEER_ID, 5),
            story_reply(602, tl_peer.clone(), PEER_ID, 6),
        ],
    );
    rpc.set_handler(move |ctor, _| {
        (ctor == tl_fns::stories::GetStoriesByID::CONSTRUCTOR_ID).then(|| {
            tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
                count: 2,
                stories: vec![
                    tl_story(5, "sunset"),
                    tl_enums::StoryItem::Deleted(tl_types::StoryItemDeleted { id: 6 }),
                ],
                pinned_to_top: None,
                chats: Vec::new(),
                users: Vec::new(),
            })
            .to_bytes()
        })
    });

    let slice = live.load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0);
    assert_eq!(
        reply_of(&slice.messages, 601),
        (String::new(), tg_lang::tr::lng_profile_loading(), None)
    );
    let edited = wait_for_edits(&mut live, peer, &[601, 602]);
    assert_eq!(
        reply_of(&edited, 601),
        ("Peer Two".to_owned(), "Story, sunset".to_owned(), None)
    );
    assert_eq!(
        reply_of(&edited, 602),
        (String::new(), tg_lang::tr::lng_deleted_story(), None)
    );
    let requests = rpc.calls::<tl_fns::stories::GetStoriesByID>();
    assert_eq!(
        requests.len(),
        1,
        "one stories.getStoriesByID for the owner"
    );
    let mut ids = requests[0].id.clone();
    ids.sort_unstable();
    assert_eq!(ids, vec![5, 6]);
    match &requests[0].peer {
        tl_enums::InputPeer::User(u) => assert_eq!(u.user_id, PEER_ID),
        other => panic!("expected the story owner's input peer, got {other:?}"),
    }
}

#[test]
fn deleted_or_edited_cached_target_repaints_its_replies() {
    let peer = group_peer();
    let tl_peer = tl_group_peer();
    let reply = |to| {
        Some(Reply {
            to,
            ..Reply::default()
        })
    };
    let (rpc, mut live) = build_live(
        DataPeerId(peer.0),
        vec![
            tl_message(1000, tl_peer.clone(), PEER_ID, "recent", None),
            tl_message(1001, tl_peer.clone(), PEER_ID, "re one", reply(50)),
            tl_message(1002, tl_peer.clone(), PEER_ID, "re two", reply(60)),
        ],
    );
    let answer_peer = tl_peer.clone();
    rpc.set_handler(move |ctor, _| {
        (ctor == tl_fns::channels::GetMessages::CONSTRUCTOR_ID).then(|| {
            channel_messages(
                vec![
                    tl_message(50, answer_peer.clone(), PEER_ID, "will be deleted", None),
                    tl_message(60, answer_peer.clone(), PEER_ID, "will be edited", None),
                ],
                Vec::new(),
            )
        })
    });
    let _ = live.load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0);
    let _ = wait_for_edits(&mut live, peer, &[1001, 1002]);

    rpc.push_updates(tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![
            tl_enums::Update::DeleteChannelMessages(tl_types::UpdateDeleteChannelMessages {
                channel_id: GROUP_ID,
                messages: vec![50],
                pts: 2,
                pts_count: 1,
            }),
            tl_enums::Update::EditChannelMessage(tl_types::UpdateEditChannelMessage {
                message: tl_message(60, tl_peer.clone(), PEER_ID, "edited text", None),
                pts: 3,
                pts_count: 1,
            }),
        ],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_340_100,
        seq: 0,
    }));
    let edited = wait_for_edits(&mut live, peer, &[1001, 1002]);
    assert_eq!(
        reply_of(&edited, 1001),
        (String::new(), tg_lang::tr::lng_deleted_message(), None)
    );
    assert_eq!(reply_of(&edited, 1002).1, "edited text");
}
