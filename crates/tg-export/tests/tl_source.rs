//! Tests for TL -> export data parsing (`tl_parse`) and the TL-backed `ExportSource`
//! (`TlExportSource` over a fake `ExportRpc` server).
//!
//! Verifies:
//! - TL conversions (`parse_message`, `parse_media` incl. Dice/Story, `parse_user`, `parse_chat`,
//!   `parse_dialogs_info`, UTF-16 emoji entity slicing, `PhotoSizeProgressive`/`PhotoCachedSize`).
//! - Takeout session initialization flags (`account.initTakeoutSession`), `InvokeWithTakeout`
//!   wrapping on every takeout RPC, `InvokeWithMessagesRange` split wrapping, 100-message history
//!   pagination (`messages.getHistory` with `limit=100, add_offset=-100`), `CHANNEL_PRIVATE`
//!   fallback to `messages.search`, 128 KiB multi-part `upload.getFile` downloads (300 KiB file
//!   in 3 chunks), `FILE_REFERENCE_EXPIRED` auto-refresh & retry, `FILE_MIGRATE_4` redirect,
//!   `LoadedFileCache` deduplication, `TAKEOUT_FILE_EMPTY` handling, `messages.getRichMessage`
//!   hydration, `messages.getCustomEmojiDocuments`, and `account.finishTakeoutSession`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;
use tg_tl::{
    Cursor, Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_e,
    functions as tl_f, types as tl_t,
};

use tg_export::api_wrap::{
    ExportRpc, FILE_CHUNK_SIZE, MESSAGES_SLICE_LIMIT, RpcCallError, TlExportSource,
};
use tg_export::controller::{ExportController, ExportState};
use tg_export::data::{DialogType, MediaContent, TextPartType};
use tg_export::output::OutputFormat;
use tg_export::settings::{Environment, ExportTypes, MediaSettings, MediaTypes, Settings};
use tg_export::stand_in::{ChannelId, ChatId, InputFileLocation, InputPeer, PeerId, UserId};
use tg_export::tl_parse::{
    parse_chat, parse_dialogs_info, parse_media, parse_message, parse_peers_lists, parse_photo,
    parse_text_tl, parse_user,
};

static TL_TEST_COUNTER: AtomicUsize = AtomicUsize::new(1);

fn temp_dir(label: &str) -> PathBuf {
    let n = TL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "tg_export_tl_{}_{}_{}",
        label,
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn dir_with_slash(dir: &Path) -> String {
    let mut s = dir.to_string_lossy().replace('\\', "/");
    if !s.ends_with('/') {
        s.push('/');
    }
    s
}

fn tl_user(
    id: i64,
    first: &str,
    last: &str,
    username: Option<&str>,
    is_self: bool,
    is_bot: bool,
) -> tl_e::User {
    tl_e::User::User(tl_t::User {
        self_: is_self,
        contact: false,
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
        access_hash: Some(id * 17),
        first_name: Some(first.to_string()),
        last_name: Some(last.to_string()),
        username: username.map(ToString::to_string),
        phone: Some("15550101999".to_string()),
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

fn tl_channel(
    id: i64,
    title: &str,
    username: Option<&str>,
    broadcast: bool,
    megagroup: bool,
) -> tl_e::Chat {
    tl_e::Chat::Channel(tl_t::Channel {
        creator: false,
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
        access_hash: Some(id * 31),
        title: title.to_string(),
        username: username.map(ToString::to_string),
        photo: tl_e::ChatPhoto::Empty,
        date: 1_700_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: None,
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

#[allow(clippy::too_many_arguments)]
fn tl_message(
    id: i32,
    date: i32,
    peer_id: tl_e::Peer,
    from_id: Option<tl_e::Peer>,
    out: bool,
    text: &str,
    entities: Option<Vec<tl_e::MessageEntity>>,
    media: Option<tl_e::MessageMedia>,
    rich_message: Option<tl_e::RichMessage>,
) -> tl_e::Message {
    tl_e::Message::Message(tl_t::Message {
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
        from_id,
        from_boosts_applied: None,
        from_rank: None,
        peer_id,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date,
        message: text.to_string(),
        media,
        reply_markup: None,
        entities,
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
        rich_message,
    })
}

#[test]
fn tl_parse_conversions_unit() {
    // 1. UTF-16 entity slicing with 4-byte emoji (2 UTF-16 code units!)
    let text = "Hi \u{1F680} bold!";
    // "Hi " = 3 UTF-16 units; 🚀 = 2 UTF-16 units; " " = 1; "bold" starts at 6, len 4.
    let parts = parse_text_tl(
        text,
        &[tl_e::MessageEntity::Bold(tl_t::MessageEntityBold {
            offset: 6,
            length: 4,
        })],
    );
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].text, "Hi \u{1F680} ");
    assert_eq!(parts[1].kind, TextPartType::Bold);
    assert_eq!(parts[1].text, "bold");
    assert_eq!(parts[2].text, "!");

    // 2. Photo parsing picks largest across PhotoCachedSize & PhotoSizeProgressive
    let photo = tl_e::Photo::Photo(tl_t::Photo {
        has_stickers: false,
        id: 777,
        access_hash: 888,
        file_reference: vec![1, 2, 3],
        date: 1_700_000_000,
        sizes: vec![
            tl_e::PhotoSize::PhotoCachedSize(tl_t::PhotoCachedSize {
                r#type: "s".to_string(),
                w: 90,
                h: 90,
                bytes: vec![0xFF, 0xD8],
            }),
            tl_e::PhotoSize::Progressive(tl_t::PhotoSizeProgressive {
                r#type: "y".to_string(),
                w: 1280,
                h: 720,
                sizes: vec![1024, 8192, 42_000],
            }),
        ],
        video_sizes: None,
        dc_id: 2,
    });
    let parsed_photo = parse_photo(&photo, true);
    assert_eq!(parsed_photo.id, 777);
    assert!(parsed_photo.spoilered);
    assert_eq!(parsed_photo.image.width, 1280);
    assert_eq!(parsed_photo.image.height, 720);
    assert_eq!(parsed_photo.image.file.size, 42_000);
    assert_eq!(parsed_photo.image.file.location.dc_id, 2);

    // 3. Dice & Story media leave MediaContent::None (matching `export_data_types.cpp:2357-2361`)
    let dice = tl_e::MessageMedia::Dice(tl_t::MessageMediaDice {
        value: 6,
        emoticon: "\u{1F3B2}".to_string(),
        game_outcome: None,
    });
    assert!(matches!(
        parse_media(&dice, "", 1_700_000_000).content,
        MediaContent::None
    ));
    let story_media = tl_e::MessageMedia::Story(tl_t::MessageMediaStory {
        via_mention: false,
        peer: tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 }),
        id: 42,
        story: None,
    });
    assert!(matches!(
        parse_media(&story_media, "", 1_700_000_000).content,
        MediaContent::None
    ));

    // 4. ParseUser / ParseChat / ParsePeersLists / ParseDialogsInfo
    let u = tl_user(1002, "Briar", "Moss", Some("briar_m"), false, false);
    let parsed_u = parse_user(&u);
    assert_eq!(parsed_u.id(), PeerId::from_user(UserId(1002)));
    assert_eq!(parsed_u.username, "briar_m");

    let ch = tl_channel(3004, "Beacon Broadcast", Some("beacon_news"), true, false);
    let parsed_c = parse_chat(&ch);
    assert_eq!(parsed_c.id(), PeerId::from_channel(ChannelId(3004)));
    assert!(parsed_c.is_broadcast);

    let peers = parse_peers_lists(std::slice::from_ref(&u), std::slice::from_ref(&ch));
    assert_eq!(peers.len(), 2);

    let dlg = tl_e::Dialog::Dialog(tl_t::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer: tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 3004 }),
        top_message: 55,
        read_inbox_max_id: 55,
        read_outbox_max_id: 55,
        unread_count: 0,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
        notify_settings: tl_e::PeerNotifySettings::PeerNotifySettings(tl_t::PeerNotifySettings {
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
        }),
        pts: None,
        draft: None,
        folder_id: None,
        ttl_period: None,
    });
    let top_msg = tl_message(
        55,
        1_700_000_555,
        tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 3004 }),
        None,
        false,
        "hi",
        None,
        None,
        None,
    );
    let parsed_dlgs = parse_dialogs_info(&[dlg], &[top_msg], &[u], &[ch], 0);
    assert_eq!(parsed_dlgs.len(), 1);
    assert_eq!(parsed_dlgs[0].kind, DialogType::PublicChannel);
    assert_eq!(parsed_dlgs[0].top_message_date, 1_700_000_555);

    // 5. Empty message parsing
    let empty = tl_e::Message::Empty(tl_t::MessageEmpty {
        id: 99,
        peer_id: Some(tl_e::Peer::Chat(tl_t::PeerChat { chat_id: 2001 })),
    });
    let parsed_empty = parse_message(&empty, PeerId::from_user(UserId(1001)), "");
    assert_eq!(parsed_empty.id, 99);
    assert_eq!(parsed_empty.peer_id, PeerId::from_chat(ChatId(2001)));
}

