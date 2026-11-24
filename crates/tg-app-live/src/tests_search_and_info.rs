//! Unit and integration tests for `live/search_and_info.rs` using `FakeRpcClient`.
//!
//! Covers:
//! - Global search (`messages.searchGlobal`) with `offset_rate`/`offset_peer`/`offset_id`,
//!   folder filters, chat type filters, and date filters;
//! - In-chat search (`messages.search`) with shared media filter tabs, pinned filter,
//!   and bidirectional pagination (`Before` / `Around` / `After`);
//! - Peer directory search (`contacts.search`) and top peers (`contacts.getTopPeers` /
//!   `contacts.resetTopPeerRating`);
//! - `InfoBackend::peer_members` via `channels.getParticipants` (all filters + pagination)
//!   and `messages.getFullChat` (basic groups);
//! - `InfoBackend::common_groups` via `messages.getCommonChats` (with `max_id` pagination);
//! - `HistoryBackend::report_messages` (`messages.report` option-based flow) and
//!   `HistoryBackend::unpin_all_messages` (`messages.unpinAllMessages` batch loop).
//!
//! All tests run exclusively against synthetic IDs/names and in-process `FakeRpcClient`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::{DcId, InvocationError, LoadDirection, RpcClient};
use tg_data::rights::{ChatAdminRights, ChatBannedRights};
use tg_data::{
    Channel, ChannelFlags, ChannelId, Chat, ChatId, MessageFlags, PeerId as DataPeerId,
    UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::search_and_info::{
    GlobalSearchCursor, GlobalSearchParams, InChatSearchParams, ParticipantFilter,
    ReportStepResult, SharedMediaFilter, TopPeerCategoryKind, compute_top_peers_hash,
    prepare_search_offsets,
};
use tg_app_core::dialogs::backend::{ChatTypeFilter, DialogsBackend, SearchRequest, SearchTab};
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::history::sections_model::MessageReportReason;
use tg_app_core::info::backend::{
    InfoBackend, MediaItem as InfoMediaItem, MemberRole, SharedMediaTab,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

// =============================================================================
// Synthetic TL builders & FakeRpcClient (recording full request bytes)
// =============================================================================

fn syn_user(
    id: i64,
    first_name: &str,
    username: &str,
    is_self: bool,
    is_bot: bool,
) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self && !is_bot,
        mutual_contact: false,
        deleted: false,
        bot: is_bot,
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
        access_hash: Some(id * 1000 + 17),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Unit".to_owned()),
        username: (!username.is_empty()).then(|| username.to_owned()),
        phone: None,
        photo: None,
        status: Some(tl_enums::UserStatus::Online(tl_types::UserStatusOnline {
            expires: i32::MAX,
        })),
        bot_info_version: is_bot.then_some(1),
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

fn syn_channel(
    id: i64,
    title: &str,
    username: &str,
    megagroup: bool,
    count: i32,
) -> tl_types::Channel {
    tl_types::Channel {
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
        forum_sidebar: false,
        in_news: false,
        id,
        access_hash: Some(id * 100 + 9),
        title: title.to_owned(),
        username: (!username.is_empty()).then(|| username.to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_750_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(count),
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

fn syn_basic_chat(id: i64, title: &str, count: i32, migrated_to: Option<i64>) -> tl_types::Chat {
    tl_types::Chat {
        creator: true,
        left: false,
        deactivated: migrated_to.is_some(),
        call_active: false,
        call_not_empty: false,
        noforwards: false,
        id,
        title: title.to_owned(),
        photo: tl_enums::ChatPhoto::Empty,
        participants_count: count,
        date: 1_750_000_000,
        version: 1,
        migrated_to: migrated_to.map(|ch_id| {
            tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                channel_id: ch_id,
                access_hash: ch_id * 100 + 9,
            })
        }),
        admin_rights: None,
        default_banned_rights: None,
    }
}

fn syn_message(
    id: i32,
    peer: tl_enums::Peer,
    from_uid: i64,
    date: i32,
    text: &str,
    pinned: bool,
    media: Option<tl_enums::MessageMedia>,
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
        pinned,
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
        reply_to: None,
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

fn syn_photo_media(photo_id: i64) -> tl_enums::MessageMedia {
    tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
        live_photo: false,
        spoiler: false,
        photo: Some(tl_enums::Photo::Photo(tl_types::Photo {
            has_stickers: false,
            id: photo_id,
            access_hash: photo_id * 13,
            file_reference: vec![1, 2, 3],
            date: 1_750_000_000,
            sizes: vec![tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
                r#type: "x".to_owned(),
                w: 800,
                h: 600,
                size: 40_000,
            })],
            video_sizes: None,
            dc_id: 2,
        })),
        ttl_seconds: None,
        video: None,
    })
}

fn syn_document_media(
    doc_id: i64,
    mime_type: &str,
    size: i64,
    attrs: Vec<tl_enums::DocumentAttribute>,
) -> tl_enums::MessageMedia {
    tl_enums::MessageMedia::Document(tl_types::MessageMediaDocument {
        nopremium: false,
        spoiler: false,
        video: false,
        round: false,
        voice: false,
        document: Some(tl_enums::Document::Document(tl_types::Document {
            id: doc_id,
            access_hash: doc_id * 11,
            file_reference: vec![4, 5, 6],
            date: 1_750_000_000,
            mime_type: mime_type.to_owned(),
            size,
            thumbs: None,
            video_thumbs: None,
            dc_id: 2,
            attributes: attrs,
        })),
        alt_documents: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
    })
}

fn syn_webpage_media(url: &str, title: &str) -> tl_enums::MessageMedia {
    tl_enums::MessageMedia::WebPage(tl_types::MessageMediaWebPage {
        force_large_media: false,
        force_small_media: false,
        manual: false,
        safe: true,
        webpage: tl_enums::WebPage::WebPage(tl_types::WebPage {
            has_large_media: false,
            video_cover_photo: false,
            id: 99_001,
            url: url.to_owned(),
            display_url: "example.org".to_owned(),
            hash: 0,
            r#type: Some("article".to_owned()),
            site_name: Some("Example".to_owned()),
            title: Some(title.to_owned()),
            description: Some("Shared link preview".to_owned()),
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
    })
}

fn syn_dialog(peer_uid: i64, top_message: i32) -> tl_enums::Dialog {
    tl_enums::Dialog::Dialog(tl_types::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
        top_message,
        read_inbox_max_id: top_message,
        read_outbox_max_id: top_message,
        unread_count: 0,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
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
        pts: None,
        draft: None,
        folder_id: None,
        ttl_period: None,
    })
}

fn default_admin_rights_tl(post: bool, ban: bool) -> tl_enums::ChatAdminRights {
    tl_enums::ChatAdminRights::ChatAdminRights(tl_types::ChatAdminRights {
        change_info: true,
        post_messages: post,
        edit_messages: false,
        delete_messages: true,
        ban_users: ban,
        invite_users: true,
        pin_messages: true,
        add_admins: false,
        anonymous: false,
        manage_call: false,
        other: false,
        manage_topics: false,
        post_stories: false,
        edit_stories: false,
        delete_stories: false,
        manage_direct_messages: false,
        manage_ranks: false,
        manage_linked_peers: false,
        manage_welcome_messages: false,
    })
}

fn default_banned_rights_tl(send_messages: bool, until_date: i32) -> tl_enums::ChatBannedRights {
    tl_enums::ChatBannedRights::ChatBannedRights(tl_types::ChatBannedRights {
        view_messages: false,
        send_messages,
        send_media: true,
        send_stickers: false,
        send_gifs: false,
        send_games: false,
        send_inline: false,
        embed_links: false,
        send_polls: false,
        change_info: false,
        invite_users: false,
        pin_messages: false,
        manage_topics: false,
        send_photos: false,
        send_videos: false,
        send_roundvideos: false,
        send_audios: false,
        send_voices: false,
        send_docs: false,
        send_plain: false,
        edit_rank: false,
        send_reactions: false,
        manage_linked_peers: false,
        until_date,
    })
}

type RecordedRequest = (u32, Vec<u8>);
type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedRequest>>>,
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

    fn recorded_requests<R: Identifiable + Deserializable>(&self) -> Vec<R> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == R::CONSTRUCTOR_ID)
            .map(|(_, bytes)| R::from_bytes(bytes).expect("deserialize recorded TL request"))
            .collect()
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

