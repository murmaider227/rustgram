//! Integration and unit tests for WP-25 (Search, Media & Polish) using `FakeRpcClient`.
//!
//! Covers:
//! - Jump-to-date Calendar (`messages.getSearchResultsCalendar`) & `CalendarBoxState`;
//! - Public Posts search (`channels.checkSearchPostsFlood` + `channels.searchPosts`) via `SearchTab::PublicPosts`;
//! - Global Media tab (`Info::GlobalMedia` via `messages.searchGlobal`) & `InfoWrapWidget::open_global_media`;
//! - Automatic `FILE_REFERENCE_EXPIRED` refresh and download retry (`live/media.rs`) across
//!   private messages, channel messages, sticker sets, and user profile photos;
//! - Live `HistoryBackend::contacts` (`contacts.getContacts`), `nearby_venues` (inline venue bot
//!   `messages.getInlineBotResults` + `contacts.getLocated` fallback), and `EditCaptionBox`
//!   (`messages.editMessage` with `invert_media`, `spoiler`, and replacement media upload).
//!
//! All tests use synthetic IDs/names and in-process `FakeRpcClient` only.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::convert::apply_tl_message;
use tg_api::rpc::RpcError;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{
    Channel, ChannelFlags, ChannelId, FullMsgId as DataFullMsgId, MessageContent, MessageMedia,
    MsgId as DataMsgId, PeerId as DataPeerId, UserId as DataUserId,
};
use tg_media::transfer::{FileOrigin, MediaInvoker, TransferLocation};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::boxes::calendar::{CalendarBoxAction, CalendarBoxState, month_start_unix_utc};
use tg_app::dialogs::backend::{DialogsBackend, SearchRequest, SearchTab};
use tg_app::history::backend::{HistoryBackend, HistoryMedia, PhotoAttachment};
use tg_app::history::edit_caption::{
    EditCaptionBoxState, EditMediaCaptionRequest, ReplacementMediaFile, media_supports_edit_caption,
};
use tg_app::info::backend::{InfoBackend, MediaItem as InfoMediaItem, SharedMediaTab};
use tg_app::info::wrap_widget::{InfoSection, InfoWrapWidget};
use tg_app::live::LiveSession;
use tg_app::live::media::{
    TelegramMediaInvoker, download_file_bytes_with_refresh_async, is_file_reference_error,
    refresh_file_reference_async,
};
use tg_app::live::search_and_info::GlobalSearchCursor;
use tg_app::stand_in::types::{MsgId, PeerId};

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

// =============================================================================
// Synthetic TL builders & FakeRpcClient
// =============================================================================

fn syn_user(
    id: i64,
    first_name: &str,
    last_name: &str,
    username: &str,
    phone: Option<&str>,
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
        bot_inline_geo: is_bot,
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
        access_hash: Some(id * 1000 + 23),
        first_name: Some(first_name.to_owned()),
        last_name: (!last_name.is_empty()).then(|| last_name.to_owned()),
        username: (!username.is_empty()).then(|| username.to_owned()),
        phone: phone.map(str::to_owned),
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

fn syn_channel(id: i64, title: &str, username: &str, megagroup: bool) -> tl_types::Channel {
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
        access_hash: Some(id * 100 + 7),
        title: title.to_owned(),
        username: (!username.is_empty()).then(|| username.to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_750_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(128),
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

fn syn_message(
    id: i32,
    peer: tl_enums::Peer,
    from_uid: i64,
    date: i32,
    text: &str,
    media: Option<tl_enums::MessageMedia>,
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

fn syn_photo(photo_id: i64, file_ref: Vec<u8>) -> tl_types::Photo {
    tl_types::Photo {
        has_stickers: false,
        id: photo_id,
        access_hash: photo_id * 13,
        file_reference: file_ref,
        date: 1_750_000_000,
        sizes: vec![tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
            r#type: "x".to_owned(),
            w: 800,
            h: 600,
            size: 16_384,
        })],
        video_sizes: None,
        dc_id: 2,
    }
}

fn syn_photo_media(photo_id: i64, file_ref: Vec<u8>) -> tl_enums::MessageMedia {
    tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
        live_photo: false,
        spoiler: false,
        photo: Some(tl_enums::Photo::Photo(syn_photo(photo_id, file_ref))),
        ttl_seconds: None,
        video: None,
    })
}