// ============================================================================
// Fake ExportRpc server exercising the full export_api_wrap.cpp request plan
// ============================================================================

#[derive(Debug, Clone)]
struct RecordedCall {
    dc_id: i32,
    takeout_id: Option<i64>,
    split_range: Option<(i32, i32)>,
    inner_ctor: u32,
}

struct FakeServerState {
    calls: Vec<RecordedCall>,
    init_takeout: Option<tl_f::account::InitTakeoutSession>,
    finish_takeout_success: Option<bool>,
    upload_offsets: Vec<(i32, i64, i32)>,
    stale_ref_expired_once: bool,
    migrated_dc_once: bool,
    custom_emoji_requested: Vec<i64>,
    rich_message_requested: Vec<i32>,
    history_calls: Vec<(i32, i32, i32)>, // (offset_id, add_offset, limit)
    search_calls: usize,
    big_file_bytes: Vec<u8>,
    small_photo_bytes: Vec<u8>,
    photo_download_calls: HashMap<i64, usize>,
}

struct FakeExportRpc {
    takeout_id: i64,
    state: Mutex<FakeServerState>,
}

impl FakeExportRpc {
    fn new() -> Self {
        // 300 KiB deterministic payload -> 3 chunks of 128 KiB, 128 KiB, 44 KiB
        let big_file_bytes: Vec<u8> = (0..(300 * 1024)).map(|i| (i % 251) as u8).collect();
        let small_photo_bytes = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4, 5, 6, 7, 8];
        Self {
            takeout_id: 77_000_123,
            state: Mutex::new(FakeServerState {
                calls: Vec::new(),
                init_takeout: None,
                finish_takeout_success: None,
                upload_offsets: Vec::new(),
                stale_ref_expired_once: false,
                migrated_dc_once: false,
                custom_emoji_requested: Vec::new(),
                rich_message_requested: Vec::new(),
                history_calls: Vec::new(),
                search_calls: 0,
                big_file_bytes,
                small_photo_bytes,
                photo_download_calls: HashMap::new(),
            }),
        }
    }

    fn dispatch_bytes(&self, dc_id: i32, raw: &[u8]) -> Result<Vec<u8>, RpcCallError> {
        let mut cursor = Cursor::new(raw);
        let peek_ctor = |c: &Cursor<'_>| -> Result<u32, RpcCallError> {
            let rem_len = c.remaining();
            if rem_len < 4 {
                return Err(RpcCallError::new(400, "BAD", "short buffer"));
            }
            let pos = raw.len() - rem_len;
            Ok(u32::from_le_bytes([
                raw[pos],
                raw[pos + 1],
                raw[pos + 2],
                raw[pos + 3],
            ]))
        };
        let mut ctor = peek_ctor(&cursor)?;
        let mut takeout_id = None;
        let mut split_range = None;

        // Unwrap `InvokeWithTakeout`
        if ctor == tl_f::InvokeWithTakeout::<tl_f::messages::GetSplitRanges>::CONSTRUCTOR_ID {
            let _ = cursor.read_u32().unwrap();
            takeout_id = Some(i64::deserialize(&mut cursor).unwrap());
            ctor = peek_ctor(&cursor)?;
        }
        // Unwrap `InvokeWithMessagesRange`
        if ctor == tl_f::InvokeWithMessagesRange::<tl_f::messages::GetSplitRanges>::CONSTRUCTOR_ID {
            let _ = cursor.read_u32().unwrap();
            let range = tl_e::MessageRange::deserialize(&mut cursor).unwrap();
            let tl_e::MessageRange::MessageRange(r) = range;
            split_range = Some((r.min_id, r.max_id));
            ctor = peek_ctor(&cursor)?;
        }

        let mut st = self.state.lock().unwrap();
        st.calls.push(RecordedCall {
            dc_id,
            takeout_id,
            split_range,
            inner_ctor: ctor,
        });

        let self_u = tl_user(1001, "Alden", "Vance", Some("alden_v"), true, false);
        let briar_u = tl_user(1002, "Briar", "Moss", Some("briar_m"), false, false);
        let priv_chan = tl_channel(3003, "Restricted Chan", None, true, false);

        // 1. users.getUsers
        if ctor == tl_f::users::GetUsers::CONSTRUCTOR_ID {
            let resp: Vec<tl_e::User> = vec![self_u];
            return Ok(resp.to_bytes());
        }

        // 2. account.initTakeoutSession
        if ctor == tl_f::account::InitTakeoutSession::CONSTRUCTOR_ID {
            let req = tl_f::account::InitTakeoutSession::deserialize(&mut cursor).unwrap();
            st.init_takeout = Some(req);
            let resp = tl_e::account::Takeout::Takeout(tl_t::account::Takeout {
                id: self.takeout_id,
            });
            return Ok(resp.to_bytes());
        }

        // 3. account.finishTakeoutSession
        if ctor == tl_f::account::FinishTakeoutSession::CONSTRUCTOR_ID {
            let req = tl_f::account::FinishTakeoutSession::deserialize(&mut cursor).unwrap();
            assert_eq!(
                takeout_id,
                Some(self.takeout_id),
                "finishTakeoutSession must be wrapped in InvokeWithTakeout"
            );
            st.finish_takeout_success = Some(req.success);
            return Ok(true.to_bytes());
        }

        // All remaining calls MUST be wrapped in `InvokeWithTakeout` with our session ID!
        assert_eq!(
            takeout_id,
            Some(self.takeout_id),
            "call 0x{ctor:08x} was not wrapped in InvokeWithTakeout"
        );

        // 4. messages.getSplitRanges
        if ctor == tl_f::messages::GetSplitRanges::CONSTRUCTOR_ID {
            let resp = vec![
                tl_e::MessageRange::MessageRange(tl_t::MessageRange {
                    min_id: 1,
                    max_id: 500,
                }),
                tl_e::MessageRange::MessageRange(tl_t::MessageRange {
                    min_id: 501,
                    max_id: 1000,
                }),
            ];
            return Ok(resp.to_bytes());
        }

        // 5. users.getFullUser
        if ctor == tl_f::users::GetFullUser::CONSTRUCTOR_ID {
            let resp = tl_e::users::UserFull::UserFull(tl_t::users::UserFull {
                full_user: tl_e::UserFull::UserFull(tl_t::UserFull {
                    blocked: false,
                    phone_calls_available: false,
                    phone_calls_private: false,
                    can_pin_message: false,
                    has_scheduled: false,
                    video_calls_available: false,
                    voice_messages_forbidden: false,
                    translations_disabled: false,
                    stories_pinned_available: false,
                    blocked_my_stories_from: false,
                    wallpaper_overridden: false,
                    contact_require_premium: false,
                    read_dates_private: false,
                    sponsored_enabled: false,
                    can_view_revenue: false,
                    bot_can_manage_emoji_status: false,
                    display_gifts_button: false,
                    noforwards_my_enabled: false,
                    noforwards_peer_enabled: false,
                    unofficial_security_risk: false,
                    id: 1001,
                    about: Some("Fake TL Bio".to_string()),
                    settings: tl_e::PeerSettings::PeerSettings(tl_t::PeerSettings {
                        report_spam: false,
                        add_contact: false,
                        block_contact: false,
                        share_contact: false,
                        need_contacts_exception: false,
                        report_geo: false,
                        autoarchived: false,
                        invite_members: false,
                        request_chat_broadcast: false,
                        business_bot_paused: false,
                        business_bot_can_reply: false,
                        geo_distance: None,
                        request_chat_title: None,
                        request_chat_date: None,
                        business_bot_id: None,
                        business_bot_manage_url: None,
                        charge_paid_message_stars: None,
                        registration_month: None,
                        phone_country: None,
                        name_change_date: None,
                        photo_change_date: None,
                    }),
                    personal_photo: None,
                    profile_photo: None,
                    fallback_photo: None,
                    notify_settings: tl_e::PeerNotifySettings::PeerNotifySettings(
                        tl_t::PeerNotifySettings {
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
                    bot_info: None,
                    pinned_msg_id: None,
                    common_chats_count: 0,
                    folder_id: None,
                    ttl_period: None,
                    theme: None,
                    private_forward_name: None,
                    bot_group_admin_rights: None,
                    bot_broadcast_admin_rights: None,
                    wallpaper: None,
                    stories: None,
                    business_work_hours: None,
                    business_location: None,
                    business_greeting_message: None,
                    business_away_message: None,
                    business_intro: None,
                    birthday: None,
                    personal_channel_id: None,
                    personal_channel_message: None,
                    stargifts_count: None,
                    starref_program: None,
                    bot_verification: None,
                    send_paid_messages_stars: None,
                    disallowed_gifts: None,
                    stars_rating: None,
                    stars_my_pending_rating: None,
                    stars_my_pending_rating_date: None,
                    main_tab: None,
                    saved_music: None,
                    note: None,
                    bot_manager_id: None,
                    wallet_card: None,
                    share_theme: None,
                }),
                chats: Vec::new(),
                users: vec![self_u],
            });
            return Ok(resp.to_bytes());
        }

        // 6. contacts.getSaved / contacts.getTopPeers
        if ctor == tl_f::contacts::GetSaved::CONSTRUCTOR_ID {
            let resp = vec![tl_e::SavedContact::SavedPhoneContact(
                tl_t::SavedPhoneContact {
                    phone: "15550101002".to_string(),
                    first_name: "Briar".to_string(),
                    last_name: "Moss".to_string(),
                    date: 1_700_000_000,
                },
            )];
            return Ok(resp.to_bytes());
        }
        if ctor == tl_f::contacts::GetTopPeers::CONSTRUCTOR_ID {
            let resp = tl_e::contacts::TopPeers::TopPeers(tl_t::contacts::TopPeers {
                categories: vec![tl_e::TopPeerCategoryPeers::TopPeerCategoryPeers(
                    tl_t::TopPeerCategoryPeers {
                        category: tl_e::TopPeerCategory::Correspondents,
                        count: 1,
                        peers: vec![tl_e::TopPeer::TopPeer(tl_t::TopPeer {
                            peer: tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 }),
                            rating: 9.5,
                        })],
                    },
                )],
                chats: Vec::new(),
                users: vec![briar_u],
            });
            return Ok(resp.to_bytes());
        }

        // 7. account.getAuthorizations / getWebAuthorizations
        if ctor == tl_f::account::GetAuthorizations::CONSTRUCTOR_ID {
            let resp =
                tl_e::account::Authorizations::Authorizations(tl_t::account::Authorizations {
                    authorization_ttl_days: 180,
                    authorizations: vec![tl_e::Authorization::Authorization(tl_t::Authorization {
                        current: true,
                        official_app: true,
                        password_pending: false,
                        encrypted_requests_disabled: false,
                        call_requests_disabled: false,
                        unconfirmed: false,
                        hash: 1,
                        device_model: "Desk".to_string(),
                        platform: "Windows".to_string(),
                        system_version: "11".to_string(),
                        api_id: 2040,
                        app_name: "Telegram Desktop".to_string(),
                        app_version: "5.0".to_string(),
                        date_created: 1_700_000_000,
                        date_active: 1_700_000_100,
                        ip: "198.51.100.5".to_string(),
                        country: "US".to_string(),
                        region: "CA".to_string(),
                    })],
                });
            return Ok(resp.to_bytes());
        }
        if ctor == tl_f::account::GetWebAuthorizations::CONSTRUCTOR_ID {
            let resp = tl_e::account::WebAuthorizations::WebAuthorizations(
                tl_t::account::WebAuthorizations {
                    authorizations: Vec::new(),
                    users: Vec::new(),
                },
            );
            return Ok(resp.to_bytes());
        }

        // 8. channels.getLeftChannels
        if ctor == tl_f::channels::GetLeftChannels::CONSTRUCTOR_ID {
            let resp = tl_e::messages::Chats::Chats(tl_t::messages::Chats { chats: Vec::new() });
            return Ok(resp.to_bytes());
        }

        // 9. messages.getDialogs
        if ctor == tl_f::messages::GetDialogs::CONSTRUCTOR_ID {
            let _req = tl_f::messages::GetDialogs::deserialize(&mut cursor).unwrap();
            assert!(split_range.is_some(), "getDialogs must carry split range");
            // Only return dialogs in split 0 (min_id == 1)
            if split_range == Some((1, 500)) {
                let notify =
                    tl_e::PeerNotifySettings::PeerNotifySettings(tl_t::PeerNotifySettings {
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
                    });
                let resp = tl_e::messages::Dialogs::Dialogs(tl_t::messages::Dialogs {
                    dialogs: vec![
                        tl_e::Dialog::Dialog(tl_t::Dialog {
                            pinned: false,
                            unread_mark: false,
                            view_forum_as_messages: false,
                            peer: tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 }),
                            top_message: 235,
                            read_inbox_max_id: 235,
                            read_outbox_max_id: 235,
                            unread_count: 0,
                            unread_mentions_count: 0,
                            unread_reactions_count: 0,
                            unread_poll_votes_count: 0,
                            notify_settings: notify.clone(),
                            pts: None,
                            draft: None,
                            folder_id: None,
                            ttl_period: None,
                        }),
                        tl_e::Dialog::Dialog(tl_t::Dialog {
                            pinned: false,
                            unread_mark: false,
                            view_forum_as_messages: false,
                            peer: tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 3003 }),
                            top_message: 1,
                            read_inbox_max_id: 1,
                            read_outbox_max_id: 1,
                            unread_count: 0,
                            unread_mentions_count: 0,
                            unread_reactions_count: 0,
                            unread_poll_votes_count: 0,
                            notify_settings: notify,
                            pts: None,
                            draft: None,
                            folder_id: None,
                            ttl_period: None,
                        }),
                    ],
                    messages: Vec::new(),
                    chats: vec![priv_chan],
                    users: vec![self_u, briar_u],
                });
                return Ok(resp.to_bytes());
            } else {
                let resp = tl_e::messages::Dialogs::Dialogs(tl_t::messages::Dialogs {
                    dialogs: Vec::new(),
                    messages: Vec::new(),
                    chats: Vec::new(),
                    users: Vec::new(),
                });
                return Ok(resp.to_bytes());
            }
        }

        // 10. messages.getHistory (235 messages for Briar -> 3 slices of 100, 100, 35;
        //     CHANNEL_PRIVATE for channel 3003 to trigger `messages.search` fallback!)
        if ctor == tl_f::messages::GetHistory::CONSTRUCTOR_ID {
            let req = tl_f::messages::GetHistory::deserialize(&mut cursor).unwrap();
            assert!(split_range.is_some(), "getHistory must carry split range");
            if matches!(req.peer, InputPeer::Channel(_)) {
                return Err(RpcCallError::new(400, "CHANNEL_PRIVATE", "channel private"));
            }
            st.history_calls
                .push((req.offset_id, req.add_offset, req.limit));

            let total = 235_i32;
            if req.limit == 1 {
                let resp = tl_e::messages::Messages::Slice(tl_t::messages::MessagesSlice {
                    inexact: false,
                    count: total,
                    next_rate: None,
                    offset_id_offset: None,
                    search_flood: None,
                    messages: vec![tl_message(
                        1,
                        1_700_000_001,
                        tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 }),
                        Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                        true,
                        "msg 1",
                        None,
                        None,
                        None,
                    )],
                    topics: Vec::new(),
                    chats: Vec::new(),
                    users: vec![self_u, briar_u],
                });
                return Ok(resp.to_bytes());
            }

            assert_eq!(req.limit, MESSAGES_SLICE_LIMIT);
            assert_eq!(req.add_offset, -MESSAGES_SLICE_LIMIT);
            let start = req.offset_id.max(1);
            let end = (start + req.limit - 1).min(total);
            let mut msgs = Vec::new();
            // Return newest-first like real Telegram servers to test chronological sorting.
            for id in (start..=end).rev() {
                let (media, entities, rich) = match id {
                    // Msg 1: 300 KiB document -> tests 128 KiB multi-part download (3 chunks)
                    //        + FILE_REFERENCE_EXPIRED refresh + FILE_MIGRATE_4 redirect!
                    1 => (
                        Some(tl_e::MessageMedia::Document(tl_t::MessageMediaDocument {
                            nopremium: false,
                            spoiler: false,
                            video: false,
                            round: false,
                            voice: false,
                            document: Some(tl_e::Document::Document(tl_t::Document {
                                id: 50_001,
                                access_hash: 50_001,
                                file_reference: b"stale_ref".to_vec(),
                                date: 1_700_000_001,
                                mime_type: "application/octet-stream".to_string(),
                                size: st.big_file_bytes.len() as i64,
                                thumbs: None,
                                video_thumbs: None,
                                dc_id: 2,
                                attributes: vec![tl_e::DocumentAttribute::Filename(
                                    tl_t::DocumentAttributeFilename {
                                        file_name: "big300k.bin".to_string(),
                                    },
                                )],
                            })),
                            alt_documents: None,
                            video_cover: None,
                            video_timestamp: None,
                            ttl_seconds: None,
                        })),
                        None,
                        None,
                    ),
                    // Msg 2 & Msg 3: same photo location -> tests LoadedFileCache deduplication!
                    2 | 3 => (
                        Some(tl_e::MessageMedia::Photo(tl_t::MessageMediaPhoto {
                            spoiler: false,
                            live_photo: false,
                            photo: Some(tl_e::Photo::Photo(tl_t::Photo {
                                has_stickers: false,
                                id: 60_001,
                                access_hash: 60_001,
                                file_reference: b"ok_ref".to_vec(),
                                date: 1_700_000_002,
                                sizes: vec![tl_e::PhotoSize::PhotoSize(tl_t::PhotoSize {
                                    r#type: "x".to_string(),
                                    w: 100,
                                    h: 100,
                                    size: st.small_photo_bytes.len() as i32,
                                })],
                                video_sizes: None,
                                dc_id: 2,
                            })),
                            ttl_seconds: None,
                            video: None,
                        })),
                        None,
                        None,
                    ),
                    // Msg 4: CustomEmoji entity -> triggers `messages.getCustomEmojiDocuments`
                    4 => (
                        None,
                        Some(vec![tl_e::MessageEntity::CustomEmoji(
                            tl_t::MessageEntityCustomEmoji {
                                offset: 0,
                                length: 1,
                                document_id: 70_001,
                            },
                        )]),
                        None,
                    ),
                    // Msg 5: partial RichMessage (`part = true`) -> triggers `messages.getRichMessage`
                    5 => (
                        None,
                        None,
                        Some(tl_e::RichMessage::RichMessage(tl_t::RichMessage {
                            rtl: false,
                            part: true,
                            blocks: Vec::new(),
                            photos: Vec::new(),
                            documents: Vec::new(),
                        })),
                    ),
                    _ => (None, None, None),
                };
                let body = if id == 4 {
                    "\u{2B50} custom".to_string()
                } else {
                    format!("msg {id}")
                };
                msgs.push(tl_message(
                    id,
                    1_700_000_000 + id,
                    tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 }),
                    Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                    true,
                    &body,
                    entities,
                    media,
                    rich,
                ));
            }
            let resp = tl_e::messages::Messages::Slice(tl_t::messages::MessagesSlice {
                inexact: false,
                count: total,
                next_rate: None,
                offset_id_offset: None,
                search_flood: None,
                messages: msgs,
                topics: Vec::new(),
                chats: Vec::new(),
                users: vec![self_u, briar_u],
            });
            return Ok(resp.to_bytes());
        }

        // 11. messages.search (fallback for CHANNEL_PRIVATE channel 3003)
        if ctor == tl_f::messages::Search::CONSTRUCTOR_ID {
            let req = tl_f::messages::Search::deserialize(&mut cursor).unwrap();
            st.search_calls += 1;
            let resp = tl_e::messages::Messages::Messages(tl_t::messages::Messages {
                messages: if req.offset_id <= 1 {
                    vec![tl_message(
                        1,
                        1_700_000_500,
                        tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 3003 }),
                        Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                        true,
                        "my own channel msg",
                        None,
                        None,
                        None,
                    )]
                } else {
                    Vec::new()
                },
                topics: Vec::new(),
                chats: vec![priv_chan],
                users: vec![self_u],
            });
            return Ok(resp.to_bytes());
        }

        // 12. messages.getMessages (file_reference refresh for msg 1!)
        if ctor == tl_f::messages::GetMessages::CONSTRUCTOR_ID {
            let resp = tl_e::messages::Messages::Messages(tl_t::messages::Messages {
                messages: vec![tl_message(
                    1,
                    1_700_000_001,
                    tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 }),
                    Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                    true,
                    "msg 1",
                    None,
                    Some(tl_e::MessageMedia::Document(tl_t::MessageMediaDocument {
                        nopremium: false,
                        spoiler: false,
                        video: false,
                        round: false,
                        voice: false,
                        document: Some(tl_e::Document::Document(tl_t::Document {
                            id: 50_001,
                            access_hash: 50_001,
                            file_reference: b"fresh_ref".to_vec(),
                            date: 1_700_000_001,
                            mime_type: "application/octet-stream".to_string(),
                            size: st.big_file_bytes.len() as i64,
                            thumbs: None,
                            video_thumbs: None,
                            dc_id: 2,
                            attributes: vec![tl_e::DocumentAttribute::Filename(
                                tl_t::DocumentAttributeFilename {
                                    file_name: "big300k.bin".to_string(),
                                },
                            )],
                        })),
                        alt_documents: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    })),
                    None,
                )],
                topics: Vec::new(),
                chats: Vec::new(),
                users: vec![self_u, briar_u],
            });
            return Ok(resp.to_bytes());
        }

        // 13. messages.getCustomEmojiDocuments
        if ctor == tl_f::messages::GetCustomEmojiDocuments::CONSTRUCTOR_ID {
            let req = tl_f::messages::GetCustomEmojiDocuments::deserialize(&mut cursor).unwrap();
            st.custom_emoji_requested
                .extend_from_slice(&req.document_id);
            let resp = vec![tl_e::Document::Document(tl_t::Document {
                id: 70_001,
                access_hash: 70_001,
                file_reference: b"emoji_ref".to_vec(),
                date: 1_700_000_004,
                mime_type: "image/png".to_string(),
                size: st.small_photo_bytes.len() as i64,
                thumbs: None,
                video_thumbs: None,
                dc_id: 2,
                attributes: vec![tl_e::DocumentAttribute::CustomEmoji(
                    tl_t::DocumentAttributeCustomEmoji {
                        free: true,
                        text_color: false,
                        alt: "\u{2B50}".to_string(),
                        stickerset: tl_e::InputStickerSet::Empty,
                    },
                )],
            })];
            return Ok(resp.to_bytes());
        }

        // 14. messages.getRichMessage
        if ctor == tl_f::messages::GetRichMessage::CONSTRUCTOR_ID {
            let req = tl_f::messages::GetRichMessage::deserialize(&mut cursor).unwrap();
            st.rich_message_requested.push(req.id);
            let resp = tl_e::messages::Messages::Messages(tl_t::messages::Messages {
                messages: vec![tl_message(
                    req.id,
                    1_700_000_005,
                    tl_e::Peer::User(tl_t::PeerUser { user_id: 1002 }),
                    Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                    true,
                    "hydrated",
                    None,
                    None,
                    Some(tl_e::RichMessage::RichMessage(tl_t::RichMessage {
                        rtl: false,
                        part: false,
                        blocks: vec![tl_e::PageBlock::Paragraph(tl_t::PageBlockParagraph {
                            text: tl_e::RichText::TextPlain(tl_t::TextPlain {
                                text: "Hydrated Rich Body".to_string(),
                            }),
                        })],
                        photos: Vec::new(),
                        documents: Vec::new(),
                    })),
                )],
                topics: Vec::new(),
                chats: Vec::new(),
                users: vec![self_u, briar_u],
            });
            return Ok(resp.to_bytes());
        }

        // 15. upload.getFile (128 KiB parts, FILE_REFERENCE_EXPIRED, FILE_MIGRATE_4, TAKEOUT_FILE_EMPTY)
        if ctor == tl_f::upload::GetFile::CONSTRUCTOR_ID {
            let req = tl_f::upload::GetFile::deserialize(&mut cursor).unwrap();
            assert_eq!(
                req.limit, FILE_CHUNK_SIZE,
                "upload.getFile must use 128 KiB chunks"
            );
            match req.location {
                InputFileLocation::InputTakeoutFileLocation => {
                    return Err(RpcCallError::new(
                        400,
                        "TAKEOUT_FILE_EMPTY",
                        "no other data",
                    ));
                }
                InputFileLocation::InputDocumentFileLocation(doc) if doc.id == 50_001 => {
                    if doc.file_reference == b"stale_ref" {
                        st.stale_ref_expired_once = true;
                        return Err(RpcCallError::new(400, "FILE_REFERENCE_EXPIRED", "expired"));
                    }
                    if dc_id != 4 {
                        st.migrated_dc_once = true;
                        return Err(RpcCallError::new(303, "FILE_MIGRATE_4", "migrate dc 4"));
                    }
                    st.upload_offsets.push((dc_id, req.offset, req.limit));
                    let start = req.offset as usize;
                    let end = (start + req.limit as usize).min(st.big_file_bytes.len());
                    let bytes = if start < st.big_file_bytes.len() {
                        st.big_file_bytes[start..end].to_vec()
                    } else {
                        Vec::new()
                    };
                    let resp = tl_e::upload::File::File(tl_t::upload::File {
                        r#type: tl_e::storage::FileType::FileUnknown,
                        mtime: 1_700_000_001,
                        bytes,
                    });
                    return Ok(resp.to_bytes());
                }
                InputFileLocation::InputDocumentFileLocation(doc) if doc.id == 70_001 => {
                    let resp = tl_e::upload::File::File(tl_t::upload::File {
                        r#type: tl_e::storage::FileType::FilePng,
                        mtime: 1_700_000_004,
                        bytes: st.small_photo_bytes.clone(),
                    });
                    return Ok(resp.to_bytes());
                }
                InputFileLocation::InputPhotoFileLocation(photo) if photo.id == 60_001 => {
                    *st.photo_download_calls.entry(photo.id).or_insert(0) += 1;
                    let resp = tl_e::upload::File::File(tl_t::upload::File {
                        r#type: tl_e::storage::FileType::FilePng,
                        mtime: 1_700_000_002,
                        bytes: st.small_photo_bytes.clone(),
                    });
                    return Ok(resp.to_bytes());
                }
                other => {
                    return Err(RpcCallError::new(
                        400,
                        "LOCATION_INVALID",
                        format!("unexpected location {other:?}"),
                    ));
                }
            }
        }

        Err(RpcCallError::new(
            400,
            "METHOD_UNEXPECTED",
            format!("unhandled constructor 0x{ctor:08x}"),
        ))
    }
}