struct Harness {
    peer_alpha: PeerId,
    uid_alpha: i64,
    user_alpha: tl_types::User,
    fake: FakeRpcClient,
    runtime: Arc<SessionRuntime<crate::ErasedRpcClient>>,
    live: LiveSession,
}

impl Harness {
    fn start() -> Self {
        let self_uid = 800_001i64;
        let uid_alpha = 800_002i64;
        let self_user = syn_user(self_uid, "SelfAccount", "self_unit", true, false);
        let user_alpha = syn_user(uid_alpha, "Alpha", "alpha_unit", false, false);
        let top_msg = syn_message(
            10,
            tl_enums::Peer::User(tl_types::PeerUser { user_id: uid_alpha }),
            uid_alpha,
            1_750_000_000,
            "Initial greeting",
            false,
            None,
        );

        let fake = FakeRpcClient::new();
        fake.enqueue_ok(
            tl_fns::users::GetUsers::CONSTRUCTOR_ID,
            &vec![tl_enums::User::User(self_user)],
        );
        fake.enqueue_ok(
            tl_fns::updates::GetState::CONSTRUCTOR_ID,
            &tl_enums::updates::State::State(tl_types::updates::State {
                pts: 100,
                qts: 0,
                date: 1_750_000_000,
                seq: 1,
                unread_count: 0,
            }),
        );
        fake.enqueue_ok(
            tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
            &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
                date: 1_750_000_001,
                seq: 1,
            }),
        );
        fake.enqueue_ok(
            tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
            &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
                dialogs: vec![syn_dialog(uid_alpha, 10)],
                messages: vec![top_msg],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(user_alpha.clone())],
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
                    pts: 100,
                    qts: 0,
                    date: 1_750_000_001,
                    seq: 1,
                    unread_count: 0,
                }),
            }),
        );

        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId::from_tl(self_uid),
                crate::ErasedRpcClient::new(fake.clone()),
                SessionRuntimeConfig::default(),
            )
            .expect("start SessionRuntime"),
        );
        runtime
            .tokio_handle()
            .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
            .expect("wait_ready timeout")
            .expect("startup ready");

        let live = LiveSession::new(Arc::clone(&runtime), 42);
        Self {
            peer_alpha: PeerId::from_user(uid_alpha as u64),
            uid_alpha,
            user_alpha,
            fake,
            runtime,
            live,
        }
    }

    fn seed_channel(&self, ch_id: i64, title: &str, megagroup: bool) -> PeerId {
        let cid = ChannelId::from_tl(ch_id);
        let mut ch = Channel::new(cid);
        ch.access_hash = (ch_id * 100 + 9) as u64;
        ch.set_name(title, "");
        ch.flags = ChannelFlags::CAN_VIEW_PARTICIPANTS;
        if megagroup {
            ch.flags.insert(ChannelFlags::MEGAGROUP);
            ch.mg_info = Some(tg_data::MegagroupInfo::default());
        } else {
            ch.flags.insert(ChannelFlags::BROADCAST);
        }
        let session = self.runtime.session();
        self.runtime.tokio_handle().block_on(async {
            let mut s = session.write().await;
            s.upsert_channel(ch, tg_data::PeerUpdateFlags::FULL_INFO);
        });
        PeerId::from_channel(ch_id as u64)
    }

    fn seed_basic_chat(&self, chat_id: i64, title: &str) -> PeerId {
        let cid = ChatId::from_tl(chat_id);
        let mut chat = Chat::new(cid);
        chat.set_name(title);
        let session = self.runtime.session();
        self.runtime.tokio_handle().block_on(async {
            let mut s = session.write().await;
            s.upsert_chat(chat, tg_data::PeerUpdateFlags::FULL_INFO);
        });
        PeerId::from_chat(chat_id as u64)
    }

    fn shutdown(self) {
        drop(self.live);
        if let Ok(rt) = Arc::try_unwrap(self.runtime) {
            rt.shutdown();
        }
    }
}

// =============================================================================
// 1. Global search (`messages.searchGlobal`) with pagination, folder, and dates
// =============================================================================

#[test]
fn global_search_pagination_folder_and_date_filters() {
    let h = Harness::start();
    let uid_beta = 800_003i64;
    let user_beta = syn_user(uid_beta, "Beta", "beta_unit", false, false);

    // Page 1: messages.messagesSlice with next_rate = 1_750_000_500
    h.fake.enqueue_ok(
        tl_fns::messages::SearchGlobal::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
            inexact: false,
            count: 3,
            next_rate: Some(1_750_000_500),
            offset_id_offset: None,
            search_flood: None,
            messages: vec![
                syn_message(
                    200,
                    tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: h.uid_alpha,
                    }),
                    h.uid_alpha,
                    1_750_000_900,
                    "Rust release notes part 1",
                    false,
                    None,
                ),
                syn_message(
                    150,
                    tl_enums::Peer::User(tl_types::PeerUser { user_id: uid_beta }),
                    uid_beta,
                    1_750_000_500,
                    "Rust release notes part 2",
                    false,
                    None,
                ),
            ],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![
                tl_enums::User::User(h.user_alpha.clone()),
                tl_enums::User::User(user_beta.clone()),
            ],
        }),
    );

    // Page 2: final messages.messages (complete)
    h.fake.enqueue_ok(
        tl_fns::messages::SearchGlobal::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![syn_message(
                90,
                tl_enums::Peer::User(tl_types::PeerUser { user_id: uid_beta }),
                uid_beta,
                1_750_000_100,
                "Rust release notes part 3",
                false,
                None,
            )],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(user_beta)],
        }),
    );

    let params = GlobalSearchParams {
        query: "Rust".to_owned(),
        chat_type_filter: ChatTypeFilter::Private,
        folder_id: Some(1),
        filter: SharedMediaFilter::Empty,
        min_date: 1_750_000_000,
        max_date: 1_750_001_000,
        limit: 2,
    };

    let page1 = h
        .live
        .search_global_page(&params, GlobalSearchCursor::default())
        .expect("page 1");
    assert_eq!(page1.total_count, 3);
    assert_eq!(page1.messages.len(), 2);
    assert!(page1.has_more);
    assert_eq!(page1.next_cursor.offset_rate, 1_750_000_500);
    assert_eq!(page1.next_cursor.offset_id, MsgId(150));
    assert_eq!(
        page1.next_cursor.offset_peer,
        Some(PeerId::from_user(uid_beta as u64))
    );

    let page2 = h
        .live
        .search_global_page(&params, page1.next_cursor)
        .expect("page 2");
    assert_eq!(page2.messages.len(), 1);
    assert_eq!(page2.messages[0].msg_id, MsgId(90));
    assert!(!page2.has_more);

    let reqs = h.fake.recorded_requests::<tl_fns::messages::SearchGlobal>();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].users_only);
    assert!(!reqs[0].groups_only);
    assert!(!reqs[0].broadcasts_only);
    assert_eq!(reqs[0].folder_id, Some(1));
    assert_eq!(reqs[0].min_date, 1_750_000_000);
    assert_eq!(reqs[0].max_date, 1_750_001_000);
    assert_eq!(reqs[0].offset_rate, 0);
    assert_eq!(reqs[0].offset_id, 0);
    assert!(matches!(reqs[0].offset_peer, tl_enums::InputPeer::Empty));

    // Page 2 carries offset_rate, offset_peer (Beta), and offset_id (150).
    assert_eq!(reqs[1].offset_rate, 1_750_000_500);
    assert_eq!(reqs[1].offset_id, 150);
    assert!(matches!(
        reqs[1].offset_peer,
        tl_enums::InputPeer::User(ref u) if u.user_id == uid_beta
    ));

    h.shutdown();
}

