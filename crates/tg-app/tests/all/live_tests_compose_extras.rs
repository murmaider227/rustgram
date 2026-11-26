//! Integration and unit tests for WP-09 live compose extras (`LiveSession` + UI state).
//!
//! All tests run strictly against in-process `FakeRpcClient` and synthetic data.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{
    Channel, ChannelFlags, ChannelId, Message, MessageContent, MessageMedia, MessageReactionCount,
    MessageReactionsSummary, MsgId as DataMsgId, PeerId as DataPeerId, PollAnswer,
    PollAnswerVoters, PollData, PollId, PollResults, ReactionId, TextWithEntities as DataText,
    User, UserFlags, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::dialogs::backend::DialogsBackend;
use tg_app::history::backend::{
    HistoryBackend, HistoryMessage, MessageReactionsPage, PollAttachment, PollOption,
    PollVoterEntry, PollVotesPage, ReactionCounter, ReactionPeerEntry, SendMessageExtended,
};
use tg_app::history::compose_extras::{
    LINK_PREVIEW_DEBOUNCE_MS, LinkPreviewEditorState, TranslateBarState, detect_chat_language,
};
use tg_app::history::sections_model::LinkPreviewData;
use tg_app::info::reactions_list::{PollVotersWidget, ReactionsListWidget};
use tg_app::live::LiveSession;
use tg_app::live::compose_extras::parse_slowmode_wait_error;
use tg_app::live::convert::{now_unix_i32, now_unix_i64};
use tg_app::stand_in::types::{MsgId, PeerId};

// =============================================================================
// FakeRpcClient + synthetic fixtures
// =============================================================================

type RequestCapture = (u32, Vec<u8>);
type ResponseEntry = (u32, Result<Vec<u8>, InvocationError>);
type DynamicHandler =
    Arc<dyn Fn(u32, &[u8]) -> Option<Result<Vec<u8>, InvocationError>> + Send + Sync>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RequestCapture>>>,
    responses: Arc<Mutex<VecDeque<ResponseEntry>>>,
    dynamic: Arc<Mutex<Option<DynamicHandler>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            dynamic: Arc::new(Mutex::new(None)),
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

    fn set_dynamic_handler(&self, handler: DynamicHandler) {
        *self.dynamic.lock().expect("lock dynamic") = Some(handler);
    }

    fn recorded(&self) -> Vec<RequestCapture> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn count_ctor(&self, ctor: u32) -> usize {
        self.recorded().iter().filter(|(c, _)| *c == ctor).count()
    }
}

impl RpcClient for FakeRpcClient {
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
            .push((ctor, bytes.clone()));

        if let Some(handler) = self.dynamic.lock().expect("lock dynamic").clone()
            && let Some(res) = handler(ctor, &bytes)
        {
            let raw = res?;
            return R::Return::from_bytes(&raw)
                .map_err(|e| InvocationError::Deserialize(e.to_string()));
        }

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
                q.remove(pos).map(|(_, r)| r)
            } else if crate::common::is_background_list_request(ctor) {
                // Never the answer a test queued for another request (TGB-2941).
                None
            } else {
                q.pop_front().map(|(_, r)| r)
            }
        };
        let raw = next.unwrap_or(Err(InvocationError::Dropped))?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
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

fn synthetic_tl_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
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
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 1000 + 7),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Tester".to_owned()),
        username: Some(username.to_owned()),
        phone: Some("0000000000".to_owned()),
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: Some("en".to_owned()),
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

fn synthetic_tl_channel(id: i64, title: &str, megagroup: bool) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: false,
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
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 31 + 9),
        title: title.to_owned(),
        username: Some(format!("chan_{id}")),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_795_340_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(120),
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

fn synthetic_tl_channel_full(
    channel_id: i64,
    slowmode_seconds: Option<i32>,
    slowmode_next_send_date: Option<i32>,
    default_send_as: Option<tl_enums::Peer>,
    translations_disabled: bool,
) -> tl_enums::messages::ChatFull {
    tl_enums::messages::ChatFull::ChatFull(tl_types::messages::ChatFull {
        full_chat: tl_enums::ChatFull::ChannelFull(tl_types::ChannelFull {
            can_view_participants: true,
            can_set_username: false,
            can_set_stickers: false,
            hidden_prehistory: false,
            can_set_location: false,
            has_scheduled: false,
            can_view_stats: false,
            blocked: false,
            can_delete_channel: false,
            antispam: false,
            participants_hidden: false,
            translations_disabled,
            stories_pinned_available: false,
            view_forum_as_messages: false,
            restricted_sponsored: false,
            can_view_revenue: false,
            paid_media_allowed: false,
            can_view_stars_revenue: false,
            paid_reactions_available: false,
            stargifts_available: false,
            paid_messages_available: false,
            has_welcome_messages: false,
            id: channel_id,
            about: "Synthetic test group".to_owned(),
            participants_count: Some(120),
            admins_count: Some(2),
            kicked_count: None,
            banned_count: None,
            online_count: Some(15),
            read_inbox_max_id: 100,
            read_outbox_max_id: 100,
            unread_count: 0,
            chat_photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
            notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                tl_types::PeerNotifySettings {
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
                },
            ),
            exported_invite: None,
            bot_info: Vec::new(),
            migrated_from_chat_id: None,
            migrated_from_max_id: None,
            pinned_msg_id: None,
            stickerset: None,
            available_min_id: None,
            folder_id: None,
            linked_chat_id: None,
            location: None,
            slowmode_seconds,
            slowmode_next_send_date,
            stats_dc: None,
            pts: 10,
            call: None,
            ttl_period: None,
            pending_suggestions: None,
            groupcall_default_join_as: None,
            theme_emoticon: None,
            requests_pending: None,
            recent_requesters: None,
            default_send_as,
            available_reactions: None,
            reactions_limit: None,
            stories: None,
            wallpaper: None,
            boosts_applied: None,
            boosts_unrestrict: None,
            emojiset: None,
            bot_verification: None,
            stargifts_count: None,
            send_paid_messages_stars: None,
            main_tab: None,
            guard_bot_id: None,
            share_theme: None,
        }),
        chats: Vec::new(),
        users: Vec::new(),
    })
}

fn synthetic_tl_doc(id: i64) -> tl_enums::Document {
    tl_enums::Document::Empty(tl_types::DocumentEmpty { id })
}