impl ExportRpc for FakeExportRpc {
    async fn invoke<R: RemoteCall + Identifiable + Send + Sync>(
        &self,
        dc_id: i32,
        request: &R,
    ) -> Result<R::Return, RpcCallError> {
        let wire = request.to_bytes();
        let resp_bytes = self.dispatch_bytes(dc_id, &wire)?;
        let mut cursor = Cursor::new(&resp_bytes);
        R::Return::deserialize(&mut cursor)
            .map_err(|e| RpcCallError::new(500, "DESERIALIZE_ERROR", format!("{e:?}")))
    }
}

#[tokio::test]
async fn tl_export_source_full_takeout_plan_against_fake_rpc() {
    let dir = temp_dir("fake_rpc");
    let fake_rpc = FakeExportRpc::new();
    let expected_big_bytes = fake_rpc.state.lock().unwrap().big_file_bytes.clone();

    let source = TlExportSource::new(fake_rpc);
    let settings = Settings {
        path: dir_with_slash(&dir),
        format: OutputFormat::HtmlAndJson,
        types: ExportTypes::PERSONAL_INFO
            | ExportTypes::CONTACTS
            | ExportTypes::SESSIONS
            | ExportTypes::OTHER_DATA
            | ExportTypes::PERSONAL_CHATS
            | ExportTypes::PRIVATE_CHANNELS,
        full_chats: ExportTypes::PERSONAL_CHATS
            | ExportTypes::BOT_CHATS
            | ExportTypes::PRIVATE_CHANNELS,
        media: MediaSettings {
            types: MediaTypes::PHOTO | MediaTypes::FILE | MediaTypes::STICKER,
            size_limit: 4 * 1024 * 1024,
        },
        ..Default::default()
    };

    let (mut controller, _handle) = ExportController::new(source, InputPeer::Empty);
    controller.set_skip_path_normalization(true);
    let final_state = controller.run(&settings, &Environment::default()).await;
    assert!(
        matches!(final_state, ExportState::Finished(_)),
        "expected Finished, got: {final_state:?}"
    );

    // Inspect recorded RPC trace inside `FakeExportRpc`
    let st = controller.source().rpc().state.lock().unwrap();

    // 1. `account.initTakeoutSession` flags matched settings exactly
    let init = st
        .init_takeout
        .as_ref()
        .expect("initTakeoutSession must be called");
    assert!(init.contacts);
    assert!(init.message_users);
    assert!(!init.message_chats);
    assert!(!init.message_megagroups);
    assert!(init.message_channels);
    assert!(init.files);
    assert_eq!(init.file_max_size, Some(4 * 1024 * 1024));

    // 2. `account.finishTakeoutSession(success = true)` called at end
    assert_eq!(st.finish_takeout_success, Some(true));

    // 3. History sliced in pages of 100 (`add_offset = -100`, offsets 1, 101, 201)
    let page_calls: Vec<(i32, i32, i32)> = st
        .history_calls
        .iter()
        .copied()
        .filter(|&(_, _, limit)| limit == MESSAGES_SLICE_LIMIT)
        .collect();
    assert_eq!(
        page_calls,
        vec![(1, -100, 100), (101, -100, 100), (201, -100, 100)]
    );

    // 4. `CHANNEL_PRIVATE` triggered `messages.search` fallback for channel 3003
    assert!(st.search_calls >= 2);

    // 5. 300 KiB file downloaded in 3 chunks of 128 KiB on DC 4 after FILE_REFERENCE_EXPIRED + FILE_MIGRATE_4
    assert!(st.stale_ref_expired_once);
    assert!(st.migrated_dc_once);
    assert_eq!(
        st.upload_offsets,
        vec![
            (4, 0, FILE_CHUNK_SIZE),
            (4, i64::from(FILE_CHUNK_SIZE), FILE_CHUNK_SIZE),
            (4, i64::from(FILE_CHUNK_SIZE) * 2, FILE_CHUNK_SIZE),
        ]
    );

    // 6. LoadedFileCache deduplicated photo 60_001 across messages 2 and 3 (downloaded ONCE!)
    assert_eq!(st.photo_download_calls.get(&60_001).copied(), Some(1));

    // 7. Custom emoji and partial rich message hydrated via RPC
    assert_eq!(st.custom_emoji_requested, vec![70_001]);
    assert_eq!(st.rich_message_requested, vec![5]);

    // 7b. Verify recorded RPC envelope metadata (DCs, takeout_id, split ranges, constructors)
    assert!(
        st.calls.iter().any(|c| {
            c.inner_ctor == tl_f::upload::GetFile::CONSTRUCTOR_ID
                && c.dc_id == 4
                && c.takeout_id == Some(77_000_123)
        }),
        "expected upload.getFile on migrated DC 4 with takeout wrapper"
    );
    assert!(
        st.calls.iter().any(|c| {
            c.inner_ctor == tl_f::messages::GetHistory::CONSTRUCTOR_ID
                && c.split_range == Some((1, 500))
                && c.takeout_id == Some(77_000_123)
        }),
        "expected messages.getHistory wrapped in InvokeWithMessagesRange(1..=500) + InvokeWithTakeout"
    );
    drop(st);

    // 8. Verify exact reassembled 300 KiB file on disk
    let downloaded = fs::read(dir.join("chats/chat_1/files/big300k.bin")).expect("read 300k file");
    assert_eq!(downloaded, expected_big_bytes);

    // 9. Verify `result.json` contains all 235 chronological messages + hydrated rich message + custom emoji + channel search fallback
    let json_raw = fs::read_to_string(dir.join("result.json")).expect("read result.json");
    let root: Value = serde_json::from_str(&json_raw).expect("valid JSON");
    assert_eq!(root["personal_information"]["bio"], "Fake TL Bio");
    let chats = root["chats"]["list"].as_array().unwrap();
    assert_eq!(chats.len(), 2);

    let briar_msgs = chats[0]["messages"].as_array().unwrap();
    assert_eq!(briar_msgs.len(), 235);
    assert_eq!(briar_msgs[0]["id"], 1);
    assert_eq!(briar_msgs[0]["file"], "chats/chat_1/files/big300k.bin");
    // Messages 2 and 3 share the deduplicated cached photo path
    assert_eq!(briar_msgs[1]["photo"], briar_msgs[2]["photo"]);
    // Message 4 resolved custom emoji path
    assert_eq!(
        briar_msgs[3]["text_entities"][0]["document_id"],
        "stickers/file_1@14-11-2023_22-13-24.png"
    );
    // Message 5 hydrated rich message
    assert_eq!(
        briar_msgs[4]["rich_message"]["blocks"][0]["text"]["text"],
        "Hydrated Rich Body"
    );
    assert_eq!(briar_msgs[234]["id"], 235);

    // Channel 3003 fell back to `messages.search` and exported the outgoing message
    let chan_msgs = chats[1]["messages"].as_array().unwrap();
    assert_eq!(chan_msgs.len(), 1);
    assert_eq!(chan_msgs[0]["text"], "my own channel msg");

    let _ = fs::remove_dir_all(&dir);
}