fn syn_doc_media(doc_id: i64, file_name: &str, file_ref: Vec<u8>) -> tl_enums::MessageMedia {
    tl_enums::MessageMedia::Document(tl_types::MessageMediaDocument {
        nopremium: false,
        spoiler: false,
        video: false,
        round: false,
        voice: false,
        document: Some(tl_enums::Document::Document(tl_types::Document {
            id: doc_id,
            access_hash: doc_id * 11,
            file_reference: file_ref,
            date: 1_750_000_000,
            mime_type: "application/pdf".to_owned(),
            size: 4096,
            thumbs: None,
            video_thumbs: None,
            dc_id: 2,
            attributes: vec![tl_enums::DocumentAttribute::Filename(
                tl_types::DocumentAttributeFilename {
                    file_name: file_name.to_owned(),
                },
            )],
        })),
        alt_documents: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
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

struct Harness {
    peer_alpha: PeerId,
    uid_alpha: i64,
    fake: FakeRpcClient,
    runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    live: LiveSession,
}

impl Harness {
    fn start() -> Self {
        let self_uid = 900_001i64;
        let uid_alpha = 900_002i64;
        let self_user = syn_user(self_uid, "SelfUnit", "", "self_wp25", None, true, false);
        let user_alpha = syn_user(
            uid_alpha,
            "Alpha",
            "Tester",
            "alpha_wp25",
            Some("+9996610002"),
            false,
            false,
        );
        let top_msg = syn_message(
            10,
            tl_enums::Peer::User(tl_types::PeerUser { user_id: uid_alpha }),
            uid_alpha,
            1_762_000_000,
            "Hello WP-25",
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
                pts: 10,
                qts: 0,
                date: 1_762_000_000,
                seq: 1,
                unread_count: 0,
            }),
        );
        fake.enqueue_ok(
            tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
            &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
                date: 1_762_000_001,
                seq: 1,
            }),
        );
        fake.enqueue_ok(
            tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
            &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
                dialogs: vec![syn_dialog(uid_alpha, 10)],
                messages: vec![top_msg],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(user_alpha)],
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
                    date: 1_762_000_001,
                    seq: 1,
                    unread_count: 0,
                }),
            }),
        );

        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId::from_tl(self_uid),
                tg_app::live::ErasedRpcClient::new(fake.clone()),
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
            fake,
            runtime,
            live,
        }
    }
}

// =============================================================================
// 1. Jump-to-date Calendar (`messages.getSearchResultsCalendar`)
// =============================================================================

#[test]
fn calendar_get_search_results_multi_page_and_box_jump() {
    let h = Harness::start();

    // November 2026: day 20 (page 1) and day 5 (page 2).
    let nov_20 = month_start_unix_utc(2026, 11) + 19 * 86_400 + 3600;
    let nov_05 = month_start_unix_utc(2026, 11) + 4 * 86_400 + 3600;

    h.fake.enqueue_ok(
        tl_fns::messages::GetSearchResultsCalendar::CONSTRUCTOR_ID,
        &tl_enums::messages::SearchResultsCalendar::SearchResultsCalendar(
            tl_types::messages::SearchResultsCalendar {
                inexact: false,
                count: 5,
                min_date: nov_05,
                min_msg_id: 200,
                offset_id_offset: None,
                periods: vec![
                    tl_enums::SearchResultsCalendarPeriod::SearchResultsCalendarPeriod(
                        tl_types::SearchResultsCalendarPeriod {
                            date: nov_20,
                            min_msg_id: 410,
                            max_msg_id: 415,
                            count: 3,
                        },
                    ),
                ],
                messages: vec![syn_message(
                    415,
                    tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: h.uid_alpha,
                    }),
                    h.uid_alpha,
                    nov_20,
                    "Nov 20 msg",
                    None,
                )],
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::GetSearchResultsCalendar::CONSTRUCTOR_ID,
        &tl_enums::messages::SearchResultsCalendar::SearchResultsCalendar(
            tl_types::messages::SearchResultsCalendar {
                inexact: false,
                count: 5,
                min_date: nov_05,
                min_msg_id: 200,
                offset_id_offset: None,
                periods: vec![
                    tl_enums::SearchResultsCalendarPeriod::SearchResultsCalendarPeriod(
                        tl_types::SearchResultsCalendarPeriod {
                            date: nov_05,
                            min_msg_id: 200,
                            max_msg_id: 202,
                            count: 2,
                        },
                    ),
                ],
                messages: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );

    let days = HistoryBackend::search_results_calendar(&h.live, h.peer_alpha, 2026, 11)
        .expect("calendar fetch");
    assert_eq!(days.len(), 2);
    assert_eq!(days[0].day, 5);
    assert_eq!(days[0].max_msg_id, MsgId(202));
    assert_eq!(days[1].day, 20);
    assert_eq!(days[1].max_msg_id, MsgId(415));
    assert_eq!(days[1].count, 3);

    // Verify 2 paginated RPCs were issued with shrinking offset_date / offset_id.
    let reqs: Vec<tl_fns::messages::GetSearchResultsCalendar> = h.fake.recorded_requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].offset_id, 0);
    assert_eq!(reqs[1].offset_id, 410);
    assert_eq!(reqs[1].offset_date, nov_20 - 1);

    // Verify cache hit on second call (no extra RPC).
    let cached = HistoryBackend::search_results_calendar(&h.live, h.peer_alpha, 2026, 11)
        .expect("cached calendar");
    assert_eq!(cached.len(), 2);
    assert_eq!(
        h.fake
            .recorded_requests::<tl_fns::messages::GetSearchResultsCalendar>()
            .len(),
        2
    );

    // Verify CalendarBoxState stores month data and resolves JumpToDate with exact max_msg_id.
    let mut cal_box = CalendarBoxState::new(h.peer_alpha, "Alpha Tester", nov_20);
    assert!(cal_box.needs_load_current_month());
    cal_box.set_month_days(2026, 11, days);
    assert!(!cal_box.needs_load_current_month());
    cal_box.selected_day = Some((2026, 11, 20));
    assert_eq!(
        cal_box.build_jump_action(),
        Some(CalendarBoxAction::JumpToDate {
            peer_id: h.peer_alpha,
            msg_id: MsgId(415),
            date_unix: nov_20,
        })
    );
}

