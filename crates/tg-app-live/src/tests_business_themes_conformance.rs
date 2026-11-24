//! WP-58 protocol completion & conformance tests for Business (non-quick-reply),
//! Monetization (`channels.restrictSponsoredMessages`), Chatlists
//! (`chatlists.getLeaveChatlistSuggestions`), Cloud Themes (`account.saveTheme`,
//! `account.getUniqueGiftChatThemes`, `account.installTheme`), Share Themes
//! (`messages.getShareThemeBackdrops`, `messages.setShareTheme`), Peer Colors
//! (`help.getPeerColors`, `help.getPeerProfileColors`), and Account Extras
//! (`account.confirmPhone`, `account.confirmBotConnection`, `help.getSupport`,
//! `{account,channels}.setMainProfileTab`, `users.suggestBirthday`,
//! `account.reportProfilePhoto`).
//!
//! Runs exclusively against an in-process `FakeRpcClient` with synthetic IDs/users.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{ChannelFlags, ChannelId, Peer, PeerId as DataPeerId, UserId as DataUserId};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_theme::{CloudThemeType, Color};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::profile_extras::{disable_business_bot, toggle_business_bot_paused};
use tg_app_core::business::backend::BusinessBackend;
use tg_app_core::business::model::{
    AWAY_START_TIME_MIN, AwaySchedule, AwayScheduleType, AwaySettings, BusinessBotCandidate,
    BusinessChatTypes, BusinessChats, BusinessLocation, BusinessRecipients, ChatIntro,
    ChatbotsPermissions, ChatbotsSettings, GeoPoint, GreetingSettings, WorkingHours,
    WorkingInterval, WorkingIntervals,
};
use tg_app_core::info::backend::{InfoBackend, ProfileMainTab, ProfilePhotoRef, ReportReason};
use tg_app_core::info::values::Birthday;
use tg_app_core::session_api::AppSession;
use tg_app_core::settings::backend::SettingsBackend;
use tg_app_core::stand_in::types::PeerId;
use tg_app_core::statistics::backend::StatsBackend;
use tg_app_core::themes::backend::{
    CloudBaseTheme, CloudThemeInstallRequest, PeerColorsResult, PeerProfileColorsResult,
    ShareThemeBackdropsResult, ShareThemeChoice, ShareThemeReaction, ThemesBackend,
    UniqueGiftChatThemesResult,
};

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

    fn enqueue_rpc_err(&self, expected_ctor: u32, code: i32, name: &str, value: Option<u32>) {
        self.responses.lock().expect("lock responses").push_back((
            expected_ctor,
            Err(InvocationError::Rpc(RpcError {
                code,
                name: name.to_owned(),
                value,
            })),
        ));
    }

    fn recorded_calls(&self) -> Vec<(u32, Vec<u8>)> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn clear_recorded(&self) {
        self.recorded.lock().expect("lock recorded").clear();
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(self.main_dc_id(), req).await
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

const SELF_ID: i64 = 8001;
const CONTACT_ID: i64 = 8002;
const BOT_ID: i64 = 8003;
const UNCACHED_BOT_ID: i64 = 8004;
const SUPPORT_ID: i64 = 424_000;
const CHANNEL_ID: i64 = 9001;

fn synthetic_tl_user(
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
        support: id == SUPPORT_ID,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: true,
        attach_menu_enabled: false,
        bot_can_edit: is_bot,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: is_bot,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 17 + 5),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: (!username.is_empty()).then(|| username.to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: is_bot.then_some(1),
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

fn synthetic_tl_channel(
    id: i64,
    title: &str,
    username: &str,
    broadcast: bool,
) -> tl_types::Channel {
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
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 31 + 13),
        title: title.to_owned(),
        username: (!username.is_empty()).then(|| username.to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_795_356_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(500),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: Some(5),
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    }
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_356_000,
        seq: 0,
    })
}

fn synthetic_unique_star_gift(id: i64, slug: &str, title: &str, num: i32) -> tl_enums::StarGift {
    tl_enums::StarGift::Unique(tl_types::StarGiftUnique {
        require_premium: false,
        resale_ton_only: false,
        theme_available: true,
        burned: false,
        crafted: false,
        id,
        gift_id: id + 100,
        title: title.to_owned(),
        slug: slug.to_owned(),
        num,
        owner_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: SELF_ID,
        })),
        owner_name: Some("Self Owner".to_owned()),
        owner_address: None,
        attributes: Vec::new(),
        availability_issued: 10,
        availability_total: 100,
        gift_address: None,
        resell_amount: None,
        released_by: None,
        value_amount: None,
        value_currency: None,
        value_usd_amount: None,
        theme_peer: None,
        peer_color: None,
        host_id: None,
        offer_min_stars: None,
        craft_chance_permille: None,
    })
}

struct Wp58Harness {
    _runtime: Arc<SessionRuntime<crate::ErasedRpcClient>>,
    rpc: FakeRpcClient,
    live: LiveSession,
}

impl Wp58Harness {
    fn start() -> Self {
        let rpc = FakeRpcClient::new(2);
        let cfg = SessionRuntimeConfig {
            init_updates_state: false,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            ..SessionRuntimeConfig::default()
        };
        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId::from_tl(SELF_ID),
                crate::ErasedRpcClient::new(rpc.clone()),
                cfg,
            )
            .expect("start SessionRuntime"),
        );
        runtime.tokio_handle().block_on(async {
            let mut sess = runtime.session().write().await;
            let users = vec![
                tl_enums::User::User(synthetic_tl_user(
                    SELF_ID,
                    "Jordan",
                    "jordan_self",
                    true,
                    false,
                )),
                tl_enums::User::User(synthetic_tl_user(
                    CONTACT_ID,
                    "Avery",
                    "avery_contact",
                    false,
                    false,
                )),
                tl_enums::User::User(synthetic_tl_user(
                    BOT_ID,
                    "Desk Bot",
                    "desk_biz_bot",
                    false,
                    true,
                )),
            ];
            let chats = vec![tl_enums::Chat::Channel(synthetic_tl_channel(
                CHANNEL_ID,
                "Signal Channel",
                "signal_chan",
                true,
            ))];
            apply_tl_users_and_chats(&mut sess, &users, &chats);
            if let Some(Peer::Channel(ch)) =
                sess.peer_mut(DataPeerId::from_channel(ChannelId(CHANNEL_ID as u64)))
            {
                ch.flags |= ChannelFlags::CREATOR | ChannelFlags::BROADCAST;
            }
        });

        let live = LiveSession::new(Arc::clone(&runtime), 58);
        Self {
            _runtime: runtime,
            rpc,
            live,
        }
    }

    fn self_peer() -> PeerId {
        PeerId::from_user(SELF_ID as u64)
    }

    fn contact_peer() -> PeerId {
        PeerId::from_user(CONTACT_ID as u64)
    }

    fn channel_peer() -> PeerId {
        PeerId::from_channel(CHANNEL_ID as u64)
    }
}

// =============================================================================
// 1. Peer Colors & Peer Profile Colors (`help.getPeerColors`, `help.getPeerProfileColors`)
// =============================================================================

