//! FakeRpcClient unit tests for WP-53 Chat Engagement & Read-Receipt Protocol:
//! - Batched `messages.getMessagesViews(increment=true)` + `messages.getExtendedMedia`
//! - `messages.getUnread{Mentions,Reactions,PollVotes}` pagination & `messages.read{Mentions,Reactions,PollVotes}`
//! - `messages.getMessageReadParticipants` & `messages.getOutboxReadDate` (incl. privacy errors)
//! - `messages.readDiscussion`
//! - `messages.reportMessagesDelivery` (chunking at 50)
//! - `ephemeral.deleteMessage` & `ephemeral.reportMessage`
//! - `channels.exportMessageLink` & `channels.getMessageAuthor`
//! - `messages.migrateChat` & `messages.getFutureChatCreatorAfterLeave`
//!
//! Runs exclusively against an in-process `FakeRpcClient` with synthetic IDs/users.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::convert::apply_tl_users_and_chats;
use tg_api::rpc::RpcError;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{
    ChannelId, ChatId, ExtendedMedia, ForumTopicData, FullMsgId as DataFullMsgId, Message,
    MessageContent, MessageMedia, MsgId as DataMsgId, PaidMediaData, PeerId as DataPeerId,
    TextWithEntities as DataText, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::convert::{from_data_peer, to_data_msg, to_data_peer};
use crate::engagement::{
    flush_delivery_reports, queue_message_delivery_report, tracked_ephemeral_entry,
    tracks_unread_things,
};
use tg_app_core::history::backend::{
    DEFAULT_CHAT_READ_MARK_SIZE_THRESHOLD, DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS,
    EphemeralReportOutcome, HistoryBackend, OutboxReadDateStatus, REPORT_DELIVERIES_PER_REQUEST,
    UNREAD_THINGS_FIRST_LIMIT, UNREAD_THINGS_NEXT_LIMIT, UnreadThingsKind, UnreadThingsScope,
    who_read_exists_for_dm, who_read_exists_for_group,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

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
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
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

    fn enqueue_rpc_error(&self, expected_ctor: u32, code: i32, name: &str) {
        self.responses.lock().expect("lock responses").push_back((
            expected_ctor,
            Err(InvocationError::Rpc(RpcError::new(code, name))),
        ));
    }

    fn push_update(&self, upd: tl_enums::Updates) {
        let _ = self.updates_tx.send(upd);
    }

    fn recorded_calls(&self) -> Vec<(u32, Vec<u8>)> {
        self.recorded.lock().expect("lock recorded").clone()
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
        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
                q.remove(pos).map(|(_, r)| r)
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

fn synthetic_user(id: i64, first: &str, username: &str, is_self: bool) -> tl_types::User {
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
        access_hash: Some(id * 31 + 7),
        first_name: Some(first.to_owned()),
        last_name: None,
        username: Some(username.to_owned()),
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

fn synthetic_channel(
    id: i64,
    title: &str,
    username: Option<&str>,
    broadcast: bool,
    megagroup: bool,
    forum: bool,
) -> tl_types::Channel {
    tl_types::Channel {
        creator: true,
        left: false,
        broadcast,
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
        forum,
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
        access_hash: Some(id * 19 + 3),
        title: title.to_owned(),
        username: username.map(str::to_owned),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_700_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(25),
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
    }
}

fn synthetic_basic_chat(
    id: i64,
    title: &str,
    creator: bool,
    deactivated: bool,
    migrated_to: Option<tl_enums::InputChannel>,
) -> tl_types::Chat {
    tl_types::Chat {
        creator,
        left: false,
        deactivated,
        call_active: false,
        call_not_empty: false,
        noforwards: false,
        id,
        title: title.to_owned(),
        photo: tl_enums::ChatPhoto::Empty,
        participants_count: 12,
        date: 1_700_000_000,
        version: 1,
        migrated_to,
        admin_rights: None,
        default_banned_rights: None,
    }
}

fn synthetic_tl_message(
    peer: tl_enums::Peer,
    from_user: i64,
    id: i32,
    text: &str,
    mentioned: bool,
    media_unread: bool,
    unread_reaction: bool,
) -> tl_enums::Message {
    let reactions = unread_reaction.then(|| {
        tl_enums::MessageReactions::MessageReactions(tl_types::MessageReactions {
            min: false,
            can_see_list: true,
            reactions_as_tags: false,
            results: vec![tl_enums::ReactionCount::ReactionCount(
                tl_types::ReactionCount {
                    chosen_order: None,
                    reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                        emoticon: "👍".to_owned(),
                    }),
                    count: 1,
                },
            )],
            recent_reactions: Some(vec![tl_enums::MessagePeerReaction::MessagePeerReaction(
                tl_types::MessagePeerReaction {
                    big: false,
                    unread: true,
                    my: false,
                    peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: from_user }),
                    date: 1_700_000_500,
                    reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                        emoticon: "👍".to_owned(),
                    }),
                },
            )]),
            top_reactors: None,
        })
    });

    tl_enums::Message::Message(tl_types::Message {
        out: false,
        mentioned,
        media_unread,
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
            user_id: from_user,
        })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date: 1_700_000_100 + id,
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
        reactions,
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