fn build_live_fixture(
    is_premium: bool,
) -> (
    tokio::runtime::Runtime,
    FakeRpcClient,
    LiveSession,
    PeerId,
    PeerId,
) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("tokio rt");
    let rpc = FakeRpcClient::new(2);
    let session_rt = Arc::new(
        SessionRuntime::start_on(
            DataUserId(1000),
            tg_app::live::ErasedRpcClient::new(rpc.clone()),
            rt.handle().clone(),
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

    let chan_id = ChannelId(5001);
    let chan_peer_data = DataPeerId::from_channel(chan_id);
    let alias_chan_id = ChannelId(5002);
    let alias_peer_data = DataPeerId::from_channel(alias_chan_id);
    let self_uid = DataUserId(1000);
    let peer_uid = DataUserId(1001);

    rt.block_on(async {
        let mut s = session_rt.session().write().await;

        let mut self_user = User::new(self_uid, true);
        self_user.first_name = "SynthAlpha".to_owned();
        self_user.last_name = "Tester".to_owned();
        self_user.common.name = "SynthAlpha Tester".to_owned();
        self_user.access_hash = 1_000_007;
        if is_premium {
            self_user.flags.insert(UserFlags::PREMIUM);
        }
        s.upsert_user(self_user, tg_data::PeerUpdateFlags::empty());

        let mut other_user = User::new(peer_uid, false);
        other_user.first_name = "SynthBeta".to_owned();
        other_user.last_name = "Tester".to_owned();
        other_user.common.name = "SynthBeta Tester".to_owned();
        other_user.access_hash = 1_001_007;
        s.upsert_user(other_user, tg_data::PeerUpdateFlags::empty());

        let mut group = Channel::new(chan_id);
        group.common.name = "Core Discussion".to_owned();
        group.access_hash = 5_001_009;
        group.flags.remove(ChannelFlags::FORBIDDEN);
        group.flags.insert(ChannelFlags::MEGAGROUP);
        s.upsert_channel(group, tg_data::PeerUpdateFlags::empty());

        let mut alias = Channel::new(alias_chan_id);
        alias.common.name = "Dev Broadcast".to_owned();
        alias.access_hash = 5_002_009;
        alias.flags.remove(ChannelFlags::FORBIDDEN);
        alias.flags.insert(ChannelFlags::BROADCAST);
        s.upsert_channel(alias, tg_data::PeerUpdateFlags::empty());

        // Seed one text message and one non-anonymous poll message in the supergroup.
        let mut msg1 = Message::new_text(
            chan_peer_data,
            DataMsgId(42),
            Some(DataPeerId::from_user(peer_uid)),
            now_unix_i32() - 60,
            DataText::plain("Привет мир! Как дела?"),
        );
        msg1.reactions = Some(MessageReactionsSummary {
            results: vec![MessageReactionCount {
                id: ReactionId::Emoji("👍".to_owned()),
                count: 2,
                chosen_order: None,
            }],
            ..MessageReactionsSummary::default()
        });

        let mut poll_msg = Message::new_text(
            chan_peer_data,
            DataMsgId(43),
            Some(DataPeerId::from_user(self_uid)),
            now_unix_i32() - 30,
            DataText::default(),
        );
        poll_msg.flags.insert(tg_data::MessageFlags::OUTGOING);
        poll_msg.content = MessageContent::Regular {
            text: DataText::default(),
            media: MessageMedia::Poll(Box::new(PollData {
                id: PollId(99_001),
                question: DataText::plain("Preferred build target?"),
                answers: vec![
                    PollAnswer {
                        text: DataText::plain("Windows x64"),
                        option: b"opt0".to_vec(),
                    },
                    PollAnswer {
                        text: DataText::plain("Linux x64"),
                        option: b"opt1".to_vec(),
                    },
                ],
                closed: false,
                public_voters: true,
                multiple_choice: false,
                quiz: false,
                close_period: None,
                close_date: None,
                results: PollResults {
                    results: vec![
                        PollAnswerVoters {
                            option: b"opt0".to_vec(),
                            voters: 2,
                            chosen: true,
                            correct: false,
                        },
                        PollAnswerVoters {
                            option: b"opt1".to_vec(),
                            voters: 1,
                            chosen: false,
                            correct: false,
                        },
                    ],
                    total_voters: Some(3),
                    recent_voters: vec![DataPeerId::from_user(self_uid)],
                    solution: None,
                },
                ..PollData::default()
            })),
        };
        s.add_history_slice(
            chan_peer_data,
            vec![msg1, poll_msg],
            tg_data::MsgRange::new(DataMsgId(1), DataMsgId(i64::from(i32::MAX - 1))),
            Some(2),
        );
    });

    let live = LiveSession::new(session_rt, 99);
    (
        rt,
        rpc,
        live,
        PeerId(chan_peer_data.0),
        PeerId(alias_peer_data.0),
    )
}

// =============================================================================
// 1. Send-As tests (`channels.getSendAs`, `messages.saveDefaultSendAs`)
// =============================================================================

#[test]
fn send_as_fetches_caches_saves_default_and_forwards_on_send() {
    let (_rt, rpc, mut live, chan_peer, alias_peer) = build_live_fixture(false);

    // 1. ChannelFull + channels.getSendAs
    rpc.enqueue_ok(
        tl_fns::channels::GetFullChannel::CONSTRUCTOR_ID,
        &synthetic_tl_channel_full(
            5001,
            None,
            None,
            Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                channel_id: 5002,
            })),
            false,
        ),
    );
    rpc.enqueue_ok(
        tl_fns::channels::GetSendAs::CONSTRUCTOR_ID,
        &tl_enums::channels::SendAsPeers::SendAsPeers(tl_types::channels::SendAsPeers {
            peers: vec![
                tl_enums::SendAsPeer::SendAsPeer(tl_types::SendAsPeer {
                    premium_required: false,
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 1000 }),
                }),
                tl_enums::SendAsPeer::SendAsPeer(tl_types::SendAsPeer {
                    premium_required: true,
                    peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 5002 }),
                }),
            ],
            chats: vec![synthetic_tl_channel(5002, "Dev Broadcast", false)],
            users: vec![tl_enums::User::User(synthetic_tl_user(
                1000,
                "SynthAlpha",
                "synth_alpha",
                true,
            ))],
        }),
    );

    let peers = live.send_as_peers(chan_peer);
    assert_eq!(peers.len(), 2);
    assert_eq!(peers[0].peer_id, PeerId::from_user(1000));
    assert_eq!(peers[1].peer_id, alias_peer);
    assert!(peers[1].is_channel);
    assert_eq!(live.default_send_as_peer(chan_peer), Some(alias_peer));

    // Cached within 30s TTL: second call does NOT hit the network again.
    let peers_cached = live.send_as_peers(chan_peer);
    assert_eq!(peers_cached.len(), 2);
    assert_eq!(
        rpc.count_ctor(tl_fns::channels::GetSendAs::CONSTRUCTOR_ID),
        1
    );

    // 2. messages.saveDefaultSendAs
    rpc.enqueue_ok(tl_fns::messages::SaveDefaultSendAs::CONSTRUCTOR_ID, &true);
    live.save_default_send_as(chan_peer, PeerId::from_user(1000))
        .expect("save default send-as");
    assert_eq!(
        live.default_send_as_peer(chan_peer),
        Some(PeerId::from_user(1000))
    );
    let save_bytes = rpc
        .recorded()
        .into_iter()
        .find(|(c, _)| *c == tl_fns::messages::SaveDefaultSendAs::CONSTRUCTOR_ID)
        .expect("SaveDefaultSendAs invoked")
        .1;
    let decoded_save =
        tl_fns::messages::SaveDefaultSendAs::from_bytes(&save_bytes).expect("decode save");
    assert!(matches!(
        decoded_save.send_as,
        tl_enums::InputPeer::Self_ | tl_enums::InputPeer::User(_)
    ));

    // 3. send_message_extended forwards `send_as` and `effect_id`
    rpc.enqueue_ok(
        tl_fns::messages::SendMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewChannelMessage(
                tl_types::UpdateNewChannelMessage {
                    message: tl_enums::Message::Message(tl_types::Message {
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
                        id: 500,
                        from_id: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                            channel_id: 5002,
                        })),
                        from_boosts_applied: None,
                        from_rank: None,
                        peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel {
                            channel_id: 5001,
                        }),
                        saved_peer_id: None,
                        fwd_from: None,
                        via_bot_id: None,
                        via_business_bot_id: None,
                        guestchat_via_from: None,
                        reply_to: None,
                        date: now_unix_i32(),
                        message: "Posting as channel".to_owned(),
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
                        effect: Some(5104841245755180586),
                        factcheck: None,
                        report_delivery_until_date: None,
                        paid_message_stars: None,
                        suggested_post: None,
                        schedule_repeat_period: None,
                        summary_from_language: None,
                        rich_message: None,
                    }),
                    pts: 11,
                    pts_count: 1,
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: now_unix_i32(),
            seq: 1,
        }),
    );

    let sent = live
        .send_message_extended(
            chan_peer,
            SendMessageExtended {
                text: TextWithEntities::simple("Posting as channel"),
                send_as: Some(alias_peer),
                effect_id: Some(5104841245755180586),
                ..SendMessageExtended::default()
            },
        )
        .expect("send as channel");
    assert!(sent.id.is_local());
    let sent = crate::send_wait::wait_sent(&mut live, chan_peer, sent.id);
    assert_eq!(sent.id, MsgId(500));

    let send_bytes = rpc
        .recorded()
        .into_iter()
        .find(|(c, _)| *c == tl_fns::messages::SendMessage::CONSTRUCTOR_ID)
        .expect("SendMessage invoked")
        .1;
    let decoded_send = tl_fns::messages::SendMessage::from_bytes(&send_bytes).expect("decode send");
    assert!(matches!(
        decoded_send.send_as,
        Some(tl_enums::InputPeer::Channel(ref c)) if c.channel_id == 5002
    ));
    assert_eq!(decoded_send.effect, Some(5104841245755180586));
}

