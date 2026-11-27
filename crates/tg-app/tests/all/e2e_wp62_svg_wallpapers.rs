//! WP-62 end-to-end integration tests:
//! - Synthetic `.tgv` (gzipped SVG) pattern wallpaper downloaded via `FakeRpcClient`
//!   (`account.getWallPapers` + `upload.getFile`) and rasterized directly via
//!   `tg_theme::svg_pattern` inside `ChatBackground` for both positive (`+40`) and
//!   negative (`-50` dark-mode) intensity without procedural `PatternKind` fallback,
//!   plus a deterministic headless `egui_kittest` wgpu snapshot render check.
//! - Headless `egui_kittest` UI flow driving `ThemesStudio` against `LiveSession::themes_backend()`
//!   backed by `FakeRpcClient` for:
//!   - `account.getUniqueGiftChatThemes` + selecting a unique gift theme + `messages.setChatTheme`
//!     (`inputChatThemeUniqueGift`)
//!   - `messages.getShareThemeBackdrops` + `help.getPeerColors` + `help.getPeerProfileColors`
//!     + selecting a circular backdrop swatch + live preview texture update + `messages.setShareTheme`
//!   - `account.saveTheme` save/unsave dispatch.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::Vec2;
use egui_kittest::HarnessBuilder;
use egui_kittest::kittest::Queryable;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::UserId as DataUserId;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_theme::svg_pattern::{PatternTileMode, render_tiled_pattern_wallpaper};
use tg_theme::{WallPaper, WallPaperFlags};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_ui::fonts::register_tdesktop_fonts;
use tokio::sync::broadcast;

use tg_app::live::session::LiveSession;
use tg_app::stand_in::types::PeerId;
use tg_app::themes::{
    BackgroundConfig, ChatBackground, ChatThemeChoice, PatternKind, ThemesStudio, ThemesTab,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const SELF_UID: i64 = 720_001;
const PEER_UID: i64 = 720_002;

static WGPU_LOCK: Mutex<()> = Mutex::new(());

fn sample_svg_pattern_bytes() -> Vec<u8> {
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128">
  <circle cx="32" cy="32" r="18" fill="#000000"/>
  <rect x="76" y="16" width="32" height="32" rx="8" fill="#000000"/>
  <polygon points="32,74 44,108 16,86 48,86 20,108" fill="#000000"/>
  <circle cx="92" cy="92" r="16" fill="none" stroke="#000000" stroke-width="6"/>
</svg>"##
        .to_vec()
}

/// Minimal RFC-1952 gzip encoder (stored deflate block + IEEE CRC-32) producing valid `.tgv` bytes.
fn gzip_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 32);
    out.extend_from_slice(&[0x1F, 0x8B, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xFF]);
    let mut offset = 0;
    loop {
        let remain = data.len().saturating_sub(offset);
        let chunk_len = remain.min(0xFFFF);
        let is_final = offset + chunk_len >= data.len();
        out.push(if is_final { 0x01 } else { 0x00 });
        let len = chunk_len as u16;
        let nlen = !len;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&nlen.to_le_bytes());
        out.extend_from_slice(&data[offset..offset + chunk_len]);
        offset += chunk_len;
        if is_final {
            break;
        }
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    crc = !crc;
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

fn synthetic_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
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
        status: Some(tl_enums::UserStatus::Recently(
            tl_types::UserStatusRecently { by_me: false },
        )),
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

fn synthetic_dialog(peer_uid: i64, top_message: i32) -> tl_enums::Dialog {
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

fn synthetic_message(id: i32, peer_uid: i64, text: &str) -> tl_enums::Message {
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
            user_id: peer_uid,
        })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date: 1_750_000_000,
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
            user_id: SELF_UID,
        })),
        owner_name: Some("Nova Self".to_owned()),
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

fn synthetic_document(id: i64, access_hash: i64, mime: &str, size: i64) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash,
        file_reference: vec![9, 8, 7, 6],
        date: 1_750_000_000,
        mime_type: mime.to_owned(),
        size,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: Vec::new(),
    })
}

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
type RecordedCall = (u32, Vec<u8>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedCall>>>,
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

    fn recorded_for(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, bytes)| bytes.clone())
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
                None
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

struct LiveThemesHarness {
    peer: PeerId,
    fake: FakeRpcClient,
    runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    live: LiveSession,
}

