//! Fake-server regression tests for bugs discovered during real Test-DC2 single-account smoke runs:
//! 1. `SessionRuntime::apply_updates` (`ApiWrap::applyUpdates`, `apiwrap.cpp:2332-2336`) and
//!    `SessionRuntime::delete_messages` (`Data::Histories::deleteMessages`,
//!    `data/data_histories.cpp:975-1065` + `ApiWrap::applyAffectedMessages`, `apiwrap.cpp:2350-2358`)
//!    routing RPC-returned updates and deletions through `SessionActor`'s single `UpdatesHandler`.
//! 2. `SessionRuntime::edit_message` handling `400 MESSAGE_NOT_MODIFIED` (including after MTProto
//!    retries a `500 RPC_CALL_FAIL` whose first attempt committed on the server, matching
//!    `HistoryWidget::saveEdit` in `history/history_widget.cpp:5736` and
//!    `EditMessageWithUploadedMedia` in `api/api_editing.cpp:426`) by syncing difference so
//!    `Session` converges to the committed edit.
//! 3. `UpdatesHandler` & `UpdatesStateMachine::remove_channel` stopping `channels.getChannels` and
//!    `updates.getChannelDifference` retry loops on forbidden / `CHANNEL_PRIVATE` /
//!    `CHANNEL_INVALID` channels (`Updates::applyUpdateNoPtsCheck(mtpc_updateChannel)` in
//!    `api/api_updates.cpp:2539-2562`, `Updates::channelDifferenceFail` in
//!    `api/api_updates.cpp:470-478`, `ChatParticipants::requestSelf` in
//!    `api/api_chat_participants.cpp:748`).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient, RpcError, SendOptions, UpdatesHandler};
use tg_data::{
    ChangeEvent, ChannelFlags, ChannelId, FullMsgId, MsgId, PeerId, Session, UpdatesStateMachine,
    UserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
struct RecordedCall {
    constructor_id: u32,
    request_bytes: Vec<u8>,
}

impl RecordedCall {
    fn decode<R: Identifiable + Deserializable>(&self) -> Option<R> {
        if self.constructor_id == R::CONSTRUCTOR_ID {
            R::from_bytes(&self.request_bytes).ok()
        } else {
            None
        }
    }
}

type RpcResponseQueue = VecDeque<Result<Vec<u8>, InvocationError>>;

struct ScriptedRpcInner {
    main_dc: AtomicI32,
    calls: Mutex<Vec<RecordedCall>>,
    queues: Mutex<HashMap<u32, RpcResponseQueue>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

#[derive(Clone)]
struct ScriptedRpc {
    inner: Arc<ScriptedRpcInner>,
}

impl ScriptedRpc {
    fn new(dc_id: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            inner: Arc::new(ScriptedRpcInner {
                main_dc: AtomicI32::new(dc_id),
                calls: Mutex::new(Vec::new()),
                queues: Mutex::new(HashMap::new()),
                updates_tx,
            }),
        }
    }

    fn enqueue_ok<R>(&self, resp: &R::Return)
    where
        R: Identifiable + RemoteCall,
        R::Return: Serializable,
    {
        self.inner
            .queues
            .lock()
            .entry(R::CONSTRUCTOR_ID)
            .or_default()
            .push_back(Ok(resp.to_bytes()));
    }

    fn enqueue_err<R>(&self, err: InvocationError)
    where
        R: Identifiable + RemoteCall,
    {
        self.inner
            .queues
            .lock()
            .entry(R::CONSTRUCTOR_ID)
            .or_default()
            .push_back(Err(err));
    }

    fn calls_of<R: Identifiable + Deserializable>(&self) -> Vec<R> {
        self.inner
            .calls
            .lock()
            .iter()
            .filter_map(RecordedCall::decode::<R>)
            .collect()
    }
}

impl RpcClient for ScriptedRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.inner.calls.lock().push(RecordedCall {
            constructor_id: ctor,
            request_bytes: bytes,
        });
        let next = self
            .inner
            .queues
            .lock()
            .get_mut(&ctor)
            .and_then(VecDeque::pop_front);
        match next {
            Some(Ok(resp_bytes)) => R::Return::from_bytes(&resp_bytes)
                .map_err(|e| InvocationError::Deserialize(e.to_string())),
            Some(Err(err)) => Err(err),
            None => Err(InvocationError::Transport(format!(
                "no scripted response for constructor 0x{ctor:08x}"
            ))),
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
        self.inner.main_dc.load(Ordering::Acquire)
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        self.inner.main_dc.store(dc_id, Ordering::Release);
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.inner.updates_tx.subscribe())
    }
}