// =============================================================================
// 2. Slow Mode tests (`ChannelFull.slowmode_seconds` / `slowmode_next_send_date`)
// =============================================================================

#[test]
fn slow_mode_reads_channel_full_blocks_sends_and_allows_scheduled() {
    let (_rt, rpc, mut live, chan_peer, _) = build_live_fixture(false);
    let now = now_unix_i32();
    let next_send = now + 25;

    rpc.enqueue_ok(
        tl_fns::channels::GetFullChannel::CONSTRUCTOR_ID,
        &synthetic_tl_channel_full(5001, Some(30), Some(next_send), None, false),
    );

    let sm = live.slow_mode_state(chan_peer);
    assert_eq!(sm.seconds, 30);
    assert_eq!(sm.next_send_unix, i64::from(next_send));
    assert!(sm.remaining_secs(now_unix_i64()) > 0);

    // Non-scheduled send is blocked locally before touching the network!
    let blocked = live.send_message_extended(
        chan_peer,
        SendMessageExtended {
            text: TextWithEntities::simple("Too fast!"),
            ..SendMessageExtended::default()
        },
    );
    assert!(blocked.is_err());
    assert!(
        blocked
            .expect_err("slowmode err")
            .to_lowercase()
            .contains("slow mode")
    );
    assert_eq!(
        rpc.count_ctor(tl_fns::messages::SendMessage::CONSTRUCTOR_ID),
        0
    );

    // Scheduled messages bypass slow mode countdown (tdesktop `history_widget.cpp:6083`).
    rpc.enqueue_ok(
        tl_fns::messages::SendMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewChannelMessage(
                tl_types::UpdateNewChannelMessage {
                    message: tl_enums::Message::Message(tl_types::Message {
                        out: true,
                        mentioned: false,
                        media_unread: false,
                        silent: false,
                        post: false,
                        from_scheduled: true,
                        legacy: false,
                        edit_hide: false,
                        pinned: false,
                        noforwards: false,
                        invert_media: false,
                        offline: false,
                        video_processing_pending: false,
                        paid_suggested_post_stars: false,
                        paid_suggested_post_ton: false,
                        id: 501,
                        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 1000 })),
                        from_boosts_applied: None,
                        from_rank: None,
                        peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel {
                            channel_id: 5001,
                        }),
                        saved_peer_id: None,
                        fwd_from: None,
                        via_bot_id: None,
                        via_business_bot_id: None,
                        guestchat_via_from: None,
                        reply_to: None,
                        date: now + 3600,
                        message: "Scheduled during slowmode".to_owned(),
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
                    }),
                    pts: 12,
                    pts_count: 1,
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: now,
            seq: 2,
        }),
    );

    let sched = live
        .send_message_extended(
            chan_peer,
            SendMessageExtended {
                text: TextWithEntities::simple("Scheduled during slowmode"),
                schedule_date: Some(now + 3600),
                ..SendMessageExtended::default()
            },
        )
        .expect("scheduled send allowed during slowmode");
    assert_eq!(sched.id, MsgId(501));
    assert_eq!(
        parse_slowmode_wait_error("RPC 420 SLOWMODE_WAIT_47"),
        Some(47)
    );
}