// ============================================================================
// Secondary fake RPC covering userpics, stories (with story FILE_REFERENCE refresh),
// profile music, single-peer migrated supergroup, and single-topic export.
// ============================================================================

#[derive(Default)]
struct ExtraRpcState {
    story_ref_refreshed: bool,
    replies_calls: usize,
    full_channel_calls: usize,
}

struct ExtraFakeRpc {
    takeout_id: i64,
    state: Mutex<ExtraRpcState>,
}

impl ExtraFakeRpc {
    fn new() -> Self {
        Self {
            takeout_id: 88_000_456,
            state: Mutex::new(ExtraRpcState::default()),
        }
    }

    fn dispatch(&self, _dc_id: i32, raw: &[u8]) -> Result<Vec<u8>, RpcCallError> {
        let mut cursor = Cursor::new(raw);
        let peek_ctor = |c: &Cursor<'_>| -> Result<u32, RpcCallError> {
            let rem_len = c.remaining();
            if rem_len < 4 {
                return Err(RpcCallError::new(400, "BAD", "short"));
            }
            let pos = raw.len() - rem_len;
            Ok(u32::from_le_bytes([
                raw[pos],
                raw[pos + 1],
                raw[pos + 2],
                raw[pos + 3],
            ]))
        };
        let mut ctor = peek_ctor(&cursor)?;
        if ctor == tl_f::InvokeWithTakeout::<tl_f::messages::GetSplitRanges>::CONSTRUCTOR_ID {
            let _ = cursor.read_u32().unwrap();
            let tid = i64::deserialize(&mut cursor).unwrap();
            assert_eq!(tid, self.takeout_id);
            ctor = peek_ctor(&cursor)?;
        }
        if ctor == tl_f::InvokeWithMessagesRange::<tl_f::messages::GetSplitRanges>::CONSTRUCTOR_ID {
            let _ = cursor.read_u32().unwrap();
            let _ = tl_e::MessageRange::deserialize(&mut cursor).unwrap();
            ctor = peek_ctor(&cursor)?;
        }