fn self_tl_user(id: i64) -> tl_types::User {
    tl_types::User {
        self_: true,
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
        bot_forum_sidebar: false,
        bot_guard: false,
        bot_guestchat: false,
        id,
        access_hash: Some(1111),
        first_name: Some("Test".to_owned()),
        last_name: Some("User".to_owned()),
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

fn tl_text_message(id: i32, peer: tl_enums::Peer, text: &str) -> tl_enums::Message {
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
        from_id: Some(peer.clone()),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        reply_to: None,
        date: 1_700_000_050,
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

fn tl_channel(id: i64, access_hash: i64, title: &str) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: true,
        left: false,
        broadcast: false,
        verified: false,
        megagroup: true,
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
        access_hash: Some(access_hash),
        title: title.to_owned(),
        username: None,
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_700_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(1),
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
    })
}

#[tokio::test]
async fn apply_updates_and_delete_messages_flow_through_single_updates_handler() {
    let rpc = ScriptedRpc::new(2);
    let self_uid = UserId(42);
    let self_peer = PeerId::from_user(self_uid);
    let tl_self_peer = tl_enums::Peer::User(tl_types::PeerUser { user_id: 42 });

    rpc.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(
        tl_types::updates::State {
            pts: 10,
            qts: 0,
            date: 1_700_000_000,
            seq: 1,
            unread_count: 0,
        },
    ));

    let cfg = SessionRuntimeConfig {
        init_updates_state: true,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        idle_ping_enabled: false,
        tick_interval: Duration::from_millis(50),
        ..SessionRuntimeConfig::default()
    };
    let runtime = SessionRuntime::start_on(
        self_uid,
        rpc.clone(),
        tokio::runtime::Handle::current(),
        cfg,
    )
    .expect("start runtime");
    runtime.seed_self_user_async(&self_tl_user(42)).await;
    runtime.wait_ready().await.expect("wait_ready");
    let mut changes = runtime.subscribe_changes();

    // 1. Send message -> pts 11.
    rpc.enqueue_ok::<tl_fns::messages::SendMessage>(&tl_enums::Updates::UpdateShortSentMessage(
        tl_types::UpdateShortSentMessage {
            out: true,
            id: 100,
            pts: 11,
            pts_count: 1,
            date: 1_700_000_010,
            media: None,
            entities: None,
            ttl_period: None,
        },
    ));
    let _ = runtime
        .send_message(self_peer, "Original", SendOptions::default())
        .await
        .expect("send_message");

    // 2. Apply editMessage Updates via `SessionRuntime::apply_updates` -> pts 12.
    let edit_upd = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::EditMessage(tl_types::UpdateEditMessage {
            message: tl_text_message(100, tl_self_peer.clone(), "Edited via apply_updates"),
            pts: 12,
            pts_count: 1,
        })],
        users: vec![],
        chats: vec![],
        date: 1_700_000_020,
        seq: 0,
    });
    runtime.apply_updates(edit_upd).await.expect("apply edit");

    // 3. Apply pin Updates via `SessionRuntime::apply_updates` -> pts 13.
    let pin_upd = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::PinnedMessages(
            tl_types::UpdatePinnedMessages {
                pinned: true,
                peer: tl_self_peer.clone(),
                messages: vec![100],
                pts: 13,
                pts_count: 1,
            },
        )],
        users: vec![],
        chats: vec![],
        date: 1_700_000_030,
        seq: 0,
    });
    runtime.apply_updates(pin_upd).await.expect("apply pin");

    // 4. Apply reaction Updates via `SessionRuntime::apply_updates`.
    let react_upd = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::MessageReactions(
            tl_types::UpdateMessageReactions {
                peer: tl_self_peer.clone(),
                msg_id: 100,
                top_msg_id: None,
                saved_peer_id: None,
                reactions: tl_enums::MessageReactions::MessageReactions(
                    tl_types::MessageReactions {
                        min: false,
                        can_see_list: false,
                        reactions_as_tags: false,
                        results: vec![tl_enums::ReactionCount::ReactionCount(
                            tl_types::ReactionCount {
                                chosen_order: Some(0),
                                reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                                    emoticon: "\u{1F44D}".to_owned(),
                                }),
                                count: 1,
                            },
                        )],
                        recent_reactions: None,
                        top_reactors: None,
                    },
                ),
            },
        )],
        users: vec![],
        chats: vec![],
        date: 1_700_000_035,
        seq: 0,
    });
    runtime.apply_updates(react_upd).await.expect("apply react");

    {
        let s = runtime.session().read().await;
        let msg = s
            .message(FullMsgId::new(self_peer, MsgId(100)))
            .expect("message present");
        assert_eq!(
            msg.text().map(|t| t.text.as_str()),
            Some("Edited via apply_updates")
        );
        assert!(msg.is_pinned(), "message must be pinned");
        assert!(
            msg.reactions.as_ref().is_some_and(|r| r.results.len() == 1),
            "message must carry applied reaction"
        );
    }

    // 5. Delete via `SessionRuntime::delete_messages` -> pts 14, removed locally.
    rpc.enqueue_ok::<tl_fns::messages::DeleteMessages>(
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 14,
                pts_count: 1,
            },
        ),
    );
    let aff = runtime
        .delete_messages(self_peer, vec![MsgId(100)], true)
        .await
        .expect("delete_messages");
    assert_eq!(aff.pts, 14);
    {
        let s = runtime.session().read().await;
        assert!(
            s.message(FullMsgId::new(self_peer, MsgId(100))).is_none(),
            "deleted message must be removed from Session"
        );
    }

    let persisted = runtime.persisted_updates_state().await.expect("state");
    assert_eq!(persisted.pts, 14, "pts must advance cleanly to 14");
    assert!(
        rpc.calls_of::<tl_fns::updates::GetDifference>().is_empty(),
        "contiguous apply_updates/delete_messages must not trigger spurious getDifference"
    );

    let mut saw_message_event = false;
    while let Ok(ev) = changes.try_recv() {
        if matches!(ev, ChangeEvent::MessageUpdated { .. }) {
            saw_message_event = true;
        }
    }
    assert!(
        saw_message_event,
        "ChangeEvent broadcast must receive updates"
    );

    runtime.shutdown_async().await;
}