#[test]
fn slow_mode_server_wait_error_updates_cooldown() {
    let (_rt, rpc, mut live, chan_peer, _) = build_live_fixture(false);

    rpc.enqueue_ok(
        tl_fns::channels::GetFullChannel::CONSTRUCTOR_ID,
        &synthetic_tl_channel_full(5001, Some(10), None, None, false),
    );
    rpc.enqueue_err(
        tl_fns::messages::SendMessage::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError::new(420, "SLOWMODE_WAIT_42")),
    );

    let pending = live
        .send_message_extended(
            chan_peer,
            SendMessageExtended {
                text: TextWithEntities::simple("Triggered server slowmode"),
                ..SendMessageExtended::default()
            },
        )
        .expect("the send is queued");
    // The server's SLOWMODE_WAIT arrives with the failed send (TGB-2931).
    match crate::send_wait::wait_send_end(&mut live, chan_peer, pending.id) {
        crate::send_wait::SendEnd::Failed(error) => assert!(error.is_some()),
        crate::send_wait::SendEnd::Sent(m) => panic!("unexpected success {m:?}"),
    }
    let sm = live.slow_mode_state(chan_peer);
    let rem = sm.remaining_secs(now_unix_i64());
    assert!(
        (35..=45).contains(&rem),
        "expected ~42s remaining, got {rem}"
    );
}

// =============================================================================
// 3. Link Preview tests (`messages.getWebPagePreview`, debounce, send options)
// =============================================================================

#[test]
fn link_preview_resolves_debounces_and_sends_media_options() {
    let (_rt, rpc, mut live, chan_peer, _) = build_live_fixture(false);
    rpc.enqueue_ok(
        tl_fns::channels::GetFullChannel::CONSTRUCTOR_ID,
        &synthetic_tl_channel_full(5001, None, None, None, false),
    );

    rpc.enqueue_ok(
        tl_fns::messages::GetWebPagePreview::CONSTRUCTOR_ID,
        &tl_enums::messages::WebPagePreview::WebPagePreview(tl_types::messages::WebPagePreview {
            media: tl_enums::MessageMedia::WebPage(tl_types::MessageMediaWebPage {
                force_large_media: true,
                force_small_media: false,
                manual: false,
                safe: true,
                webpage: tl_enums::WebPage::WebPage(tl_types::WebPage {
                    has_large_media: true,
                    video_cover_photo: false,
                    id: 777,
                    url: "https://example.org/rust".to_owned(),
                    display_url: "example.org/rust".to_owned(),
                    hash: 1,
                    r#type: Some("article".to_owned()),
                    site_name: Some("Example Docs".to_owned()),
                    title: Some("Rust Guide".to_owned()),
                    description: Some("Systems programming reference".to_owned()),
                    photo: None,
                    embed_url: None,
                    embed_type: None,
                    embed_width: None,
                    embed_height: None,
                    duration: None,
                    author: None,
                    document: None,
                    cached_page: None,
                    attributes: None,
                }),
            }),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    // Debounce behavior in LinkPreviewEditorState
    let mut editor = LinkPreviewEditorState::default();
    let pending1 = editor.sync_from_text_debounced("Check https://example.org/rust", 1_000, &live);
    assert!(pending1, "must wait debounce window before invoking RPC");
    assert_eq!(
        rpc.count_ctor(tl_fns::messages::GetWebPagePreview::CONSTRUCTOR_ID),
        0
    );

    let pending2 = editor.sync_from_text_debounced(
        "Check https://example.org/rust",
        1_000 + LINK_PREVIEW_DEBOUNCE_MS - 50,
        &live,
    );
    assert!(pending2);
    assert_eq!(
        rpc.count_ctor(tl_fns::messages::GetWebPagePreview::CONSTRUCTOR_ID),
        0
    );

    let pending3 = editor.sync_from_text_debounced(
        "Check https://example.org/rust",
        1_000 + LINK_PREVIEW_DEBOUNCE_MS,
        &live,
    );
    assert!(!pending3);
    let lp = editor.preview.clone().expect("preview resolved");
    assert_eq!(lp.site_name, "Example Docs");
    assert_eq!(lp.title, "Rust Guide");
    assert!(lp.large_media);
    assert_eq!(
        rpc.count_ctor(tl_fns::messages::GetWebPagePreview::CONSTRUCTOR_ID),
        1
    );

    // Second lookup for same URL uses cache (0 new RPCs)
    let cached = live
        .resolve_link_preview("https://example.org/rust")
        .expect("cached preview");
    assert_eq!(cached.title, "Rust Guide");
    assert_eq!(
        rpc.count_ctor(tl_fns::messages::GetWebPagePreview::CONSTRUCTOR_ID),
        1
    );

    // Toggle options: small media + above text, and send -> sends `messages.sendMedia(InputMediaWebPage)`
    let mut customized = lp;
    customized.large_media = false;
    customized.above_text = true;

    rpc.enqueue_ok(
        tl_fns::messages::SendMedia::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewChannelMessage(
                tl_types::UpdateNewChannelMessage {
                    message: tl_enums::Message::Message(tl_types::Message {
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
                        invert_media: true,
                        offline: false,
                        video_processing_pending: false,
                        paid_suggested_post_stars: false,
                        paid_suggested_post_ton: false,
                        id: 510,
                        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 1000 })),
                        from_boosts_applied: None,
                        from_rank: None,
                        peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel {
                            channel_id: 5001,
                        }),
                        saved_peer_id: None,
                        fwd_from: None,
                        via_bot_id: None,
                        via_business_bot_id: None,
                        guestchat_via_from: None,
                        reply_to: None,
                        date: now_unix_i32(),
                        message: "Check https://example.org/rust".to_owned(),
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
                    }),
                    pts: 13,
                    pts_count: 1,
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: now_unix_i32(),
            seq: 0,
        }),
    );

    let sent = live
        .send_message_extended(
            chan_peer,
            SendMessageExtended {
                text: TextWithEntities::simple("Check https://example.org/rust"),
                link_preview: Some(LinkPreviewData { ..customized }),
                no_webpage: false,
                ..SendMessageExtended::default()
            },
        )
        .expect("send with webpage options");
    assert_eq!(sent.id, MsgId(510));

    let send_media_bytes = rpc
        .recorded()
        .into_iter()
        .find(|(c, _)| *c == tl_fns::messages::SendMedia::CONSTRUCTOR_ID)
        .expect("SendMedia invoked")
        .1;
    let decoded = tl_fns::messages::SendMedia::from_bytes(&send_media_bytes).expect("decode");
    assert!(decoded.invert_media, "above_text must map to invert_media");
    match decoded.media {
        tl_enums::InputMedia::WebPage(wp) => {
            assert!(!wp.force_large_media);
            assert!(wp.force_small_media);
            assert!(wp.optional);
            assert_eq!(wp.url, "https://example.org/rust");
        }
        other => panic!("expected InputMedia::WebPage, got {other:?}"),
    }

    // Draft with `no_webpage: true` (`disabled_url = Some("")`) suppresses automatic preview fetch.
    let mut draft_editor = LinkPreviewEditorState::default();
    draft_editor.disabled_url = Some(String::new());
    let still_pending =
        draft_editor.sync_from_text_debounced("See https://example.org/rust", 5_000, &live);
    assert!(!still_pending);
    assert!(draft_editor.preview.is_none());
    assert!(draft_editor.is_disabled());

    // Re-enabling allows preview to resolve again after debounce.
    draft_editor.re_enable_preview();
    assert!(draft_editor.sync_from_text_debounced("See https://example.org/rust", 5_100, &live));
    assert!(!draft_editor.sync_from_text_debounced(
        "See https://example.org/rust",
        5_100 + LINK_PREVIEW_DEBOUNCE_MS,
        &live
    ));
    assert!(draft_editor.preview.is_some());
}