#[test]
fn dialogs_search_uses_search_global_and_contacts_search_with_tab_flags() {
    let h = Harness::start();
    let ch_id = 900_101i64;
    let ch = syn_channel(ch_id, "Rust News", "rust_news_unit", false, 4200);

    h.fake.enqueue_ok(
        tl_fns::contacts::Search::CONSTRUCTOR_ID,
        &tl_enums::contacts::Found::Found(tl_types::contacts::Found {
            my_results: Vec::new(),
            results: vec![tl_enums::Peer::Channel(tl_types::PeerChannel {
                channel_id: ch_id,
            })],
            chats: vec![tl_enums::Chat::Channel(ch.clone())],
            users: Vec::new(),
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::SearchGlobal::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![syn_message(
                77,
                tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: ch_id }),
                h.uid_alpha,
                1_750_000_300,
                "Rust 2024 edition announcement",
                false,
                None,
            )],
            topics: Vec::new(),
            chats: vec![tl_enums::Chat::Channel(ch)],
            users: vec![tl_enums::User::User(h.user_alpha.clone())],
        }),
    );

    let req = SearchRequest {
        query: "rust".to_owned(),
        tab: SearchTab::Channels,
        chat_type_filter: ChatTypeFilter::Channels,
        in_peer: None,
        in_archive: false,
        selected_tag: None,
        messages_offset: 0,
        messages_limit: 20,
    };

    let res = h.live.search_dialogs(&req);
    assert_eq!(res.global_peers.len(), 1);
    assert_eq!(res.global_peers[0].username, "rust_news_unit");
    assert_eq!(res.messages.len(), 1);
    assert_eq!(res.messages[0].msg_id, MsgId(77));

    // Verify `contacts.search` passed `broadcasts = true` for `SearchTab::Channels`.
    let peer_reqs = h.fake.recorded_requests::<tl_fns::contacts::Search>();
    assert_eq!(peer_reqs.len(), 1);
    assert!(peer_reqs[0].broadcasts);
    assert!(!peer_reqs[0].bots);

    // Calling again with identical query must hit cache and not re-issue RPCs.
    let res2 = h.live.search_dialogs(&req);
    assert_eq!(res2.messages.len(), 1);
    assert_eq!(
        h.fake
            .recorded_requests::<tl_fns::messages::SearchGlobal>()
            .len(),
        1
    );
    assert_eq!(
        h.fake.recorded_requests::<tl_fns::contacts::Search>().len(),
        1
    );

    h.shutdown();
}

// =============================================================================
// 2. In-chat search, shared media tabs, pinned filter, bidirectional pagination
// =============================================================================

#[test]
fn in_chat_search_bidirectional_pagination_offsets() {
    // Known-answer check against `data_search_controller.cpp:159-181`.
    assert_eq!(
        prepare_search_offsets(MsgId(0), LoadDirection::Before, 50),
        (0, 0)
    );
    assert_eq!(
        prepare_search_offsets(MsgId(100), LoadDirection::Before, 50),
        (100, 0)
    );
    assert_eq!(
        prepare_search_offsets(MsgId(100), LoadDirection::Around, 50),
        (100, -25)
    );
    assert_eq!(
        prepare_search_offsets(MsgId(100), LoadDirection::After, 50),
        (101, -50)
    );

    let h = Harness::start();
    let peer_tl = tl_enums::Peer::User(tl_types::PeerUser {
        user_id: h.uid_alpha,
    });

    // Around anchor 100 -> returns [110, 100, 90]
    h.fake.enqueue_ok(
        tl_fns::messages::Search::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
            inexact: false,
            count: 10,
            next_rate: None,
            offset_id_offset: None,
            search_flood: None,
            messages: vec![
                syn_message(
                    110,
                    peer_tl.clone(),
                    h.uid_alpha,
                    1_750_000_110,
                    "hit 110",
                    false,
                    None,
                ),
                syn_message(
                    100,
                    peer_tl.clone(),
                    h.uid_alpha,
                    1_750_000_100,
                    "hit 100",
                    false,
                    None,
                ),
                syn_message(
                    90,
                    peer_tl.clone(),
                    h.uid_alpha,
                    1_750_000_090,
                    "hit 90",
                    false,
                    None,
                ),
            ],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(h.user_alpha.clone())],
        }),
    );
    // Paginate older (Before 90) -> returns [80, 70]
    h.fake.enqueue_ok(
        tl_fns::messages::Search::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
            inexact: false,
            count: 10,
            next_rate: None,
            offset_id_offset: None,
            search_flood: None,
            messages: vec![
                syn_message(
                    80,
                    peer_tl.clone(),
                    h.uid_alpha,
                    1_750_000_080,
                    "hit 80",
                    false,
                    None,
                ),
                syn_message(
                    70,
                    peer_tl.clone(),
                    h.uid_alpha,
                    1_750_000_070,
                    "hit 70",
                    false,
                    None,
                ),
            ],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(h.user_alpha.clone())],
        }),
    );
    // Paginate newer (After 110) -> returns [130, 120]
    h.fake.enqueue_ok(
        tl_fns::messages::Search::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
            inexact: false,
            count: 10,
            next_rate: None,
            offset_id_offset: None,
            search_flood: None,
            messages: vec![
                syn_message(
                    130,
                    peer_tl.clone(),
                    h.uid_alpha,
                    1_750_000_130,
                    "hit 130",
                    false,
                    None,
                ),
                syn_message(
                    120,
                    peer_tl,
                    h.uid_alpha,
                    1_750_000_120,
                    "hit 120",
                    false,
                    None,
                ),
            ],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(h.user_alpha.clone())],
        }),
    );

    let mut params = InChatSearchParams::new(h.peer_alpha, "hit", SharedMediaFilter::Empty);
    params.around_id = MsgId(100);
    params.direction = LoadDirection::Around;
    params.limit = 4;

    let around = h.live.search_in_chat(&params).expect("around");
    assert_eq!(around.oldest_id, Some(MsgId(90)));
    assert_eq!(around.newest_id, Some(MsgId(110)));
    assert_eq!(around.no_skip_from, MsgId(90));
    assert_eq!(around.no_skip_till, MsgId(110));

    params.around_id = around.oldest_id.expect("oldest");
    params.direction = LoadDirection::Before;
    params.limit = 2;
    let older = h.live.search_in_chat(&params).expect("older");
    assert_eq!(
        older.messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![MsgId(80), MsgId(70)]
    );
    assert!(older.has_more_older);

    params.around_id = around.newest_id.expect("newest");
    params.direction = LoadDirection::After;
    params.limit = 2;
    let newer = h.live.search_in_chat(&params).expect("newer");
    assert_eq!(
        newer.messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![MsgId(130), MsgId(120)]
    );
    assert!(newer.has_more_newer);

    let reqs = h.fake.recorded_requests::<tl_fns::messages::Search>();
    assert_eq!(reqs.len(), 3);
    assert_eq!((reqs[0].offset_id, reqs[0].add_offset), (100, -2));
    assert_eq!((reqs[1].offset_id, reqs[1].add_offset), (90, 0));
    assert_eq!((reqs[2].offset_id, reqs[2].add_offset), (111, -2));

    h.shutdown();
}