#[tokio::test]
async fn edit_message_message_not_modified_syncs_channel_difference() {
    let rpc = ScriptedRpc::new(2);
    let self_uid = UserId(42);
    let ch_id = ChannelId(9001);
    let ch_peer = PeerId::from_channel(ch_id);
    let tl_ch_peer = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 9001 });

    rpc.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(
        tl_types::updates::State {
            pts: 5,
            qts: 0,
            date: 1_700_000_000,
            seq: 1,
            unread_count: 0,
        },
    ));

    let cfg = SessionRuntimeConfig {
        init_updates_state: true,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        idle_ping_enabled: false,
        tick_interval: Duration::from_millis(50),
        ..SessionRuntimeConfig::default()
    };
    let runtime = SessionRuntime::start_on(
        self_uid,
        rpc.clone(),
        tokio::runtime::Handle::current(),
        cfg,
    )
    .expect("start runtime");
    runtime.seed_self_user_async(&self_tl_user(42)).await;
    runtime.wait_ready().await.expect("wait_ready");

    // Seed channel + initial message id=2 at channel_pts=3.
    {
        let mut s = runtime.session().write().await;
        apply_tl_users_and_chats(&mut s, &[], &[tl_channel(9001, 555_555, "Test Megagroup")]);
    }
    runtime.init_channel_pts(ch_id, 2).await.expect("init pts");
    runtime
        .apply_updates(tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewChannelMessage(
                tl_types::UpdateNewChannelMessage {
                    message: tl_text_message(2, tl_ch_peer.clone(), "megagroup post 1"),
                    pts: 3,
                    pts_count: 1,
                },
            )],
            users: vec![],
            chats: vec![],
            date: 1_700_000_010,
            seq: 0,
        }))
        .await
        .expect("seed channel msg");

    // Simulate Test-DC2 behavior: first editMessage committed on server then returned
    // 500 RPC_CALL_FAIL; MTProto retry receives 400 MESSAGE_NOT_MODIFIED.
    rpc.enqueue_err::<tl_fns::messages::EditMessage>(InvocationError::Rpc(RpcError {
        code: 400,
        name: "MESSAGE_NOT_MODIFIED".to_owned(),
        value: None,
    }));
    // `SessionRuntime::edit_message` responds to MESSAGE_NOT_MODIFIED by calling
    // `updates.getChannelDifference`, which delivers the committed edit.
    rpc.enqueue_ok::<tl_fns::updates::GetChannelDifference>(
        &tl_enums::updates::ChannelDifference::ChannelDifference(
            tl_types::updates::ChannelDifference {
                r#final: true,
                pts: 4,
                timeout: None,
                new_messages: vec![],
                other_updates: vec![tl_enums::Update::EditChannelMessage(
                    tl_types::UpdateEditChannelMessage {
                        message: tl_text_message(2, tl_ch_peer.clone(), "megagroup post 1 EDITED"),
                        pts: 4,
                        pts_count: 1,
                    },
                )],
                chats: vec![],
                users: vec![],
            },
        ),
    );

    let res = runtime
        .edit_message(tl_fns::messages::EditMessage {
            no_webpage: false,
            invert_media: false,
            peer: tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                channel_id: 9001,
                access_hash: 555_555,
            }),
            id: 2,
            message: Some("megagroup post 1 EDITED".to_owned()),
            media: None,
            reply_markup: None,
            entities: None,
            schedule_date: None,
            schedule_repeat_period: None,
            quick_reply_shortcut_id: None,
            rich_message: None,
        })
        .await
        .expect("MESSAGE_NOT_MODIFIED must be treated as non-fatal");
    assert!(res.is_none(), "MESSAGE_NOT_MODIFIED returns Ok(None)");

    assert_eq!(
        rpc.calls_of::<tl_fns::updates::GetChannelDifference>()
            .len(),
        1,
        "must sync channel difference after MESSAGE_NOT_MODIFIED"
    );
    let s = runtime.session().read().await;
    let msg = s
        .message(FullMsgId::new(ch_peer, MsgId(2)))
        .expect("edited msg in Session");
    assert_eq!(
        msg.text().map(|t| t.text.as_str()),
        Some("megagroup post 1 EDITED"),
        "Session must converge to the committed edit via getChannelDifference"
    );
    drop(s);

    runtime.shutdown_async().await;
}