        let mut st = self.state.lock().unwrap();
        let self_u = tl_user(1001, "Alden", "Vance", Some("alden_v"), true, false);
        let supergroup = tl_channel(4001, "Migrated Forum", Some("migrated_forum"), false, true);
        let legacy_group = tl_e::Chat::Chat(tl_t::Chat {
            creator: true,
            left: false,
            deactivated: true,
            call_active: false,
            call_not_empty: false,
            noforwards: false,
            id: 2001,
            title: "Legacy Group".to_string(),
            photo: tl_e::ChatPhoto::Empty,
            participants_count: 5,
            date: 1_699_000_000,
            version: 1,
            migrated_to: Some(tl_e::InputChannel::InputChannel(tl_t::InputChannel {
                channel_id: 4001,
                access_hash: 4001,
            })),
            admin_rights: None,
            default_banned_rights: None,
        });

        if ctor == tl_f::users::GetUsers::CONSTRUCTOR_ID {
            return Ok(vec![self_u].to_bytes());
        }
        if ctor == tl_f::account::InitTakeoutSession::CONSTRUCTOR_ID {
            let resp = tl_e::account::Takeout::Takeout(tl_t::account::Takeout {
                id: self.takeout_id,
            });
            return Ok(resp.to_bytes());
        }
        if ctor == tl_f::account::FinishTakeoutSession::CONSTRUCTOR_ID {
            return Ok(true.to_bytes());
        }
        if ctor == tl_f::messages::GetSplitRanges::CONSTRUCTOR_ID {
            let resp = vec![tl_e::MessageRange::MessageRange(tl_t::MessageRange {
                min_id: 1,
                max_id: 1000,
            })];
            return Ok(resp.to_bytes());
        }
        if ctor == tl_f::photos::GetUserPhotos::CONSTRUCTOR_ID {
            let resp = tl_e::photos::Photos::Photos(tl_t::photos::Photos {
                photos: vec![tl_e::Photo::Photo(tl_t::Photo {
                    has_stickers: false,
                    id: 90_001,
                    access_hash: 90_001,
                    file_reference: b"up_ok".to_vec(),
                    date: 1_700_000_010,
                    sizes: vec![tl_e::PhotoSize::PhotoSize(tl_t::PhotoSize {
                        r#type: "x".to_string(),
                        w: 64,
                        h: 64,
                        size: 8,
                    })],
                    video_sizes: None,
                    dc_id: 2,
                })],
                users: vec![self_u],
            });
            return Ok(resp.to_bytes());
        }
        if ctor == tl_f::stories::GetStoriesArchive::CONSTRUCTOR_ID {
            let resp = tl_e::stories::Stories::Stories(tl_t::stories::Stories {
                count: 1,
                stories: vec![tl_e::StoryItem::StoryItem(tl_t::StoryItem {
                    pinned: true,
                    public: false,
                    close_friends: false,
                    min: false,
                    noforwards: false,
                    edited: false,
                    contacts: false,
                    selected_contacts: false,
                    out: true,
                    id: 77,
                    date: 1_700_000_020,
                    from_id: None,
                    fwd_from: None,
                    expire_date: 1_700_086_420,
                    caption: Some("Story caption".to_string()),
                    entities: None,
                    media: Box::new(tl_e::MessageMedia::Photo(tl_t::MessageMediaPhoto {
                        spoiler: false,
                        live_photo: false,
                        photo: Some(tl_e::Photo::Photo(tl_t::Photo {
                            has_stickers: false,
                            id: 91_001,
                            access_hash: 91_001,
                            file_reference: b"story_stale".to_vec(),
                            date: 1_700_000_020,
                            sizes: vec![tl_e::PhotoSize::PhotoSize(tl_t::PhotoSize {
                                r#type: "x".to_string(),
                                w: 64,
                                h: 64,
                                size: 8,
                            })],
                            video_sizes: None,
                            dc_id: 2,
                        })),
                        ttl_seconds: None,
                        video: None,
                    })),
                    media_areas: None,
                    privacy: None,
                    views: None,
                    sent_reaction: None,
                    albums: None,
                    music: None,
                })],
                pinned_to_top: None,
                chats: Vec::new(),
                users: vec![self_u],
            });
            return Ok(resp.to_bytes());
        }
        if ctor == tl_f::stories::GetStoriesByID::CONSTRUCTOR_ID {
            let req = tl_f::stories::GetStoriesByID::deserialize(&mut cursor).unwrap();
            assert_eq!(req.id, vec![77]);
            st.story_ref_refreshed = true;
            let resp = tl_e::stories::Stories::Stories(tl_t::stories::Stories {
                count: 1,
                stories: vec![tl_e::StoryItem::StoryItem(tl_t::StoryItem {
                    pinned: true,
                    public: false,
                    close_friends: false,
                    min: false,
                    noforwards: false,
                    edited: false,
                    contacts: false,
                    selected_contacts: false,
                    out: true,
                    id: 77,
                    date: 1_700_000_020,
                    from_id: None,
                    fwd_from: None,
                    expire_date: 1_700_086_420,
                    caption: Some("Story caption".to_string()),
                    entities: None,
                    media: Box::new(tl_e::MessageMedia::Photo(tl_t::MessageMediaPhoto {
                        spoiler: false,
                        live_photo: false,
                        photo: Some(tl_e::Photo::Photo(tl_t::Photo {
                            has_stickers: false,
                            id: 91_001,
                            access_hash: 91_001,
                            file_reference: b"story_fresh".to_vec(),
                            date: 1_700_000_020,
                            sizes: vec![tl_e::PhotoSize::PhotoSize(tl_t::PhotoSize {
                                r#type: "x".to_string(),
                                w: 64,
                                h: 64,
                                size: 8,
                            })],
                            video_sizes: None,
                            dc_id: 2,
                        })),
                        ttl_seconds: None,
                        video: None,
                    })),
                    media_areas: None,
                    privacy: None,
                    views: None,
                    sent_reaction: None,
                    albums: None,
                    music: None,
                })],
                pinned_to_top: None,
                chats: Vec::new(),
                users: vec![self_u],
            });
            return Ok(resp.to_bytes());
        }
        if ctor == tl_f::users::GetSavedMusic::CONSTRUCTOR_ID {
            let resp = tl_e::users::SavedMusic::SavedMusic(tl_t::users::SavedMusic {
                count: 1,
                documents: vec![tl_e::Document::Document(tl_t::Document {
                    id: 92_001,
                    access_hash: 92_001,
                    file_reference: b"music_ok".to_vec(),
                    date: 1_700_000_030,
                    mime_type: "audio/mpeg".to_string(),
                    size: 10,
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 2,
                    attributes: vec![tl_e::DocumentAttribute::Audio(
                        tl_t::DocumentAttributeAudio {
                            voice: false,
                            duration: 180,
                            title: Some("Starlight".to_string()),
                            performer: Some("Echo".to_string()),
                            waveform: None,
                        },
                    )],
                })],
            });
            return Ok(resp.to_bytes());
        }
        if ctor == tl_f::channels::GetChannels::CONSTRUCTOR_ID {
            let resp = tl_e::messages::Chats::Chats(tl_t::messages::Chats {
                chats: vec![supergroup],
            });
            return Ok(resp.to_bytes());
        }
        if ctor == tl_f::channels::GetFullChannel::CONSTRUCTOR_ID {
            st.full_channel_calls += 1;
            let resp = tl_e::messages::ChatFull::ChatFull(tl_t::messages::ChatFull {
                full_chat: tl_e::ChatFull::ChannelFull(tl_t::ChannelFull {
                    can_view_participants: false,
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
                    translations_disabled: false,
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
                    id: 4001,
                    about: String::new(),
                    participants_count: Some(10),
                    admins_count: None,
                    kicked_count: None,
                    banned_count: None,
                    online_count: None,
                    read_inbox_max_id: 0,
                    read_outbox_max_id: 0,
                    unread_count: 0,
                    chat_photo: tl_e::Photo::Empty(tl_t::PhotoEmpty { id: 0 }),
                    notify_settings: tl_e::PeerNotifySettings::PeerNotifySettings(
                        tl_t::PeerNotifySettings {
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
                    bot_info: Vec::new(),
                    migrated_from_chat_id: Some(2001),
                    migrated_from_max_id: Some(50),
                    pinned_msg_id: None,
                    stickerset: None,
                    available_min_id: None,
                    folder_id: None,
                    linked_chat_id: None,
                    location: None,
                    slowmode_seconds: None,
                    slowmode_next_send_date: None,
                    stats_dc: None,
                    pts: 1,
                    call: None,
                    ttl_period: None,
                    pending_suggestions: None,
                    groupcall_default_join_as: None,
                    theme_emoticon: None,
                    requests_pending: None,
                    recent_requesters: None,
                    default_send_as: None,
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
                chats: vec![supergroup, legacy_group],
                users: vec![self_u],
            });
            return Ok(resp.to_bytes());
        }
        if ctor == tl_f::messages::GetHistory::CONSTRUCTOR_ID {
            let req = tl_f::messages::GetHistory::deserialize(&mut cursor).unwrap();
            let msgs = match req.peer {
                InputPeer::Chat(_) => vec![tl_message(
                    50,
                    1_700_000_001,
                    tl_e::Peer::Chat(tl_t::PeerChat { chat_id: 2001 }),
                    Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                    true,
                    "Legacy basic group msg",
                    None,
                    None,
                    None,
                )],
                InputPeer::Channel(_) => vec![tl_message(
                    10,
                    1_700_000_100,
                    tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 4001 }),
                    Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                    true,
                    "Supergroup msg",
                    None,
                    None,
                    None,
                )],
                _ => Vec::new(),
            };
            let resp = tl_e::messages::Messages::Messages(tl_t::messages::Messages {
                messages: msgs,
                topics: Vec::new(),
                chats: vec![supergroup, legacy_group],
                users: vec![self_u],
            });
            return Ok(resp.to_bytes());
        }
        if ctor == tl_f::channels::GetMessages::CONSTRUCTOR_ID {
            let resp = tl_e::messages::Messages::Messages(tl_t::messages::Messages {
                messages: vec![tl_message(
                    100,
                    1_700_000_200,
                    tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 4001 }),
                    Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                    true,
                    "Topic Root",
                    None,
                    None,
                    None,
                )],
                topics: Vec::new(),
                chats: vec![supergroup],
                users: vec![self_u],
            });
            return Ok(resp.to_bytes());
        }
        if ctor == tl_f::messages::GetReplies::CONSTRUCTOR_ID {
            st.replies_calls += 1;
            let resp = tl_e::messages::Messages::Messages(tl_t::messages::Messages {
                messages: vec![tl_message(
                    101,
                    1_700_000_205,
                    tl_e::Peer::Channel(tl_t::PeerChannel { channel_id: 4001 }),
                    Some(tl_e::Peer::User(tl_t::PeerUser { user_id: 1001 })),
                    true,
                    "Topic Reply 1",
                    None,
                    None,
                    None,
                )],
                topics: Vec::new(),
                chats: vec![supergroup],
                users: vec![self_u],
            });
            return Ok(resp.to_bytes());
        }
        if ctor == tl_f::upload::GetFile::CONSTRUCTOR_ID {
            let req = tl_f::upload::GetFile::deserialize(&mut cursor).unwrap();
            match req.location {
                InputFileLocation::InputPhotoFileLocation(p)
                    if p.id == 91_001 && p.file_reference == b"story_stale" =>
                {
                    return Err(RpcCallError::new(
                        400,
                        "FILE_REFERENCE_EXPIRED",
                        "story ref expired",
                    ));
                }
                _ => {
                    let resp = tl_e::upload::File::File(tl_t::upload::File {
                        r#type: tl_e::storage::FileType::FilePng,
                        mtime: 1_700_000_020,
                        bytes: vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4],
                    });
                    return Ok(resp.to_bytes());
                }
            }
        }
        Err(RpcCallError::new(
            400,
            "UNHANDLED",
            format!("unhandled 0x{ctor:08x}"),
        ))
    }
}