// =============================================================================
// 2. Public Posts Search (`channels.checkSearchPostsFlood` + `channels.searchPosts`)
// =============================================================================

#[test]
fn public_posts_search_query_hashtag_pagination_and_flood() {
    let h = Harness::start();
    let ch_id = 77_100i64;
    let ch = syn_channel(ch_id, "Public Tech News", "public_tech_news", false);
    let ch_peer = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: ch_id });

    // 1. Empty-query checkSearchPostsFlood via DialogsBackend::search_dialogs(SearchTab::PublicPosts).
    h.fake.enqueue_ok(
        tl_fns::channels::CheckSearchPostsFlood::CONSTRUCTOR_ID,
        &tl_enums::SearchPostsFlood::SearchPostsFlood(tl_types::SearchPostsFlood {
            query_is_free: true,
            total_daily: 10,
            remains: 7,
            wait_till: None,
            stars_amount: 10,
        }),
    );

    let empty_res = DialogsBackend::search_dialogs(
        &h.live,
        &SearchRequest {
            query: String::new(),
            tab: SearchTab::PublicPosts,
            ..SearchRequest::default()
        },
    );
    let flood = empty_res.posts_flood.expect("flood info present");
    assert!(flood.query_is_free);
    assert_eq!(flood.total_daily, 10);
    assert_eq!(flood.remains, 7);

    // 2. Keyword query with pagination (`channels.searchPosts` with `query: Some("rustlang")`).
    h.fake.enqueue_ok(
        tl_fns::channels::SearchPosts::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Slice(tl_types::messages::MessagesSlice {
            inexact: false,
            count: 2,
            next_rate: Some(1_762_000_100),
            offset_id_offset: None,
            search_flood: Some(tl_enums::SearchPostsFlood::SearchPostsFlood(
                tl_types::SearchPostsFlood {
                    query_is_free: true,
                    total_daily: 10,
                    remains: 6,
                    wait_till: None,
                    stars_amount: 10,
                },
            )),
            messages: vec![syn_message(
                901,
                ch_peer.clone(),
                h.uid_alpha,
                1_762_000_200,
                "Rustlang release highlights",
                None,
            )],
            topics: Vec::new(),
            chats: vec![tl_enums::Chat::Channel(ch.clone())],
            users: Vec::new(),
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::channels::SearchPosts::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![syn_message(
                900,
                ch_peer.clone(),
                h.uid_alpha,
                1_762_000_100,
                "Earlier rustlang post",
                None,
            )],
            topics: Vec::new(),
            chats: vec![tl_enums::Chat::Channel(ch.clone())],
            users: Vec::new(),
        }),
    );

    let res = DialogsBackend::search_dialogs(
        &h.live,
        &SearchRequest {
            query: "rustlang".to_owned(),
            tab: SearchTab::PublicPosts,
            messages_offset: 0,
            messages_limit: 20,
            ..SearchRequest::default()
        },
    );
    assert_eq!(res.messages.len(), 2);
    assert_eq!(res.messages[0].msg_id, MsgId(901));
    assert_eq!(res.messages[1].msg_id, MsgId(900));
    assert_eq!(res.posts_flood.as_ref().map(|f| f.remains), Some(6));

    // 3. Hashtag query (`#rustgram`) populates `hashtag` instead of `query`.
    h.fake.enqueue_ok(
        tl_fns::channels::SearchPosts::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![syn_message(
                905,
                ch_peer,
                h.uid_alpha,
                1_762_000_300,
                "#rustgram update shipped",
                None,
            )],
            topics: Vec::new(),
            chats: vec![tl_enums::Chat::Channel(ch)],
            users: Vec::new(),
        }),
    );

    let tag_page = h
        .live
        .search_public_posts_page("#rustgram", GlobalSearchCursor::default(), 20, None)
        .expect("hashtag searchPosts");
    assert_eq!(tag_page.messages.len(), 1);
    assert_eq!(tag_page.messages[0].msg_id, MsgId(905));

    let reqs: Vec<tl_fns::channels::SearchPosts> = h.fake.recorded_requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[0].query.as_deref(), Some("rustlang"));
    assert_eq!(reqs[0].hashtag, None);
    assert_eq!(reqs[1].offset_rate, 1_762_000_100);
    assert_eq!(reqs[1].offset_id, 901);
    assert_eq!(reqs[2].hashtag.as_deref(), Some("rustgram"));
    assert_eq!(reqs[2].query, None);
}