#[tokio::test]
async fn forbidden_and_private_channel_stops_refetch_and_channel_difference_loops() {
    // 1. Unit check: `UpdatesStateMachine::remove_channel` clears channel pts & fail deadlines.
    let mut sm = UpdatesStateMachine::<()>::new();
    let ch_unit = ChannelId(7001);
    sm.init_channel_pts(ch_unit, 10);
    sm.set_active_channel(ch_unit, true, 1_000);
    sm.on_channel_difference_fail(ch_unit, 1_000);
    assert!(sm.next_deadline().is_some());
    sm.remove_channel(ch_unit);
    assert!(sm.channel_pts(ch_unit).is_none());
    assert!(sm.next_deadline().is_none());

    // 2. Integration check with `UpdatesHandler` + `ScriptedRpc`:
    let rpc = ScriptedRpc::new(2);
    rpc.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(
        tl_types::updates::State {
            pts: 10,
            qts: 0,
            date: 1_700_000_000,
            seq: 1,
            unread_count: 0,
        },
    ));
    let mut session = Session::new(UserId(42));
    let mut updates = UpdatesHandler::new();
    updates
        .initialize(&rpc, &mut session, 0)
        .await
        .expect("initialize updates");

    // Seed two channels: 7010 (will receive channelForbidden) and 7020 (will get CHANNEL_PRIVATE).
    apply_tl_users_and_chats(
        &mut session,
        &[],
        &[
            tl_channel(7010, 1010, "Deleted With Forbidden"),
            tl_channel(7020, 2020, "Deleted Returning Private"),
        ],
    );
    updates.init_channel_pts(ChannelId(7010), 5);
    updates.init_channel_pts(ChannelId(7020), 5);
    updates.set_active_channel(ChannelId(7020), true, 1_000);

    // 2A. Batch contains `channelForbidden` + `updateChannel(7010)` (like `channels.deleteChannel`).
    //     Must NOT call `channels.getChannels` and must remove channel 7010 from state machine.
    let del_batch = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::Channel(tl_types::UpdateChannel {
            channel_id: 7010,
        })],
        users: vec![],
        chats: vec![tl_enums::Chat::ChannelForbidden(
            tl_types::ChannelForbidden {
                broadcast: false,
                megagroup: true,
                monoforum: false,
                id: 7010,
                access_hash: 1010,
                title: "Deleted With Forbidden".to_owned(),
                until_date: None,
            },
        )],
        date: 1_700_000_010,
        seq: 0,
    });
    updates
        .handle_updates(&rpc, &mut session, del_batch, None, 1_000)
        .await
        .expect("handle forbidden batch");
    assert_eq!(
        rpc.calls_of::<tl_fns::channels::GetChannels>().len(),
        0,
        "must NOT invoke channels.getChannels for a forbidden channel"
    );
    assert!(updates.channel_pts(ChannelId(7010)).is_none());

    // 2B. `updateChannel(7020)` triggers `channels.getChannels`, which returns 400 CHANNEL_PRIVATE.
    //     Must mark 7020 FORBIDDEN, stop polling, and ignore a subsequent `updateChannel(7020)`.
    rpc.enqueue_err::<tl_fns::channels::GetChannels>(InvocationError::Rpc(RpcError {
        code: 400,
        name: "CHANNEL_PRIVATE".to_owned(),
        value: None,
    }));
    let upd_7020 = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::Channel(tl_types::UpdateChannel {
            channel_id: 7020,
        })],
        users: vec![],
        chats: vec![],
        date: 1_700_000_020,
        seq: 0,
    });
    updates
        .handle_updates(&rpc, &mut session, upd_7020.clone(), None, 2_000)
        .await
        .expect("first updateChannel handled");
    assert_eq!(rpc.calls_of::<tl_fns::channels::GetChannels>().len(), 1);
    assert!(
        session
            .channel(ChannelId(7020))
            .is_some_and(|c| c.flags.contains(ChannelFlags::FORBIDDEN)),
        "CHANNEL_PRIVATE must mark channel FORBIDDEN"
    );
    assert!(
        updates.channel_pts(ChannelId(7020)).is_none(),
        "CHANNEL_PRIVATE must clear channel PTS/timers"
    );

    // Second `updateChannel(7020)` must NOT fire another `channels.getChannels`.
    updates
        .handle_updates(&rpc, &mut session, upd_7020, None, 3_000)
        .await
        .expect("second updateChannel ignored");
    assert_eq!(
        rpc.calls_of::<tl_fns::channels::GetChannels>().len(),
        1,
        "subsequent updateChannel for FORBIDDEN channel must not spam channels.getChannels"
    );

    // 2C. `updates.getChannelDifference` returning 400 CHANNEL_PRIVATE must mark FORBIDDEN
    //     and NOT arm a fail-backoff retry timer.
    apply_tl_users_and_chats(&mut session, &[], &[tl_channel(7030, 3030, "Private Diff")]);
    updates.init_channel_pts(ChannelId(7030), 5);
    updates.set_active_channel(ChannelId(7030), true, 4_000);
    rpc.enqueue_err::<tl_fns::updates::GetChannelDifference>(InvocationError::Rpc(RpcError {
        code: 400,
        name: "CHANNEL_PRIVATE".to_owned(),
        value: None,
    }));
    let too_long = tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::ChannelTooLong(tl_types::UpdateChannelTooLong {
            channel_id: 7030,
            pts: None,
        }),
        date: 1_700_000_030,
    });
    updates
        .handle_updates(&rpc, &mut session, too_long, None, 4_000)
        .await
        .expect("CHANNEL_PRIVATE in getChannelDifference must resolve cleanly without error loop");
    assert_eq!(
        rpc.calls_of::<tl_fns::updates::GetChannelDifference>()
            .len(),
        1
    );
    assert!(
        session
            .channel(ChannelId(7030))
            .is_some_and(|c| c.flags.contains(ChannelFlags::FORBIDDEN)),
        "CHANNEL_PRIVATE in getChannelDifference must mark channel FORBIDDEN"
    );
    assert!(
        updates.channel_pts(ChannelId(7030)).is_none(),
        "CHANNEL_PRIVATE in getChannelDifference must remove channel timers"
    );
    // Advancing ticks must NOT retry `updates.getChannelDifference` for channel 7030.
    updates
        .on_tick(&rpc, &mut session, 100_000)
        .await
        .expect("tick ok");
    assert_eq!(
        rpc.calls_of::<tl_fns::updates::GetChannelDifference>()
            .len(),
        1,
        "must NOT retry getChannelDifference in backoff loop after CHANNEL_PRIVATE"
    );
}