struct TestHarness {
    _rt: tokio::runtime::Runtime,
    fake: FakeRpcClient,
    live: LiveSession,
    channel_peer: PeerId,
    megagroup_peer: PeerId,
    basic_chat_peer: PeerId,
    dm_user_peer: PeerId,
    bot_user_peer: PeerId,
}

impl TestHarness {
    fn new() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build tokio runtime");
        let self_user = synthetic_user(1001, "Self", "self_user", true);
        let dm_user = synthetic_user(2002, "PeerOne", "peer_one", false);
        let mut bot_user = synthetic_user(3003, "HelperBot", "helper_bot", false);
        bot_user.bot = true;
        let broadcast_ch =
            synthetic_channel(5001, "News Channel", Some("news_chan"), true, false, false);
        let megagroup_ch =
            synthetic_channel(5002, "Dev Group", Some("dev_group"), false, true, true);
        let basic_chat = synthetic_basic_chat(6001, "Legacy Group", true, false, None);

        let fake = FakeRpcClient::new(2);
        fake.enqueue_ok(
            tl_fns::users::GetUsers::CONSTRUCTOR_ID,
            &vec![tl_enums::User::User(self_user.clone())],
        );
        fake.enqueue_ok(
            tl_fns::updates::GetState::CONSTRUCTOR_ID,
            &tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: 1_700_000_000,
                seq: 1,
                unread_count: 0,
            }),
        );
        fake.enqueue_ok(
            tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
            &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
                date: 1_700_000_001,
                seq: 1,
            }),
        );
        fake.enqueue_ok(
            tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
            &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
                dialogs: Vec::new(),
                messages: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
            }),
        );
        fake.enqueue_ok(
            tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID,
            &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                dialogs: Vec::new(),
                messages: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
                state: tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 10,
                    qts: 0,
                    date: 1_700_000_001,
                    seq: 1,
                    unread_count: 0,
                }),
            }),
        );
        let runtime = Arc::new(
            SessionRuntime::start_on(
                DataUserId(1001),
                crate::ErasedRpcClient::new(fake.clone()),
                rt.handle().clone(),
                SessionRuntimeConfig::default(),
            )
            .expect("start SessionRuntime"),
        );
        rt.block_on(async {
            tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready())
                .await
                .expect("wait_ready timeout")
                .expect("wait_ready ok");
        });

        let live = LiveSession::new(runtime, 53);

        live.write_session(|session| {
            apply_tl_users_and_chats(
                session,
                &[
                    tl_enums::User::User(self_user),
                    tl_enums::User::User(dm_user),
                    tl_enums::User::User(bot_user),
                ],
                &[
                    tl_enums::Chat::Channel(broadcast_ch),
                    tl_enums::Chat::Channel(megagroup_ch),
                    tl_enums::Chat::Chat(basic_chat),
                ],
            );
        });

        Self {
            _rt: rt,
            fake,
            live,
            channel_peer: from_data_peer(DataPeerId::from_channel(ChannelId(5001))),
            megagroup_peer: from_data_peer(DataPeerId::from_channel(ChannelId(5002))),
            basic_chat_peer: from_data_peer(DataPeerId::from_chat(ChatId(6001))),
            dm_user_peer: from_data_peer(DataPeerId::from_user(DataUserId(2002))),
            bot_user_peer: from_data_peer(DataPeerId::from_user(DataUserId(3003))),
        }
    }
}