#[test]
fn peer_colors_and_profile_colors_hash_cache_and_levels() {
    let h = Wp58Harness::start();

    h.rpc.enqueue_ok(
        tl_fns::help::GetPeerColors::CONSTRUCTOR_ID,
        &tl_enums::help::PeerColors::PeerColors(tl_types::help::PeerColors {
            hash: 112233,
            colors: vec![
                tl_enums::help::PeerColorOption::PeerColorOption(tl_types::help::PeerColorOption {
                    hidden: false,
                    color_id: 7,
                    colors: Some(tl_enums::help::PeerColorSet::PeerColorSet(
                        tl_types::help::PeerColorSet {
                            colors: vec![0xE15D54, 0xF0915B],
                        },
                    )),
                    dark_colors: None, // must fall back to light_colors (api_peer_colors.cpp:178)
                    channel_min_level: Some(2),
                    group_min_level: Some(4),
                }),
                tl_enums::help::PeerColorOption::PeerColorOption(tl_types::help::PeerColorOption {
                    hidden: true,
                    color_id: 9,
                    colors: Some(tl_enums::help::PeerColorSet::PeerColorSet(
                        tl_types::help::PeerColorSet {
                            colors: vec![0x3366FF],
                        },
                    )),
                    dark_colors: Some(tl_enums::help::PeerColorSet::PeerColorSet(
                        tl_types::help::PeerColorSet {
                            colors: vec![0x5599FF],
                        },
                    )),
                    channel_min_level: Some(3),
                    group_min_level: None,
                }),
                // Invalid out-of-range color_id (>= 64) must be ignored (api_peer_colors.cpp:159)
                tl_enums::help::PeerColorOption::PeerColorOption(tl_types::help::PeerColorOption {
                    hidden: false,
                    color_id: 64,
                    colors: None,
                    dark_colors: None,
                    channel_min_level: None,
                    group_min_level: None,
                }),
            ],
        }),
    );

    let res = h.live.get_peer_colors(0).expect("getPeerColors");
    let PeerColorsResult::Colors {
        hash,
        options,
        suggested,
    } = res
    else {
        panic!("expected PeerColorsResult::Colors");
    };
    assert_eq!(hash, 112233);
    assert_eq!(options.len(), 2);
    assert_eq!(suggested, vec![7]);
    assert_eq!(
        options[0].light_colors,
        vec![Color::rgb(0xE1, 0x5D, 0x54), Color::rgb(0xF0, 0x91, 0x5B)]
    );
    assert_eq!(options[0].dark_colors, options[0].light_colors);
    assert_eq!(options[1].dark_colors, vec![Color::rgb(0x55, 0x99, 0xFF)]);

    // Hash-cached NotModified keeps cached entries
    h.rpc.enqueue_ok(
        tl_fns::help::GetPeerColors::CONSTRUCTOR_ID,
        &tl_enums::help::PeerColors::NotModified,
    );
    assert_eq!(
        h.live.get_peer_colors(hash).expect("getPeerColors cached"),
        PeerColorsResult::NotModified
    );
    let (cached_hash, cached_opts, cached_sugg) = h.live.cached_peer_colors();
    assert_eq!(cached_hash, 112233);
    assert_eq!(cached_opts.len(), 2);
    assert_eq!(cached_sugg, vec![7]);

    // Verify wire hash sent on second call
    let calls = h.rpc.recorded_calls();
    let second = tl_fns::help::GetPeerColors::from_bytes(&calls[1].1).expect("decode 2nd call");
    assert_eq!(second.hash, 112233);

    // Profile colors (`help.getPeerProfileColors`)
    h.rpc.enqueue_ok(
        tl_fns::help::GetPeerProfileColors::CONSTRUCTOR_ID,
        &tl_enums::help::PeerColors::PeerColors(tl_types::help::PeerColors {
            hash: 445566,
            colors: vec![tl_enums::help::PeerColorOption::PeerColorOption(
                tl_types::help::PeerColorOption {
                    hidden: false,
                    color_id: 12,
                    colors: Some(tl_enums::help::PeerColorSet::PeerColorProfileSet(
                        tl_types::help::PeerColorProfileSet {
                            palette_colors: vec![0x112233],
                            bg_colors: vec![0x445566, 0x778899],
                            story_colors: vec![0xAABBCC],
                        },
                    )),
                    dark_colors: Some(tl_enums::help::PeerColorSet::PeerColorProfileSet(
                        tl_types::help::PeerColorProfileSet {
                            palette_colors: vec![0x010203],
                            bg_colors: vec![0x040506],
                            story_colors: vec![0x070809],
                        },
                    )),
                    channel_min_level: Some(5),
                    group_min_level: Some(6),
                },
            )],
        }),
    );
    let prof_res = h
        .live
        .get_peer_profile_colors(0)
        .expect("getPeerProfileColors");
    let PeerProfileColorsResult::Colors {
        hash: phash,
        options: popts,
    } = prof_res
    else {
        panic!("expected PeerProfileColorsResult::Colors");
    };
    assert_eq!(phash, 445566);
    assert_eq!(popts.len(), 1);
    assert_eq!(popts[0].color_id, 12);
    assert_eq!(popts[0].light.bg.len(), 2);
    assert_eq!(popts[0].dark.palette, vec![Color::rgb(0x01, 0x02, 0x03)]);

    h.rpc.enqueue_ok(
        tl_fns::help::GetPeerProfileColors::CONSTRUCTOR_ID,
        &tl_enums::help::PeerColors::NotModified,
    );
    assert_eq!(
        h.live
            .get_peer_profile_colors(phash)
            .expect("getPeerProfileColors cached"),
        PeerProfileColorsResult::NotModified
    );
    assert_eq!(h.live.cached_peer_profile_colors().0, 445566);

    // Required boost levels (`api_peer_colors.cpp:100-124`)
    let chan_raw = Wp58Harness::channel_peer().0;
    let default_idx =
        tg_data::decide_color_index(crate::convert::to_data_peer(Wp58Harness::channel_peer()));
    assert_eq!(
        h.live
            .peer_color_required_level(chan_raw, default_idx, false, false),
        0
    );
    assert_eq!(
        h.live.peer_color_required_level(chan_raw, 7, false, false),
        2
    );
    assert_eq!(
        h.live.peer_color_required_level(chan_raw, 7, true, false),
        4
    );
    assert_eq!(
        h.live.peer_color_required_level(chan_raw, 12, false, true),
        5
    );
    assert_eq!(
        h.live.peer_color_required_level(chan_raw, 12, true, true),
        6
    );
    assert_eq!(
        h.live.peer_color_required_level(chan_raw, 33, false, false),
        1
    );

    // Verify 1-hour `PEER_COLORS_REFRESH_MS` cadence wired to `tg_api::cached_lists::PeerColorsSync`
    h.rpc.clear_recorded();
    h.rpc.enqueue_ok(
        tl_fns::help::GetPeerColors::CONSTRUCTOR_ID,
        &tl_enums::help::PeerColors::NotModified,
    );
    assert!(
        h.live
            .refresh_peer_colors_if_due(10_000, false)
            .expect("first timed refresh")
    );
    // Second call within PEER_COLORS_REFRESH_MS is throttled (no RPC sent)
    assert!(
        !h.live
            .refresh_peer_colors_if_due(
                10_000 + tg_api::cached_lists::PEER_COLORS_REFRESH_MS - 1,
                false
            )
            .expect("throttled refresh")
    );
    assert_eq!(h.rpc.recorded_calls().len(), 1);
    // After 1 hour, refresh runs again with cached hash
    h.rpc.enqueue_ok(
        tl_fns::help::GetPeerColors::CONSTRUCTOR_ID,
        &tl_enums::help::PeerColors::NotModified,
    );
    assert!(
        h.live
            .refresh_peer_colors_if_due(
                10_000 + tg_api::cached_lists::PEER_COLORS_REFRESH_MS,
                false
            )
            .expect("due refresh")
    );
    assert_eq!(h.rpc.recorded_calls().len(), 2);
}

// =============================================================================
// 2. Cloud Themes (`account.saveTheme`, `account.getUniqueGiftChatThemes`, `account.installTheme`)
// =============================================================================