#[test]
fn active_channel_short_poll_rearms_when_init_channel_pts_arrives_after_tick() {
    // Regression for `Updates::addActiveChat` (`api/api_updates.cpp:780-783`) and
    // `ChannelData::ptsInit`: if `set_active_channel(ch, true)` is called before `pts` is
    // initialized and the 1 s short-poll deadline expires before `init_channel_pts(ch, pts)`,
    // `init_channel_pts` must re-arm the short-poll deadline so active channel polling resumes.
    let mut machine: UpdatesStateMachine<()> = UpdatesStateMachine::new();
    let _ = machine.init_state(10, 1_700_000_000, 0, 1, 1_000);

    let ch = ChannelId(9001);
    machine.set_active_channel(ch, true, 1_000);
    // Tick past the initial 1 s deadline while channel pts is still uninitialized.
    let actions_before_init = machine.on_tick(2_500);
    assert!(
        actions_before_init.is_empty(),
        "must not request getChannelDifference before channel pts is initialized"
    );

    // Now history loads and initializes channel pts.
    machine.init_channel_pts(ch, 3);
    assert!(
        machine.next_deadline().is_some(),
        "init_channel_pts must re-arm short-poll deadline for already-active channel"
    );

    let actions_after_init = machine.on_tick(5_000);
    assert!(
        actions_after_init.iter().any(|a| matches!(
            a,
            tg_data::UpdateAction::RequestChannelDifference {
                channel_id,
                pts: 3,
                ..
            } if *channel_id == ch
        )),
        "expected RequestChannelDifference after init_channel_pts re-armed active poll, got {actions_after_init:?}"
    );
}