// =============================================================================
// 3. Global Media Tab (`Info::GlobalMedia` via `messages.searchGlobal`)
// =============================================================================

#[test]
fn global_media_tab_search_global_and_section_ui() {
    let h = Harness::start();
    let ch_id = 77_200i64;
    let ch = syn_channel(ch_id, "Design Gallery", "design_gallery", false);

    h.fake.enqueue_ok(
        tl_fns::messages::SearchGlobal::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![
                syn_message(
                    510,
                    tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: h.uid_alpha,
                    }),
                    h.uid_alpha,
                    1_762_000_500,
                    "Sunset photo",
                    Some(syn_photo_media(55_001, vec![9, 9])),
                ),
                syn_message(
                    511,
                    tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: ch_id }),
                    h.uid_alpha,
                    1_762_000_400,
                    "Gallery shot",
                    Some(syn_photo_media(55_002, vec![8, 8])),
                ),
            ],
            topics: Vec::new(),
            chats: vec![tl_enums::Chat::Channel(ch.clone())],
            users: Vec::new(),
        }),
    );

    let page = InfoBackend::global_media(&h.live, SharedMediaTab::PhotosVideos, "", 20);
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.entries[0].peer_id, h.peer_alpha);
    assert!(matches!(
        page.entries[0].item,
        InfoMediaItem::Photo { id: 510, .. }
    ));
    assert_eq!(page.entries[1].peer_title, "Design Gallery");

    // Verify `messages.searchGlobal` received `InputMessagesFilterPhotoVideo`.
    let reqs: Vec<tl_fns::messages::SearchGlobal> = h.fake.recorded_requests();
    assert_eq!(reqs.len(), 1);
    assert!(matches!(
        reqs[0].filter,
        tl_enums::MessagesFilter::InputMessagesFilterPhotoVideo
    ));

    // Also test Files tab (`InputMessagesFilterDocument`) and InfoWrapWidget::open_global_media.
    h.fake.enqueue_ok(
        tl_fns::messages::SearchGlobal::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![syn_message(
                610,
                tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: ch_id }),
                h.uid_alpha,
                1_762_000_600,
                "",
                Some(syn_doc_media(66_001, "spec_v3.pdf", vec![7, 7])),
            )],
            topics: Vec::new(),
            chats: vec![tl_enums::Chat::Channel(ch)],
            users: Vec::new(),
        }),
    );
    let files_page = InfoBackend::global_media(&h.live, SharedMediaTab::Files, "spec", 20);
    assert_eq!(files_page.entries.len(), 1);
    assert!(matches!(
        &files_page.entries[0].item,
        InfoMediaItem::File { filename, .. } if filename == "spec_v3.pdf"
    ));

    let mut wrap = InfoWrapWidget::for_peer(h.peer_alpha);
    wrap.open_global_media(SharedMediaTab::PhotosVideos);
    assert_eq!(
        wrap.current_section(),
        InfoSection::GlobalMedia {
            peer_id: h.peer_alpha,
            tab: SharedMediaTab::PhotosVideos,
        }
    );
}

// =============================================================================
// 4. File Reference Refresh (`FILE_REFERENCE_EXPIRED` in `live/media.rs`)
// =============================================================================