#[test]
fn shared_media_tabs_and_pinned_filter_via_messages_search() {
    let h = Harness::start();
    let peer_tl = tl_enums::Peer::User(tl_types::PeerUser {
        user_id: h.uid_alpha,
    });

    // Enqueue one response per SharedMediaTab in SharedMediaTab::ALL order.
    let tab_messages: [(SharedMediaTab, tl_enums::Message); 6] = [
        (
            SharedMediaTab::PhotosVideos,
            syn_message(
                11,
                peer_tl.clone(),
                h.uid_alpha,
                1_750_000_011,
                "photo cap",
                false,
                Some(syn_photo_media(501)),
            ),
        ),
        (
            SharedMediaTab::Files,
            syn_message(
                12,
                peer_tl.clone(),
                h.uid_alpha,
                1_750_000_012,
                "",
                false,
                Some(syn_document_media(
                    502,
                    "application/pdf",
                    2048,
                    vec![tl_enums::DocumentAttribute::Filename(
                        tl_types::DocumentAttributeFilename {
                            file_name: "spec.pdf".to_owned(),
                        },
                    )],
                )),
            ),
        ),
        (
            SharedMediaTab::Links,
            syn_message(
                13,
                peer_tl.clone(),
                h.uid_alpha,
                1_750_000_013,
                "https://example.org",
                false,
                Some(syn_webpage_media("https://example.org", "Spec Link")),
            ),
        ),
        (
            SharedMediaTab::Voice,
            syn_message(
                14,
                peer_tl.clone(),
                h.uid_alpha,
                1_750_000_014,
                "",
                false,
                Some(syn_document_media(
                    504,
                    "audio/ogg",
                    9000,
                    vec![tl_enums::DocumentAttribute::Audio(
                        tl_types::DocumentAttributeAudio {
                            voice: true,
                            duration: 7,
                            title: None,
                            performer: None,
                            waveform: Some(vec![0xAA, 0x55]),
                        },
                    )],
                )),
            ),
        ),
        (
            SharedMediaTab::Music,
            syn_message(
                15,
                peer_tl.clone(),
                h.uid_alpha,
                1_750_000_015,
                "",
                false,
                Some(syn_document_media(
                    505,
                    "audio/mpeg",
                    500_000,
                    vec![tl_enums::DocumentAttribute::Audio(
                        tl_types::DocumentAttributeAudio {
                            voice: false,
                            duration: 185,
                            title: Some("Track One".to_owned()),
                            performer: Some("Synth Artist".to_owned()),
                            waveform: None,
                        },
                    )],
                )),
            ),
        ),
        (
            SharedMediaTab::Gifs,
            syn_message(
                16,
                peer_tl.clone(),
                h.uid_alpha,
                1_750_000_016,
                "",
                false,
                Some(syn_document_media(
                    506,
                    "video/mp4",
                    120_000,
                    vec![
                        tl_enums::DocumentAttribute::Animated,
                        tl_enums::DocumentAttribute::Video(tl_types::DocumentAttributeVideo {
                            round_message: false,
                            supports_streaming: true,
                            nosound: true,
                            duration: 3.0,
                            w: 320,
                            h: 240,
                            preload_prefix_size: None,
                            video_start_ts: None,
                            video_codec: None,
                        }),
                    ],
                )),
            ),
        ),
    ];

    for (_, msg) in &tab_messages {
        h.fake.enqueue_ok(
            tl_fns::messages::Search::CONSTRUCTOR_ID,
            &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
                messages: vec![msg.clone()],
                topics: Vec::new(),
                chats: Vec::new(),
                users: vec![tl_enums::User::User(h.user_alpha.clone())],
            }),
        );
    }

    // Pinned messages response
    h.fake.enqueue_ok(
        tl_fns::messages::Search::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![syn_message(
                42,
                peer_tl,
                h.uid_alpha,
                1_750_000_042,
                "Important pinned note",
                true,
                None,
            )],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(h.user_alpha.clone())],
        }),
    );

    let media = h
        .live
        .shared_media(h.peer_alpha, SharedMediaTab::PhotosVideos, "");
    assert!(matches!(
        media.as_slice(),
        [InfoMediaItem::Photo { id: 11, .. }]
    ));

    let files = h.live.shared_media(h.peer_alpha, SharedMediaTab::Files, "");
    assert!(
        matches!(files.as_slice(), [InfoMediaItem::File { id: 12, filename, .. }] if filename == "spec.pdf")
    );

    let links = h.live.shared_media(h.peer_alpha, SharedMediaTab::Links, "");
    assert!(
        matches!(links.as_slice(), [InfoMediaItem::Link { id: 13, title, .. }] if title == "Spec Link")
    );

    let voice = h.live.shared_media(h.peer_alpha, SharedMediaTab::Voice, "");
    assert!(matches!(
        voice.as_slice(),
        [InfoMediaItem::Voice {
            id: 14,
            duration_secs: 7,
            ..
        }]
    ));

    let music = h.live.shared_media(h.peer_alpha, SharedMediaTab::Music, "");
    assert!(
        matches!(music.as_slice(), [InfoMediaItem::Music { id: 15, title, .. }] if title == "Track One")
    );

    let gifs = h.live.shared_media(h.peer_alpha, SharedMediaTab::Gifs, "");
    assert!(matches!(
        gifs.as_slice(),
        [InfoMediaItem::Gif { id: 16, .. }]
    ));

    let pinned = h.live.pinned_messages(h.peer_alpha);
    assert_eq!(pinned.len(), 1);
    assert_eq!(pinned[0].msg_id, MsgId(42));
    assert!(pinned[0].preview.contains("Important pinned note"));

    let reqs = h.fake.recorded_requests::<tl_fns::messages::Search>();
    assert_eq!(reqs.len(), 7);
    assert!(matches!(
        reqs[0].filter,
        tl_enums::MessagesFilter::InputMessagesFilterPhotoVideo
    ));
    assert!(matches!(
        reqs[1].filter,
        tl_enums::MessagesFilter::InputMessagesFilterDocument
    ));
    assert!(matches!(
        reqs[2].filter,
        tl_enums::MessagesFilter::InputMessagesFilterUrl
    ));
    assert!(matches!(
        reqs[3].filter,
        tl_enums::MessagesFilter::InputMessagesFilterRoundVoice
    ));
    assert!(matches!(
        reqs[4].filter,
        tl_enums::MessagesFilter::InputMessagesFilterMusic
    ));
    assert!(matches!(
        reqs[5].filter,
        tl_enums::MessagesFilter::InputMessagesFilterGif
    ));
    assert!(matches!(
        reqs[6].filter,
        tl_enums::MessagesFilter::InputMessagesFilterPinned
    ));

    h.shutdown();
}

// =============================================================================
// 3. Top Peers (`contacts.getTopPeers` & `contacts.resetTopPeerRating`)
// =============================================================================

#[test]
fn top_peers_fetch_hash_not_modified_and_reset_rating() {
    // Known-answer hash check (`api/api_hash.h`: HashInit=0, xorshift + add).
    assert_eq!(compute_top_peers_hash(&[]), 0);
    assert_eq!(compute_top_peers_hash(&[42]), 42);
    let expected_two = {
        let mut h: u64 = 42;
        h ^= h >> 21;
        h ^= h << 35;
        h ^= h >> 4;
        h.wrapping_add(99) as i64
    };
    assert_eq!(compute_top_peers_hash(&[42, 99]), expected_two);

    let mut h = Harness::start();
    let uid_top = 800_009i64;
    let user_top = syn_user(uid_top, "Gamma", "gamma_unit", false, false);

    h.fake.enqueue_ok(
        tl_fns::contacts::GetTopPeers::CONSTRUCTOR_ID,
        &tl_enums::contacts::TopPeers::TopPeers(tl_types::contacts::TopPeers {
            categories: vec![tl_enums::TopPeerCategoryPeers::TopPeerCategoryPeers(
                tl_types::TopPeerCategoryPeers {
                    category: tl_enums::TopPeerCategory::Correspondents,
                    count: 2,
                    peers: vec![
                        tl_enums::TopPeer::TopPeer(tl_types::TopPeer {
                            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: uid_top }),
                            rating: 9.5,
                        }),
                        tl_enums::TopPeer::TopPeer(tl_types::TopPeer {
                            peer: tl_enums::Peer::User(tl_types::PeerUser {
                                user_id: h.uid_alpha,
                            }),
                            rating: 4.2,
                        }),
                    ],
                },
            )],
            chats: Vec::new(),
            users: vec![
                tl_enums::User::User(user_top),
                tl_enums::User::User(h.user_alpha.clone()),
            ],
        }),
    );

    let top = h.live.top_peers();
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].peer_id, PeerId::from_user(uid_top as u64));
    assert_eq!(top[0].short_name, "Gamma");
    assert_eq!(top[1].peer_id, h.peer_alpha);

    // Second `top_peers()` uses cache (still 1 GetTopPeers RPC).
    assert_eq!(h.live.top_peers().len(), 2);
    assert_eq!(
        h.fake
            .recorded_requests::<tl_fns::contacts::GetTopPeers>()
            .len(),
        1
    );

    // Explicit refresh sends non-zero hash and handles `TopPeersNotModified`.
    h.fake.enqueue_ok(
        tl_fns::contacts::GetTopPeers::CONSTRUCTOR_ID,
        &tl_enums::contacts::TopPeers::NotModified,
    );
    let refreshed = h
        .live
        .fetch_top_peers(TopPeerCategoryKind::Correspondents, 0, 64)
        .expect("refresh not modified");
    assert_eq!(refreshed.len(), 2);
    let get_reqs = h.fake.recorded_requests::<tl_fns::contacts::GetTopPeers>();
    assert_eq!(get_reqs[0].hash, 0);
    assert_eq!(
        get_reqs[1].hash,
        compute_top_peers_hash(&[uid_top as u64, h.uid_alpha as u64])
    );

    // `remove_top_peer` invokes `contacts.resetTopPeerRating`.
    h.fake
        .enqueue_ok(tl_fns::contacts::ResetTopPeerRating::CONSTRUCTOR_ID, &true);
    h.live.remove_top_peer(PeerId::from_user(uid_top as u64));
    let after = h.live.top_peers();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].peer_id, h.peer_alpha);

    let reset_reqs = h
        .fake
        .recorded_requests::<tl_fns::contacts::ResetTopPeerRating>();
    assert_eq!(reset_reqs.len(), 1);
    assert!(matches!(
        reset_reqs[0].category,
        tl_enums::TopPeerCategory::Correspondents
    ));
    assert!(matches!(
        reset_reqs[0].peer,
        tl_enums::InputPeer::User(ref u) if u.user_id == uid_top
    ));

    h.shutdown();
}