// =============================================================================
// 4. Translation tests (`messages.translateText`, `messages.togglePeerTranslations`)
// =============================================================================

#[test]
fn translation_single_batch_toggle_peer_and_do_not_translate() {
    let (_rt, rpc, mut live, chan_peer, _) = build_live_fixture(false);

    rpc.enqueue_ok(
        tl_fns::channels::GetFullChannel::CONSTRUCTOR_ID,
        &synthetic_tl_channel_full(5001, None, None, None, false),
    );
    rpc.enqueue_ok(
        tl_fns::messages::TranslateText::CONSTRUCTOR_ID,
        &tl_enums::messages::TranslatedText::TranslateResult(tl_types::messages::TranslateResult {
            result: vec![tl_enums::TextWithEntities::TextWithEntities(
                tl_types::TextWithEntities {
                    text: "Hello world! How are you?".to_owned(),
                    entities: Vec::new(),
                },
            )],
        }),
    );

    let tr = live
        .translate_message(chan_peer, MsgId(42), "en")
        .expect("translate single message");
    assert_eq!(tr, "Hello world! How are you?");

    // Cached translation decorates subsequent history slices
    let slice = live.load_slice_around(chan_peer, MsgId(42), 5, 5);
    let m42 = slice
        .messages
        .iter()
        .find(|m| m.id == MsgId(42))
        .expect("msg 42 present");
    assert_eq!(
        m42.extra.translated_text,
        Some(("en".to_owned(), "Hello world! How are you?".to_owned()))
    );

    // Language detector recognizes Russian text and TranslateBarState respects "Do not translate"
    let detected = detect_chat_language(&slice.messages);
    assert_eq!(detected, Some("ru"));
    let mut bar = TranslateBarState::default();
    bar.reset_for_peer(chan_peer);
    assert!(bar.should_show(detected, false, &live.do_not_translate_languages()));

    // Adding "ru" to Do Not Translate suppresses the bar
    live.set_do_not_translate_languages(vec!["en".to_owned(), "ru".to_owned()]);
    assert!(!bar.should_show(detected, false, &live.do_not_translate_languages()));

    // Clear translation ("Show Original")
    live.clear_message_translation(chan_peer, MsgId(42));
    let slice_after = live.load_slice_around(chan_peer, MsgId(42), 5, 5);
    let m42_after = slice_after
        .messages
        .iter()
        .find(|m| m.id == MsgId(42))
        .expect("msg 42");
    assert!(m42_after.extra.translated_text.is_none());

    // Batch translate whole chat
    rpc.enqueue_ok(
        tl_fns::messages::TranslateText::CONSTRUCTOR_ID,
        &tl_enums::messages::TranslatedText::TranslateResult(tl_types::messages::TranslateResult {
            result: vec![tl_enums::TextWithEntities::TextWithEntities(
                tl_types::TextWithEntities {
                    text: "Hola mundo!".to_owned(),
                    entities: Vec::new(),
                },
            )],
        }),
    );
    let batch = live
        .translate_messages_batch(chan_peer, &[MsgId(42)], "es")
        .expect("batch translate");
    assert_eq!(batch, vec![(MsgId(42), "Hola mundo!".to_owned())]);

    // Toggle peer translations disabled via `messages.togglePeerTranslations`
    rpc.enqueue_ok(
        tl_fns::messages::TogglePeerTranslations::CONSTRUCTOR_ID,
        &true,
    );
    live.toggle_peer_translations(chan_peer, true)
        .expect("disable peer translations");
    assert!(live.peer_translations_disabled(chan_peer));
    assert!(!bar.should_show(Some("fr"), true, &["en".to_owned()]));
}

// =============================================================================
// 5. Reactions tests (available catalog hash cache, multi-reaction, who-reacted,
//    default reaction, saved reaction tags)
// =============================================================================