#[test]
fn cloud_themes_save_install_and_unique_gift_chat_themes() {
    let h = Wp58Harness::start();

    // Regression: `account.installTheme` always sets `format = Some("tdesktop")` even when
    // `theme` is None (base theme / reset), matching `CloudThemes::install` (`data_cloud_themes.cpp:296`).
    h.rpc
        .enqueue_ok(tl_fns::account::InstallTheme::CONSTRUCTOR_ID, &true);
    h.live
        .install_theme(&CloudThemeInstallRequest {
            dark: true,
            theme_id: None,
            slug: None,
            base_theme: Some(CloudBaseTheme::Night),
        })
        .expect("installTheme base");
    let install_call = h
        .rpc
        .recorded_calls()
        .into_iter()
        .find(|(c, _)| *c == tl_fns::account::InstallTheme::CONSTRUCTOR_ID)
        .expect("installTheme recorded");
    let decoded_install =
        tl_fns::account::InstallTheme::from_bytes(&install_call.1).expect("decode InstallTheme");
    assert!(decoded_install.dark);
    assert!(decoded_install.theme.is_none());
    assert_eq!(decoded_install.format.as_deref(), Some("tdesktop"));
    assert!(matches!(
        decoded_install.base_theme,
        Some(tl_enums::BaseTheme::Night)
    ));

    // `account.saveTheme` (save & unsave by ID + unsave by slug)
    h.rpc
        .enqueue_ok(tl_fns::account::SaveTheme::CONSTRUCTOR_ID, &true);
    h.live
        .save_theme(777, 888, false)
        .expect("saveTheme save=true");
    h.rpc
        .enqueue_ok(tl_fns::account::SaveTheme::CONSTRUCTOR_ID, &true);
    h.live
        .save_theme(777, 888, true)
        .expect("saveTheme unsave=true");
    h.rpc
        .enqueue_ok(tl_fns::account::SaveTheme::CONSTRUCTOR_ID, &true);
    h.live
        .save_theme_by_slug("midnight-sky", true)
        .expect("saveTheme by slug");

    let save_calls: Vec<_> = h
        .rpc
        .recorded_calls()
        .into_iter()
        .filter(|(c, _)| *c == tl_fns::account::SaveTheme::CONSTRUCTOR_ID)
        .map(|(_, b)| tl_fns::account::SaveTheme::from_bytes(&b).expect("decode SaveTheme"))
        .collect();
    assert_eq!(save_calls.len(), 3);
    assert!(!save_calls[0].unsave);
    assert!(save_calls[1].unsave);
    assert!(matches!(
        save_calls[2].theme,
        tl_enums::InputTheme::Slug(ref s) if s.slug == "midnight-sky"
    ));

    // `account.getUniqueGiftChatThemes` pagination + light-mode outgoing_accent fallback
    h.rpc.enqueue_ok(
        tl_fns::account::GetUniqueGiftChatThemes::CONSTRUCTOR_ID,
        &tl_enums::account::ChatThemes::ChatThemes(tl_types::account::ChatThemes {
            hash: 999_001,
            themes: vec![tl_enums::ChatTheme::UniqueGift(
                tl_types::ChatThemeUniqueGift {
                    gift: synthetic_unique_star_gift(501, "crystal-1", "Crystal Star", 1),
                    theme_settings: vec![
                        tl_enums::ThemeSettings::ThemeSettings(tl_types::ThemeSettings {
                            message_colors_animated: false,
                            base_theme: tl_enums::BaseTheme::Day,
                            accent_color: 0x3377CC,
                            outbox_accent_color: None,
                            message_colors: None,
                            wallpaper: None,
                        }),
                        tl_enums::ThemeSettings::ThemeSettings(tl_types::ThemeSettings {
                            message_colors_animated: true,
                            base_theme: tl_enums::BaseTheme::Night,
                            accent_color: 0x66AAFF,
                            outbox_accent_color: None,
                            message_colors: Some(vec![0x112233, 0x223344]),
                            wallpaper: None,
                        }),
                    ],
                },
            )],
            chats: Vec::new(),
            users: Vec::new(),
            next_offset: Some("page2".to_owned()),
        }),
    );

    let page1 = h
        .live
        .get_unique_gift_chat_themes("", 0, 0)
        .expect("getUniqueGiftChatThemes page 1");
    let UniqueGiftChatThemesResult::Themes {
        hash,
        themes,
        next_offset,
    } = page1
    else {
        panic!("expected UniqueGiftChatThemesResult::Themes");
    };
    assert_eq!(hash, 999_001);
    assert_eq!(next_offset.as_deref(), Some("page2"));
    assert_eq!(themes.len(), 1);
    assert_eq!(themes[0].slug, "crystal-1");
    // Verify light-mode fallback (`data_cloud_themes.cpp:201-209`)
    let light = themes[0]
        .theme
        .settings
        .get(&CloudThemeType::Light)
        .expect("light variant");
    assert_eq!(
        light.outgoing_accent_color,
        Some(Color::rgb(0x33, 0x77, 0xCC))
    );
    let dark = themes[0]
        .theme
        .settings
        .get(&CloudThemeType::Dark)
        .expect("dark variant");
    assert_eq!(dark.outgoing_accent_color, None);
    assert_eq!(dark.outgoing_messages_colors.len(), 2);

    // Check default limit = 24 (`kGiftThemesLimit`) was sent on wire
    let gift_call = h
        .rpc
        .recorded_calls()
        .into_iter()
        .find(|(c, _)| *c == tl_fns::account::GetUniqueGiftChatThemes::CONSTRUCTOR_ID)
        .expect("GetUniqueGiftChatThemes recorded");
    let decoded_gift =
        tl_fns::account::GetUniqueGiftChatThemes::from_bytes(&gift_call.1).expect("decode");
    assert_eq!(decoded_gift.limit, 24);
    assert_eq!(decoded_gift.offset, "");

    // NotModified preserves cache
    h.rpc.enqueue_ok(
        tl_fns::account::GetUniqueGiftChatThemes::CONSTRUCTOR_ID,
        &tl_enums::account::ChatThemes::NotModified,
    );
    assert_eq!(
        h.live
            .get_unique_gift_chat_themes("", 24, hash)
            .expect("not modified"),
        UniqueGiftChatThemesResult::NotModified
    );
    assert_eq!(h.live.cached_unique_gift_chat_themes().len(), 1);
}

// =============================================================================
// 3. Share Themes (`messages.getShareThemeBackdrops`, `messages.setShareTheme`)
// =============================================================================