// =============================================================================
// 4. `InfoBackend::peer_members`: `channels.getParticipants` & `messages.getFullChat`
// =============================================================================

#[test]
fn channel_participants_all_filters_roles_and_pagination() {
    let h = Harness::start();
    let ch_peer = h.seed_channel(901_000, "Dev Group", true);

    let uid_owner = 810_001i64;
    let uid_admin = 810_002i64;
    let uid_bot = 810_003i64;
    let uid_banned = 810_004i64;

    let u_owner = syn_user(uid_owner, "Owner", "owner_u", false, false);
    let u_admin = syn_user(uid_admin, "Admin", "admin_u", false, false);
    let u_bot = syn_user(uid_bot, "HelperBot", "helper_bot", false, true);
    let u_banned = syn_user(uid_banned, "Muted", "muted_u", false, false);

    // 1. Recent (via InfoBackend::peer_members with empty query)
    h.fake.enqueue_ok(
        tl_fns::channels::GetParticipants::CONSTRUCTOR_ID,
        &tl_enums::channels::ChannelParticipants::ChannelParticipants(
            tl_types::channels::ChannelParticipants {
                count: 4,
                participants: vec![
                    tl_enums::ChannelParticipant::Creator(tl_types::ChannelParticipantCreator {
                        user_id: uid_owner,
                        admin_rights: default_admin_rights_tl(true, true),
                        rank: Some("Founder".to_owned()),
                    }),
                    tl_enums::ChannelParticipant::Admin(tl_types::ChannelParticipantAdmin {
                        can_edit: true,
                        self_: false,
                        user_id: uid_admin,
                        inviter_id: Some(uid_owner),
                        promoted_by: uid_owner,
                        date: 1_750_000_000,
                        admin_rights: default_admin_rights_tl(false, true),
                        rank: Some("Mod".to_owned()),
                    }),
                ],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(u_owner), tl_enums::User::User(u_admin)],
            },
        ),
    );

    // 2. Search (via InfoBackend::peer_members with query = "help")
    h.fake.enqueue_ok(
        tl_fns::channels::GetParticipants::CONSTRUCTOR_ID,
        &tl_enums::channels::ChannelParticipants::ChannelParticipants(
            tl_types::channels::ChannelParticipants {
                count: 1,
                participants: vec![tl_enums::ChannelParticipant::ChannelParticipant(
                    tl_types::ChannelParticipant {
                        user_id: uid_bot,
                        date: 1_750_000_000,
                        subscription_until_date: None,
                        rank: None,
                    },
                )],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(u_bot.clone())],
            },
        ),
    );

    // 3. Admins, Bots, Kicked, Banned via explicit `load_channel_participants`
    h.fake.enqueue_ok(
        tl_fns::channels::GetParticipants::CONSTRUCTOR_ID,
        &tl_enums::channels::ChannelParticipants::ChannelParticipants(
            tl_types::channels::ChannelParticipants {
                count: 1,
                participants: vec![tl_enums::ChannelParticipant::ChannelParticipant(
                    tl_types::ChannelParticipant {
                        user_id: uid_bot,
                        date: 1_750_000_000,
                        subscription_until_date: None,
                        rank: None,
                    },
                )],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(u_bot)],
            },
        ),
    );
    h.fake.enqueue_ok(
        tl_fns::channels::GetParticipants::CONSTRUCTOR_ID,
        &tl_enums::channels::ChannelParticipants::ChannelParticipants(
            tl_types::channels::ChannelParticipants {
                count: 1,
                participants: vec![tl_enums::ChannelParticipant::Banned(
                    tl_types::ChannelParticipantBanned {
                        left: false,
                        peer: tl_enums::Peer::User(tl_types::PeerUser {
                            user_id: uid_banned,
                        }),
                        kicked_by: uid_admin,
                        date: 1_750_000_000,
                        banned_rights: default_banned_rights_tl(true, 1_800_000_000),
                        rank: None,
                    },
                )],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(u_banned)],
            },
        ),
    );

    let recent = h.live.peer_members(ch_peer, "");
    assert_eq!(recent.len(), 2);
    assert!(matches!(
        recent[0].role,
        MemberRole::Owner { custom_title: Some(ref t) } if t == "Founder"
    ));
    assert!(matches!(
        recent[1].role,
        MemberRole::Admin {
            custom_title: Some(ref t),
            rights,
        } if t == "Mod" && rights.contains(ChatAdminRights::BAN_USERS)
    ));

    let searched = h.live.peer_members(ch_peer, "help");
    assert_eq!(searched.len(), 1);
    assert!(searched[0].is_bot);
    assert_eq!(searched[0].role, MemberRole::Member);

    let bots_page = h
        .live
        .load_channel_participants(ch_peer, ParticipantFilter::Bots, 0, 50, 0)
        .expect("bots");
    assert_eq!(bots_page.members.len(), 1);
    assert!(!bots_page.has_more);

    let banned_page = h
        .live
        .load_channel_participants(
            ch_peer,
            ParticipantFilter::Banned("mut".to_owned()),
            0,
            50,
            0,
        )
        .expect("banned");
    assert_eq!(banned_page.members.len(), 1);
    assert!(matches!(
        banned_page.members[0].role,
        MemberRole::Restricted { rights, until: 1_800_000_000 }
        if rights.contains(ChatBannedRights::SEND_MESSAGES)
    ));

    let reqs = h
        .fake
        .recorded_requests::<tl_fns::channels::GetParticipants>();
    assert_eq!(reqs.len(), 4);
    assert!(matches!(
        reqs[0].filter,
        tl_enums::ChannelParticipantsFilter::ChannelParticipantsRecent
    ));
    assert!(matches!(
        reqs[1].filter,
        tl_enums::ChannelParticipantsFilter::ChannelParticipantsSearch(ref s) if s.q == "help"
    ));
    assert!(matches!(
        reqs[2].filter,
        tl_enums::ChannelParticipantsFilter::ChannelParticipantsBots
    ));
    assert!(matches!(
        reqs[3].filter,
        tl_enums::ChannelParticipantsFilter::ChannelParticipantsBanned(ref b) if b.q == "mut"
    ));

    h.shutdown();
}