#[test]
fn file_reference_refresh_message_origin_and_download_retry() {
    let h = Harness::start();
    let photo_id = 880_001i64;
    let stale_ref = vec![0x01, 0x02, 0x03];
    let fresh_ref = vec![0xAA, 0xBB, 0xCC, 0xDD];
    let msg_id = 77i32;

    // Seed Session with a message whose photo has a stale file_reference.
    let data_peer = DataPeerId::from_user(DataUserId::from_tl(h.uid_alpha));
    let full_id = DataFullMsgId::new(data_peer, DataMsgId(i64::from(msg_id)));
    h.runtime.tokio_handle().block_on(async {
        let mut s = h.runtime.session().write().await;
        let seeded = syn_message(
            msg_id,
            tl_enums::Peer::User(tl_types::PeerUser {
                user_id: h.uid_alpha,
            }),
            h.uid_alpha,
            1_762_000_000,
            "",
            Some(syn_photo_media(photo_id, stale_ref.clone())),
        );
        apply_tl_message(&mut s, &seeded);
    });

    // 1st `upload.getFile` -> FILE_REFERENCE_EXPIRED.
    let expired_err = InvocationError::Rpc(RpcError {
        code: 400,
        name: "FILE_REFERENCE_EXPIRED".to_owned(),
        value: None,
    });
    assert!(is_file_reference_error(&expired_err));
    h.fake
        .enqueue_err(tl_fns::upload::GetFile::CONSTRUCTOR_ID, expired_err);

    // Refresh RPC: `messages.getMessages` returns the message with the fresh file_reference.
    h.fake.enqueue_ok(
        tl_fns::messages::GetMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![syn_message(
                msg_id,
                tl_enums::Peer::User(tl_types::PeerUser {
                    user_id: h.uid_alpha,
                }),
                h.uid_alpha,
                1_762_000_000,
                "",
                Some(syn_photo_media(photo_id, fresh_ref.clone())),
            )],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    // 2nd `upload.getFile` succeeds with payload bytes.
    let expected_bytes = b"FRESH_PHOTO_BYTES_OK".to_vec();
    h.fake.enqueue_ok(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileJpeg,
            mtime: 1_762_000_000,
            bytes: expected_bytes.clone(),
        }),
    );

    let initial_loc =
        tl_enums::InputFileLocation::InputPhotoFileLocation(tl_types::InputPhotoFileLocation {
            id: photo_id,
            access_hash: photo_id * 13,
            file_reference: stale_ref.clone(),
            thumb_size: "x".to_owned(),
        });
    let origin = FileOrigin {
        peer_id: Some(data_peer.0),
        msg_id: Some(msg_id),
        sticker_set_id: None,
    };

    let downloaded = h
        .runtime
        .tokio_handle()
        .block_on(download_file_bytes_with_refresh_async(
            &h.fake,
            h.runtime.session(),
            origin,
            2,
            initial_loc,
            Some(expected_bytes.len() as u64),
            |_, _| {},
        ));
    assert_eq!(downloaded.expect("download after refresh"), expected_bytes);

    // Verify 2 `upload.getFile` calls: 1st with stale_ref, 2nd with fresh_ref.
    let get_files: Vec<tl_fns::upload::GetFile> = h.fake.recorded_requests();
    assert_eq!(get_files.len(), 2);
    match &get_files[0].location {
        tl_enums::InputFileLocation::InputPhotoFileLocation(p) => {
            assert_eq!(p.file_reference, stale_ref);
        }
        other => panic!("unexpected location: {other:?}"),
    }
    match &get_files[1].location {
        tl_enums::InputFileLocation::InputPhotoFileLocation(p) => {
            assert_eq!(p.file_reference, fresh_ref);
        }
        other => panic!("unexpected location: {other:?}"),
    }

    // Verify `Session` was also updated with the fresh file_reference.
    let stored_ref = h.runtime.tokio_handle().block_on(async {
        let s = h.runtime.session().read().await;
        match &s.message(full_id).expect("msg").content {
            MessageContent::Regular {
                media: MessageMedia::Photo(p),
                ..
            } => p.file_reference.clone(),
            other => panic!("unexpected content: {other:?}"),
        }
    });
    assert_eq!(stored_ref, fresh_ref);
}