#[test]
fn share_theme_backdrops_and_set_share_theme() {
    let h = Wp58Harness::start();

    h.rpc.enqueue_ok(
        tl_fns::messages::GetShareThemeBackdrops::CONSTRUCTOR_ID,
        &tl_enums::ShareThemeBackdrops::ShareThemeBackdrops(tl_types::ShareThemeBackdrops {
            hash: 314159,
            backdrops: vec![
                tl_enums::ShareThemeBackdropOption::ShareThemeBackdropOption(
                    tl_types::ShareThemeBackdropOption {
                        backdrop_id: 42,
                        name: "Aurora".to_owned(),
                        center_color: 0x102030,
                        edge_color: 0x405060,
                        pattern_color: 0x708090,
                        text_color: 0xFFFFFF,
                    },
                ),
            ],
        }),
    );

    let res = h
        .live
        .get_share_theme_backdrops(0)
        .expect("getShareThemeBackdrops");
    let ShareThemeBackdropsResult::Backdrops { hash, backdrops } = res else {
        panic!("expected Backdrops");
    };
    assert_eq!(hash, 314159);
    assert_eq!(backdrops.len(), 1);
    assert_eq!(backdrops[0].backdrop_id, 42);
    assert_eq!(backdrops[0].center_color, Color::rgb(0x10, 0x20, 0x30));

    h.rpc.enqueue_ok(
        tl_fns::messages::GetShareThemeBackdrops::CONSTRUCTOR_ID,
        &tl_enums::ShareThemeBackdrops::NotModified,
    );
    assert_eq!(
        h.live
            .get_share_theme_backdrops(hash)
            .expect("backdrops not modified"),
        ShareThemeBackdropsResult::NotModified
    );
    assert_eq!(h.live.cached_share_theme_backdrops().len(), 1);

    // `messages.setShareTheme`: empty EmojiBackdrop collapses to ShareThemeDefault
    let self_raw = Wp58Harness::self_peer().0;
    let chan_raw = Wp58Harness::channel_peer().0;

    h.rpc
        .enqueue_ok(tl_fns::messages::SetShareTheme::CONSTRUCTOR_ID, &true);
    h.live
        .set_share_theme(
            self_raw,
            &ShareThemeChoice::EmojiBackdrop {
                backdrop_id: None,
                emojis: Vec::new(),
            },
        )
        .expect("empty backdrop -> default");

    // EmojiBackdrop with backdrop_id + reactions
    h.rpc
        .enqueue_ok(tl_fns::messages::SetShareTheme::CONSTRUCTOR_ID, &true);
    let choice_eb = ShareThemeChoice::EmojiBackdrop {
        backdrop_id: Some(42),
        emojis: vec![
            ShareThemeReaction::Emoji("✨".to_owned()),
            ShareThemeReaction::CustomEmoji(555_666),
        ],
    };
    h.live
        .set_share_theme(chan_raw, &choice_eb)
        .expect("set EmojiBackdrop");
    assert_eq!(h.live.cached_peer_share_theme(chan_raw), Some(choice_eb));

    // Gift (user msg_id, channel saved_id, slug)
    h.rpc
        .enqueue_ok(tl_fns::messages::SetShareTheme::CONSTRUCTOR_ID, &true);
    h.live
        .set_share_theme(
            self_raw,
            &ShareThemeChoice::Gift {
                msg_id: 912,
                peer_id: None,
                saved_id: 0,
                slug: None,
            },
        )
        .expect("set Gift user");

    h.rpc
        .enqueue_ok(tl_fns::messages::SetShareTheme::CONSTRUCTOR_ID, &true);
    h.live
        .set_share_theme(
            chan_raw,
            &ShareThemeChoice::Gift {
                msg_id: 0,
                peer_id: Some(chan_raw),
                saved_id: 7788,
                slug: None,
            },
        )
        .expect("set Gift chat");

    h.rpc
        .enqueue_ok(tl_fns::messages::SetShareTheme::CONSTRUCTOR_ID, &true);
    h.live
        .set_share_theme(
            self_raw,
            &ShareThemeChoice::Gift {
                msg_id: 0,
                peer_id: None,
                saved_id: 0,
                slug: Some("crystal-1".to_owned()),
            },
        )
        .expect("set Gift slug");

    let set_calls: Vec<_> = h
        .rpc
        .recorded_calls()
        .into_iter()
        .filter(|(c, _)| *c == tl_fns::messages::SetShareTheme::CONSTRUCTOR_ID)
        .map(|(_, b)| {
            tl_fns::messages::SetShareTheme::from_bytes(&b).expect("decode SetShareTheme")
        })
        .collect();
    assert_eq!(set_calls.len(), 5);
    assert!(matches!(set_calls[0].theme, tl_enums::ShareTheme::Default));
    assert!(matches!(
        set_calls[1].theme,
        tl_enums::ShareTheme::EmojiBackdrop(ref eb)
            if eb.backdrop_id == Some(42) && eb.emojis.as_ref().is_some_and(|e| e.len() == 2)
    ));
    assert!(matches!(
        set_calls[2].theme,
        tl_enums::ShareTheme::InputShareThemeGift(tl_types::InputShareThemeGift {
            stargift: tl_enums::InputSavedStarGift::User(tl_types::InputSavedStarGiftUser {
                msg_id: 912
            })
        })
    ));
    assert!(matches!(
        set_calls[3].theme,
        tl_enums::ShareTheme::InputShareThemeGift(tl_types::InputShareThemeGift {
            stargift: tl_enums::InputSavedStarGift::Chat(tl_types::InputSavedStarGiftChat {
                saved_id: 7788,
                ..
            })
        })
    ));
    assert!(matches!(
        set_calls[4].theme,
        tl_enums::ShareTheme::InputShareThemeGift(tl_types::InputShareThemeGift {
            stargift: tl_enums::InputSavedStarGift::Slug(ref s)
        }) if s.slug == "crystal-1"
    ));
}

// =============================================================================
// 4. Channel Monetization (`channels.restrictSponsoredMessages`)
// =============================================================================

#[test]
fn channel_restrict_sponsored_messages_rpc() {
    let h = Wp58Harness::start();
    let chan = Wp58Harness::channel_peer();
    h.live.set_stats_target_peer(chan);

    h.rpc.enqueue_ok(
        tl_fns::channels::RestrictSponsoredMessages::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    h.live
        .restrict_sponsored_messages(true)
        .expect("restrict_sponsored_messages(true)");

    h.rpc.enqueue_ok(
        tl_fns::channels::RestrictSponsoredMessages::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    h.live
        .restrict_channel_sponsored_messages(chan, false)
        .expect("restrict_channel_sponsored_messages(false)");

    let calls: Vec<_> = h
        .rpc
        .recorded_calls()
        .into_iter()
        .filter(|(c, _)| *c == tl_fns::channels::RestrictSponsoredMessages::CONSTRUCTOR_ID)
        .map(|(_, b)| tl_fns::channels::RestrictSponsoredMessages::from_bytes(&b).expect("decode"))
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].restricted);
    assert!(!calls[1].restricted);
    assert!(matches!(
        calls[0].channel,
        tl_enums::InputChannel::InputChannel(ref ch) if ch.channel_id == CHANNEL_ID
    ));

    // Error propagation
    h.rpc.enqueue_rpc_err(
        tl_fns::channels::RestrictSponsoredMessages::CONSTRUCTOR_ID,
        400,
        "BOOSTS_REQUIRED",
        None,
    );
    let err = h
        .live
        .restrict_sponsored_messages(true)
        .expect_err("boosts required");
    assert!(err.contains("BOOSTS_REQUIRED"));
}

// =============================================================================
// 5. Chatlists (`chatlists.getLeaveChatlistSuggestions`)
// =============================================================================

#[test]
fn chatlist_leave_suggestions_rpc() {
    let h = Wp58Harness::start();

    let returned_peers = vec![
        tl_enums::Peer::Channel(tl_types::PeerChannel {
            channel_id: CHANNEL_ID,
        }),
        tl_enums::Peer::User(tl_types::PeerUser {
            user_id: CONTACT_ID,
        }),
    ];
    h.rpc.enqueue_ok(
        tl_fns::chatlists::GetLeaveChatlistSuggestions::CONSTRUCTOR_ID,
        &returned_peers,
    );

    let suggested = h
        .live
        .leave_folder_suggestions(5)
        .expect("leave_folder_suggestions");
    assert_eq!(suggested.len(), 2);
    assert_eq!(suggested[0], Wp58Harness::channel_peer().0);
    assert_eq!(suggested[1], Wp58Harness::contact_peer().0);

    let call = h
        .rpc
        .recorded_calls()
        .into_iter()
        .find(|(c, _)| *c == tl_fns::chatlists::GetLeaveChatlistSuggestions::CONSTRUCTOR_ID)
        .expect("GetLeaveChatlistSuggestions recorded");
    let decoded =
        tl_fns::chatlists::GetLeaveChatlistSuggestions::from_bytes(&call.1).expect("decode");
    assert!(matches!(
        decoded.chatlist,
        tl_enums::InputChatlist::DialogFilter(tl_types::InputChatlistDialogFilter { filter_id: 5 })
    ));
}

// =============================================================================
// 6. Confirm Phone (`account.sendConfirmPhoneCode`, `account.confirmPhone`)
// =============================================================================