impl LiveThemesHarness {
    fn start() -> Self {
        let self_user = synthetic_user(SELF_UID, "Nova", "nova_self", true);
        let peer_user = synthetic_user(PEER_UID, "Orbit", "orbit_peer", false);
        let top_msg = synthetic_message(10, PEER_UID, "WP-62 live themes test");
        let dialog = synthetic_dialog(PEER_UID, 10);

        let fake = FakeRpcClient::new(2);
        fake.enqueue_ok(
            tl_fns::users::GetUsers::CONSTRUCTOR_ID,
            &vec![tl_enums::User::User(self_user)],
        );
        fake.enqueue_ok(
            tl_fns::updates::GetState::CONSTRUCTOR_ID,
            &tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
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
                dialogs: vec![dialog],
                messages: vec![top_msg],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(peer_user)],
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
                    date: 1_750_000_001,
                    seq: 1,
                    unread_count: 0,
                }),
            }),
        );

        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId::from_tl(SELF_UID),
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

        let live = LiveSession::new(Arc::clone(&runtime), 62);
        Self {
            peer: PeerId::from_user(PEER_UID as u64),
            fake,
            runtime,
            live,
        }
    }

    fn shutdown(self) {
        drop(self.live);
        if let Ok(rt) = Arc::try_unwrap(self.runtime) {
            rt.shutdown();
        }
    }
}