#[test]
fn wp53_views_batching_and_extended_media_refresh() {
    let mut h = TestHarness::new();
    let chan_data = to_data_peer(h.channel_peer);

    // Seed 5 channel posts + 1 paid media post with locked preview.
    h.live.write_session(|session| {
        for id in 101..=105 {
            let mut msg = Message::new_text(
                chan_data,
                DataMsgId(id),
                None,
                1_700_000_000 + id as i32,
                DataText::plain(format!("Channel post {id}")),
            );
            msg.views = Some(10);
            msg.forwards = Some(1);
            session.insert_message(msg);
        }

        let mut paid_msg = Message::new_text(
            chan_data,
            DataMsgId(200),
            None,
            1_700_000_300,
            DataText::plain("Locked paid post"),
        );
        paid_msg.content = MessageContent::Regular {
            text: DataText::plain("Locked paid post"),
            media: MessageMedia::PaidMedia(Box::new(PaidMediaData {
                stars_amount: 50,
                extended_media: vec![ExtendedMedia::Preview {
                    width: Some(640),
                    height: Some(480),
                    thumb: None,
                    duration: None,
                }],
            })),
        };
        session.insert_message(paid_msg);
    });

    // 1. Queue 5 visible channel post IDs (plus duplicates).
    h.live.schedule_message_views_increment(
        h.channel_peer,
        &[MsgId(101), MsgId(102), MsgId(103), MsgId(102)],
    );
    h.live
        .schedule_message_views_increment(h.channel_peer, &[MsgId(104), MsgId(105), MsgId(101)]);

    let views_vec: Vec<tl_enums::MessageViews> = (101..=105)
        .map(|i| {
            tl_enums::MessageViews::MessageViews(tl_types::MessageViews {
                views: Some(500 + i),
                forwards: Some(20 + (i - 100)),
                replies: Some(tl_enums::MessageReplies::MessageReplies(
                    tl_types::MessageReplies {
                        comments: true,
                        replies: (i - 100) * 3,
                        replies_pts: 100,
                        recent_repliers: None,
                        channel_id: Some(5002),
                        max_id: Some(900 + i),
                        read_max_id: Some(890 + i),
                    },
                )),
            })
        })
        .collect();
    h.fake.enqueue_ok(
        tl_fns::messages::GetMessagesViews::CONSTRUCTOR_ID,
        &tl_enums::messages::MessageViews::MessageViews(tl_types::messages::MessageViews {
            views: views_vec,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let infos = h
        .live
        .flush_message_views(Some(h.channel_peer), true)
        .expect("flush_message_views");
    assert_eq!(infos.len(), 5);
    assert_eq!(infos[0].msg_id, MsgId(101));
    assert_eq!(infos[0].views, Some(601));
    assert_eq!(infos[0].forwards, Some(21));
    assert_eq!(infos[0].replies_count, Some(3));
    assert!(infos[0].comments);

    // Verify wire request had increment=true and exact 5 deduplicated sorted IDs.
    let calls = h.fake.recorded_calls();
    let (_, req_bytes) = calls
        .iter()
        .find(|(c, _)| *c == tl_fns::messages::GetMessagesViews::CONSTRUCTOR_ID)
        .expect("GetMessagesViews recorded");
    let decoded =
        tl_fns::messages::GetMessagesViews::from_bytes(req_bytes).expect("decode GetMessagesViews");
    assert!(decoded.increment);
    assert_eq!(decoded.id, vec![101, 102, 103, 104, 105]);

    // Verify cached counts updated in tg_data::Session.
    h.live.read_session(|session| {
        for id in 101..=105_i64 {
            let msg = session
                .message(DataFullMsgId::new(chan_data, DataMsgId(id)))
                .expect("cached post");
            assert_eq!(msg.views, Some(500 + id as i32));
            assert_eq!(msg.forwards, Some(20 + (id as i32 - 100)));
            assert_eq!(msg.replies_count, Some((id as i32 - 100) * 3));
        }
    });

    // Re-scheduling already-incremented IDs is a no-op until remove_incremented_views.
    h.live
        .schedule_message_views_increment(h.channel_peer, &[MsgId(101)]);
    let empty = h
        .live
        .flush_message_views(Some(h.channel_peer), true)
        .expect("dedup flush");
    assert!(empty.is_empty());

    h.live.remove_incremented_views(h.channel_peer);
    h.fake.enqueue_ok(
        tl_fns::messages::GetMessagesViews::CONSTRUCTOR_ID,
        &tl_enums::messages::MessageViews::MessageViews(tl_types::messages::MessageViews {
            views: vec![tl_enums::MessageViews::MessageViews(
                tl_types::MessageViews {
                    views: Some(999),
                    forwards: None,
                    replies: None,
                },
            )],
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    h.live
        .schedule_message_views_increment(h.channel_peer, &[MsgId(101)]);
    let after_clear = h
        .live
        .flush_message_views(Some(h.channel_peer), true)
        .expect("flush after clear");
    assert_eq!(after_clear.len(), 1);
    assert_eq!(after_clear[0].views, Some(999));

    // 2. Poll extended media for paid post 200 -> unlocks preview into MessageMedia::Photo.
    h.fake.enqueue_ok(
        tl_fns::messages::GetExtendedMedia::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::MessageExtendedMedia(
                tl_types::UpdateMessageExtendedMedia {
                    peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 5001 }),
                    msg_id: 200,
                    extended_media: vec![tl_enums::MessageExtendedMedia::MessageExtendedMedia(
                        tl_types::MessageExtendedMedia {
                            media: Box::new(tl_enums::MessageMedia::Photo(
                                tl_types::MessageMediaPhoto {
                                    spoiler: false,
                                    live_photo: false,
                                    photo: Some(tl_enums::Photo::Empty(tl_types::PhotoEmpty {
                                        id: 7777,
                                    })),
                                    ttl_seconds: None,
                                    video: None,
                                },
                            )),
                        },
                    )],
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_700_000_400,
            seq: 2,
        }),
    );

    let refreshed = h
        .live
        .poll_extended_media(h.channel_peer, &[MsgId(200)], true)
        .expect("poll_extended_media");
    assert_eq!(refreshed.len(), 1);
    assert_eq!(refreshed[0].msg_id, MsgId(200));
    assert_eq!(refreshed[0].total_items, 1);
    assert_eq!(refreshed[0].unlocked_items, 1);

    h.live.read_session(|session| {
        let msg = session
            .message(DataFullMsgId::new(chan_data, DataMsgId(200)))
            .expect("paid msg");
        let MessageContent::Regular {
            media: MessageMedia::PaidMedia(paid),
            ..
        } = &msg.content
        else {
            panic!("expected PaidMedia");
        };
        assert_eq!(paid.extended_media.len(), 1);
        assert!(matches!(paid.extended_media[0], ExtendedMedia::Media(_)));
    });

    // 3. Extended media polling chunks >100 IDs into batches of 100 (`kMaxPollPerRequest`).
    let many_ids: Vec<MsgId> = (300..425).map(MsgId).collect();
    for _ in 0..2 {
        h.fake.enqueue_ok(
            tl_fns::messages::GetExtendedMedia::CONSTRUCTOR_ID,
            &tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_700_000_450,
                seq: 3,
            }),
        );
    }
    let _ = h
        .live
        .poll_extended_media(h.channel_peer, &many_ids, true)
        .expect("chunked poll_extended_media");
    let ext_calls: Vec<tl_fns::messages::GetExtendedMedia> = h
        .fake
        .recorded_calls()
        .iter()
        .filter(|(c, _)| *c == tl_fns::messages::GetExtendedMedia::CONSTRUCTOR_ID)
        .map(|(_, b)| tl_fns::messages::GetExtendedMedia::from_bytes(b).expect("decode"))
        .collect();
    assert_eq!(ext_calls.len(), 3);
    assert_eq!(ext_calls[1].id.len(), 100);
    assert_eq!(ext_calls[2].id.len(), 25);
}

#[test]
fn wp53_unread_things_pagination_mark_read_and_read_discussion() {
    let mut h = TestHarness::new();
    let mg_tl_peer = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 5002 });
    let scope = UnreadThingsScope::history(h.megagroup_peer);

    // Eligibility checks (`api_unread_things.cpp:33-50`).
    assert!(tracks_unread_things(
        &h.live,
        h.megagroup_peer,
        UnreadThingsKind::Mentions
    ));
    assert!(tracks_unread_things(
        &h.live,
        h.megagroup_peer,
        UnreadThingsKind::Reactions
    ));
    assert!(tracks_unread_things(
        &h.live,
        h.megagroup_peer,
        UnreadThingsKind::PollVotes
    ));
    assert!(!tracks_unread_things(
        &h.live,
        h.channel_peer,
        UnreadThingsKind::Mentions
    ));
    assert!(tracks_unread_things(
        &h.live,
        h.dm_user_peer,
        UnreadThingsKind::Reactions
    ));
    assert!(!tracks_unread_things(
        &h.live,
        h.dm_user_peer,
        UnreadThingsKind::Mentions
    ));

    // Page 1 of unread mentions (`offset_id=1, add_offset=-10, limit=10`).
    h.fake.enqueue_ok(
        tl_fns::messages::GetUnreadMentions::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 50,
            count: 3,
            offset_id_offset: None,
            messages: vec![
                synthetic_tl_message(
                    mg_tl_peer.clone(),
                    2002,
                    11,
                    "@self ping 1",
                    true,
                    true,
                    false,
                ),
                synthetic_tl_message(
                    mg_tl_peer.clone(),
                    2002,
                    22,
                    "@self ping 2",
                    true,
                    true,
                    false,
                ),
            ],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let p1 = h
        .live
        .load_unread_things_page(scope, UnreadThingsKind::Mentions)
        .expect("mentions page 1");
    assert_eq!(p1.full_count, 3);
    assert_eq!(p1.loaded_ids, vec![MsgId(11), MsgId(22)]);
    assert!(p1.has_more);

    // Page 2 of unread mentions (`offset_id=22, add_offset=-101, limit=100`).
    h.fake.enqueue_ok(
        tl_fns::messages::GetUnreadMentions::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 51,
            count: 3,
            offset_id_offset: None,
            messages: vec![synthetic_tl_message(
                mg_tl_peer.clone(),
                2002,
                33,
                "@self ping 3",
                true,
                true,
                false,
            )],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let p2 = h
        .live
        .load_unread_things_page(scope, UnreadThingsKind::Mentions)
        .expect("mentions page 2");
    assert_eq!(p2.full_count, 3);
    assert_eq!(p2.loaded_ids, vec![MsgId(11), MsgId(22), MsgId(33)]);
    assert!(!p2.has_more);

    // Verify exact tdesktop pagination args on wire.
    let mention_reqs: Vec<tl_fns::messages::GetUnreadMentions> = h
        .fake
        .recorded_calls()
        .iter()
        .filter(|(c, _)| *c == tl_fns::messages::GetUnreadMentions::CONSTRUCTOR_ID)
        .map(|(_, b)| tl_fns::messages::GetUnreadMentions::from_bytes(b).expect("decode"))
        .collect();
    assert_eq!(mention_reqs.len(), 2);
    assert_eq!(mention_reqs[0].offset_id, 1);
    assert_eq!(mention_reqs[0].limit, UNREAD_THINGS_FIRST_LIMIT);
    assert_eq!(mention_reqs[0].add_offset, -UNREAD_THINGS_FIRST_LIMIT);
    assert_eq!(mention_reqs[1].offset_id, 22);
    assert_eq!(mention_reqs[1].limit, UNREAD_THINGS_NEXT_LIMIT);
    assert_eq!(mention_reqs[1].add_offset, -(UNREAD_THINGS_NEXT_LIMIT + 1));

    // Unread reactions & poll votes pages.
    h.fake.enqueue_ok(
        tl_fns::messages::GetUnreadReactions::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
            inexact: false,
            count: 1,
            next_rate: None,
            offset_id_offset: None,
            search_flood: None,
            messages: vec![synthetic_tl_message(
                mg_tl_peer.clone(),
                2002,
                44,
                "Reacted msg",
                false,
                false,
                true,
            )],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let rx_page = h
        .live
        .load_unread_things_page(scope, UnreadThingsKind::Reactions)
        .expect("reactions page");
    assert_eq!(rx_page.full_count, 1);
    assert_eq!(rx_page.loaded_ids, vec![MsgId(44)]);

    h.fake.enqueue_ok(
        tl_fns::messages::GetUnreadPollVotes::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![synthetic_tl_message(
                mg_tl_peer, 2002, 55, "Poll msg", false, false, false,
            )],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let poll_page = h
        .live
        .load_unread_things_page(scope, UnreadThingsKind::PollVotes)
        .expect("poll votes page");
    assert_eq!(poll_page.full_count, 1);
    assert_eq!(poll_page.loaded_ids, vec![MsgId(55)]);

    let counts = h.live.unread_things_counts(scope);
    assert_eq!(counts.mentions, 3);
    assert_eq!(counts.reactions, 1);
    assert_eq!(counts.poll_votes, 1);

    // Mark all mentions read with 2-page AffectedHistory pagination (`offset=10` then `offset=0`).
    h.fake.enqueue_ok(
        tl_fns::messages::ReadMentions::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 60,
                pts_count: 2,
                offset: 10,
            },
        ),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::ReadMentions::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 61,
                pts_count: 1,
                offset: 0,
            },
        ),
    );
    let read_m = h
        .live
        .read_unread_mentions(h.megagroup_peer, None)
        .expect("read_unread_mentions");
    assert_eq!(read_m.pages, 2);
    assert_eq!(read_m.last_pts, 61);
    assert_eq!(read_m.total_pts_count, 3);

    h.fake.enqueue_ok(
        tl_fns::messages::ReadReactions::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 62,
                pts_count: 1,
                offset: 0,
            },
        ),
    );
    let _ = h
        .live
        .read_unread_reactions(h.megagroup_peer, None, None)
        .expect("read_unread_reactions");

    h.fake.enqueue_ok(
        tl_fns::messages::ReadPollVotes::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 63,
                pts_count: 1,
                offset: 0,
            },
        ),
    );
    let _ = h
        .live
        .read_unread_poll_votes(h.megagroup_peer, None)
        .expect("read_unread_poll_votes");

    // Verify all local counters zeroed in both engagement state and tg_data::Session.
    let zeroed = h.live.unread_things_counts(scope);
    assert_eq!(zeroed.mentions, 0);
    assert_eq!(zeroed.reactions, 0);
    assert_eq!(zeroed.poll_votes, 0);
    h.live.read_session(|s| {
        let hist = s
            .history(to_data_peer(h.megagroup_peer))
            .expect("megagroup history");
        assert_eq!(hist.unread_mentions_count, 0);
        assert_eq!(hist.unread_reactions_count, 0);
        assert_eq!(hist.unread_poll_votes_count, 0);
    });

    // Seed a forum topic and verify `messages.readDiscussion` zeroes its unread count.
    let mg_data = to_data_peer(h.megagroup_peer);
    h.live.write_session(|session| {
        let mut topic = ForumTopicData::new(mg_data, DataMsgId(700));
        topic.title = "Discussion Topic".to_owned();
        topic.unread_count = Some(4);
        topic.last_known_server_msg_id = DataMsgId(712);
        session
            .ensure_forum(mg_data)
            .topics
            .insert(DataMsgId(700), topic);
    });
    h.fake
        .enqueue_ok(tl_fns::messages::ReadDiscussion::CONSTRUCTOR_ID, &true);
    let ok = h
        .live
        .read_discussion(h.megagroup_peer, MsgId(700), MsgId(712))
        .expect("read_discussion");
    assert!(ok);
    h.live.read_session(|session| {
        let topic = session
            .forum(mg_data)
            .and_then(|f| f.topic(to_data_msg(MsgId(700))))
            .expect("topic");
        assert_eq!(topic.read_inbox_max_id, DataMsgId(712));
        assert_eq!(topic.unread_count, Some(0));
    });
}

