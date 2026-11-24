//! Scripted fake-RPC tests verifying:
//! 1. RPC-returned `Updates` and `AffectedMessages` across `tg-app-live` route through
//!    `SessionRuntime::{apply_updates_blocking, delete_messages, edit_message}` into the
//!    single `UpdatesHandler`, advancing `pts`/`seq` in one place and applying every update
//!    at most once (new message, edit, delete, pin, reaction, read, channel creation,
//!    and `MESSAGE_NOT_MODIFIED` difference-sync convergence).
//! 2. `communities::moderate_messages` kick path uses `tg_api::channels::kicked_restrictions_for_peer`
//!    per peer (`ChannelData::KickedRestrictedRights`, `data/data_channel.cpp:495-514`) and
//!    routes selected-message deletion through `SessionRuntime::delete_messages`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{
    ChangeEvent, ChannelId, FullMsgId as DataFullMsgId, MessageUpdateFlags, MsgId as DataMsgId,
    PeerId as DataPeerId, ReactionId, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::session::LiveSession;
use tg_app_core::core_types::dto::moderate_messages::ModerateMessagesRequest;
use tg_app_core::history::backend::{HistoryBackend, SendMessageExtended};
use tg_app_core::info::backend::{CreateChannelRequest, InfoBackend};
use tg_app_core::stand_in::types::{MsgId, PeerId};

type QueuedReply = (u32, Result<Vec<u8>, InvocationError>);
type RecordedCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct ScriptedFakeRpc {
    main_dc: Arc<Mutex<DcId>>,
    responses: Arc<Mutex<VecDeque<QueuedReply>>>,
    recorded: RecordedCalls,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl ScriptedFakeRpc {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            recorded: Arc::new(Mutex::new(Vec::new())),
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

    fn clear_recorded(&self) {
        self.recorded.lock().expect("lock recorded").clear();
    }

    fn recorded_of<R: Identifiable + Deserializable>(&self) -> Vec<R> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(ctor, _)| *ctor == R::CONSTRUCTOR_ID)
            .filter_map(|(_, bytes)| R::from_bytes(bytes).ok())
            .collect()
    }
}

impl RpcClient for ScriptedFakeRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));
        let popped = {
            let mut q = self.responses.lock().expect("lock responses");
            q.iter()
                .position(|(c, _)| *c == ctor)
                .and_then(|pos| q.remove(pos))
                .map(|(_, r)| r)
        };
        match popped {
            Some(Ok(raw)) => {
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            Some(Err(err)) => Err(err),
            None => Err(InvocationError::Dropped),
        }
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

fn synthetic_tl_user(id: i64, first_name: &str, is_self: bool) -> tl_types::User {
    tl_types::User {
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
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_forum_sidebar: false,
        bot_guard: false,
        bot_guestchat: false,
        id,
        access_hash: Some(id * 17 + 1),
        first_name: Some(first_name.to_owned()),
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
    }
}

fn synthetic_tl_channel(id: i64, title: &str, broadcast: bool) -> tl_types::Channel {
    tl_types::Channel {
        creator: true,
        left: false,
        broadcast,
        verified: false,
        megagroup: !broadcast,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: false,
        call_not_empty: false,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: false,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        forum_sidebar: false,
        in_news: false,
        id,
        access_hash: Some(id * 31 + 3),
        title: title.to_owned(),
        username: None,
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_795_340_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(12),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
        linked_monoforum_id: None,
        pinned_msg_id: None,
    }
}

fn synthetic_tl_message(
    id: i32,
    peer: tl_enums::Peer,
    from_id: tl_enums::Peer,
    text: &str,
    edit_date: Option<i32>,
) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out: true,
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
        guestchat_via_from: None,
        id,
        from_id: Some(from_id),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        reply_to: None,
        date: 1_795_400_000,
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

fn build_harness(
    initial_pts: i32,
    initial_seq: i32,
) -> (
    tokio::runtime::Runtime,
    ScriptedFakeRpc,
    Arc<SessionRuntime<crate::ErasedRpcClient>>,
    LiveSession,
) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let rpc = ScriptedFakeRpc::new();
    rpc.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: initial_pts,
            qts: 0,
            date: 1_795_400_000,
            seq: initial_seq,
            unread_count: 0,
        }),
    );

    let cfg = SessionRuntimeConfig {
        init_updates_state: true,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        idle_ping_enabled: false,
        ..SessionRuntimeConfig::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start_on(
            DataUserId(100),
            crate::ErasedRpcClient::new(rpc.clone()),
            rt.handle().clone(),
            cfg,
        )
        .expect("start SessionRuntime"),
    );
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), runtime.wait_ready())
            .await
            .expect("wait_ready timeout")
            .expect("wait_ready ok");
    });

    let live = LiveSession::new(Arc::clone(&runtime), 77);
    live.write_session(|s| {
        apply_tl_users_and_chats(
            s,
            &[
                tl_enums::User::User(synthetic_tl_user(100, "Self", true)),
                tl_enums::User::User(synthetic_tl_user(201, "Partner", false)),
            ],
            &[
                tl_enums::Chat::Channel(synthetic_tl_channel(501, "Group Five", false)),
                tl_enums::Chat::Channel(synthetic_tl_channel(502, "Posting Channel", true)),
            ],
        );
    });
    (rt, rpc, runtime, live)
}