#[test]
fn wp62_fake_rpc_tgv_pattern_wallpaper_download_rasterize_and_wgpu_snapshot() {
    crate::common::init_headless_env();
    let h = LiveThemesHarness::start();
    let tgv_bytes = gzip_stored(&sample_svg_pattern_bytes());

    // Enqueue getThemes / getChatThemes / getWallPapers + upload.getFile for the `.tgv` document
    h.fake.enqueue_ok(
        tl_fns::account::GetThemes::CONSTRUCTOR_ID,
        &tl_enums::account::Themes::NotModified,
    );
    h.fake.enqueue_ok(
        tl_fns::account::GetChatThemes::CONSTRUCTOR_ID,
        &tl_enums::account::Themes::NotModified,
    );
    h.fake.enqueue_ok(
        tl_fns::account::GetWallPapers::CONSTRUCTOR_ID,
        &tl_enums::account::WallPapers::WallPapers(tl_types::account::WallPapers {
            hash: 62_001,
            wallpapers: vec![tl_enums::WallPaper::WallPaper(tl_types::WallPaper {
                id: 9001,
                creator: false,
                default: false,
                pattern: true,
                dark: false,
                access_hash: 77_001,
                slug: "meadow-vector".to_owned(),
                document: synthetic_document(
                    55_001,
                    77_001,
                    "application/x-tgwallpattern",
                    tgv_bytes.len() as i64,
                ),
                settings: Some(tl_enums::WallPaperSettings::WallPaperSettings(
                    tl_types::WallPaperSettings {
                        blur: false,
                        motion: true,
                        background_color: Some(0xDB_DD_BB),
                        second_background_color: Some(0x6B_A5_87),
                        third_background_color: Some(0xD5_D8_8D),
                        fourth_background_color: Some(0x88_B8_84),
                        intensity: Some(40),
                        rotation: Some(45),
                        emoticon: None,
                    },
                )),
            })],
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FilePartial,
            mtime: 1_750_000_000,
            bytes: tgv_bytes.clone(),
        }),
    );

    let mut studio = ThemesStudio::new();
    studio.set_backend(h.live.themes_backend());
    studio
        .refresh_cloud_catalogs()
        .expect("refresh cloud catalogs with tgv wallpaper");

    // Confirm `upload.getFile` was issued over `FakeRpcClient` and `.tgv` bytes hydrated
    assert_eq!(
        h.fake
            .recorded_for(tl_fns::upload::GetFile::CONSTRUCTOR_ID)
            .len(),
        1,
        "expected upload.getFile for .tgv pattern document"
    );
    let item = studio
        .bg_picker
        .items()
        .iter()
        .find(|it| it.id == "cloud_wp_9001")
        .expect("cloud_wp_9001 hydrated into picker");
    assert!(item.config.has_svg_pattern());
    assert_eq!(item.config.pattern, PatternKind::None);

    // Positive (+40) composite must match `tg_theme::svg_pattern::render_tiled_pattern_wallpaper`
    let mut bg = ChatBackground::new();
    let eff_rot = tg_theme::compute_real_rotation(
        item.config.wallpaper.background_colors.len(),
        item.config.wallpaper.rotation,
        0,
    );
    let rendered_pos = bg.render_image(&item.config, 192, 192, 0);
    let expected_pos = render_tiled_pattern_wallpaper(
        &tgv_bytes,
        192,
        192,
        &item.config.wallpaper.background_colors,
        eff_rot,
        40,
        Some((93, 192)), // pattern scaled to the area height at 1440:2960 (`CacheBackgroundByRequest`)
        PatternTileMode::Centered,
    )
    .expect("direct svg_pattern render +40");
    assert_eq!(rendered_pos.as_raw(), expected_pos.as_raw());

    // Negative (-50 dark-mode) composite must also match `render_tiled_pattern_wallpaper`
    let mut wp_dark: WallPaper = item.config.wallpaper.clone().with_pattern_intensity(-50);
    wp_dark.flags = WallPaperFlags::PATTERN.union(WallPaperFlags::DARK);
    let cfg_dark =
        BackgroundConfig::from_svg_pattern(wp_dark.clone(), tgv_bytes.clone(), false).expect("-50");
    let rendered_neg = bg.render_image(&cfg_dark, 192, 192, 0);
    let expected_neg = render_tiled_pattern_wallpaper(
        &tgv_bytes,
        192,
        192,
        &wp_dark.background_colors,
        eff_rot,
        -50,
        Some((93, 192)), // pattern scaled to the area height at 1440:2960 (`CacheBackgroundByRequest`)
        PatternTileMode::Centered,
    )
    .expect("direct svg_pattern render -50");
    assert_eq!(rendered_neg.as_raw(), expected_neg.as_raw());
    assert_ne!(rendered_pos.as_raw(), rendered_neg.as_raw());

    // Deterministic headless `egui_kittest` wgpu snapshot render of the rasterizer-backed background
    {
        let _guard = WGPU_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut snap_bg = ChatBackground::new();
        snap_bg.set_dark_mode(true);
        snap_bg.set_global(cfg_dark);

        let mut harness = HarnessBuilder::default()
            .with_size(Vec2::new(256.0, 256.0))
            .with_step_dt(1.0 / 30.0)
            .with_render_cursor(false)
            .with_accessibility_check(false)
            .renderer(crate::common::serialized_wgpu_renderer())
            .build_ui_state(
                |ui, chat_bg: &mut ChatBackground| {
                    let rect = ui.max_rect();
                    chat_bg.paint(ui, rect, None);
                },
                snap_bg,
            );
        harness.step();
        harness.step();
        let img = harness.render().expect("wgpu render tgv background");
        assert_eq!(img.dimensions(), (256, 256));
        let (mut lo, mut hi) = (u32::MAX, 0u32);
        for px in img.pixels() {
            let luma = u32::from(px[0]) + u32::from(px[1]) + u32::from(px[2]);
            lo = lo.min(luma);
            hi = hi.max(luma);
        }
        assert!(
            hi.saturating_sub(lo) >= 40,
            "expected visible gradient+pattern contrast in wgpu snapshot, got {lo}..={hi}"
        );
    }

    h.shutdown();
}