#[test]
fn file_reference_refresh_channel_sticker_set_and_user_photo_origins() {
    let h = Harness::start();

    // Seed a channel peer so `channels.getMessages` can build InputChannel.
    let ch_id = 88_900i64;
    let ch_data_peer = DataPeerId::from_channel(ChannelId::from_tl(ch_id));
    h.runtime.tokio_handle().block_on(async {
        let mut s = h.runtime.session().write().await;
        let mut ch = Channel::new(ChannelId::from_tl(ch_id));
        ch.set_name("Media Channel", "");
        ch.access_hash = (ch_id * 100 + 7) as u64;
        ch.flags = ChannelFlags::BROADCAST;
        s.upsert_channel(ch, tg_data::PeerUpdateFlags::NAME);
    });

    // A. Channel message origin -> `channels.getMessages`.
    let ch_doc_ref = vec![0xDE, 0xAD, 0xBE, 0xEF];
    h.fake.enqueue_ok(
        tl_fns::channels::GetMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 5,
            count: 1,
            offset_id_offset: None,
            messages: vec![syn_message(
                42,
                tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: ch_id }),
                h.uid_alpha,
                1_762_000_000,
                "",
                Some(syn_doc_media(700_001, "clip.pdf", ch_doc_ref.clone())),
            )],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let ch_origin = FileOrigin {
        peer_id: Some(ch_data_peer.0),
        msg_id: Some(42),
        sticker_set_id: None,
    };
    let ch_loc = TransferLocation::Document {
        id: 700_001,
        access_hash: 700_001 * 11,
        file_reference: vec![0x00],
        thumb_size: String::new(),
    };
    let ch_ref = h
        .runtime
        .tokio_handle()
        .block_on(refresh_file_reference_async(
            &h.fake,
            h.runtime.session(),
            &ch_origin,
            &ch_loc,
        ))
        .expect("channel message refresh");
    assert_eq!(ch_ref, ch_doc_ref);

    // B. Sticker set origin -> `messages.getStickerSet`.
    let sticker_ref = vec![0xCA, 0xFE, 0x01];
    h.fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
                archived: false,
                official: false,
                masks: false,
                emojis: false,
                text_color: false,
                channel_emoji_status: false,
                creator: false,
                installed_date: None,
                id: 500_001,
                access_hash: 500_009,
                title: "Unit Pack".to_owned(),
                short_name: "unit_pack".to_owned(),
                thumbs: None,
                thumb_dc_id: None,
                thumb_version: None,
                thumb_document_id: None,
                count: 1,
                hash: 0,
            }),
            packs: Vec::new(),
            keywords: Vec::new(),
            documents: vec![tl_enums::Document::Document(tl_types::Document {
                id: 700_002,
                access_hash: 700_002 * 11,
                file_reference: sticker_ref.clone(),
                date: 1_762_000_000,
                mime_type: "application/x-tgsticker".to_owned(),
                size: 1024,
                thumbs: None,
                video_thumbs: None,
                dc_id: 2,
                attributes: Vec::new(),
            })],
        }),
    );
    let set_origin = FileOrigin {
        peer_id: None,
        msg_id: None,
        sticker_set_id: Some(500_001),
    };
    let sticker_loc = TransferLocation::Document {
        id: 700_002,
        access_hash: 700_002 * 11,
        file_reference: vec![0x00],
        thumb_size: String::new(),
    };
    let set_ref = h
        .runtime
        .tokio_handle()
        .block_on(refresh_file_reference_async(
            &h.fake,
            h.runtime.session(),
            &set_origin,
            &sticker_loc,
        ))
        .expect("sticker set refresh");
    assert_eq!(set_ref, sticker_ref);

    // C. User photo origin -> `photos.getUserPhotos`.
    let user_photo_ref = vec![0x55, 0x66, 0x77];
    let alpha_data_peer = DataPeerId::from_user(DataUserId::from_tl(h.uid_alpha));
    h.fake.enqueue_ok(
        tl_fns::photos::GetUserPhotos::CONSTRUCTOR_ID,
        &tl_enums::photos::Photos::Photos(tl_types::photos::Photos {
            photos: vec![tl_enums::Photo::Photo(syn_photo(
                700_003,
                user_photo_ref.clone(),
            ))],
            users: Vec::new(),
        }),
    );
    let photo_origin = FileOrigin {
        peer_id: Some(alpha_data_peer.0),
        msg_id: None,
        sticker_set_id: None,
    };
    let photo_loc = TransferLocation::Photo {
        id: 700_003,
        access_hash: 700_003 * 13,
        file_reference: vec![0x00],
        thumb_size: "x".to_owned(),
    };
    let photo_ref = h
        .runtime
        .tokio_handle()
        .block_on(refresh_file_reference_async(
            &h.fake,
            h.runtime.session(),
            &photo_origin,
            &photo_loc,
        ))
        .expect("user photo refresh");
    assert_eq!(photo_ref, user_photo_ref);

    // D. `TelegramMediaInvoker::refresh_file_reference` trait path.
    let invoker_ref = vec![0x11, 0x22, 0x33, 0x44];
    h.fake.enqueue_ok(
        tl_fns::photos::GetUserPhotos::CONSTRUCTOR_ID,
        &tl_enums::photos::Photos::Photos(tl_types::photos::Photos {
            photos: vec![tl_enums::Photo::Photo(syn_photo(
                700_003,
                invoker_ref.clone(),
            ))],
            users: Vec::new(),
        }),
    );
    let invoker = TelegramMediaInvoker::new(h.fake.clone(), Arc::clone(h.runtime.session()));
    let refreshed = h
        .runtime
        .tokio_handle()
        .block_on(invoker.refresh_file_reference(photo_origin, photo_loc))
        .expect("invoker refresh");
    assert_eq!(refreshed, invoker_ref);
}

// =============================================================================
// 5. History Extras: Live Contacts, Nearby Venues, Edit-Caption Box
// =============================================================================