#[test]
fn confirm_phone_code_and_error_handling() {
    let mut h = Wp58Harness::start();

    h.rpc.enqueue_ok(
        tl_fns::account::SendConfirmPhoneCode::CONSTRUCTOR_ID,
        &tl_enums::auth::SentCode::SentCode(tl_types::auth::SentCode {
            r#type: tl_enums::auth::SentCodeType::Sms(tl_types::auth::SentCodeTypeSms {
                length: 5,
            }),
            phone_code_hash: "confirm_hash_abc".to_owned(),
            next_type: None,
            timeout: Some(60),
        }),
    );
    let phone_code_hash = h
        .live
        .send_confirm_phone_code("link_hash_xyz")
        .expect("sendConfirmPhoneCode");
    assert_eq!(phone_code_hash, "confirm_hash_abc");

    // Empty code rejected locally without RPC
    h.rpc.clear_recorded();
    let empty_err = h
        .live
        .confirm_phone_code(&phone_code_hash, "   ")
        .expect_err("empty code");
    assert_eq!(empty_err, "PHONE_CODE_EMPTY");
    assert!(h.rpc.recorded_calls().is_empty());

    // Valid code submits `account.confirmPhone`
    h.rpc
        .enqueue_ok(tl_fns::account::ConfirmPhone::CONSTRUCTOR_ID, &true);
    let ok = h
        .live
        .confirm_phone_code(&phone_code_hash, "12345")
        .expect("confirmPhone");
    assert!(ok);

    let call = h
        .rpc
        .recorded_calls()
        .into_iter()
        .find(|(c, _)| *c == tl_fns::account::ConfirmPhone::CONSTRUCTOR_ID)
        .expect("ConfirmPhone recorded");
    let decoded = tl_fns::account::ConfirmPhone::from_bytes(&call.1).expect("decode ConfirmPhone");
    assert_eq!(decoded.phone_code_hash, "confirm_hash_abc");
    assert_eq!(decoded.phone_code, "12345");

    // Server PHONE_CODE_INVALID error surfaces cleanly
    h.rpc.enqueue_rpc_err(
        tl_fns::account::ConfirmPhone::CONSTRUCTOR_ID,
        400,
        "PHONE_CODE_INVALID",
        None,
    );
    let err = h
        .live
        .confirm_phone_code(&phone_code_hash, "00000")
        .expect_err("invalid code");
    assert!(err.contains("PHONE_CODE_INVALID"));
}

// =============================================================================
// 7. Confirm Bot Connection (`account.confirmBotConnection`) & Support (`help.getSupport`)
// =============================================================================

#[test]
fn confirm_bot_connection_and_support_contact_rpcs() {
    let mut h = Wp58Harness::start();

    // Cached bot with access_hash -> single `account.confirmBotConnection` call
    h.rpc
        .enqueue_ok(tl_fns::account::ConfirmBotConnection::CONSTRUCTOR_ID, &true);
    assert!(
        h.live
            .confirm_bot_connection(BOT_ID)
            .expect("confirm cached bot")
    );
    let calls = h.rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let decoded = tl_fns::account::ConfirmBotConnection::from_bytes(&calls[0].1).expect("decode");
    assert!(matches!(
        decoded.bot_id,
        tl_enums::InputUser::InputUser(ref u) if u.user_id == BOT_ID && u.access_hash == BOT_ID * 17 + 5
    ));

    // Uncached bot -> fetches `account.getConnectedBots` first (`api_authorizations.cpp:449-457`)
    h.rpc.clear_recorded();
    h.rpc.enqueue_ok(
        tl_fns::account::GetConnectedBots::CONSTRUCTOR_ID,
        &tl_enums::account::ConnectedBots::ConnectedBots(tl_types::account::ConnectedBots {
            connected_bots: Vec::new(),
            users: vec![tl_enums::User::User(synthetic_tl_user(
                UNCACHED_BOT_ID,
                "Fresh Biz Bot",
                "fresh_biz_bot",
                false,
                true,
            ))],
        }),
    );
    h.rpc
        .enqueue_ok(tl_fns::account::ConfirmBotConnection::CONSTRUCTOR_ID, &true);
    assert!(
        h.live
            .confirm_bot_connection(UNCACHED_BOT_ID)
            .expect("confirm uncached bot via fallback")
    );
    let calls2 = h.rpc.recorded_calls();
    assert_eq!(calls2.len(), 2);
    assert_eq!(
        calls2[0].0,
        tl_fns::account::GetConnectedBots::CONSTRUCTOR_ID
    );
    assert_eq!(
        calls2[1].0,
        tl_fns::account::ConfirmBotConnection::CONSTRUCTOR_ID
    );
    let decoded2 =
        tl_fns::account::ConfirmBotConnection::from_bytes(&calls2[1].1).expect("decode 2");
    assert!(matches!(
        decoded2.bot_id,
        tl_enums::InputUser::InputUser(ref u)
            if u.user_id == UNCACHED_BOT_ID && u.access_hash == UNCACHED_BOT_ID * 17 + 5
    ));

    // `help.getSupport`
    h.rpc.enqueue_ok(
        tl_fns::help::GetSupport::CONSTRUCTOR_ID,
        &tl_enums::help::Support::Support(tl_types::help::Support {
            phone_number: "+42400".to_owned(),
            user: tl_enums::User::User(synthetic_tl_user(
                SUPPORT_ID,
                "Volunteer Support",
                "tg_support_test",
                false,
                false,
            )),
        }),
    );
    let support = h.live.get_support_contact().expect("help.getSupport");
    assert_eq!(support.phone_number, "+42400");
    assert_eq!(support.user_id, SUPPORT_ID);
    assert_eq!(support.display_name, "Volunteer Support");
}

// =============================================================================
// 8. Main Profile Tab (`account.setMainProfileTab`, `channels.setMainProfileTab`),
//    Suggest Birthday (`users.suggestBirthday`), Report Photo (`account.reportProfilePhoto`)
// =============================================================================