#[test]
fn basic_group_peer_members_via_messages_get_full_chat() {
    let h = Harness::start();
    let chat_id = 700_555i64;
    let chat_peer = h.seed_basic_chat(chat_id, "Basic Squad");

    let u1 = syn_user(820_001, "Creator", "creator_u", false, false);
    let u2 = syn_user(820_002, "CoAdmin", "coadmin_u", false, false);
    let u3 = syn_user(820_003, "Regular", "regular_u", false, false);

    h.fake.enqueue_ok(
        tl_fns::messages::GetFullChat::CONSTRUCTOR_ID,
        &tl_enums::messages::ChatFull::ChatFull(tl_types::messages::ChatFull {
            full_chat: tl_enums::ChatFull::ChatFull(tl_types::ChatFull {
                can_set_username: false,
                has_scheduled: false,
                translations_disabled: false,
                has_welcome_messages: false,
                id: chat_id,
                about: "Small group".to_owned(),
                participants: tl_enums::ChatParticipants::ChatParticipants(
                    tl_types::ChatParticipants {
                        chat_id,
                        participants: vec![
                            tl_enums::ChatParticipant::ChatParticipant(tl_types::ChatParticipant {
                                user_id: 820_003,
                                inviter_id: 820_001,
                                date: 1_750_000_000,
                                rank: None,
                            }),
                            tl_enums::ChatParticipant::Admin(tl_types::ChatParticipantAdmin {
                                user_id: 820_002,
                                inviter_id: 820_001,
                                date: 1_750_000_000,
                                rank: Some("Lead".to_owned()),
                            }),
                            tl_enums::ChatParticipant::Creator(tl_types::ChatParticipantCreator {
                                user_id: 820_001,
                                rank: None,
                            }),
                        ],
                        version: 3,
                    },
                ),
                chat_photo: None,
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
                exported_invite: None,
                bot_info: None,
                pinned_msg_id: None,
                folder_id: None,
                call: None,
                ttl_period: None,
                groupcall_default_join_as: None,
                theme_emoticon: None,
                requests_pending: None,
                recent_requesters: None,
                available_reactions: None,
                reactions_limit: None,
            }),
            chats: vec![tl_enums::Chat::Chat(syn_basic_chat(
                chat_id,
                "Basic Squad",
                3,
                None,
            ))],
            users: vec![
                tl_enums::User::User(u1),
                tl_enums::User::User(u2),
                tl_enums::User::User(u3),
            ],
        }),
    );

    let all = h.live.peer_members(chat_peer, "");
    assert_eq!(all.len(), 3);
    // Sorted: Owner -> Admin -> Member
    assert!(matches!(all[0].role, MemberRole::Owner { .. }));
    assert!(matches!(
        all[1].role,
        MemberRole::Admin { custom_title: Some(ref t), .. } if t == "Lead"
    ));
    assert_eq!(all[2].role, MemberRole::Member);

    // Filtered query reuses cached basic-chat participants without another RPC.
    let filtered = h.live.peer_members(chat_peer, "coadmin");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].peer_id, PeerId::from_user(820_002));
    assert_eq!(
        h.fake
            .recorded_requests::<tl_fns::messages::GetFullChat>()
            .len(),
        1
    );

    h.shutdown();
}

// =============================================================================
// 5. `InfoBackend::common_groups` via `messages.getCommonChats`
// =============================================================================

#[test]
fn common_groups_pagination_skips_migrated_chats() {
    let h = Harness::start();

    let ch_a = syn_channel(910_001, "Common Megagroup", "", true, 120);
    let migrated_legacy = syn_basic_chat(710_002, "Old Legacy", 10, Some(910_001));
    let ch_b = syn_channel(910_003, "Second Common Group", "", true, 45);

    // Page 1: messages.chatsSlice(count = 3) with 1 channel + 1 migrated basic chat
    h.fake.enqueue_ok(
        tl_fns::messages::GetCommonChats::CONSTRUCTOR_ID,
        &tl_enums::messages::Chats::Slice(tl_types::messages::ChatsSlice {
            count: 3,
            chats: vec![
                tl_enums::Chat::Channel(ch_a),
                tl_enums::Chat::Chat(migrated_legacy),
            ],
        }),
    );
    // Page 2: messages.chats with remaining group
    h.fake.enqueue_ok(
        tl_fns::messages::GetCommonChats::CONSTRUCTOR_ID,
        &tl_enums::messages::Chats::Chats(tl_types::messages::Chats {
            chats: vec![tl_enums::Chat::Channel(ch_b)],
        }),
    );

    let first = h.live.common_groups(h.peer_alpha);
    // Migrated legacy chat is filtered out.
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].title, "Common Megagroup");
    assert_eq!(first[0].members_count, 120);

    // Load page 2 using cached `last_max_id = 710_002`.
    let all = h
        .live
        .load_more_common_groups(h.peer_alpha)
        .expect("page 2");
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].title, "Second Common Group");
    assert_eq!(all[1].members_count, 45);

    let reqs = h
        .fake
        .recorded_requests::<tl_fns::messages::GetCommonChats>();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].max_id, 0);
    assert_eq!(reqs[1].max_id, 710_002);

    h.shutdown();
}

// =============================================================================
// 6. `HistoryBackend::report_messages` (option-based flow) & `unpin_all_messages`
// =============================================================================

#[test]
fn report_messages_option_based_flow_multi_step_and_comment() {
    let mut h = Harness::start();

    // Step 1: initial option = b"" -> ChooseOption (root menu)
    h.fake.enqueue_ok(
        tl_fns::messages::Report::CONSTRUCTOR_ID,
        &tl_enums::ReportResult::ChooseOption(tl_types::ReportResultChooseOption {
            title: "What is wrong with this message?".to_owned(),
            options: vec![
                tl_enums::MessageReportOption::MessageReportOption(tl_types::MessageReportOption {
                    text: "Spam".to_owned(),
                    option: b"opt_spam".to_vec(),
                }),
                tl_enums::MessageReportOption::MessageReportOption(tl_types::MessageReportOption {
                    text: "Copyright".to_owned(),
                    option: b"opt_copy".to_vec(),
                }),
            ],
        }),
    );
    // Step 2: option = b"opt_copy" -> AddComment (required)
    h.fake.enqueue_ok(
        tl_fns::messages::Report::CONSTRUCTOR_ID,
        &tl_enums::ReportResult::AddComment(tl_types::ReportResultAddComment {
            optional: false,
            option: b"opt_copy_comment".to_vec(),
        }),
    );
    // Step 3: option = b"opt_copy_comment", message = "Details" -> Reported
    h.fake.enqueue_ok(
        tl_fns::messages::Report::CONSTRUCTOR_ID,
        &tl_enums::ReportResult::Reported,
    );

    HistoryBackend::report_messages(
        &mut h.live,
        h.peer_alpha,
        &[MsgId(10)],
        MessageReportReason::Copyright,
        "Contains unauthorized material",
    )
    .expect("option-based report flow");

    let reqs = h.fake.recorded_requests::<tl_fns::messages::Report>();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[0].option, Vec::<u8>::new());
    assert_eq!(reqs[0].message, "");
    assert_eq!(reqs[1].option, b"opt_copy".to_vec());
    assert_eq!(reqs[2].option, b"opt_copy_comment".to_vec());
    assert_eq!(reqs[2].message, "Contains unauthorized material");

    // Verify `MESSAGE_ID_REQUIRED` handling via `report_messages_step`.
    h.fake.enqueue_err(
        tl_fns::messages::Report::CONSTRUCTOR_ID,
        InvocationError::Rpc(tg_api::RpcError {
            code: 400,
            name: "MESSAGE_ID_REQUIRED".to_owned(),
            value: None,
        }),
    );
    let step = h
        .live
        .report_messages_step(h.peer_alpha, &[], b"opt_spam", "")
        .expect("step result");
    assert_eq!(step, ReportStepResult::MessageIdRequired);

    // Verify non-optional AddComment rejects an empty comment.
    h.fake.enqueue_ok(
        tl_fns::messages::Report::CONSTRUCTOR_ID,
        &tl_enums::ReportResult::AddComment(tl_types::ReportResultAddComment {
            optional: false,
            option: b"opt_need_text".to_vec(),
        }),
    );
    let err = HistoryBackend::report_messages(
        &mut h.live,
        h.peer_alpha,
        &[MsgId(10)],
        MessageReportReason::Other,
        "   ",
    )
    .expect_err("empty comment must fail when optional == false");
    assert!(err.contains("comment is required"));

    h.shutdown();
}