fn drain_changes(rx: &mut broadcast::Receiver<ChangeEvent>) -> Vec<ChangeEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

#[test]
fn returned_updates_applied_once_advances_pts_and_covers_all_cases() {
    let (_rt, rpc, runtime, mut live) = build_harness(100, 1);
    let user_peer = PeerId::from_user(201);
    let data_user_peer = DataPeerId::from_user(DataUserId(201));
    let ch_id = ChannelId(501);
    let ch_peer = PeerId::from_channel(501);
    let data_ch_peer = DataPeerId::from_channel(ch_id);
    let tl_self = tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 });
    let tl_user = tl_enums::Peer::User(tl_types::PeerUser { user_id: 201 });
    let tl_chan = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 501 });

    live.init_channel_pts(ch_id, 50).expect("init_channel_pts");
    let mut changes = runtime.subscribe_changes();

    // 1. NewMessage via `forward_messages` -> applied ONCE, advances common pts 100 -> 101.
    let fwd_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message: synthetic_tl_message(201, tl_user.clone(), tl_self.clone(), "Forwarded", None),
            pts: 101,
            pts_count: 1,
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_400_010,
        seq: 0,
    });
    rpc.enqueue_ok(
        tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID,
        &fwd_updates,
    );
    let forwarded = HistoryBackend::forward_messages(&mut live, user_peer, &[MsgId(10)], user_peer)
        .expect("forward_messages");
    assert_eq!(forwarded.len(), 1);
    assert_eq!(forwarded[0].id, MsgId(201));
    assert_eq!(live.updates_common_pts(), 101);
    let evs = drain_changes(&mut changes);
    let new_added_count = evs
        .iter()
        .filter(|e| {
            matches!(
                e,
                ChangeEvent::MessageUpdated { id, flags }
                    if *id == DataFullMsgId::new(data_user_peer, DataMsgId(201))
                        && flags.contains(MessageUpdateFlags::NEW_ADDED)
            )
        })
        .count();
    assert_eq!(
        new_added_count, 1,
        "returned UpdateNewMessage must be applied exactly once"
    );

    // 2. Re-delivering the exact same `(pts=101, pts_count=1)` batch is skipped by PtsWaiter.
    let _ = live.apply_updates_to_session(&fwd_updates);
    assert_eq!(live.updates_common_pts(), 101);
    assert!(
        drain_changes(&mut changes).is_empty(),
        "duplicate pts batch must not mutate session or emit change events twice"
    );

    // 3. EditMessage via `HistoryBackend::edit_message` -> applied ONCE with server edit_date,
    //    advances common pts 101 -> 102.
    let edit_ts = 1_795_400_555;
    rpc.enqueue_ok(
        tl_fns::messages::EditMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::EditMessage(tl_types::UpdateEditMessage {
                message: synthetic_tl_message(
                    201,
                    tl_user.clone(),
                    tl_self.clone(),
                    "Edited by server",
                    Some(edit_ts),
                ),
                pts: 102,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: edit_ts,
            seq: 0,
        }),
    );
    let edited = live
        .edit_message(
            user_peer,
            MsgId(201),
            TextWithEntities::simple("Edited by server"),
        )
        .expect("edit_message");
    assert_eq!(edited.text.text, "Edited by server");
    assert_eq!(live.updates_common_pts(), 102);
    live.read_session(|s| {
        let msg = s
            .message(DataFullMsgId::new(data_user_peer, DataMsgId(201)))
            .expect("edited msg in session");
        assert_eq!(msg.edit_date, Some(edit_ts));
    });
    let evs = drain_changes(&mut changes);
    let edit_count = evs
        .iter()
        .filter(|e| {
            matches!(
                e,
                ChangeEvent::MessageUpdated { id, flags }
                    if *id == DataFullMsgId::new(data_user_peer, DataMsgId(201))
                        && flags.contains(MessageUpdateFlags::EDITED)
            )
        })
        .count();
    assert_eq!(
        edit_count, 1,
        "returned UpdateEditMessage must emit EDITED exactly once"
    );

    // 4. Pin via `HistoryBackend::pin_message` -> applied ONCE, advances common pts 102 -> 103.
    rpc.enqueue_ok(
        tl_fns::messages::UpdatePinnedMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::PinnedMessages(
                tl_types::UpdatePinnedMessages {
                    pinned: true,
                    peer: tl_user.clone(),
                    messages: vec![201],
                    pts: 103,
                    pts_count: 1,
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_400_600,
            seq: 0,
        }),
    );
    let _ = live
        .pin_message(user_peer, MsgId(201), false)
        .expect("pin_message");
    assert_eq!(live.updates_common_pts(), 103);
    live.read_session(|s| {
        let msg = s
            .message(DataFullMsgId::new(data_user_peer, DataMsgId(201)))
            .expect("pinned msg in session");
        assert!(msg.is_pinned());
    });
    let evs = drain_changes(&mut changes);
    let pin_events = evs
        .iter()
        .filter(|e| {
            matches!(
                e,
                ChangeEvent::MessageUpdated { id, flags }
                    if *id == DataFullMsgId::new(data_user_peer, DataMsgId(201))
                        && flags.contains(MessageUpdateFlags::EDITED)
            )
        })
        .count();
    assert_eq!(
        pin_events, 1,
        "returned UpdatePinnedMessages must update pinned flag exactly once"
    );

    // 5. Reaction via `HistoryBackend::toggle_reaction` -> applied ONCE with server count.
    rpc.enqueue_ok(
        tl_fns::messages::SendReaction::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::MessageReactions(
                tl_types::UpdateMessageReactions {
                    peer: tl_user.clone(),
                    msg_id: 201,
                    top_msg_id: None,
                    saved_peer_id: None,
                    reactions: tl_enums::MessageReactions::MessageReactions(
                        tl_types::MessageReactions {
                            min: false,
                            can_see_list: true,
                            reactions_as_tags: false,
                            results: vec![tl_enums::ReactionCount::ReactionCount(
                                tl_types::ReactionCount {
                                    chosen_order: Some(0),
                                    reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                                        emoticon: "👍".to_owned(),
                                    }),
                                    count: 7,
                                },
                            )],
                            recent_reactions: None,
                            top_reactors: None,
                        },
                    ),
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_400_610,
            seq: 0,
        }),
    );
    let reactions = live
        .toggle_reaction(user_peer, MsgId(201), "👍")
        .expect("toggle_reaction");
    assert_eq!(reactions.len(), 1);
    assert_eq!(reactions[0].count, 7);
    live.read_session(|s| {
        let msg = s
            .message(DataFullMsgId::new(data_user_peer, DataMsgId(201)))
            .expect("reacted msg");
        let r = msg.reactions.as_ref().expect("reactions stored");
        assert_eq!(r.results.len(), 1);
        assert_eq!(r.results[0].id, ReactionId::Emoji("👍".to_owned()));
        assert_eq!(r.results[0].count, 7);
    });
    let evs = drain_changes(&mut changes);
    let react_events = evs
        .iter()
        .filter(|e| {
            matches!(
                e,
                ChangeEvent::MessageUpdated { id, flags }
                    if *id == DataFullMsgId::new(data_user_peer, DataMsgId(201))
                        && flags.contains(MessageUpdateFlags::EDITED)
            )
        })
        .count();
    assert_eq!(
        react_events, 1,
        "returned UpdateMessageReactions must emit EDITED exactly once"
    );

    // 6. Read updates (`ReadHistoryInbox` + `ReadHistoryOutbox`) advance pts 103 -> 104 -> 105
    //    and update history read watermarks.
    let read_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![
            tl_enums::Update::ReadHistoryInbox(tl_types::UpdateReadHistoryInbox {
                folder_id: None,
                peer: tl_user.clone(),
                top_msg_id: None,
                max_id: 201,
                still_unread_count: 0,
                pts: 104,
                pts_count: 1,
            }),
            tl_enums::Update::ReadHistoryOutbox(tl_types::UpdateReadHistoryOutbox {
                peer: tl_user.clone(),
                max_id: 201,
                pts: 105,
                pts_count: 1,
            }),
        ],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_400_620,
        seq: 0,
    });
    let _ = live.apply_updates_to_session(&read_updates);
    assert_eq!(live.updates_common_pts(), 105);
    live.read_session(|s| {
        let h = s.history(data_user_peer).expect("user history");
        assert_eq!(h.inbox_read_till_id(), DataMsgId(201));
        assert_eq!(h.outbox_read_till_id(), DataMsgId(201));
        assert_eq!(h.unread_count, Some(0));
    });
    let _ = drain_changes(&mut changes);

    // 7. Delete via `HistoryBackend::delete_messages` (routes through `SessionRuntime::delete_messages`)
    //    -> removes msg 201 locally, advances common pts 105 -> 106, emits DESTROYED ONCE.
    rpc.enqueue_ok(
        tl_fns::messages::DeleteMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 106,
                pts_count: 1,
            },
        ),
    );
    HistoryBackend::delete_messages(&mut live, user_peer, &[MsgId(201)], true)
        .expect("delete_messages user");
    assert_eq!(live.updates_common_pts(), 106);
    live.read_session(|s| {
        assert!(
            s.message(DataFullMsgId::new(data_user_peer, DataMsgId(201)))
                .is_none(),
            "deleted message must be removed from Session"
        );
    });
    let evs = drain_changes(&mut changes);
    let destroy_events = evs
        .iter()
        .filter(|e| {
            matches!(
                e,
                ChangeEvent::MessageUpdated { id, flags }
                    if *id == DataFullMsgId::new(data_user_peer, DataMsgId(201))
                        && flags.contains(MessageUpdateFlags::DESTROYED)
            )
        })
        .count();
    assert_eq!(
        destroy_events, 1,
        "delete_messages must emit DESTROYED exactly once"
    );

    // 8. Delete via `Update::DeleteMessages` in returned `Updates` -> advances pts 106 -> 107 -> 108.
    let _ = live.apply_updates_to_session(&tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message: synthetic_tl_message(202, tl_user.clone(), tl_self.clone(), "Temp", None),
            pts: 107,
            pts_count: 1,
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_400_630,
        seq: 0,
    }));
    assert!(live.read_session(|s| {
        s.message(DataFullMsgId::new(data_user_peer, DataMsgId(202)))
            .is_some()
    }));
    let _ = live.apply_updates_to_session(&tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::DeleteMessages(
            tl_types::UpdateDeleteMessages {
                messages: vec![202],
                pts: 108,
                pts_count: 1,
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_400_640,
        seq: 0,
    }));
    assert_eq!(live.updates_common_pts(), 108);
    assert!(live.read_session(|s| {
        s.message(DataFullMsgId::new(data_user_peer, DataMsgId(202)))
            .is_none()
    }));

    // 9. Channel updates (`NewChannelMessage` -> `EditChannelMessage` -> `PinnedChannelMessages`
    //    -> `delete_messages`) advance channel pts 50 -> 51 -> 52 -> 53 -> 54.
    let _ = live.apply_updates_to_session(&tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewChannelMessage(
            tl_types::UpdateNewChannelMessage {
                message: synthetic_tl_message(
                    401,
                    tl_chan.clone(),
                    tl_self.clone(),
                    "Chan post",
                    None,
                ),
                pts: 51,
                pts_count: 1,
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_400_650,
        seq: 0,
    }));
    assert_eq!(live.updates_channel_pts(ch_id), Some(51));

    rpc.enqueue_ok(
        tl_fns::messages::EditMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::EditChannelMessage(
                tl_types::UpdateEditChannelMessage {
                    message: synthetic_tl_message(
                        401,
                        tl_chan.clone(),
                        tl_self.clone(),
                        "Chan edited",
                        Some(1_795_400_660),
                    ),
                    pts: 52,
                    pts_count: 1,
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_400_660,
            seq: 0,
        }),
    );
    let ch_edited = live
        .edit_message(ch_peer, MsgId(401), TextWithEntities::simple("Chan edited"))
        .expect("edit channel msg");
    assert_eq!(ch_edited.text.text, "Chan edited");
    assert_eq!(live.updates_channel_pts(ch_id), Some(52));

    rpc.enqueue_ok(
        tl_fns::messages::UpdatePinnedMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::PinnedChannelMessages(
                tl_types::UpdatePinnedChannelMessages {
                    pinned: true,
                    channel_id: 501,
                    messages: vec![401],
                    pts: 53,
                    pts_count: 1,
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_400_670,
            seq: 0,
        }),
    );
    let _ = live
        .pin_message(ch_peer, MsgId(401), false)
        .expect("pin channel msg");
    assert_eq!(live.updates_channel_pts(ch_id), Some(53));
    assert!(live.read_session(|s| {
        s.message(DataFullMsgId::new(data_ch_peer, DataMsgId(401)))
            .is_some_and(|m| m.is_pinned())
    }));

    rpc.enqueue_ok(
        tl_fns::channels::DeleteMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 54,
                pts_count: 1,
            },
        ),
    );
    HistoryBackend::delete_messages(&mut live, ch_peer, &[MsgId(401)], true)
        .expect("delete channel msg");
    assert_eq!(live.updates_channel_pts(ch_id), Some(54));
    assert!(live.read_session(|s| {
        s.message(DataFullMsgId::new(data_ch_peer, DataMsgId(401)))
            .is_none()
    }));
}