#[test]
fn history_contacts_live_fetches_get_contacts() {
    let h = Harness::start();
    let uid_b = 900_101i64;
    let uid_c = 900_102i64;

    h.fake.enqueue_ok(
        tl_fns::contacts::GetContacts::CONSTRUCTOR_ID,
        &tl_enums::contacts::Contacts::Contacts(tl_types::contacts::Contacts {
            contacts: vec![
                tl_enums::Contact::Contact(tl_types::Contact {
                    user_id: uid_b,
                    mutual: true,
                }),
                tl_enums::Contact::Contact(tl_types::Contact {
                    user_id: uid_c,
                    mutual: false,
                }),
            ],
            saved_count: 2,
            users: vec![
                tl_enums::User::User(syn_user(
                    uid_b,
                    "Beta",
                    "Contact",
                    "beta_unit",
                    Some("+9996610101"),
                    false,
                    false,
                )),
                tl_enums::User::User(syn_user(
                    uid_c,
                    "Gamma",
                    "",
                    "gamma_unit",
                    Some("+9996610102"),
                    false,
                    false,
                )),
            ],
        }),
    );

    let contacts = HistoryBackend::contacts(&h.live);
    assert_eq!(contacts.len(), 2);
    assert_eq!(contacts[0].first_name, "Beta");
    assert_eq!(contacts[0].last_name, "Contact");
    assert_eq!(contacts[0].phone_number, "+9996610101");
    assert_eq!(contacts[1].first_name, "Gamma");

    // Second call hits cache without extra `contacts.getContacts` RPC.
    let contacts2 = HistoryBackend::contacts(&h.live);
    assert_eq!(contacts2.len(), 2);
    assert_eq!(
        h.fake
            .recorded_requests::<tl_fns::contacts::GetContacts>()
            .len(),
        1
    );
}

#[test]
fn history_nearby_venues_inline_bot_and_get_located_fallback() {
    let h = Harness::start();
    let bot_uid = 900_555i64;
    let venue_bot = syn_user(bot_uid, "VenueBot", "", "foursquare", None, false, true);

    // A. Primary tdesktop flow: `contacts.resolveUsername("foursquare")` + `messages.getInlineBotResults`.
    h.fake.enqueue_ok(
        tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID,
        &tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: bot_uid }),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(venue_bot)],
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::GetInlineBotResults::CONSTRUCTOR_ID,
        &tl_enums::messages::BotResults::BotResults(tl_types::messages::BotResults {
            gallery: false,
            query_id: 12345,
            next_offset: None,
            switch_pm: None,
            switch_webview: None,
            results: vec![tl_enums::BotInlineResult::BotInlineResult(
                tl_types::BotInlineResult {
                    id: "v1".to_owned(),
                    r#type: "venue".to_owned(),
                    title: Some("Central Roastery".to_owned()),
                    description: Some("100 Bean Blvd".to_owned()),
                    url: None,
                    thumb: None,
                    content: None,
                    send_message: tl_enums::BotInlineMessage::MediaVenue(
                        tl_types::BotInlineMessageMediaVenue {
                            geo: tl_enums::GeoPoint::GeoPoint(tl_types::GeoPoint {
                                long: -122.4194,
                                lat: 37.7749,
                                access_hash: 1,
                                accuracy_radius: None,
                            }),
                            title: "Central Roastery".to_owned(),
                            address: "100 Bean Blvd".to_owned(),
                            provider: "foursquare".to_owned(),
                            venue_id: "fsq_001".to_owned(),
                            venue_type: "Coffee Shop".to_owned(),
                            reply_markup: None,
                        },
                    ),
                },
            )],
            cache_time: 60,
            users: Vec::new(),
        }),
    );

    let venues = HistoryBackend::nearby_venues_for_coords(&h.live, 37.7749, -122.4194, "coffee");
    assert_eq!(venues.len(), 1);
    assert_eq!(venues[0].id, "fsq_001");
    assert_eq!(venues[0].title, "Central Roastery");
    assert_eq!(venues[0].address, "100 Bean Blvd");
    assert_eq!(venues[0].category, "Coffee Shop");

    // B. Fallback flow when inline bot returns empty/error: `contacts.getLocated`.
    let nearby_ch_id = 77_888i64;
    let nearby_ch = syn_channel(nearby_ch_id, "Downtown Plaza Group", "plaza_group", true);
    h.fake.enqueue_err(
        tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError {
            code: 400,
            name: "USERNAME_NOT_OCCUPIED".to_owned(),
            value: None,
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::contacts::GetLocated::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::PeerLocated(tl_types::UpdatePeerLocated {
                peers: vec![tl_enums::PeerLocated::PeerLocated(tl_types::PeerLocated {
                    peer: tl_enums::Peer::Channel(tl_types::PeerChannel {
                        channel_id: nearby_ch_id,
                    }),
                    expires: i32::MAX,
                    distance: 250,
                })],
            })],
            users: Vec::new(),
            chats: vec![tl_enums::Chat::Channel(nearby_ch)],
            date: 1_762_000_000,
            seq: 2,
        }),
    );

    let fallback_venues = HistoryBackend::nearby_venues_for_coords(&h.live, 48.8566, 2.3522, "");
    assert_eq!(fallback_venues.len(), 1);
    assert_eq!(fallback_venues[0].title, "Downtown Plaza Group");
    assert_eq!(fallback_venues[0].address, "250 m away");
    assert_eq!(fallback_venues[0].category, "Nearby Group");
}