#[test]
fn unpin_all_messages_loops_until_offset_zero_and_clears_local_flags() {
    let mut h = Harness::start();
    let peer_tl = tl_enums::Peer::User(tl_types::PeerUser {
        user_id: h.uid_alpha,
    });

    // Seed two pinned messages via `messages.search(InputMessagesFilterPinned)`.
    h.fake.enqueue_ok(
        tl_fns::messages::Search::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![
                syn_message(
                    50,
                    peer_tl.clone(),
                    h.uid_alpha,
                    1_750_000_050,
                    "Pin A",
                    true,
                    None,
                ),
                syn_message(51, peer_tl, h.uid_alpha, 1_750_000_051, "Pin B", true, None),
            ],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(h.user_alpha.clone())],
        }),
    );
    assert_eq!(h.live.pinned_messages(h.peer_alpha).len(), 2);

    // Batch 1 returns offset = 1 (more to unpin), Batch 2 returns offset = 0 (done).
    h.fake.enqueue_ok(
        tl_fns::messages::UnpinAllMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 105,
                pts_count: 1,
                offset: 1,
            },
        ),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::UnpinAllMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 106,
                pts_count: 1,
                offset: 0,
            },
        ),
    );

    HistoryBackend::unpin_all_messages(&mut h.live, h.peer_alpha, Some(MsgId(500)))
        .expect("unpin_all_messages");

    let reqs = h
        .fake
        .recorded_requests::<tl_fns::messages::UnpinAllMessages>();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].top_msg_id, Some(500));
    assert_eq!(reqs[1].top_msg_id, Some(500));

    // Verify pinned cache was invalidated and re-querying returns empty when server returns empty.
    h.fake.enqueue_ok(
        tl_fns::messages::Search::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: Vec::new(),
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    assert!(h.live.pinned_messages(h.peer_alpha).is_empty());

    // Also verify local message flags were cleared when unpinning without topic filter.
    let session = h.runtime.session();
    let data_peer = DataPeerId::from_user(DataUserId::from_tl(h.uid_alpha));
    let still_pinned = h.runtime.tokio_handle().block_on(async {
        let s = session.read().await;
        s.message(tg_data::FullMsgId::new(data_peer, tg_data::MsgId(50)))
            .is_some_and(|m| m.flags.contains(MessageFlags::PINNED))
    });
    // Message 50 had no top_msg_id=500 so it stayed untouched by topic-scoped unpin;
    // now unpin all without topic filter and confirm flag is cleared.
    assert!(still_pinned);

    h.fake.enqueue_ok(
        tl_fns::messages::UnpinAllMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 107,
                pts_count: 2,
                offset: 0,
            },
        ),
    );
    HistoryBackend::unpin_all_messages(&mut h.live, h.peer_alpha, None)
        .expect("unpin all whole chat");
    let still_pinned_after = h.runtime.tokio_handle().block_on(async {
        let s = session.read().await;
        s.message(tg_data::FullMsgId::new(data_peer, tg_data::MsgId(50)))
            .is_some_and(|m| m.flags.contains(MessageFlags::PINNED))
    });
    assert!(!still_pinned_after);

    h.shutdown();
}

#[test]
fn channel_participants_admins_kicked_and_multi_page_pagination() {
    let h = Harness::start();
    let ch_peer = h.seed_channel(902_000, "Paged Supergroup", true);

    let uid1 = 830_001i64;
    let uid2 = 830_002i64;
    let uid3 = 830_003i64;
    let uid_kicked = 830_004i64;

    let u1 = syn_user(uid1, "LeadAdmin", "lead_admin", false, false);
    let u2 = syn_user(uid2, "MemberTwo", "member_two", false, false);
    let u3 = syn_user(uid3, "MemberThree", "member_three", false, false);
    let u_kicked = syn_user(uid_kicked, "RemovedUser", "removed_u", false, false);

    // Page 1 of Recent (offset 0, limit 2, total 3)
    h.fake.enqueue_ok(
        tl_fns::channels::GetParticipants::CONSTRUCTOR_ID,
        &tl_enums::channels::ChannelParticipants::ChannelParticipants(
            tl_types::channels::ChannelParticipants {
                count: 3,
                participants: vec![
                    tl_enums::ChannelParticipant::Admin(tl_types::ChannelParticipantAdmin {
                        can_edit: true,
                        self_: false,
                        user_id: uid1,
                        inviter_id: None,
                        promoted_by: uid1,
                        date: 1_750_000_000,
                        admin_rights: default_admin_rights_tl(true, true),
                        rank: Some("Ops".to_owned()),
                    }),
                    tl_enums::ChannelParticipant::ChannelParticipant(
                        tl_types::ChannelParticipant {
                            user_id: uid2,
                            date: 1_750_000_001,
                            subscription_until_date: None,
                            rank: None,
                        },
                    ),
                ],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(u1.clone()), tl_enums::User::User(u2)],
            },
        ),
    );
    // Page 2 of Recent (offset 2, limit 2, total 3)
    h.fake.enqueue_ok(
        tl_fns::channels::GetParticipants::CONSTRUCTOR_ID,
        &tl_enums::channels::ChannelParticipants::ChannelParticipants(
            tl_types::channels::ChannelParticipants {
                count: 3,
                participants: vec![tl_enums::ChannelParticipant::ChannelParticipant(
                    tl_types::ChannelParticipant {
                        user_id: uid3,
                        date: 1_750_000_002,
                        subscription_until_date: None,
                        rank: None,
                    },
                )],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(u3)],
            },
        ),
    );
    // Admins filter
    h.fake.enqueue_ok(
        tl_fns::channels::GetParticipants::CONSTRUCTOR_ID,
        &tl_enums::channels::ChannelParticipants::ChannelParticipants(
            tl_types::channels::ChannelParticipants {
                count: 1,
                participants: vec![tl_enums::ChannelParticipant::Admin(
                    tl_types::ChannelParticipantAdmin {
                        can_edit: true,
                        self_: false,
                        user_id: uid1,
                        inviter_id: None,
                        promoted_by: uid1,
                        date: 1_750_000_000,
                        admin_rights: default_admin_rights_tl(true, true),
                        rank: Some("Ops".to_owned()),
                    },
                )],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(u1)],
            },
        ),
    );
    // Kicked filter with Left participant
    h.fake.enqueue_ok(
        tl_fns::channels::GetParticipants::CONSTRUCTOR_ID,
        &tl_enums::channels::ChannelParticipants::ChannelParticipants(
            tl_types::channels::ChannelParticipants {
                count: 1,
                participants: vec![tl_enums::ChannelParticipant::Left(
                    tl_types::ChannelParticipantLeft {
                        peer: tl_enums::Peer::User(tl_types::PeerUser {
                            user_id: uid_kicked,
                        }),
                    },
                )],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(u_kicked)],
            },
        ),
    );

    let p1 = h
        .live
        .load_channel_participants(ch_peer, ParticipantFilter::Recent, 0, 2, 0)
        .expect("page 1");
    assert_eq!(p1.members.len(), 2);
    assert_eq!(p1.total_count, 3);
    assert_eq!(p1.next_offset, 2);
    assert!(p1.has_more);

    let p2 = h
        .live
        .load_channel_participants(ch_peer, ParticipantFilter::Recent, p1.next_offset, 2, 0)
        .expect("page 2");
    assert_eq!(p2.members.len(), 1);
    assert_eq!(p2.next_offset, 3);
    assert!(!p2.has_more);

    let admins = h
        .live
        .load_channel_participants(ch_peer, ParticipantFilter::Admins, 0, 50, 0)
        .expect("admins");
    assert_eq!(admins.members.len(), 1);
    assert!(matches!(admins.members[0].role, MemberRole::Admin { .. }));

    let kicked = h
        .live
        .load_channel_participants(
            ch_peer,
            ParticipantFilter::Kicked("rem".to_owned()),
            0,
            50,
            0,
        )
        .expect("kicked");
    assert_eq!(kicked.members.len(), 1);
    assert_eq!(
        kicked.members[0].peer_id,
        PeerId::from_user(uid_kicked as u64)
    );

    let reqs = h
        .fake
        .recorded_requests::<tl_fns::channels::GetParticipants>();
    assert_eq!(reqs.len(), 4);
    assert_eq!(reqs[0].offset, 0);
    assert_eq!(reqs[1].offset, 2);
    assert!(matches!(
        reqs[2].filter,
        tl_enums::ChannelParticipantsFilter::ChannelParticipantsAdmins
    ));
    assert!(matches!(
        reqs[3].filter,
        tl_enums::ChannelParticipantsFilter::ChannelParticipantsKicked(ref k) if k.q == "rem"
    ));

    h.shutdown();
}