#[test]
fn wp62_fake_rpc_kittest_unique_gift_theme_and_share_theme_backdrops_e2e() {
    crate::common::init_headless_env();
    let h = LiveThemesHarness::start();

    // Stub initial studio catalog RPCs + all WP-58 endpoints
    h.fake.enqueue_ok(
        tl_fns::account::GetThemes::CONSTRUCTOR_ID,
        &tl_enums::account::Themes::NotModified,
    );
    h.fake.enqueue_ok(
        tl_fns::account::GetChatThemes::CONSTRUCTOR_ID,
        &tl_enums::account::Themes::NotModified,
    );
    h.fake.enqueue_ok(
        tl_fns::account::GetWallPapers::CONSTRUCTOR_ID,
        &tl_enums::account::WallPapers::NotModified,
    );
    h.fake.enqueue_ok(
        tl_fns::account::GetUniqueGiftChatThemes::CONSTRUCTOR_ID,
        &tl_enums::account::ChatThemes::ChatThemes(tl_types::account::ChatThemes {
            hash: 91_001,
            themes: vec![tl_enums::ChatTheme::UniqueGift(
                tl_types::ChatThemeUniqueGift {
                    gift: synthetic_unique_star_gift(801, "PlushPepe-77", "Emerald Relic", 77),
                    theme_settings: vec![
                        tl_enums::ThemeSettings::ThemeSettings(tl_types::ThemeSettings {
                            message_colors_animated: false,
                            base_theme: tl_enums::BaseTheme::Day,
                            accent_color: 0x2F_9E_6F,
                            outbox_accent_color: Some(0x2F_9E_6F),
                            message_colors: Some(vec![0x2F_9E_6F]),
                            wallpaper: None,
                        }),
                        tl_enums::ThemeSettings::ThemeSettings(tl_types::ThemeSettings {
                            message_colors_animated: true,
                            base_theme: tl_enums::BaseTheme::Night,
                            accent_color: 0x1E_4A_38,
                            outbox_accent_color: Some(0x24_52_3D),
                            message_colors: Some(vec![0x1E_4A_38, 0x24_52_3D]),
                            wallpaper: None,
                        }),
                    ],
                },
            )],
            chats: Vec::new(),
            users: Vec::new(),
            next_offset: None,
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::GetShareThemeBackdrops::CONSTRUCTOR_ID,
        &tl_enums::ShareThemeBackdrops::ShareThemeBackdrops(tl_types::ShareThemeBackdrops {
            hash: 88_110,
            backdrops: vec![
                tl_enums::ShareThemeBackdropOption::ShareThemeBackdropOption(
                    tl_types::ShareThemeBackdropOption {
                        backdrop_id: 10,
                        name: "Azure Reef".to_owned(),
                        center_color: 0x2A_9D_F4,
                        edge_color: 0x12_4E_89,
                        pattern_color: 0x0B_30_57,
                        text_color: 0xFF_FF_FF,
                    },
                ),
                tl_enums::ShareThemeBackdropOption::ShareThemeBackdropOption(
                    tl_types::ShareThemeBackdropOption {
                        backdrop_id: 11,
                        name: "Amber Dusk".to_owned(),
                        center_color: 0xF0_8A_4B,
                        edge_color: 0x8E_2E_24,
                        pattern_color: 0x54_19_14,
                        text_color: 0xFF_FF_FF,
                    },
                ),
            ],
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::help::GetPeerColors::CONSTRUCTOR_ID,
        &tl_enums::help::PeerColors::PeerColors(tl_types::help::PeerColors {
            hash: 3101,
            colors: vec![tl_enums::help::PeerColorOption::PeerColorOption(
                tl_types::help::PeerColorOption {
                    hidden: false,
                    color_id: 2,
                    colors: Some(tl_enums::help::PeerColorSet::PeerColorSet(
                        tl_types::help::PeerColorSet {
                            colors: vec![0x40_A9_20],
                        },
                    )),
                    dark_colors: None,
                    channel_min_level: None,
                    group_min_level: None,
                },
            )],
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::help::GetPeerProfileColors::CONSTRUCTOR_ID,
        &tl_enums::help::PeerColors::PeerColors(tl_types::help::PeerColors {
            hash: 3202,
            colors: vec![tl_enums::help::PeerColorOption::PeerColorOption(
                tl_types::help::PeerColorOption {
                    hidden: false,
                    color_id: 0,
                    colors: Some(tl_enums::help::PeerColorSet::PeerColorProfileSet(
                        tl_types::help::PeerColorProfileSet {
                            palette_colors: vec![0xB8_47_40],
                            bg_colors: vec![0xD8_64_5A, 0x8C_2D_29],
                            story_colors: vec![0xFF_8A_78],
                        },
                    )),
                    dark_colors: None,
                    channel_min_level: None,
                    group_min_level: None,
                },
            )],
        }),
    );

    // Enqueue commit responses for `messages.setChatTheme`, `messages.setShareTheme`, `account.saveTheme`
    h.fake.enqueue_ok(
        tl_fns::messages::SetChatTheme::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_002,
            seq: 2,
        }),
    );
    h.fake
        .enqueue_ok(tl_fns::messages::SetShareTheme::CONSTRUCTOR_ID, &true);
    h.fake
        .enqueue_ok(tl_fns::account::SaveTheme::CONSTRUCTOR_ID, &true);

    let mut studio = ThemesStudio::new();
    studio.set_backend(h.live.themes_backend());
    studio.set_target_peer(h.peer.0, "Orbit Tester");
    studio.active_tab = ThemesTab::ChatThemes;
    studio
        .refresh_cloud_catalogs()
        .expect("refresh all catalogs via FakeRpcClient");

    assert_eq!(studio.chat_theme_picker.gift_themes().len(), 1);
    assert_eq!(studio.share_theme_picker.backdrops().len(), 2);
    assert_eq!(studio.share_theme_picker.peer_colors().len(), 1);
    assert_eq!(studio.share_theme_picker.peer_profile_colors().len(), 1);

    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(1100.0, 900.0))
        .with_step_dt(1.0 / 30.0)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            crate::common::apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut ThemesStudio| {
                st.show_at(ui, true, 1_000);
                crate::common::apply_deterministic_style(ui.ctx());
            },
            studio,
        );

    harness.step();
    harness.step();

    // 1. Select Unique Gift theme via `egui_kittest` and apply -> verify `messages.setChatTheme`
    let gift_gen_before = harness.state().chat_theme_picker.preview_generation();
    harness.get_by_label("Emerald Relic gift theme #77").click();
    harness.step();
    harness.step();
    assert_eq!(
        harness.state().chat_theme_picker.selected_choice(),
        &ChatThemeChoice::UniqueGift("PlushPepe-77".to_owned())
    );
    assert!(harness.state().chat_theme_picker.preview_generation() > gift_gen_before);

    let apply_chat_label = tg_lang::tr::lng_chat_theme_apply();
    harness.get_by_label(&apply_chat_label).click();
    harness.step();
    harness.step();

    let chat_theme_calls = h
        .fake
        .recorded_for(tl_fns::messages::SetChatTheme::CONSTRUCTOR_ID);
    assert_eq!(chat_theme_calls.len(), 1);
    let decoded_chat_theme = tl_fns::messages::SetChatTheme::from_bytes(&chat_theme_calls[0])
        .expect("decode SetChatTheme");
    assert!(matches!(
        decoded_chat_theme.theme,
        tl_enums::InputChatTheme::UniqueGift(ref g) if g.slug == "PlushPepe-77"
    ));

    // 2. Select Share Theme backdrop swatch -> verify preview texture regenerates -> Apply -> `messages.setShareTheme`
    let share_gen_before = harness.state().share_theme_picker.preview_generation();
    harness.get_by_label("Share backdrop: Amber Dusk").click();
    harness.step();
    harness.step();
    assert_eq!(
        harness.state().share_theme_picker.selected_backdrop_id(),
        Some(11)
    );
    assert!(
        harness.state().share_theme_picker.preview_generation() > share_gen_before,
        "selecting share backdrop swatch must regenerate preview texture"
    );

    harness.get_by_label("Apply Share Theme").click();
    harness.step();
    harness.step();

    let share_calls = h
        .fake
        .recorded_for(tl_fns::messages::SetShareTheme::CONSTRUCTOR_ID);
    assert_eq!(share_calls.len(), 1);
    let decoded_share =
        tl_fns::messages::SetShareTheme::from_bytes(&share_calls[0]).expect("decode SetShareTheme");
    assert!(matches!(
        decoded_share.theme,
        tl_enums::ShareTheme::EmojiBackdrop(ref e) if e.backdrop_id == Some(11)
    ));

    // 3. Verify `account.saveTheme` dispatch via `ThemesStudio::save_cloud_theme_live`
    harness
        .state_mut()
        .save_cloud_theme_live(901, 4444, false)
        .expect("save_cloud_theme_live");
    let save_calls = h
        .fake
        .recorded_for(tl_fns::account::SaveTheme::CONSTRUCTOR_ID);
    assert_eq!(save_calls.len(), 1);
    let decoded_save =
        tl_fns::account::SaveTheme::from_bytes(&save_calls[0]).expect("decode SaveTheme");
    assert!(!decoded_save.unsave);
    assert!(matches!(
        decoded_save.theme,
        tl_enums::InputTheme::InputTheme(ref t) if t.id == 901 && t.access_hash == 4444
    ));

    // Confirm peer colors & profile colors RPCs were issued during catalog hydration
    assert_eq!(
        h.fake
            .recorded_for(tl_fns::help::GetPeerColors::CONSTRUCTOR_ID)
            .len(),
        1
    );
    assert_eq!(
        h.fake
            .recorded_for(tl_fns::help::GetPeerProfileColors::CONSTRUCTOR_ID)
            .len(),
        1
    );

    h.shutdown();
}