#[test]
fn available_reactions_cached_by_hash_and_not_modified() {
    let (_rt, rpc, live, _, _) = build_live_fixture(false);

    rpc.enqueue_ok(
        tl_fns::messages::GetAvailableReactions::CONSTRUCTOR_ID,
        &tl_enums::messages::AvailableReactions::AvailableReactions(
            tl_types::messages::AvailableReactions {
                hash: 12345,
                reactions: vec![
                    tl_enums::AvailableReaction::AvailableReaction(tl_types::AvailableReaction {
                        inactive: false,
                        premium: false,
                        reaction: "👍".to_owned(),
                        title: "Thumbs Up".to_owned(),
                        static_icon: synthetic_tl_doc(1),
                        appear_animation: synthetic_tl_doc(2),
                        select_animation: synthetic_tl_doc(3),
                        activate_animation: synthetic_tl_doc(4),
                        effect_animation: synthetic_tl_doc(5),
                        around_animation: None,
                        center_icon: None,
                    }),
                    tl_enums::AvailableReaction::AvailableReaction(tl_types::AvailableReaction {
                        inactive: false,
                        premium: true,
                        reaction: "🐳".to_owned(),
                        title: "Whale".to_owned(),
                        static_icon: synthetic_tl_doc(6),
                        appear_animation: synthetic_tl_doc(7),
                        select_animation: synthetic_tl_doc(8),
                        activate_animation: synthetic_tl_doc(9),
                        effect_animation: synthetic_tl_doc(10),
                        around_animation: None,
                        center_icon: None,
                    }),
                    tl_enums::AvailableReaction::AvailableReaction(tl_types::AvailableReaction {
                        inactive: true,
                        premium: false,
                        reaction: "👻".to_owned(),
                        title: "Inactive".to_owned(),
                        static_icon: synthetic_tl_doc(11),
                        appear_animation: synthetic_tl_doc(12),
                        select_animation: synthetic_tl_doc(13),
                        activate_animation: synthetic_tl_doc(14),
                        effect_animation: synthetic_tl_doc(15),
                        around_animation: None,
                        center_icon: None,
                    }),
                ],
            },
        ),
    );

    let items = live.available_reactions();
    assert_eq!(items.len(), 2, "inactive reaction should be filtered");
    assert_eq!(items[0].emoji, "👍");
    assert!(!items[0].is_premium);
    assert_eq!(items[1].emoji, "🐳");
    assert!(items[1].is_premium);

    // Explicit refresh sends hash = 12345 and preserves cache on NotModified
    rpc.enqueue_ok(
        tl_fns::messages::GetAvailableReactions::CONSTRUCTOR_ID,
        &tl_enums::messages::AvailableReactions::NotModified,
    );
    let refreshed = live.refresh_available_reactions_live();
    assert_eq!(refreshed.len(), 2);

    let calls: Vec<tl_fns::messages::GetAvailableReactions> = rpc
        .recorded()
        .into_iter()
        .filter(|(c, _)| *c == tl_fns::messages::GetAvailableReactions::CONSTRUCTOR_ID)
        .map(|(_, b)| tl_fns::messages::GetAvailableReactions::from_bytes(&b).expect("decode"))
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].hash, 0);
    assert_eq!(calls[1].hash, 12345);
}

#[test]
fn multi_reaction_toggle_supports_three_for_premium_and_one_for_standard() {
    // Premium account: up to 3 simultaneous reactions per message
    let (_rt, rpc, mut live, chan_peer, _) = build_live_fixture(true);
    let empty_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: now_unix_i32(),
        seq: 0,
    });
    rpc.set_dynamic_handler(Arc::new(move |ctor, _| {
        if ctor == tl_fns::messages::SendReaction::CONSTRUCTOR_ID {
            Some(Ok(empty_updates.to_bytes()))
        } else {
            None
        }
    }));

    let r1 = live
        .toggle_reaction(chan_peer, MsgId(42), "🔥")
        .expect("r1");
    assert!(r1.iter().any(|r| r.emoji == "🔥" && r.chosen));

    let r2 = live
        .toggle_reaction(chan_peer, MsgId(42), "❤️")
        .expect("r2");
    assert!(r2.iter().any(|r| r.emoji == "🔥" && r.chosen));
    assert!(r2.iter().any(|r| r.emoji == "❤️" && r.chosen));

    let r3 = live
        .toggle_reaction(chan_peer, MsgId(42), "🎉")
        .expect("r3");
    let chosen_emojis: Vec<&str> = r3
        .iter()
        .filter(|r| r.chosen)
        .map(|r| r.emoji.as_str())
        .collect();
    assert_eq!(chosen_emojis.len(), 3);

    // 4th reaction evicts oldest ("🔥") and keeps max 3
    let r4 = live
        .toggle_reaction(chan_peer, MsgId(42), "👏")
        .expect("r4");
    assert!(!r4.iter().any(|r| r.emoji == "🔥" && r.chosen));
    assert!(r4.iter().any(|r| r.emoji == "❤️" && r.chosen));
    assert!(r4.iter().any(|r| r.emoji == "🎉" && r.chosen));
    assert!(r4.iter().any(|r| r.emoji == "👏" && r.chosen));

    // Verify wire payload of 3rd call carried all 3 reactions
    let send_calls: Vec<tl_fns::messages::SendReaction> = rpc
        .recorded()
        .into_iter()
        .filter(|(c, _)| *c == tl_fns::messages::SendReaction::CONSTRUCTOR_ID)
        .map(|(_, b)| tl_fns::messages::SendReaction::from_bytes(&b).expect("decode"))
        .collect();
    assert_eq!(send_calls[2].reaction.as_ref().map(Vec::len), Some(3));
    assert_eq!(send_calls[3].reaction.as_ref().map(Vec::len), Some(3));

    // Toggling one off keeps the remaining 2 chosen reactions
    let r5 = live
        .toggle_reaction(chan_peer, MsgId(42), "🎉")
        .expect("r5");
    assert_eq!(r5.iter().filter(|r| r.chosen).count(), 2);

    // Standard (non-premium) account: max 1 simultaneous reaction, server UpdateMessageReactions authoritative
    let (_rt_std, rpc_std, mut live_std, chan_peer_std, _) = build_live_fixture(false);
    rpc_std.enqueue_ok(
        tl_fns::messages::SendReaction::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::MessageReactions(
                tl_types::UpdateMessageReactions {
                    peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 5001 }),
                    msg_id: 42,
                    top_msg_id: None,
                    saved_peer_id: None,
                    reactions: tl_enums::MessageReactions::MessageReactions(
                        tl_types::MessageReactions {
                            min: false,
                            can_see_list: true,
                            reactions_as_tags: false,
                            results: vec![
                                tl_enums::ReactionCount::ReactionCount(tl_types::ReactionCount {
                                    chosen_order: None,
                                    reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                                        emoticon: "👍".to_owned(),
                                    }),
                                    count: 2,
                                }),
                                tl_enums::ReactionCount::ReactionCount(tl_types::ReactionCount {
                                    chosen_order: Some(0),
                                    reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                                        emoticon: "🔥".to_owned(),
                                    }),
                                    count: 1,
                                }),
                            ],
                            recent_reactions: None,
                            top_reactors: None,
                        },
                    ),
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: now_unix_i32(),
            seq: 1,
        }),
    );
    let s1 = live_std
        .toggle_reaction(chan_peer_std, MsgId(42), "🔥")
        .expect("std r1");
    let fire_count = s1
        .iter()
        .find(|r| r.emoji == "🔥")
        .map(|r| (r.count, r.chosen));
    assert_eq!(
        fire_count,
        Some((1, true)),
        "UpdateMessageReactions must not double-count"
    );

    rpc_std.enqueue_ok(
        tl_fns::messages::SendReaction::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: now_unix_i32(),
            seq: 2,
        }),
    );
    let s2 = live_std
        .toggle_reaction(chan_peer_std, MsgId(42), "❤️")
        .expect("std r2");
    assert!(!s2.iter().any(|r| r.emoji == "🔥" && r.chosen));
    assert!(s2.iter().any(|r| r.emoji == "❤️" && r.chosen));
}