#[tokio::test]
async fn live_incoming_message_does_not_double_increment_known_unread_count() {
    // Regression for `History::addNewMessage` (`history/history.cpp:1717-1723`):
    // `Session::insert_message` increments `history.unread_count` once when `Some(n)`;
    // `SessionRuntime` must NOT increment it a second time (`0 -> 2` for a single message).
    let rpc = ScriptedRpc::new(2);
    let self_uid = UserId(200);
    let peer_uid = UserId(100);
    let peer_id = PeerId::from_user(peer_uid);

    rpc.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(
        tl_types::updates::State {
            pts: 10,
            qts: 0,
            date: 1_700_000_000,
            seq: 1,
            unread_count: 0,
        },
    ));
    rpc.enqueue_ok::<tl_fns::updates::GetDifference>(&tl_enums::updates::Difference::Empty(
        tl_types::updates::DifferenceEmpty {
            date: 1_700_000_000,
            seq: 1,
        },
    ));

    let handle = tokio::runtime::Handle::current();
    let cfg = SessionRuntimeConfig {
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        idle_ping_enabled: false,
        ..SessionRuntimeConfig::default()
    };
    let runtime =
        SessionRuntime::start_on(self_uid, rpc.clone(), handle, cfg).expect("start SessionRuntime");
    runtime.seed_self_user_async(&self_tl_user(200)).await;
    runtime.wait_ready().await.expect("wait_ready");

    // Seed peer and known unread_count = Some(0) (as after getDialogs or mark_read).
    {
        let mut s = runtime.session().write().await;
        let mut peer_tl = self_tl_user(100);
        peer_tl.self_ = false;
        apply_tl_users_and_chats(&mut s, &[tl_enums::User::User(peer_tl)], &[]);
        s.set_history_unread_count(peer_id, 0);
    }

    // Push 1 incoming message via live MTProto updates stream.
    let incoming = tl_enums::Updates::UpdateShortMessage(tl_types::UpdateShortMessage {
        out: false,
        mentioned: false,
        media_unread: false,
        silent: false,
        id: 50,
        user_id: 100,
        message: "single live incoming msg".to_owned(),
        pts: 11,
        pts_count: 1,
        date: 1_700_000_010,
        fwd_from: None,
        via_bot_id: None,
        reply_to: None,
        entities: None,
        ttl_period: None,
    });
    let _ = rpc.inner.updates_tx.send(incoming);

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let s = runtime.session().read().await;
            if s.message(FullMsgId::new(peer_id, MsgId(50))).is_some() {
                break;
            }
            drop(s);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("live message arrived");

    let unread = {
        let s = runtime.session().read().await;
        s.history(peer_id).and_then(|h| h.unread_count)
    };
    assert_eq!(
        unread,
        Some(1),
        "single live incoming message on a known unread_count=Some(0) history must increment to 1, not 2"
    );

    runtime.shutdown_async().await;
}