#[test]
fn wp53_read_receipts_delivery_ephemeral_links_author_migration_and_future_creator() {
    let mut h = TestHarness::new();

    // 1. Group read participants (`messages.getMessageReadParticipants`).
    h.fake.enqueue_ok(
        tl_fns::messages::GetMessageReadParticipants::CONSTRUCTOR_ID,
        &vec![
            tl_enums::ReadParticipantDate::ReadParticipantDate(tl_types::ReadParticipantDate {
                user_id: 2002,
                date: 1_700_001_000,
            }),
            tl_enums::ReadParticipantDate::ReadParticipantDate(tl_types::ReadParticipantDate {
                user_id: 9999,
                date: 1_700_001_050,
            }),
        ],
    );
    let readers = h
        .live
        .get_message_read_participants(h.megagroup_peer, MsgId(88))
        .expect("get_message_read_participants");
    assert_eq!(readers.len(), 2);
    assert_eq!(readers[0].name, "PeerOne");
    assert_eq!(readers[0].username.as_deref(), Some("peer_one"));
    assert_eq!(readers[0].read_date_unix, 1_700_001_000);
    assert_eq!(readers[1].name, "User 9999");

    // 2. 1:1 DM outbox read date (`messages.getOutboxReadDate`) + privacy error mapping.
    h.fake.enqueue_ok(
        tl_fns::messages::GetOutboxReadDate::CONSTRUCTOR_ID,
        &tl_enums::OutboxReadDate::OutboxReadDate(tl_types::OutboxReadDate {
            date: 1_700_002_222,
        }),
    );
    assert_eq!(
        h.live
            .get_outbox_read_date(h.dm_user_peer, MsgId(40))
            .expect("outbox read date"),
        OutboxReadDateStatus::Read {
            date_unix: 1_700_002_222
        }
    );

    h.fake.enqueue_rpc_error(
        tl_fns::messages::GetOutboxReadDate::CONSTRUCTOR_ID,
        403,
        "USER_PRIVACY_RESTRICTED",
    );
    assert_eq!(
        h.live
            .get_outbox_read_date(h.dm_user_peer, MsgId(41))
            .expect("peer hidden"),
        OutboxReadDateStatus::PeerPrivacyHidden
    );

    h.fake.enqueue_rpc_error(
        tl_fns::messages::GetOutboxReadDate::CONSTRUCTOR_ID,
        403,
        "YOUR_PRIVACY_RESTRICTED",
    );
    assert_eq!(
        h.live
            .get_outbox_read_date(h.dm_user_peer, MsgId(42))
            .expect("my hidden"),
        OutboxReadDateStatus::MyPrivacyHidden
    );

    h.fake.enqueue_rpc_error(
        tl_fns::messages::GetOutboxReadDate::CONSTRUCTOR_ID,
        400,
        "MESSAGE_TOO_OLD",
    );
    assert_eq!(
        h.live
            .get_outbox_read_date(h.dm_user_peer, MsgId(43))
            .expect("too old"),
        OutboxReadDateStatus::TooOld
    );

    // Pure `WhoReadExists` eligibility checks.
    assert!(who_read_exists_for_dm(
        true,
        false,
        false,
        false,
        false,
        false,
        1_700_000_000,
        1_700_010_000,
        DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS,
    ));
    assert!(!who_read_exists_for_dm(
        true,
        false,
        false,
        false,
        false,
        true, // readDatesPrivate
        1_700_000_000,
        1_700_010_000,
        DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS,
    ));
    assert!(who_read_exists_for_group(
        true,
        false,
        false,
        false,
        25,
        DEFAULT_CHAT_READ_MARK_SIZE_THRESHOLD,
        1_700_000_000,
        1_700_010_000,
        DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS,
    ));
    assert!(!who_read_exists_for_group(
        true,
        false,
        false,
        false,
        120, // exceeds 50 threshold
        DEFAULT_CHAT_READ_MARK_SIZE_THRESHOLD,
        1_700_000_000,
        1_700_010_000,
        DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS,
    ));

    // 3. `messages.reportMessagesDelivery` chunking at 50 (`kReportDeliveriesPerRequest`).
    let delivery_ids: Vec<MsgId> = (1..=65).map(MsgId).collect();
    h.fake.enqueue_ok(
        tl_fns::messages::ReportMessagesDelivery::CONSTRUCTOR_ID,
        &true,
    );
    h.fake.enqueue_ok(
        tl_fns::messages::ReportMessagesDelivery::CONSTRUCTOR_ID,
        &true,
    );
    let reported_count = h
        .live
        .report_messages_delivery(h.dm_user_peer, &delivery_ids, false)
        .expect("report_messages_delivery");
    assert_eq!(reported_count, 65);
    let delivery_calls: Vec<tl_fns::messages::ReportMessagesDelivery> = h
        .fake
        .recorded_calls()
        .iter()
        .filter(|(c, _)| *c == tl_fns::messages::ReportMessagesDelivery::CONSTRUCTOR_ID)
        .map(|(_, b)| tl_fns::messages::ReportMessagesDelivery::from_bytes(b).expect("decode"))
        .collect();
    assert_eq!(delivery_calls.len(), 2);
    assert_eq!(delivery_calls[0].id.len(), REPORT_DELIVERIES_PER_REQUEST);
    assert_eq!(delivery_calls[1].id.len(), 15);
    assert!(!delivery_calls[0].push);

    // Queued delivery report helper with push=true.
    queue_message_delivery_report(&h.live, h.dm_user_peer, &[MsgId(901), MsgId(902)]);
    h.fake.enqueue_ok(
        tl_fns::messages::ReportMessagesDelivery::CONSTRUCTOR_ID,
        &true,
    );
    assert_eq!(flush_delivery_reports(&h.live, true).expect("flush"), 2);

    // 4. `ephemeral.deleteMessage` & `ephemeral.reportMessage`.
    // Push an incoming `updateNewEphemeralMessage` first and verify tracking + deletion.
    h.fake
        .push_update(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
            update: tl_enums::Update::NewEphemeralMessage(tl_types::UpdateNewEphemeralMessage {
                message: tl_enums::EphemeralMessage::EphemeralMessage(tl_types::EphemeralMessage {
                    out: false,
                    invert_media: false,
                    welcome_template: false,
                    noforwards: false,
                    id: 777,
                    peer_id: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                        channel_id: 5002,
                    })),
                    from_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 3003 }),
                    receiver_id: 1001,
                    date: 1_700_003_000,
                    message: "Ephemeral bot reply".to_owned(),
                    entities: None,
                    media: None,
                    reply_markup: None,
                    rich_message: None,
                    chat_instance: None,
                    reply_to: None,
                    top_msg_id: None,
                    anchor_msg_id: None,
                }),
            }),
            date: 1_700_003_000,
        }));
    // Wait briefly for the background updates listener to process the pushed update.
    for _ in 0..30 {
        if tracked_ephemeral_entry(&h.live, h.megagroup_peer, 777).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(tracked_ephemeral_entry(&h.live, h.megagroup_peer, 777).is_some());

    h.fake
        .enqueue_ok(tl_fns::ephemeral::DeleteMessage::CONSTRUCTOR_ID, &true);
    assert!(
        h.live
            .ephemeral_delete_message(h.megagroup_peer, h.bot_user_peer, 777)
            .expect("ephemeral_delete_message")
    );
    assert!(tracked_ephemeral_entry(&h.live, h.megagroup_peer, 777).is_none());

    h.fake.enqueue_ok(
        tl_fns::ephemeral::ReportMessage::CONSTRUCTOR_ID,
        &tl_enums::ReportResult::ChooseOption(tl_types::ReportResultChooseOption {
            title: "Choose reason".to_owned(),
            options: vec![tl_enums::MessageReportOption::MessageReportOption(
                tl_types::MessageReportOption {
                    text: "Spam".to_owned(),
                    option: b"spam".to_vec(),
                },
            )],
        }),
    );
    let rep1 = h
        .live
        .ephemeral_report_message(h.megagroup_peer, 777, b"", "")
        .expect("report step 1");
    assert!(matches!(
        rep1,
        EphemeralReportOutcome::ChooseOption { ref options, .. } if options.len() == 1
    ));

    h.fake.enqueue_ok(
        tl_fns::ephemeral::ReportMessage::CONSTRUCTOR_ID,
        &tl_enums::ReportResult::Reported,
    );
    let rep2 = h
        .live
        .ephemeral_report_message(h.megagroup_peer, 777, b"spam", "unsolicited")
        .expect("report step 2");
    assert_eq!(rep2, EphemeralReportOutcome::Reported);

    // 5. `channels.exportMessageLink` & `channels.getMessageAuthor`.
    h.fake.enqueue_ok(
        tl_fns::channels::ExportMessageLink::CONSTRUCTOR_ID,
        &tl_enums::ExportedMessageLink::ExportedMessageLink(tl_types::ExportedMessageLink {
            link: "https://t.me/news_chan/101?thread=101".to_owned(),
            html: "<script src=\"https://telegram.org/js/telegram-widget.js\"></script>".to_owned(),
        }),
    );
    let exported = h
        .live
        .export_message_link(h.channel_peer, MsgId(101), false, true)
        .expect("export_message_link");
    assert_eq!(exported.link, "https://t.me/news_chan/101?thread=101");

    let author_user = synthetic_user(4004, "AdminAuthor", "admin_author", false);
    h.fake.enqueue_ok(
        tl_fns::channels::GetMessageAuthor::CONSTRUCTOR_ID,
        &tl_enums::User::User(author_user),
    );
    let author = h
        .live
        .get_message_author(h.channel_peer, MsgId(101))
        .expect("get_message_author");
    assert_eq!(author.name, "AdminAuthor");
    assert_eq!(author.username.as_deref(), Some("admin_author"));
    // Cached lookup makes no second RPC.
    let author_cached = h
        .live
        .get_message_author(h.channel_peer, MsgId(101))
        .expect("cached author");
    assert_eq!(author_cached, author);
    let author_rpc_count = h
        .fake
        .recorded_calls()
        .iter()
        .filter(|(c, _)| *c == tl_fns::channels::GetMessageAuthor::CONSTRUCTOR_ID)
        .count();
    assert_eq!(author_rpc_count, 1);

    // 6. `messages.migrateChat`.
    let migrated_channel = synthetic_channel(5999, "Legacy Group", None, false, true, false);
    let deactivated_chat = synthetic_basic_chat(
        6001,
        "Legacy Group",
        true,
        true,
        Some(tl_enums::InputChannel::InputChannel(
            tl_types::InputChannel {
                channel_id: 5999,
                access_hash: 5999 * 19 + 3,
            },
        )),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::MigrateChat::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: vec![
                tl_enums::Chat::Chat(deactivated_chat),
                tl_enums::Chat::Channel(migrated_channel),
            ],
            date: 1_700_004_000,
            seq: 3,
        }),
    );
    let mig = h
        .live
        .migrate_chat(h.basic_chat_peer)
        .expect("migrate_chat");
    assert!(!mig.already_migrated);
    assert_eq!(mig.old_chat_id, h.basic_chat_peer);
    assert_eq!(
        mig.new_channel_id,
        from_data_peer(DataPeerId::from_channel(ChannelId(5999)))
    );

    // Calling migrate_chat again returns already_migrated=true without another RPC.
    let mig_again = h
        .live
        .migrate_chat(h.basic_chat_peer)
        .expect("already migrated");
    assert!(mig_again.already_migrated);
    assert_eq!(mig_again.new_channel_id, mig.new_channel_id);

    // Pre-checks: non-creator basic group & deactivated basic group reject locally with BAD_MIGRATION.
    let non_creator_chat = synthetic_basic_chat(6002, "Member Only", false, false, None);
    let deactivated_only = synthetic_basic_chat(6003, "Deactivated Only", true, true, None);
    let limit_chat = synthetic_basic_chat(6004, "Channels Limit", true, false, None);
    h.live.write_session(|session| {
        apply_tl_users_and_chats(
            session,
            &[],
            &[
                tl_enums::Chat::Chat(non_creator_chat),
                tl_enums::Chat::Chat(deactivated_only),
                tl_enums::Chat::Chat(limit_chat),
            ],
        );
    });
    let non_creator_peer = from_data_peer(DataPeerId::from_chat(ChatId(6002)));
    let deactivated_peer = from_data_peer(DataPeerId::from_chat(ChatId(6003)));
    let limit_peer = from_data_peer(DataPeerId::from_chat(ChatId(6004)));
    assert!(
        h.live
            .migrate_chat(non_creator_peer)
            .unwrap_err()
            .contains("BAD_MIGRATION")
    );
    assert!(
        h.live
            .migrate_chat(deactivated_peer)
            .unwrap_err()
            .contains("BAD_MIGRATION")
    );
    h.fake.enqueue_rpc_error(
        tl_fns::messages::MigrateChat::CONSTRUCTOR_ID,
        400,
        "CHANNELS_TOO_MUCH",
    );
    assert!(
        h.live
            .migrate_chat(limit_peer)
            .unwrap_err()
            .contains("CHANNELS_TOO_MUCH")
    );

    // 7. `messages.getFutureChatCreatorAfterLeave`.
    let candidate = synthetic_user(4005, "NextOwner", "next_owner", false);
    h.fake.enqueue_ok(
        tl_fns::messages::GetFutureChatCreatorAfterLeave::CONSTRUCTOR_ID,
        &tl_enums::User::User(candidate),
    );
    let next_owner = h
        .live
        .future_chat_creator_after_leave(h.megagroup_peer)
        .expect("future_chat_creator_after_leave")
        .expect("candidate present");
    assert_eq!(next_owner.name, "NextOwner");
    assert_eq!(next_owner.username.as_deref(), Some("next_owner"));

    h.fake.enqueue_ok(
        tl_fns::messages::GetFutureChatCreatorAfterLeave::CONSTRUCTOR_ID,
        &tl_enums::User::Empty(tl_types::UserEmpty { id: 0 }),
    );
    let none_owner = h
        .live
        .future_chat_creator_after_leave(h.channel_peer)
        .expect("future creator empty");
    assert!(none_owner.is_none());
}