#[test]
fn edit_message_message_not_modified_after_retry_syncs_difference_and_converges() {
    let (_rt, rpc, _runtime, mut live) = build_harness(200, 1);
    let user_peer = PeerId::from_user(201);
    let data_user_peer = DataPeerId::from_user(DataUserId(201));
    let tl_self = tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 });
    let tl_user = tl_enums::Peer::User(tl_types::PeerUser { user_id: 201 });

    // Seed original message 301 at pts 201.
    let _ = live.apply_updates_to_session(&tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message: synthetic_tl_message(
                301,
                tl_user.clone(),
                tl_self.clone(),
                "Before retry",
                None,
            ),
            pts: 201,
            pts_count: 1,
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_400_100,
        seq: 0,
    }));
    assert_eq!(live.updates_common_pts(), 201);

    // Server returns `400 MESSAGE_NOT_MODIFIED` (e.g. after a 500 RPC_CALL_FAIL retry
    // where the first attempt already committed the edit on the server).
    rpc.enqueue_err(
        tl_fns::messages::EditMessage::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError {
            code: 400,
            name: "MESSAGE_NOT_MODIFIED".to_owned(),
            value: None,
        }),
    );
    // `SessionRuntime::edit_message` triggers `updates.getDifference` to converge state.
    let committed_edit_ts = 1_795_500_000;
    rpc.enqueue_ok(
        tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Difference(tl_types::updates::Difference {
            new_messages: Vec::new(),
            new_encrypted_messages: Vec::new(),
            other_updates: vec![tl_enums::Update::EditMessage(tl_types::UpdateEditMessage {
                message: synthetic_tl_message(
                    301,
                    tl_user,
                    tl_self,
                    "Committed before 500",
                    Some(committed_edit_ts),
                ),
                pts: 202,
                pts_count: 1,
            })],
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_enums::updates::State::State(tl_types::updates::State {
                pts: 202,
                qts: 0,
                date: committed_edit_ts,
                seq: 1,
                unread_count: 0,
            }),
        }),
    );

    let edited = live
        .edit_message(
            user_peer,
            MsgId(301),
            TextWithEntities::simple("Committed before 500"),
        )
        .expect("MESSAGE_NOT_MODIFIED must be treated as success after syncing difference");
    assert_eq!(edited.text.text, "Committed before 500");
    assert_eq!(live.updates_common_pts(), 202);
    live.read_session(|s| {
        let msg = s
            .message(DataFullMsgId::new(data_user_peer, DataMsgId(301)))
            .expect("converged msg");
        assert_eq!(msg.edit_date, Some(committed_edit_ts));
    });
}