#[tokio::test]
async fn live_group_and_supergroup_typing_updates_populate_session() {
    // Deterministic verification for `Updates::applyUpdateNoPtsCheck`
    // (`mtpc_updateChatUserTyping` & `mtpc_updateChannelUserTyping`, `api/api_updates.cpp:2091-2107`):
    // live `UpdateShort` typing pushes in basic groups and supergroups populate `Session::active_typing`.
    let rpc = ScriptedRpc::new(2);
    let self_uid = UserId(200);
    let sender_peer = PeerId::from_user(UserId(100));
    let chat_peer = PeerId::from_chat(tg_data::ChatId(500));
    let ch_peer = PeerId::from_channel(ChannelId(9001));

    rpc.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(
        tl_types::updates::State {
            pts: 10,
            qts: 0,
            date: 1_700_000_000,
            seq: 1,
            unread_count: 0,
        },
    ));
    rpc.enqueue_ok::<tl_fns::updates::GetDifference>(&tl_enums::updates::Difference::Empty(
        tl_types::updates::DifferenceEmpty {
            date: 1_700_000_000,
            seq: 1,
        },
    ));

    let cfg = SessionRuntimeConfig {
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        idle_ping_enabled: false,
        ..SessionRuntimeConfig::default()
    };
    let runtime = SessionRuntime::start_on(
        self_uid,
        rpc.clone(),
        tokio::runtime::Handle::current(),
        cfg,
    )
    .expect("start SessionRuntime");
    runtime.seed_self_user_async(&self_tl_user(200)).await;
    runtime.wait_ready().await.expect("wait_ready");

    let _ = rpc
        .inner
        .updates_tx
        .send(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
            update: tl_enums::Update::ChatUserTyping(tl_types::UpdateChatUserTyping {
                chat_id: 500,
                from_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 }),
                action: tl_enums::SendMessageAction::SendMessageTypingAction,
            }),
            date: 1_700_000_010,
        }));
    let _ = rpc
        .inner
        .updates_tx
        .send(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
            update: tl_enums::Update::ChannelUserTyping(tl_types::UpdateChannelUserTyping {
                channel_id: 9001,
                top_msg_id: None,
                from_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 }),
                action: tl_enums::SendMessageAction::SendMessageTypingAction,
            }),
            date: 1_700_000_011,
        }));

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let s = runtime.session().read().await;
            let chat_ok = s
                .active_typing(chat_peer)
                .into_iter()
                .any(|(from, _, act)| from == sender_peer && act == tg_data::TypingAction::Typing);
            let ch_ok = s
                .active_typing(ch_peer)
                .into_iter()
                .any(|(from, _, act)| from == sender_peer && act == tg_data::TypingAction::Typing);
            if chat_ok && ch_ok {
                break;
            }
            drop(s);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both ChatUserTyping and ChannelUserTyping applied live");

    runtime.shutdown_async().await;
}