impl ExportRpc for ExtraFakeRpc {
    async fn invoke<R: RemoteCall + Identifiable + Send + Sync>(
        &self,
        dc_id: i32,
        request: &R,
    ) -> Result<R::Return, RpcCallError> {
        let wire = request.to_bytes();
        let resp_bytes = self.dispatch(dc_id, &wire)?;
        let mut cursor = Cursor::new(&resp_bytes);
        R::Return::deserialize(&mut cursor)
            .map_err(|e| RpcCallError::new(500, "DESERIALIZE_ERROR", format!("{e:?}")))
    }
}

#[tokio::test]
async fn tl_export_source_userpics_stories_and_profile_music() {
    let dir = temp_dir("media_sections");
    let source = TlExportSource::new(ExtraFakeRpc::new());
    let settings = Settings {
        path: dir_with_slash(&dir),
        format: OutputFormat::HtmlAndJson,
        types: ExportTypes::USERPICS | ExportTypes::STORIES | ExportTypes::PROFILE_MUSIC,
        ..Default::default()
    };

    let (mut controller, _handle) = ExportController::new(source, InputPeer::Empty);
    controller.set_skip_path_normalization(true);
    let state = controller
        .run(&settings, &Environment::default_english())
        .await;
    assert!(matches!(state, ExportState::Finished(_)), "got {state:?}");
    assert!(
        controller
            .source()
            .rpc()
            .state
            .lock()
            .unwrap()
            .story_ref_refreshed,
        "expected story FILE_REFERENCE_EXPIRED to refresh via stories.getStoriesByID"
    );

    let json: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("result.json")).unwrap()).unwrap();
    assert_eq!(json["profile_pictures"].as_array().unwrap().len(), 1);
    assert_eq!(json["stories"].as_array().unwrap().len(), 1);
    assert_eq!(json["stories"][0]["pinned"], true);
    assert!(
        json["stories"][0]["media"]
            .as_str()
            .unwrap()
            .starts_with("stories/")
    );
    let stories_html = fs::read_to_string(dir.join("lists/stories.html")).unwrap();
    assert!(stories_html.contains("Story caption"));
    assert_eq!(json["profile_music"].as_array().unwrap().len(), 1);
    assert_eq!(json["profile_music"][0]["performer"], "Echo");
    assert_eq!(json["profile_music"][0]["title"], "Starlight");

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn tl_export_source_migrated_supergroup_and_single_topic() {
    // 1. Single-peer migrated supergroup (pulls legacy basic group history + supergroup history)
    let dir_mig = temp_dir("migrated_sg");
    let single_peer = InputPeer::Channel(tl_t::InputPeerChannel {
        channel_id: 4001,
        access_hash: 4001,
    });
    let settings_mig = Settings {
        path: dir_with_slash(&dir_mig),
        format: OutputFormat::Json,
        single_peer: single_peer.clone(),
        ..Default::default()
    };
    let (mut ctrl_mig, _) = ExportController::new(
        TlExportSource::new(ExtraFakeRpc::new()),
        single_peer.clone(),
    );
    ctrl_mig.set_skip_path_normalization(true);
    let st_mig = ctrl_mig
        .run(&settings_mig, &Environment::default_english())
        .await;
    assert!(matches!(st_mig, ExportState::Finished(_)), "got {st_mig:?}");
    assert_eq!(
        ctrl_mig
            .source()
            .rpc()
            .state
            .lock()
            .unwrap()
            .full_channel_calls,
        1
    );

    let mig_json: Value =
        serde_json::from_str(&fs::read_to_string(dir_mig.join("result.json")).unwrap()).unwrap();
    let mig_msgs = mig_json["messages"].as_array().unwrap();
    assert_eq!(mig_msgs.len(), 2);
    // Migrated basic group message ID is adjusted negatively before supergroup messages
    assert!(mig_msgs[0]["id"].as_i64().unwrap() < 0);
    assert_eq!(mig_msgs[0]["text"], "Legacy basic group msg");
    assert_eq!(mig_msgs[1]["id"], 10);
    assert_eq!(mig_msgs[1]["text"], "Supergroup msg");
    let _ = fs::remove_dir_all(&dir_mig);

    // 2. Single-topic export via `channels.getMessages` + `messages.getReplies`
    let dir_topic = temp_dir("single_topic");
    let settings_topic = Settings {
        path: dir_with_slash(&dir_topic),
        format: OutputFormat::Json,
        single_peer: single_peer.clone(),
        single_topic_root_id: 100,
        single_topic_title: "Forum Topic".to_string(),
        ..Default::default()
    };
    let (mut ctrl_topic, _) =
        ExportController::new(TlExportSource::new(ExtraFakeRpc::new()), single_peer);
    ctrl_topic.set_skip_path_normalization(true);
    let st_topic = ctrl_topic
        .run(&settings_topic, &Environment::default_english())
        .await;
    assert!(
        matches!(st_topic, ExportState::Finished(_)),
        "got {st_topic:?}"
    );
    assert!(
        ctrl_topic
            .source()
            .rpc()
            .state
            .lock()
            .unwrap()
            .replies_calls
            >= 1
    );

    let topic_json: Value =
        serde_json::from_str(&fs::read_to_string(dir_topic.join("result.json")).unwrap()).unwrap();
    let topic_msgs = topic_json["messages"].as_array().unwrap();
    assert_eq!(topic_msgs.len(), 2);
    assert_eq!(topic_msgs[0]["text"], "Topic Root");
    assert_eq!(topic_msgs[1]["text"], "Topic Reply 1");
    let _ = fs::remove_dir_all(&dir_topic);
}