#[test]
fn send_message_extended_and_create_channel_apply_updates_once_and_advance_state() {
    let (_rt, rpc, runtime, mut live) = build_harness(300, 1);
    let user_peer = PeerId::from_user(201);
    let data_user_peer = DataPeerId::from_user(DataUserId(201));
    let tl_self = tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 });
    let tl_user = tl_enums::Peer::User(tl_types::PeerUser { user_id: 201 });

    let mut changes = runtime.subscribe_changes();

    // 1. `send_message_extended` goes through `SessionRuntime::send_message_with_entities`,
    //    which already applies `outcome.updates` once — `compose_extras` must NOT re-apply them.
    rpc.enqueue_ok(
        tl_fns::messages::SendMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message: synthetic_tl_message(701, tl_user, tl_self, "Hello once", None),
                pts: 301,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_400_200,
            seq: 0,
        }),
    );
    let sent = live
        .send_message_extended(
            user_peer,
            SendMessageExtended {
                text: TextWithEntities::simple("Hello once"),
                ..SendMessageExtended::default()
            },
        )
        .expect("send_message_extended");
    assert_eq!(sent.id, MsgId(701));
    assert_eq!(live.updates_common_pts(), 301);
    let evs = drain_changes(&mut changes);
    let server_added = evs
        .iter()
        .filter(|e| {
            matches!(
                e,
                ChangeEvent::MessageUpdated { id, flags }
                    if *id == DataFullMsgId::new(data_user_peer, DataMsgId(701))
                        && flags.contains(MessageUpdateFlags::NEW_ADDED)
            )
        })
        .count();
    assert_eq!(
        server_added, 1,
        "send_message_extended must not double-apply outcome.updates"
    );

    // 2. `InfoBackend::create_channel` returns `Updates` with a new channel and `seq: 2`;
    //    routed through `SessionRuntime::apply_updates_blocking`, it adds the channel to
    //    `Session` and advances `seq` from 1 -> 2.
    rpc.enqueue_ok(
        tl_fns::channels::CreateChannel::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: vec![tl_enums::Chat::Channel(synthetic_tl_channel(
                909,
                "Created News",
                true,
            ))],
            date: 1_795_400_300,
            seq: 2,
        }),
    );
    let created_peer = live
        .create_channel(CreateChannelRequest {
            title: "Created News".to_owned(),
            description: "Updates test".to_owned(),
            is_public: false,
            username: None,
            no_forwards: false,
            has_custom_photo: false,
        })
        .expect("create_channel");
    assert_eq!(created_peer, PeerId::from_channel(909));
    assert!(live.read_session(|s| s.channel_loaded(ChannelId(909))));
    let persisted = runtime
        .tokio_handle()
        .block_on(runtime.persisted_updates_state())
        .expect("persisted_updates_state");
    assert_eq!(persisted.seq, 2);
}