#[test]
fn main_profile_tab_suggest_birthday_and_report_profile_photo_rpcs() {
    let mut h = Wp58Harness::start();
    let self_peer = Wp58Harness::self_peer();
    let contact = Wp58Harness::contact_peer();
    let chan = Wp58Harness::channel_peer();

    // User -> `account.setMainProfileTab`
    h.rpc
        .enqueue_ok(tl_fns::account::SetMainProfileTab::CONSTRUCTOR_ID, &true);
    h.live
        .set_main_profile_tab(self_peer, ProfileMainTab::Gifts)
        .expect("account.setMainProfileTab");
    assert_eq!(
        h.live.main_profile_tab(self_peer),
        Some(ProfileMainTab::Gifts)
    );

    // Channel -> `channels.setMainProfileTab`
    h.rpc
        .enqueue_ok(tl_fns::channels::SetMainProfileTab::CONSTRUCTOR_ID, &true);
    h.live
        .set_main_profile_tab(chan, ProfileMainTab::Posts)
        .expect("channels.setMainProfileTab");
    assert_eq!(h.live.main_profile_tab(chan), Some(ProfileMainTab::Posts));

    let calls = h.rpc.recorded_calls();
    let acct_tab = tl_fns::account::SetMainProfileTab::from_bytes(&calls[0].1).expect("decode");
    assert!(matches!(acct_tab.tab, tl_enums::ProfileTab::Gifts));
    let chan_tab = tl_fns::channels::SetMainProfileTab::from_bytes(&calls[1].1).expect("decode");
    assert!(matches!(chan_tab.tab, tl_enums::ProfileTab::Posts));
    assert!(matches!(
        chan_tab.channel,
        tl_enums::InputChannel::InputChannel(ref c) if c.channel_id == CHANNEL_ID
    ));

    // Verify all 8 ProfileMainTab mappings match `data_peer.cpp:2327-2340`
    assert!(matches!(
        crate::profile_extras::profile_tab_to_tl(ProfileMainTab::Media),
        tl_enums::ProfileTab::Media
    ));
    assert!(matches!(
        crate::profile_extras::profile_tab_to_tl(ProfileMainTab::Files),
        tl_enums::ProfileTab::Files
    ));
    assert!(matches!(
        crate::profile_extras::profile_tab_to_tl(ProfileMainTab::Music),
        tl_enums::ProfileTab::Music
    ));
    assert!(matches!(
        crate::profile_extras::profile_tab_to_tl(ProfileMainTab::Voice),
        tl_enums::ProfileTab::Voice
    ));
    assert!(matches!(
        crate::profile_extras::profile_tab_to_tl(ProfileMainTab::Links),
        tl_enums::ProfileTab::Links
    ));
    assert!(matches!(
        crate::profile_extras::profile_tab_to_tl(ProfileMainTab::Gifs),
        tl_enums::ProfileTab::Gifs
    ));

    // `users.suggestBirthday` — invalid date rejected locally
    h.rpc.clear_recorded();
    assert!(
        h.live
            .suggest_user_birthday(
                contact,
                Birthday {
                    day: 31,
                    month: 2,
                    year: Some(1995),
                }
            )
            .is_err()
    );
    assert!(h.rpc.recorded_calls().is_empty());

    // Valid birthday with and without year
    h.rpc.enqueue_ok(
        tl_fns::users::SuggestBirthday::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    h.live
        .suggest_user_birthday(
            contact,
            Birthday {
                day: 14,
                month: 5,
                year: Some(1996),
            },
        )
        .expect("suggestBirthday with year");
    h.rpc.enqueue_ok(
        tl_fns::users::SuggestBirthday::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    h.live
        .suggest_user_birthday(
            contact,
            Birthday {
                day: 29,
                month: 2,
                year: None,
            },
        )
        .expect("suggestBirthday without year");

    let bday_calls: Vec<_> = h
        .rpc
        .recorded_calls()
        .into_iter()
        .filter(|(c, _)| *c == tl_fns::users::SuggestBirthday::CONSTRUCTOR_ID)
        .map(|(_, b)| tl_fns::users::SuggestBirthday::from_bytes(&b).expect("decode"))
        .collect();
    assert_eq!(bday_calls.len(), 2);
    assert!(matches!(
        bday_calls[0].birthday,
        tl_enums::Birthday::Birthday(tl_types::Birthday {
            day: 14,
            month: 5,
            year: Some(1996)
        })
    ));
    assert!(matches!(
        bday_calls[1].birthday,
        tl_enums::Birthday::Birthday(tl_types::Birthday {
            day: 29,
            month: 2,
            year: None
        })
    ));

    // `account.reportProfilePhoto` — zero ID rejected locally
    h.rpc.clear_recorded();
    assert!(
        h.live
            .report_profile_photo(
                contact,
                ProfilePhotoRef {
                    id: 0,
                    access_hash: 1,
                    file_reference: Vec::new(),
                },
                ReportReason::Spam,
                "bad",
            )
            .is_err()
    );
    assert!(h.rpc.recorded_calls().is_empty());

    h.rpc
        .enqueue_ok(tl_fns::account::ReportProfilePhoto::CONSTRUCTOR_ID, &true);
    h.live
        .report_profile_photo(
            contact,
            ProfilePhotoRef {
                id: 55_001,
                access_hash: 77_002,
                file_reference: vec![1, 2, 3],
            },
            ReportReason::Fake,
            "Impersonation photo",
        )
        .expect("reportProfilePhoto");

    let rep_call = h
        .rpc
        .recorded_calls()
        .into_iter()
        .find(|(c, _)| *c == tl_fns::account::ReportProfilePhoto::CONSTRUCTOR_ID)
        .expect("ReportProfilePhoto recorded");
    let decoded_rep = tl_fns::account::ReportProfilePhoto::from_bytes(&rep_call.1).expect("decode");
    assert!(matches!(
        decoded_rep.photo_id,
        tl_enums::InputPhoto::InputPhoto(ref p)
            if p.id == 55_001 && p.access_hash == 77_002 && p.file_reference == vec![1, 2, 3]
    ));
    assert!(matches!(
        decoded_rep.reason,
        tl_enums::ReportReason::InputReportReasonFake
    ));
    assert_eq!(decoded_rep.message, "Impersonation photo");
}

// =============================================================================
// 9. Business Conformance (non-quick-reply): Work Hours, Location, Intro, Timezones
// =============================================================================

#[test]
fn business_conformance_hours_location_intro_and_timezones() {
    let mut h = Wp58Harness::start();

    // Seed `load_overview` responses (full user with business details + timezones)
    let sticker_doc = tl_enums::Document::Document(tl_types::Document {
        id: 60_001,
        access_hash: 987_654,
        file_reference: vec![0xDE, 0xAD],
        date: 1_795_350_000,
        mime_type: "application/x-tgsticker".to_owned(),
        size: 1024,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: Vec::new(),
    });
    h.rpc.enqueue_ok(
        tl_fns::users::GetFullUser::CONSTRUCTOR_ID,
        &tl_enums::users::UserFull::UserFull(tl_types::users::UserFull {
            full_user: tl_enums::UserFull::UserFull(tl_types::UserFull {
                blocked: false,
                phone_calls_available: false,
                phone_calls_private: false,
                can_pin_message: true,
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
                id: SELF_ID,
                about: None,
                settings: tl_enums::PeerSettings::PeerSettings(tl_types::PeerSettings {
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
                business_work_hours: Some(tl_enums::BusinessWorkHours::BusinessWorkHours(
                    tl_types::BusinessWorkHours {
                        open_now: true,
                        timezone_id: "UTC".to_owned(),
                        weekly_open: vec![tl_enums::BusinessWeeklyOpen::BusinessWeeklyOpen(
                            tl_types::BusinessWeeklyOpen {
                                start_minute: 540,
                                end_minute: 1080,
                            },
                        )],
                    },
                )),
                business_location: Some(tl_enums::BusinessLocation::BusinessLocation(
                    tl_types::BusinessLocation {
                        geo_point: Some(tl_enums::GeoPoint::GeoPoint(tl_types::GeoPoint {
                            long: -0.1276,
                            lat: 51.5072,
                            access_hash: 0,
                            accuracy_radius: None,
                        })),
                        address: "100 Sample High St".to_owned(),
                    },
                )),
                business_greeting_message: None,
                business_away_message: None,
                business_intro: Some(tl_enums::BusinessIntro::BusinessIntro(
                    tl_types::BusinessIntro {
                        title: "Welcome".to_owned(),
                        description: "How can we help?".to_owned(),
                        sticker: Some(sticker_doc),
                    },
                )),
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
            users: vec![tl_enums::User::User(synthetic_tl_user(
                SELF_ID,
                "Jordan",
                "jordan_self",
                true,
                false,
            ))],
        }),
    );
    h.rpc.enqueue_ok(
        tl_fns::help::GetTimezonesList::CONSTRUCTOR_ID,
        &tl_enums::help::TimezonesList::TimezonesList(tl_types::help::TimezonesList {
            timezones: vec![tl_enums::Timezone::Timezone(tl_types::Timezone {
                id: "UTC".to_owned(),
                name: "Coordinated Universal Time".to_owned(),
                utc_offset: 0,
            })],
            hash: 7711,
        }),
    );
    h.rpc.enqueue_ok(
        tl_fns::account::GetConnectedBots::CONSTRUCTOR_ID,
        &tl_enums::account::ConnectedBots::ConnectedBots(tl_types::account::ConnectedBots {
            connected_bots: Vec::new(),
            users: Vec::new(),
        }),
    );
    h.rpc.enqueue_ok(
        tl_fns::messages::GetQuickReplies::CONSTRUCTOR_ID,
        &tl_enums::messages::QuickReplies::NotModified,
    );
    h.rpc.enqueue_ok(
        tl_fns::account::GetBusinessChatLinks::CONSTRUCTOR_ID,
        &tl_enums::account::BusinessChatLinks::BusinessChatLinks(
            tl_types::account::BusinessChatLinks {
                links: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );
    h.rpc.enqueue_ok(
        tl_fns::channels::GetInactiveChannels::CONSTRUCTOR_ID,
        &tl_enums::messages::InactiveChats::InactiveChats(tl_types::messages::InactiveChats {
            dates: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    h.rpc.enqueue_ok(
        tl_fns::channels::GetAdminedPublicChannels::CONSTRUCTOR_ID,
        &tl_enums::messages::Chats::Chats(tl_types::messages::Chats { chats: Vec::new() }),
    );
    h.rpc.enqueue_ok(
        tl_fns::account::GetDefaultEmojiStatuses::CONSTRUCTOR_ID,
        &tl_enums::account::EmojiStatuses::NotModified,
    );

    let snap = h.live.load_overview().expect("load_overview");
    assert_eq!(snap.details.hours.timezone_id, "UTC");
    assert_eq!(
        snap.details.hours.intervals.list,
        vec![WorkingInterval::new(540 * 60, 1080 * 60)]
    );
    assert_eq!(snap.details.location.address, "100 Sample High St");
    assert_eq!(snap.details.intro.sticker_id, Some(60_001));
    assert_eq!(snap.timezones.len(), 1);

    // 1. `account.updateBusinessWorkHours`: saves normalized minutes, and clears when empty
    h.rpc.clear_recorded();
    h.rpc.enqueue_ok(
        tl_fns::account::UpdateBusinessWorkHours::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .save_working_hours(WorkingHours {
            intervals: WorkingIntervals::new(vec![
                WorkingInterval::new(10 * 3600, 14 * 3600),
                WorkingInterval::new(12 * 3600, 18 * 3600), // overlapping -> merged to [600, 1080] min
            ]),
            timezone_id: "Europe/Berlin".to_owned(),
        })
        .expect("save_working_hours");
    h.rpc.enqueue_ok(
        tl_fns::account::UpdateBusinessWorkHours::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .save_working_hours(WorkingHours::default())
        .expect("clear working_hours");

    let wh_calls: Vec<_> = h
        .rpc
        .recorded_calls()
        .into_iter()
        .filter(|(c, _)| *c == tl_fns::account::UpdateBusinessWorkHours::CONSTRUCTOR_ID)
        .map(|(_, b)| tl_fns::account::UpdateBusinessWorkHours::from_bytes(&b).expect("decode"))
        .collect();
    assert_eq!(wh_calls.len(), 2);
    let Some(tl_enums::BusinessWorkHours::BusinessWorkHours(wh)) = &wh_calls[0].business_work_hours
    else {
        panic!("expected BusinessWorkHours");
    };
    assert_eq!(wh.timezone_id, "Europe/Berlin");
    assert_eq!(wh.weekly_open.len(), 1);
    assert!(matches!(
        wh.weekly_open[0],
        tl_enums::BusinessWeeklyOpen::BusinessWeeklyOpen(tl_types::BusinessWeeklyOpen {
            start_minute: 600,
            end_minute: 1080,
        })
    ));
    assert!(wh_calls[1].business_work_hours.is_none());

    // 2. `account.updateBusinessLocation`: address+pin vs empty clear
    h.rpc.clear_recorded();
    h.rpc.enqueue_ok(
        tl_fns::account::UpdateBusinessLocation::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .save_location(BusinessLocation {
            address: " 200 Elm Ave ".to_owned(),
            point: Some(GeoPoint {
                lat: 48.1351,
                lon: 11.5820,
            }),
        })
        .expect("save_location");
    h.rpc.enqueue_ok(
        tl_fns::account::UpdateBusinessLocation::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .save_location(BusinessLocation {
            address: "   ".to_owned(),
            point: Some(GeoPoint { lat: 1.0, lon: 2.0 }),
        })
        .expect("clear location");
    let loc_calls: Vec<_> = h
        .rpc
        .recorded_calls()
        .into_iter()
        .filter(|(c, _)| *c == tl_fns::account::UpdateBusinessLocation::CONSTRUCTOR_ID)
        .map(|(_, b)| tl_fns::account::UpdateBusinessLocation::from_bytes(&b).expect("decode"))
        .collect();
    assert_eq!(loc_calls[0].address.as_deref(), Some("200 Elm Ave"));
    assert!(matches!(
        loc_calls[0].geo_point,
        Some(tl_enums::InputGeoPoint::InputGeoPoint(ref g))
            if (g.lat - 48.1351).abs() < 1e-6 && (g.long - 11.5820).abs() < 1e-6
    ));
    assert!(loc_calls[1].address.is_none());
    assert!(loc_calls[1].geo_point.is_none());

    // 3. `account.updateBusinessIntro`: cached sticker meta, `Some(0)` omitted, and clear
    h.rpc.clear_recorded();
    h.rpc
        .enqueue_ok(tl_fns::account::UpdateBusinessIntro::CONSTRUCTOR_ID, &true);
    h.live
        .save_chat_intro(ChatIntro {
            title: "Hello".to_owned(),
            description: "Ask anything".to_owned(),
            sticker_id: Some(60_001),
        })
        .expect("save_chat_intro with sticker");
    h.rpc
        .enqueue_ok(tl_fns::account::UpdateBusinessIntro::CONSTRUCTOR_ID, &true);
    h.live
        .save_chat_intro(ChatIntro {
            title: "Hello".to_owned(),
            description: String::new(),
            sticker_id: Some(0), // regression: 0 = Random must omit f_sticker!
        })
        .expect("save_chat_intro random sticker");
    h.rpc
        .enqueue_ok(tl_fns::account::UpdateBusinessIntro::CONSTRUCTOR_ID, &true);
    h.live
        .save_chat_intro(ChatIntro {
            title: String::new(),
            description: String::new(),
            sticker_id: Some(0),
        })
        .expect("clear intro when only synthetic 0");

    let intro_calls: Vec<_> = h
        .rpc
        .recorded_calls()
        .into_iter()
        .filter(|(c, _)| *c == tl_fns::account::UpdateBusinessIntro::CONSTRUCTOR_ID)
        .map(|(_, b)| tl_fns::account::UpdateBusinessIntro::from_bytes(&b).expect("decode"))
        .collect();
    assert_eq!(intro_calls.len(), 3);
    assert!(matches!(
        intro_calls[0].intro,
        Some(tl_enums::InputBusinessIntro::InputBusinessIntro(ref i))
            if matches!(
                i.sticker,
                Some(tl_enums::InputDocument::InputDocument(ref d))
                    if d.id == 60_001 && d.access_hash == 987_654 && d.file_reference == vec![0xDE, 0xAD]
            )
    ));
    assert!(matches!(
        intro_calls[1].intro,
        Some(tl_enums::InputBusinessIntro::InputBusinessIntro(ref i)) if i.sticker.is_none()
    ));
    assert!(intro_calls[2].intro.is_none());
}

// =============================================================================
// 10. Business Conformance: Greeting, Away, Chat Links & Connected Chatbots
// =============================================================================

#[test]
fn business_conformance_greeting_away_links_and_chatbots() {
    let mut h = Wp58Harness::start();

    // 1. Greeting message: regression check (enabled with shortcut_id=0 must NOT clear server!)
    h.rpc.clear_recorded();
    let greet_err = h
        .live
        .save_greeting_settings(GreetingSettings {
            recipients: BusinessRecipients::default(),
            no_activity_days: 14,
            shortcut_id: 0,
        })
        .expect_err("enabled greeting without shortcut_id must fail");
    assert!(greet_err.contains("shortcut"));
    assert!(h.rpc.recorded_calls().is_empty());

    // Valid greeting + disable greeting
    h.rpc.enqueue_ok(
        tl_fns::account::UpdateBusinessGreetingMessage::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .save_greeting_settings(GreetingSettings {
            recipients: BusinessRecipients {
                included: BusinessChats {
                    types: BusinessChatTypes::NEW_CHATS.union(BusinessChatTypes::NON_CONTACTS),
                    user_ids: vec![CONTACT_ID],
                },
                excluded: BusinessChats::default(),
                all_but_excluded: false,
            },
            no_activity_days: 7,
            shortcut_id: 101,
        })
        .expect("save greeting");

    h.rpc.enqueue_ok(
        tl_fns::account::UpdateBusinessGreetingMessage::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .save_greeting_settings(GreetingSettings {
            recipients: BusinessRecipients::default(),
            no_activity_days: 0,
            shortcut_id: 0,
        })
        .expect("disable greeting");

    let greet_calls: Vec<_> = h
        .rpc
        .recorded_calls()
        .into_iter()
        .filter(|(c, _)| *c == tl_fns::account::UpdateBusinessGreetingMessage::CONSTRUCTOR_ID)
        .map(|(_, b)| {
            tl_fns::account::UpdateBusinessGreetingMessage::from_bytes(&b).expect("decode")
        })
        .collect();
    assert_eq!(greet_calls.len(), 2);
    assert!(matches!(
        greet_calls[0].message,
        Some(tl_enums::InputBusinessGreetingMessage::InputBusinessGreetingMessage(ref g))
            if g.shortcut_id == 101 && g.no_activity_days == 7
    ));
    assert!(greet_calls[1].message.is_none());

    // 2. Away message: regression check (enabled with shortcut_id=0 must NOT clear server!)
    h.rpc.clear_recorded();
    assert!(
        h.live
            .save_away_settings(AwaySettings {
                recipients: BusinessRecipients::default(),
                schedule: AwaySchedule {
                    schedule_type: AwayScheduleType::Always,
                    custom_interval: WorkingInterval::default(),
                },
                shortcut_id: 0,
                offline_only: true,
            })
            .is_err()
    );
    assert!(h.rpc.recorded_calls().is_empty());

    // Custom interval validation + valid custom + OutsideWorkingHours + Never disable
    assert!(
        h.live
            .save_away_settings(AwaySettings {
                recipients: BusinessRecipients::default(),
                schedule: AwaySchedule {
                    schedule_type: AwayScheduleType::Custom,
                    custom_interval: WorkingInterval::new(100, 200),
                },
                shortcut_id: 102,
                offline_only: false,
            })
            .is_err()
    );

    h.rpc.enqueue_ok(
        tl_fns::account::UpdateBusinessAwayMessage::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .save_away_settings(AwaySettings {
            recipients: BusinessRecipients::default(),
            schedule: AwaySchedule {
                schedule_type: AwayScheduleType::Custom,
                custom_interval: WorkingInterval::new(
                    AWAY_START_TIME_MIN + 10_000,
                    AWAY_START_TIME_MIN + 20_000,
                ),
            },
            shortcut_id: 102,
            offline_only: true,
        })
        .expect("save custom away");

    h.rpc.enqueue_ok(
        tl_fns::account::UpdateBusinessAwayMessage::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .save_away_settings(AwaySettings {
            recipients: BusinessRecipients::default(),
            schedule: AwaySchedule {
                schedule_type: AwayScheduleType::Never,
                custom_interval: WorkingInterval::default(),
            },
            shortcut_id: 102,
            offline_only: false,
        })
        .expect("disable away");

    let away_calls: Vec<_> = h
        .rpc
        .recorded_calls()
        .into_iter()
        .filter(|(c, _)| *c == tl_fns::account::UpdateBusinessAwayMessage::CONSTRUCTOR_ID)
        .map(|(_, b)| tl_fns::account::UpdateBusinessAwayMessage::from_bytes(&b).expect("decode"))
        .collect();
    assert_eq!(away_calls.len(), 2);
    assert!(matches!(
        away_calls[0].message,
        Some(tl_enums::InputBusinessAwayMessage::InputBusinessAwayMessage(ref a))
            if a.offline_only
                && a.shortcut_id == 102
                && matches!(
                    a.schedule,
                    tl_enums::BusinessAwayMessageSchedule::Custom(
                        tl_types::BusinessAwayMessageScheduleCustom { start_date, end_date }
                    ) if start_date == AWAY_START_TIME_MIN + 10_000 && end_date == AWAY_START_TIME_MIN + 20_000
                )
    ));
    assert!(away_calls[1].message.is_none());

    // 3. Business Chat Links (create, edit, delete, resolve)
    h.rpc.enqueue_ok(
        tl_fns::account::CreateBusinessChatLink::CONSTRUCTOR_ID,
        &tl_enums::BusinessChatLink::BusinessChatLink(tl_types::BusinessChatLink {
            link: "https://t.me/m/abc123".to_owned(),
            message: "Order inquiry".to_owned(),
            entities: None,
            title: Some("Orders".to_owned()),
            views: 0,
        }),
    );
    let created = h
        .live
        .create_chat_link("Orders", "Order inquiry")
        .expect("create_chat_link");
    assert_eq!(created.link, "https://t.me/m/abc123");

    h.rpc.enqueue_ok(
        tl_fns::account::EditBusinessChatLink::CONSTRUCTOR_ID,
        &tl_enums::BusinessChatLink::BusinessChatLink(tl_types::BusinessChatLink {
            link: "https://t.me/m/abc123".to_owned(),
            message: "Updated inquiry".to_owned(),
            entities: None,
            title: Some("Sales".to_owned()),
            views: 12,
        }),
    );
    let edited = h
        .live
        .edit_chat_link("https://t.me/m/abc123", "Sales", "Updated inquiry")
        .expect("edit_chat_link");
    assert_eq!(edited.title, "Sales");
    assert_eq!(edited.clicks, 12);

    h.rpc.enqueue_ok(
        tl_fns::account::DeleteBusinessChatLink::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .delete_chat_link("https://t.me/m/abc123")
        .expect("delete_chat_link");

    // 4. Connected Chatbots (`account.updateConnectedBot`, pause/disable per peer)
    h.rpc.clear_recorded();
    h.rpc.enqueue_ok(
        tl_fns::account::UpdateConnectedBot::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    let mut perms = ChatbotsPermissions::VIEW_MESSAGES;
    perms.set(ChatbotsPermissions::REPLY_TO_MESSAGES, true);
    perms.set(ChatbotsPermissions::MARK_AS_READ, true);
    perms.set(ChatbotsPermissions::MANAGE_STORIES, true);
    h.live
        .save_chatbots(ChatbotsSettings {
            bot: Some(BusinessBotCandidate {
                id: BOT_ID,
                name: "Desk Bot".to_owned(),
                username: "desk_biz_bot".to_owned(),
                about: String::new(),
                supports_business: true,
                avatar_rgb: [0x40, 0x80, 0xC0],
            }),
            recipients: BusinessRecipients {
                included: BusinessChats {
                    types: BusinessChatTypes::CONTACTS,
                    user_ids: Vec::new(),
                },
                excluded: BusinessChats {
                    types: BusinessChatTypes::EMPTY,
                    user_ids: vec![CONTACT_ID],
                },
                all_but_excluded: false,
            },
            permissions: perms,
        })
        .expect("connect chatbot");

    // Disconnect chatbot (`deleted: true`, `rights: None`)
    h.rpc.enqueue_ok(
        tl_fns::account::UpdateConnectedBot::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    h.live
        .save_chatbots(ChatbotsSettings::default())
        .expect("disconnect chatbot");

    let bot_calls: Vec<_> = h
        .rpc
        .recorded_calls()
        .into_iter()
        .filter(|(c, _)| *c == tl_fns::account::UpdateConnectedBot::CONSTRUCTOR_ID)
        .map(|(_, b)| tl_fns::account::UpdateConnectedBot::from_bytes(&b).expect("decode"))
        .collect();
    assert_eq!(bot_calls.len(), 2);
    assert!(!bot_calls[0].deleted);
    assert!(matches!(
        bot_calls[0].rights,
        Some(tl_enums::BusinessBotRights::BusinessBotRights(ref r))
            if r.reply && r.read_messages && r.manage_stories && !r.transfer_stars
    ));
    assert!(matches!(
        bot_calls[0].recipients,
        tl_enums::InputBusinessBotRecipients::InputBusinessBotRecipients(ref r)
            if r.contacts && !r.exclude_selected && r.exclude_users.as_ref().is_some_and(|u| u.len() == 1)
    ));
    assert!(bot_calls[1].deleted);
    assert!(bot_calls[1].rights.is_none());
    assert!(matches!(
        bot_calls[1].recipients,
        tl_enums::InputBusinessBotRecipients::InputBusinessBotRecipients(ref r)
            if !r.exclude_selected && r.users.is_none() && r.exclude_users.is_none()
    ));

    // Per-peer pause/resume and disable
    let contact = Wp58Harness::contact_peer();
    h.rpc.enqueue_ok(
        tl_fns::account::ToggleConnectedBotPaused::CONSTRUCTOR_ID,
        &true,
    );
    toggle_business_bot_paused(&h.live, contact, true).expect("pause bot");
    h.rpc.enqueue_ok(
        tl_fns::account::DisablePeerConnectedBot::CONSTRUCTOR_ID,
        &true,
    );
    disable_business_bot(&h.live, contact).expect("disable bot");
}