#[test]
fn who_reacted_list_default_reaction_and_saved_reaction_tags() {
    let (_rt, rpc, mut live, chan_peer, _) = build_live_fixture(false);

    // 1. messages.getMessageReactionsList
    rpc.enqueue_ok(
        tl_fns::messages::GetMessageReactionsList::CONSTRUCTOR_ID,
        &tl_enums::messages::MessageReactionsList::MessageReactionsList(
            tl_types::messages::MessageReactionsList {
                count: 2,
                reactions: vec![tl_enums::MessagePeerReaction::MessagePeerReaction(
                    tl_types::MessagePeerReaction {
                        big: false,
                        unread: false,
                        my: false,
                        peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 1001 }),
                        date: 1_795_341_600,
                        reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                            emoticon: "👍".to_owned(),
                        }),
                    },
                )],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(synthetic_tl_user(
                    1001,
                    "SynthBeta",
                    "synth_beta",
                    false,
                ))],
                next_offset: Some("cursor_2".to_owned()),
            },
        ),
    );

    let page = live
        .message_reactions_list(chan_peer, MsgId(42), Some("👍"), None, 50)
        .expect("who reacted list");
    assert_eq!(page.total_count, 2);
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].peer_id, PeerId::from_user(1001));
    assert_eq!(page.entries[0].emoji, "👍");
    assert_eq!(page.next_offset.as_deref(), Some("cursor_2"));

    // 2. messages.setDefaultReaction
    rpc.enqueue_ok(tl_fns::messages::SetDefaultReaction::CONSTRUCTOR_ID, &true);
    live.set_default_reaction("🔥")
        .expect("set default reaction");
    assert_eq!(live.default_reaction(), "🔥");

    // 3. messages.getSavedReactionTags (hash-cached + DialogsBackend::search_tags)
    rpc.enqueue_ok(
        tl_fns::messages::GetSavedReactionTags::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedReactionTags::SavedReactionTags(
            tl_types::messages::SavedReactionTags {
                tags: vec![tl_enums::SavedReactionTag::SavedReactionTag(
                    tl_types::SavedReactionTag {
                        reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                            emoticon: "🔥".to_owned(),
                        }),
                        title: Some("Urgent".to_owned()),
                        count: 4,
                    },
                )],
                hash: 987654,
            },
        ),
    );
    let tags = live.saved_reaction_tags(None);
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].emoji, "🔥");
    assert_eq!(tags[0].title.as_deref(), Some("Urgent"));
    assert_eq!(tags[0].count, 4);

    // Next call via DialogsBackend::search_tags sends hash = 987654 and handles NotModified
    rpc.enqueue_ok(
        tl_fns::messages::GetSavedReactionTags::CONSTRUCTOR_ID,
        &tl_enums::messages::SavedReactionTags::NotModified,
    );
    let chips = live.search_tags();
    assert_eq!(chips.len(), 1);
    assert_eq!(chips[0].id, "🔥");
    assert_eq!(chips[0].label, "🔥 Urgent");
    assert_eq!(chips[0].count, 4);

    let tag_calls: Vec<tl_fns::messages::GetSavedReactionTags> = rpc
        .recorded()
        .into_iter()
        .filter(|(c, _)| *c == tl_fns::messages::GetSavedReactionTags::CONSTRUCTOR_ID)
        .map(|(_, b)| tl_fns::messages::GetSavedReactionTags::from_bytes(&b).expect("decode"))
        .collect();
    assert_eq!(tag_calls[0].hash, 0);
    assert_eq!(tag_calls[1].hash, 987654);
}

// =============================================================================
// 6. Polls tests (`messages.getPollVotes`, retract vote, stop poll)
// =============================================================================