#[test]
fn moderate_messages_uses_kicked_restrictions_for_peer_per_peer_and_deletes_via_runtime() {
    let (_rt, rpc, _runtime, mut live) = build_harness(400, 1);
    let group_ch_id = ChannelId(501);
    let group_peer = PeerId::from_channel(501);
    let data_group_peer = DataPeerId::from_channel(group_ch_id);
    let offender_user = PeerId::from_user(201);
    let offender_channel = PeerId::from_channel(502);
    let tl_group = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 501 });
    let tl_offender = tl_enums::Peer::User(tl_types::PeerUser { user_id: 201 });

    live.init_channel_pts(group_ch_id, 80)
        .expect("init channel pts");

    // Seed messages 41 and 42 in the supergroup via UpdatesHandler (channel pts 80 -> 82).
    let _ = live.apply_updates_to_session(&tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![
            tl_enums::Update::NewChannelMessage(tl_types::UpdateNewChannelMessage {
                message: synthetic_tl_message(
                    41,
                    tl_group.clone(),
                    tl_offender.clone(),
                    "spam 1",
                    None,
                ),
                pts: 81,
                pts_count: 1,
            }),
            tl_enums::Update::NewChannelMessage(tl_types::UpdateNewChannelMessage {
                message: synthetic_tl_message(42, tl_group, tl_offender, "spam 2", None),
                pts: 82,
                pts_count: 1,
            }),
        ],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_400_400,
        seq: 0,
    }));
    assert_eq!(live.updates_channel_pts(group_ch_id), Some(82));
    assert!(live.read_session(|s| {
        s.message(DataFullMsgId::new(data_group_peer, DataMsgId(41)))
            .is_some()
            && s.message(DataFullMsgId::new(data_group_peer, DataMsgId(42)))
                .is_some()
    }));

    // 1. Kick mixed batch `[offender_user, offender_channel]` + delete `[41, 42]`.
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::channels::DeleteMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 84,
                pts_count: 2,
            },
        ),
    );
    let empty_upd = tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_400_410,
        seq: 0,
    });
    rpc.enqueue_ok(tl_fns::channels::EditBanned::CONSTRUCTOR_ID, &empty_upd);
    rpc.enqueue_ok(tl_fns::channels::EditBanned::CONSTRUCTOR_ID, &empty_upd);

    let res = live
        .moderate_messages(ModerateMessagesRequest {
            peer_id: group_peer,
            message_ids: vec![41, 42],
            report_spam_peers: Vec::new(),
            delete_all_from_peers: Vec::new(),
            delete_all_reactions_peers: Vec::new(),
            ban_peers: vec![offender_user, offender_channel],
            ban_in_community_id: None,
            restrict_duration_secs: None,
        })
        .expect("moderate_messages kick");
    assert_eq!(res.deleted_selected, 2);
    assert_eq!(res.banned_peers, 2);

    // Selected messages removed locally from Session AND channel pts advanced 82 -> 84.
    assert_eq!(live.updates_channel_pts(group_ch_id), Some(84));
    assert!(live.read_session(|s| {
        s.message(DataFullMsgId::new(data_group_peer, DataMsgId(41)))
            .is_none()
            && s.message(DataFullMsgId::new(data_group_peer, DataMsgId(42)))
                .is_none()
    }));

    // Verify per-peer `channels.editBanned` wire payloads (`data/data_channel.cpp:495-514`).
    let bans = rpc.recorded_of::<tl_fns::channels::EditBanned>();
    assert_eq!(bans.len(), 2);

    // First peer: user -> KICKED_USER granular bits, NO legacy send_messages/send_media, until_date=0.
    let tl_enums::ChatBannedRights::ChatBannedRights(br_user) = &bans[0].banned_rights;
    assert!(br_user.view_messages);
    assert!(!br_user.send_messages);
    assert!(!br_user.send_media);
    assert!(br_user.send_stickers);
    assert!(br_user.send_gifs);
    assert!(br_user.send_games);
    assert!(br_user.send_inline);
    assert!(br_user.embed_links);
    assert!(br_user.send_photos);
    assert!(br_user.send_videos);
    assert!(br_user.send_roundvideos);
    assert!(br_user.send_audios);
    assert!(br_user.send_voices);
    assert!(br_user.send_docs);
    assert!(br_user.send_plain);
    assert_eq!(br_user.until_date, 0);

    // Second peer: channel -> VIEW_MESSAGES ONLY, all granular send bits false, until_date=0.
    let tl_enums::ChatBannedRights::ChatBannedRights(br_chan) = &bans[1].banned_rights;
    assert!(br_chan.view_messages);
    assert!(!br_chan.send_messages);
    assert!(!br_chan.send_media);
    assert!(!br_chan.send_stickers);
    assert!(!br_chan.send_gifs);
    assert!(!br_chan.send_games);
    assert!(!br_chan.send_inline);
    assert!(!br_chan.embed_links);
    assert!(!br_chan.send_polls);
    assert!(!br_chan.send_photos);
    assert!(!br_chan.send_videos);
    assert!(!br_chan.send_roundvideos);
    assert!(!br_chan.send_audios);
    assert!(!br_chan.send_voices);
    assert!(!br_chan.send_docs);
    assert!(!br_chan.send_plain);
    assert_eq!(br_chan.until_date, 0);

    // 2. Timed restriction (`restrict_duration_secs: Some(3600)`) restricts without view_messages.
    rpc.clear_recorded();
    rpc.enqueue_ok(tl_fns::channels::EditBanned::CONSTRUCTOR_ID, &empty_upd);
    let restrict_res = live
        .moderate_messages(ModerateMessagesRequest {
            peer_id: group_peer,
            message_ids: Vec::new(),
            report_spam_peers: Vec::new(),
            delete_all_from_peers: Vec::new(),
            delete_all_reactions_peers: Vec::new(),
            ban_peers: vec![offender_user],
            ban_in_community_id: None,
            restrict_duration_secs: Some(3600),
        })
        .expect("moderate_messages restrict");
    assert_eq!(restrict_res.banned_peers, 1);
    let restrict_bans = rpc.recorded_of::<tl_fns::channels::EditBanned>();
    assert_eq!(restrict_bans.len(), 1);
    let tl_enums::ChatBannedRights::ChatBannedRights(br_restrict) = &restrict_bans[0].banned_rights;
    assert!(!br_restrict.view_messages);
    assert!(br_restrict.send_plain);
    assert!(br_restrict.send_photos);
    assert!(br_restrict.until_date > 0);
}