#[test]
fn shared_media_bidirectional_pagination_and_date_or_sender_only_search() {
    let h = Harness::start();
    let ch_peer = h.seed_channel(903_000, "Media Hub", true);
    let ch_tl = tl_enums::Peer::Channel(tl_types::PeerChannel {
        channel_id: 903_000,
    });

    // 1. load_shared_media_page Before -> returns photo 200, 190
    h.fake.enqueue_ok(
        tl_fns::messages::Search::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
            inexact: false,
            count: 5,
            next_rate: None,
            offset_id_offset: None,
            search_flood: None,
            messages: vec![
                syn_message(
                    200,
                    ch_tl.clone(),
                    h.uid_alpha,
                    1_750_000_200,
                    "photo 200",
                    false,
                    Some(syn_photo_media(601)),
                ),
                syn_message(
                    190,
                    ch_tl.clone(),
                    h.uid_alpha,
                    1_750_000_190,
                    "photo 190",
                    false,
                    Some(syn_photo_media(602)),
                ),
            ],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(h.user_alpha.clone())],
        }),
    );
    // 2. load_shared_media_page After 200 -> returns photo 220, 210
    h.fake.enqueue_ok(
        tl_fns::messages::Search::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
            inexact: false,
            count: 5,
            next_rate: None,
            offset_id_offset: None,
            search_flood: None,
            messages: vec![
                syn_message(
                    220,
                    ch_tl.clone(),
                    h.uid_alpha,
                    1_750_000_220,
                    "photo 220",
                    false,
                    Some(syn_photo_media(603)),
                ),
                syn_message(
                    210,
                    ch_tl.clone(),
                    h.uid_alpha,
                    1_750_000_210,
                    "photo 210",
                    false,
                    Some(syn_photo_media(604)),
                ),
            ],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(h.user_alpha.clone())],
        }),
    );
    // 3. In-chat search with empty query + from_peer filter
    h.fake.enqueue_ok(
        tl_fns::messages::Search::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![syn_message(
                305,
                ch_tl,
                h.uid_alpha,
                1_750_000_305,
                "from alpha only",
                false,
                None,
            )],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(h.user_alpha.clone())],
        }),
    );
    // 4. Global search with empty query + date range only
    h.fake.enqueue_ok(
        tl_fns::messages::SearchGlobal::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![syn_message(
                310,
                tl_enums::Peer::User(tl_types::PeerUser {
                    user_id: h.uid_alpha,
                }),
                h.uid_alpha,
                1_750_000_310,
                "date range hit",
                false,
                None,
            )],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(h.user_alpha.clone())],
        }),
    );

    let older = h
        .live
        .load_shared_media_page(
            ch_peer,
            SharedMediaTab::PhotosVideos,
            "",
            MsgId(0),
            LoadDirection::Before,
            2,
        )
        .expect("older shared media");
    assert_eq!(older.items.len(), 2);
    assert_eq!(older.oldest_id, Some(MsgId(190)));
    assert_eq!(older.newest_id, Some(MsgId(200)));
    assert!(older.has_more_older);
    assert!(!older.has_more_newer);

    let newer = h
        .live
        .load_shared_media_page(
            ch_peer,
            SharedMediaTab::PhotosVideos,
            "",
            MsgId(200),
            LoadDirection::After,
            2,
        )
        .expect("newer shared media");
    assert_eq!(newer.items.len(), 2);
    assert_eq!(newer.newest_id, Some(MsgId(220)));
    assert!(newer.has_more_newer);

    let mut from_params = InChatSearchParams::new(ch_peer, "", SharedMediaFilter::Empty);
    from_params.from_peer = Some(h.peer_alpha);
    let from_page = h
        .live
        .search_in_chat(&from_params)
        .expect("from_peer search");
    assert_eq!(from_page.messages.len(), 1);
    assert_eq!(from_page.messages[0].id, MsgId(305));

    let date_params = GlobalSearchParams {
        query: String::new(),
        min_date: 1_750_000_300,
        max_date: 1_750_000_400,
        ..GlobalSearchParams::default()
    };
    let date_page = h
        .live
        .search_global_page(&date_params, GlobalSearchCursor::default())
        .expect("date-only global search");
    assert_eq!(date_page.messages.len(), 1);
    assert_eq!(date_page.messages[0].msg_id, MsgId(310));

    let search_reqs = h.fake.recorded_requests::<tl_fns::messages::Search>();
    assert_eq!(search_reqs.len(), 3);
    assert_eq!(
        (search_reqs[1].offset_id, search_reqs[1].add_offset),
        (201, -2)
    );
    assert!(matches!(
        search_reqs[2].from_id,
        Some(tl_enums::InputPeer::User(ref u)) if u.user_id == h.uid_alpha
    ));

    h.shutdown();
}

#[test]
fn top_peers_disabled_and_report_nested_choose_option_flow() {
    let mut h = Harness::start();

    h.fake.enqueue_ok(
        tl_fns::contacts::GetTopPeers::CONSTRUCTOR_ID,
        &tl_enums::contacts::TopPeers::Disabled,
    );
    assert!(h.live.top_peers().is_empty());
    // Subsequent call stays empty without re-requesting.
    assert!(h.live.top_peers().is_empty());
    assert_eq!(
        h.fake
            .recorded_requests::<tl_fns::contacts::GetTopPeers>()
            .len(),
        1
    );

    // Multi-level ChooseOption -> ChooseOption -> AddComment(optional=true) -> Reported
    h.fake.enqueue_ok(
        tl_fns::messages::Report::CONSTRUCTOR_ID,
        &tl_enums::ReportResult::ChooseOption(tl_types::ReportResultChooseOption {
            title: "Root".to_owned(),
            options: vec![tl_enums::MessageReportOption::MessageReportOption(
                tl_types::MessageReportOption {
                    text: "Spam".to_owned(),
                    option: b"lvl1_spam".to_vec(),
                },
            )],
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::Report::CONSTRUCTOR_ID,
        &tl_enums::ReportResult::ChooseOption(tl_types::ReportResultChooseOption {
            title: "Subcategory".to_owned(),
            options: vec![tl_enums::MessageReportOption::MessageReportOption(
                tl_types::MessageReportOption {
                    text: "Unsolicited ads".to_owned(),
                    option: b"lvl2_ads".to_vec(),
                },
            )],
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::Report::CONSTRUCTOR_ID,
        &tl_enums::ReportResult::AddComment(tl_types::ReportResultAddComment {
            optional: true,
            option: b"lvl3_opt".to_vec(),
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::Report::CONSTRUCTOR_ID,
        &tl_enums::ReportResult::Reported,
    );

    HistoryBackend::report_messages(
        &mut h.live,
        h.peer_alpha,
        &[MsgId(10)],
        MessageReportReason::Spam,
        "",
    )
    .expect("nested report flow with optional empty comment");

    let reqs = h.fake.recorded_requests::<tl_fns::messages::Report>();
    assert_eq!(reqs.len(), 4);
    assert_eq!(reqs[0].option, Vec::<u8>::new());
    assert_eq!(reqs[1].option, b"lvl1_spam".to_vec());
    assert_eq!(reqs[2].option, b"lvl2_ads".to_vec());
    assert_eq!(reqs[3].option, b"lvl3_opt".to_vec());
    assert_eq!(reqs[3].message, "");

    h.shutdown();
}