#[test]
fn poll_votes_retract_vote_and_stop_poll() {
    let (_rt, rpc, mut live, chan_peer, _) = build_live_fixture(false);

    // 1. messages.getPollVotes
    rpc.enqueue_ok(
        tl_fns::messages::GetPollVotes::CONSTRUCTOR_ID,
        &tl_enums::messages::VotesList::VotesList(tl_types::messages::VotesList {
            count: 2,
            votes: vec![tl_enums::MessagePeerVote::MessagePeerVote(
                tl_types::MessagePeerVote {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 1001 }),
                    option: b"opt0".to_vec(),
                    date: 1_795_341_600,
                },
            )],
            chats: Vec::new(),
            users: vec![tl_enums::User::User(synthetic_tl_user(
                1001,
                "SynthBeta",
                "synth_beta",
                false,
            ))],
            next_offset: Some("votes_p2".to_owned()),
        }),
    );

    let votes = live
        .poll_votes(chan_peer, MsgId(43), Some(0), None, 50)
        .expect("getPollVotes");
    assert_eq!(votes.total_count, 2);
    assert_eq!(votes.voters.len(), 1);
    assert_eq!(votes.voters[0].peer_id, PeerId::from_user(1001));
    assert_eq!(votes.voters[0].option_indices, vec![0]);
    assert_eq!(votes.next_offset.as_deref(), Some("votes_p2"));

    // 2. Retract vote -> `messages.sendVote` with empty options
    rpc.enqueue_ok(
        tl_fns::messages::SendVote::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: now_unix_i32(),
            seq: 0,
        }),
    );
    live.retract_poll_vote(chan_peer, MsgId(43))
        .expect("retract poll vote");

    let vote_bytes = rpc
        .recorded()
        .into_iter()
        .find(|(c, _)| *c == tl_fns::messages::SendVote::CONSTRUCTOR_ID)
        .expect("SendVote invoked")
        .1;
    let decoded_vote = tl_fns::messages::SendVote::from_bytes(&vote_bytes).expect("decode vote");
    assert!(
        decoded_vote.options.is_empty(),
        "retract vote must pass empty options vector"
    );

    let after_retract = live.load_slice_around(chan_peer, MsgId(43), 2, 2);
    let poll_after_retract = after_retract
        .messages
        .iter()
        .find(|m| m.id == MsgId(43))
        .and_then(|m| match &m.media {
            Some(tg_app::history::backend::HistoryMedia::Poll(p)) => Some(p),
            _ => None,
        })
        .expect("poll present");
    assert!(
        poll_after_retract.options.iter().all(|o| !o.chosen),
        "chosen flag must be cleared after retract"
    );
    assert_eq!(poll_after_retract.total_voters, 2);

    // 3. Stop poll -> `messages.editMessage` with `InputMediaPoll { closed: true }`
    rpc.enqueue_ok(
        tl_fns::messages::EditMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: now_unix_i32(),
            seq: 0,
        }),
    );
    live.stop_poll(chan_peer, MsgId(43)).expect("stop poll");

    let edit_bytes = rpc
        .recorded()
        .into_iter()
        .find(|(c, _)| *c == tl_fns::messages::EditMessage::CONSTRUCTOR_ID)
        .expect("EditMessage invoked")
        .1;
    let decoded_edit =
        tl_fns::messages::EditMessage::from_bytes(&edit_bytes).expect("decode editMessage");
    match decoded_edit.media {
        Some(tl_enums::InputMedia::Poll(imp)) => {
            let tl_enums::Poll::Poll(p) = imp.poll;
            assert!(p.closed, "stop_poll must send Poll with closed = true");
            assert_eq!(p.id, 99_001);
            assert_eq!(p.answers.len(), 2);
        }
        other => panic!("expected InputMedia::Poll, got {other:?}"),
    }

    let after_stop = live.load_slice_around(chan_peer, MsgId(43), 2, 2);
    let poll_after_stop = after_stop
        .messages
        .iter()
        .find(|m| m.id == MsgId(43))
        .and_then(|m| match &m.media {
            Some(tg_app::history::backend::HistoryMedia::Poll(p)) => Some(p),
            _ => None,
        })
        .expect("poll present");
    assert!(poll_after_stop.closed);

    // Retracting on a closed poll is rejected locally
    assert!(live.retract_poll_vote(chan_peer, MsgId(43)).is_err());
}

// =============================================================================
// 7. UI Widget state tests (`ReactionsListWidget`, `PollVotersWidget`)
// =============================================================================

#[test]
fn reactions_list_and_poll_voters_widgets_paginate_and_filter_cleanly() {
    let summary = vec![
        ReactionCounter {
            emoji: "👍".to_owned(),
            count: 2,
            chosen: true,
        },
        ReactionCounter {
            emoji: "🔥".to_owned(),
            count: 1,
            chosen: false,
        },
    ];
    let mut r_widget = ReactionsListWidget::new(
        PeerId::from_chat(1),
        MsgId(10),
        summary,
        MessageReactionsPage {
            total_count: 3,
            entries: vec![ReactionPeerEntry {
                peer_id: PeerId::from_user(1),
                name: "SynthAlpha".to_owned(),
                username: Some("synth_alpha".to_owned()),
                emoji: "👍".to_owned(),
                date_unix: 1_795_341_600,
                is_my: true,
                color_index: 1,
            }],
            next_offset: Some("p2".to_owned()),
        },
    );
    assert_eq!(r_widget.total_summary_count(), 3);
    r_widget.append_page(MessageReactionsPage {
        total_count: 3,
        entries: vec![
            // Duplicate is ignored
            ReactionPeerEntry {
                peer_id: PeerId::from_user(1),
                name: "SynthAlpha".to_owned(),
                username: Some("synth_alpha".to_owned()),
                emoji: "👍".to_owned(),
                date_unix: 1_795_341_600,
                is_my: true,
                color_index: 1,
            },
            ReactionPeerEntry {
                peer_id: PeerId::from_user(2),
                name: "SynthBeta".to_owned(),
                username: None,
                emoji: "🔥".to_owned(),
                date_unix: 1_795_341_700,
                is_my: false,
                color_index: 2,
            },
        ],
        next_offset: None,
    });
    assert_eq!(r_widget.entries.len(), 2);
    assert!(r_widget.next_offset.is_none());

    let poll = PollAttachment {
        question: "Pick one".to_owned(),
        is_quiz: false,
        is_anonymous: false,
        multiple_choice: false,
        closed: false,
        options: vec![
            PollOption {
                text: "Alpha".to_owned(),
                votes: 1,
                chosen: true,
                correct: false,
            },
            PollOption {
                text: "Beta".to_owned(),
                votes: 1,
                chosen: false,
                correct: false,
            },
        ],
        total_voters: 2,
        explanation: None,
    };
    let mut p_widget = PollVotersWidget::new(
        PeerId::from_chat(1),
        MsgId(11),
        poll,
        PollVotesPage {
            total_count: 2,
            voters: vec![PollVoterEntry {
                peer_id: PeerId::from_user(1),
                name: "SynthAlpha".to_owned(),
                username: None,
                option_indices: vec![0],
                date_unix: 1_795_341_600,
                color_index: 1,
            }],
            next_offset: Some("v2".to_owned()),
        },
    );
    p_widget.apply_option_page(
        Some(1),
        PollVotesPage {
            total_count: 1,
            voters: vec![PollVoterEntry {
                peer_id: PeerId::from_user(2),
                name: "SynthBeta".to_owned(),
                username: Some("synth_beta".to_owned()),
                option_indices: vec![1],
                date_unix: 1_795_341_700,
                color_index: 2,
            }],
            next_offset: None,
        },
    );
    assert_eq!(p_widget.active_option, Some(1));
    assert_eq!(p_widget.voters.len(), 1);
    assert_eq!(p_widget.voters[0].peer_id, PeerId::from_user(2));

    // Sanity check unused import warning guard for HistoryMessage/Duration
    let _ = Duration::from_millis(1);
    let _ = detect_chat_language(&[] as &[HistoryMessage]);
}