#[test]
fn history_edit_caption_and_replace_media_live() {
    let mut h = Harness::start();
    let msg_id = 88i32;
    let photo_id = 880_050i64;

    // Seed Session with an existing photo message.
    h.runtime.tokio_handle().block_on(async {
        let mut s = h.runtime.session().write().await;
        let seeded = syn_message(
            msg_id,
            tl_enums::Peer::User(tl_types::PeerUser {
                user_id: h.uid_alpha,
            }),
            h.uid_alpha,
            1_762_000_000,
            "Original caption",
            Some(syn_photo_media(photo_id, vec![1, 2, 3])),
        );
        apply_tl_message(&mut s, &seeded);
    });

    // Verify EditCaptionBoxState limit validation (free vs Premium) and media support helper.
    let sample_media = HistoryMedia::Photo(PhotoAttachment {
        width: 800,
        height: 600,
        stripped_thumb: None,
        fallback_rgb: ([0, 0, 0], [10, 10, 10]),
        file_name: "orig.jpg".to_owned(),
        size_bytes: 1024,
    });
    assert!(media_supports_edit_caption(&sample_media));

    let hist_msg = HistoryBackend::load_slice_around(&h.live, h.peer_alpha, MsgId(msg_id), 1, 1)
        .messages
        .into_iter()
        .find(|m| m.id == MsgId(msg_id))
        .expect("seeded message");
    let mut box_state = EditCaptionBoxState::from_message(h.peer_alpha, &hist_msg, false);
    box_state.caption = "a".repeat(1500);
    assert!(box_state.build_request().is_err());
    box_state.is_premium = true;
    assert!(box_state.build_request().is_ok());

    // 1. Caption + invert_media + spoiler edit (reuses existing InputMediaPhoto with spoiler=true).
    let mut edited_caption_msg = syn_message(
        msg_id,
        tl_enums::Peer::User(tl_types::PeerUser {
            user_id: h.uid_alpha,
        }),
        h.uid_alpha,
        1_762_000_000,
        "Updated caption",
        Some(syn_photo_media(photo_id, vec![4, 5, 6])),
    );
    if let tl_enums::Message::Message(ref mut m) = edited_caption_msg {
        m.edit_date = Some(1_762_000_100);
    }
    h.fake.enqueue_ok(
        tl_fns::messages::EditMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::EditMessage(tl_types::UpdateEditMessage {
                message: edited_caption_msg,
                pts: 11,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_762_000_100,
            seq: 2,
        }),
    );

    let updated = HistoryBackend::edit_message_media(
        &mut h.live,
        h.peer_alpha,
        MsgId(msg_id),
        EditMediaCaptionRequest {
            caption: TextWithEntities::simple("Updated caption"),
            invert_media: true,
            spoiler: true,
            replacement_media: None,
        },
    )
    .expect("caption edit");
    assert_eq!(updated.text.text, "Updated caption");
    assert!(updated.edited);

    // 2. Replacement photo upload (`upload.saveFilePart` + `messages.editMessage` with `InputMediaUploadedPhoto`).
    h.fake
        .enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    let mut replaced_media_msg = syn_message(
        msg_id,
        tl_enums::Peer::User(tl_types::PeerUser {
            user_id: h.uid_alpha,
        }),
        h.uid_alpha,
        1_762_000_000,
        "Replaced photo caption",
        Some(syn_photo_media(photo_id + 1, vec![7, 8, 9])),
    );
    if let tl_enums::Message::Message(ref mut m) = replaced_media_msg {
        m.edit_date = Some(1_762_000_200);
    }
    h.fake.enqueue_ok(
        tl_fns::messages::EditMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::EditMessage(tl_types::UpdateEditMessage {
                message: replaced_media_msg,
                pts: 12,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_762_000_200,
            seq: 3,
        }),
    );

    let replaced = HistoryBackend::edit_message_media(
        &mut h.live,
        h.peer_alpha,
        MsgId(msg_id),
        EditMediaCaptionRequest {
            caption: TextWithEntities::simple("Replaced photo caption"),
            invert_media: false,
            spoiler: false,
            replacement_media: Some(ReplacementMediaFile::from_bytes(
                "replacement.jpg",
                vec![0xFF, 0xD8, 0xFF, 0xD9],
            )),
        },
    )
    .expect("replace media edit");
    assert_eq!(replaced.text.text, "Replaced photo caption");

    let edit_reqs: Vec<tl_fns::messages::EditMessage> = h.fake.recorded_requests();
    assert_eq!(edit_reqs.len(), 2);
    assert!(edit_reqs[0].invert_media);
    assert!(matches!(
        edit_reqs[0].media,
        Some(tl_enums::InputMedia::Photo(ref p)) if p.spoiler
    ));
    assert!(matches!(
        edit_reqs[1].media,
        Some(tl_enums::InputMedia::UploadedPhoto(_))
    ));
    assert_eq!(
        h.fake
            .recorded_requests::<tl_fns::upload::SaveFilePart>()
            .len(),
        1
    );
}
