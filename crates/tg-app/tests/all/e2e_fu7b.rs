//! End-to-end `egui_kittest` tests and headless `wgpu` before/after snapshot capture for FU-7b
//! (`own_fu7b`, open UI follow-ups):
//!
//! 1. Settings toggle rows (`settings_toggle_row`, `settings_split_toggle_row`, power-saving group
//!    header, and business toggle rows): wrap/constrain title & subtitle strictly left of the
//!    right-hand toggle so text never runs under the toggle at 390 px.
//! 2. Unsupported story viewer (`stories/viewer.rs`): match tdesktop's `Controller::Unsupported`
//!    (`media_stories_controller.cpp:230-290`, `media_view.style:1033-1047`) with localized
//!    unsupported copy and an interactive `"Update Rustgram"` button wired to `request_update_check`.
//! 3. Profile QR -> Share Theme backdrop picker (`PeerQrBox` -> `OpenShareTheme` ->
//!    `AppThemesHost::open_share_theme_for_peer`): verify narrow-width clamping and auto-scroll.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use egui::{Event, MouseWheelUnit, Pos2, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::boxes::{BoxKind, peer_qr::PeerQrBoxState};
use tg_app::info::InfoBackend;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::theme::Palette;
use tg_app::stories::AppStoriesHost;
use tg_app::stories::mock::mock_peers;
use tg_app::themes::{AppThemesHost, ThemesTab};
use tg_app::window::section::SettingsPage;
use tg_data::{FullStoryId, StoryId, TextWithEntities};
use tg_stories::{
    ParseSource, ParsedPeerStories, ParsedStoryItem, Story, StoryMedia, StoryPeerContext,
    StoryPrivacy, StoryRepostInfo, story_item_from_tl,
};
use tg_tl::{enums as tl_enums, types as tl_types};

use crate::common::{
    AppTestState, TEST_STEP_DT, apply_deterministic_style, build_wgpu_harness, settle_layout,
    step_frames,
};

const DESKTOP_SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const MOBILE_SIZE: Vec2 = Vec2::new(390.0, 844.0);

static WGPU_FU7B_LOCK: Mutex<()> = Mutex::new(());

fn snap_dir() -> PathBuf {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("fu7b"));
    std::fs::create_dir_all(&dir).expect("create snap dir");
    dir
}

fn save_harness_png<S>(harness: &mut egui_kittest::Harness<'static, S>, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    let img = harness.render().expect("wgpu render");
    img.save(path).expect("save png");
}

fn scroll_panel_down(
    harness: &mut egui_kittest::Harness<'static, AppTestState>,
    size: Vec2,
    pixels: f32,
) {
    // Place pointer inside the right settings column on desktop, or center on mobile.
    let target = if size.x >= 900.0 {
        Pos2::new(size.x * 0.65, size.y * 0.50)
    } else {
        Pos2::new(size.x * 0.50, size.y * 0.50)
    };
    harness.input_mut().events.push(Event::PointerMoved(target));
    harness.step();
    let steps = 8;
    let delta_per_step = -(pixels / steps as f32);
    for _ in 0..steps {
        harness.input_mut().events.push(Event::PointerMoved(target));
        harness.input_mut().events.push(Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: Vec2::new(0.0, delta_per_step),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
        });
        harness.step();
    }
    step_frames(harness, 4);
}

fn build_unsupported_story_host() -> AppStoriesHost {
    let mut host = AppStoriesHost::new();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1_800_000_000_i32, |d| d.as_secs() as i32);
    let peer = mock_peers::AURORA;
    let story_id: StoryId = 9901;
    let story = Story {
        id: story_id,
        peer_id: peer,
        date: now_secs - 120,
        expires: now_secs + 86_400,
        last_update_time: now_secs,
        media: StoryMedia::Unsupported,
        caption: TextWithEntities::default(),
        out: false,
        in_profile: false,
        pinned_to_top: false,
        privacy: StoryPrivacy::Public,
        privacy_allowed: Vec::new(),
        privacy_disallowed: Vec::new(),
        no_forwards: false,
        edited: false,
        from_peer: None,
        repost: StoryRepostInfo::default(),
        music: None,
        album_ids: BTreeSet::new(),
        sent_reaction: None,
        recent_viewers: Vec::new(),
        views: Default::default(),
        channel_reactions: Default::default(),
        locations: Vec::new(),
        suggested_reactions: Vec::new(),
        channel_posts: Vec::new(),
        url_areas: Vec::new(),
        weather_areas: Vec::new(),
    };
    let parsed = ParsedPeerStories {
        peer_id: peer,
        max_read_id: 0,
        items: vec![ParsedStoryItem::Full(Box::new(story))],
        has_video_stream: false,
    };
    host.store_mut()
        .apply_peer_stories(parsed, ParseSource::DirectRequest, now_secs);
    let full_id = FullStoryId::new(peer, story_id);
    let _ = host.open_story_by_id(peer, Some(full_id.story));
    host
}

#[test]
#[ignore = "captures before/after design review PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_fu7b_snapshots() {
    let _guard = WGPU_FU7B_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_FU7B_SNAP_STAGE")
        .or_else(|_| std::env::var("TGFORK_FU7B_SNAP_STAGE"))
        .unwrap_or_else(|_| "after".to_owned());
    let dir = snap_dir();

    for (size_label, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        // Screen 1: Privacy & Security settings scrolled to New Chats / Sensitive Content toggles.
        {
            let state = AppTestState::main_workspace();
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_settings(SettingsPage::PrivacySecurity);
            }
            settle_layout(&mut h);
            scroll_panel_down(&mut h, size, 940.0);
            save_harness_png(
                &mut h,
                &dir.join(format!("privacy_toggles_{size_label}_{stage}.png")),
            );
        }

        // Screen 2: Notifications & Sounds settings (toggle rows with subtitles + split toggles).
        {
            let state = AppTestState::main_workspace();
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_settings(SettingsPage::Notifications);
            }
            settle_layout(&mut h);
            step_frames(&mut h, 6);
            save_harness_png(
                &mut h,
                &dir.join(format!("notifications_toggles_{size_label}_{stage}.png")),
            );
        }

        // Screen 3: Unsupported Story inside fullscreen StoryViewerOverlay.
        {
            struct UnsupportedStorySnapState {
                host: AppStoriesHost,
                palette: Palette,
            }
            let state = UnsupportedStorySnapState {
                host: build_unsupported_story_host(),
                palette: Palette::day(),
            };
            assert!(
                state.host.viewer().is_open(),
                "unsupported story viewer must be open before capture"
            );
            let mut h = egui_kittest::HarnessBuilder::default()
                .with_size(size)
                .with_step_dt(TEST_STEP_DT)
                .with_render_cursor(false)
                .allow_missing_glyphs()
                .with_accessibility_check(false)
                .renderer(crate::common::serialized_wgpu_renderer())
                .with_context_setup(|ctx| {
                    tg_ui::fonts::register_tdesktop_fonts(ctx);
                    apply_deterministic_style(ctx);
                })
                .build_ui_state(
                    |ui, st: &mut UnsupportedStorySnapState| {
                        let ctx = ui.ctx().clone();
                        let rect = ctx.content_rect();
                        ui.painter().rect_filled(rect, 0.0, st.palette.window_bg);
                        st.host.update_and_render(&ctx, &st.palette);
                    },
                    state,
                );
            for _ in 0..6 {
                h.step();
            }
            save_harness_png(
                &mut h,
                &dir.join(format!("story_unsupported_{size_label}_{stage}.png")),
            );
        }

        // Screen 4: Shareable Theme Backdrop picker opened from PeerQrBox -> "Share Theme".
        {
            let mut h = egui_kittest::HarnessBuilder::default()
                .with_size(size)
                .with_step_dt(TEST_STEP_DT)
                .with_render_cursor(false)
                .allow_missing_glyphs()
                .with_accessibility_check(false)
                .renderer(crate::common::serialized_wgpu_renderer())
                .with_context_setup(|ctx| {
                    tg_ui::fonts::register_tdesktop_fonts(ctx);
                    apply_deterministic_style(ctx);
                })
                .build_ui_state(
                    |ui, st: &mut AppTestState| {
                        st.show(ui);
                        let ctx = ui.ctx().clone();
                        let pal = st.controller.palette();
                        AppThemesHost::update_and_render(&ctx, pal.layer_backdrop, pal.divider);
                    },
                    AppTestState::main_workspace(),
                );
            settle_layout(&mut h);
            let ctx = h.ctx.clone();
            let peer = InfoBackend::peer_profile(
                h.state().controller.session_backend(),
                session_peers::USER_MIRA,
            )
            .expect("USER_MIRA profile");
            let qr = PeerQrBoxState::from_profile(&peer);
            let colors = tg_app::settings::theme_bridge::LiveThemeColors::default();
            h.state_mut()
                .controller
                .box_host_mut()
                .push(&ctx, BoxKind::PeerQr(qr), &colors);
            step_frames(&mut h, 6);
            h.get_by_label("Share Theme").click();
            step_frames(&mut h, 8);
            save_harness_png(
                &mut h,
                &dir.join(format!("share_theme_qr_{size_label}_{stage}.png")),
            );
        }
    }
}

#[test]
fn fu7b_item1_privacy_and_notifications_toggles_accessible_and_clickable_at_390px() {
    let state = AppTestState::main_workspace();
    let mut h = build_wgpu_harness(state, MOBILE_SIZE);
    settle_layout(&mut h);
    if let Some(session) = h.state_mut().controller.session_mut() {
        session.show_settings(SettingsPage::PrivacySecurity);
    }
    settle_layout(&mut h);
    scroll_panel_down(&mut h, MOBILE_SIZE, 940.0);

    // Every Privacy & Security toggle row with a subtitle keeps its accessible checkbox label
    // and toggles its underlying GlobalPrivacy state cleanly at 390px.
    let before_archive = h
        .state()
        .controller
        .session_backend()
        .global_privacy()
        .archive_and_mute_new;
    h.get_by_label("Archive and mute").click();
    step_frames(&mut h, 3);
    assert_ne!(
        h.state()
            .controller
            .session_backend()
            .global_privacy()
            .archive_and_mute_new,
        before_archive,
        "clicking Archive and mute toggle row at 390px must toggle archive_and_mute_new"
    );

    let before_premium = h
        .state()
        .controller
        .session_backend()
        .global_privacy()
        .new_noncontact_peers_require_premium;
    h.get_by_label("Require Premium for new non-contact chats")
        .click();
    step_frames(&mut h, 3);
    assert_ne!(
        h.state()
            .controller
            .session_backend()
            .global_privacy()
            .new_noncontact_peers_require_premium,
        before_premium,
        "clicking Require Premium toggle row at 390px must toggle new_noncontact_peers_require_premium"
    );
}

#[test]
fn fu7b_item3_unsupported_story_from_tl_renders_update_button_and_triggers_update_check() {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1_800_000_000_i32, |d| d.as_secs() as i32);

    // 1. Prove live TL reachability: `messageMediaUnsupported` parses into `StoryMedia::Unsupported`.
    let tl_story = tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
        pinned: false,
        min: false,
        public: true,
        close_friends: false,
        contacts: false,
        selected_contacts: false,
        out: false,
        noforwards: false,
        edited: false,
        id: 9902,
        date: now_secs - 60,
        from_id: None,
        fwd_from: None,
        expire_date: now_secs + 86_400,
        caption: None,
        entities: None,
        media: Box::new(tl_enums::MessageMedia::Unsupported),
        media_areas: None,
        privacy: None,
        views: None,
        sent_reaction: None,
        albums: None,
        music: None,
    });
    let parsed_item = story_item_from_tl(
        mock_peers::AURORA,
        StoryPeerContext::default(),
        &tl_story,
        now_secs,
        None,
    )
    .expect("messageMediaUnsupported must parse into a Story, not None");
    match &parsed_item {
        ParsedStoryItem::Full(story) => assert!(story.unsupported()),
        other => panic!("expected ParsedStoryItem::Full(unsupported), got {other:?}"),
    }

    // 2. Render in StoryViewerOverlay at 390x844 and click "Update Rustgram".
    struct UnsupportedState {
        host: AppStoriesHost,
        palette: Palette,
    }
    let mut host = AppStoriesHost::new();
    host.store_mut().apply_peer_stories(
        ParsedPeerStories {
            peer_id: mock_peers::AURORA,
            max_read_id: 0,
            items: vec![parsed_item],
            has_video_stream: false,
        },
        ParseSource::DirectRequest,
        now_secs,
    );
    assert!(host.open_story_by_id(mock_peers::AURORA, Some(9902)));

    let mut h = egui_kittest::HarnessBuilder::default()
        .with_size(MOBILE_SIZE)
        .with_step_dt(TEST_STEP_DT)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, st: &mut UnsupportedState| {
                let ctx = ui.ctx().clone();
                st.host.update_and_render(&ctx, &st.palette);
            },
            UnsupportedState {
                host,
                palette: Palette::day(),
            },
        );
    for _ in 0..4 {
        h.step();
    }
    assert!(h.state().host.viewer().is_open());

    h.get_by_label("Update Rustgram").click();
    h.step();
    h.step();

    assert!(
        h.state_mut().host.take_update_app_request(),
        "clicking Update Rustgram on an unsupported story must emit take_update_app_request()"
    );
    assert!(
        h.state().host.viewer().is_open(),
        "clicking Update Rustgram must not advance/close the unsupported story viewer"
    );
}

#[test]
fn fu7b_item4_profile_qr_share_theme_fits_390px_viewport_and_closes_cleanly() {
    let mut h = egui_kittest::HarnessBuilder::default()
        .with_size(MOBILE_SIZE)
        .with_step_dt(TEST_STEP_DT)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut AppTestState| {
                st.show(ui);
                let ctx = ui.ctx().clone();
                let pal = st.controller.palette();
                AppThemesHost::update_and_render(&ctx, pal.layer_backdrop, pal.divider);
            },
            AppTestState::main_workspace(),
        );
    settle_layout(&mut h);
    let ctx = h.ctx.clone();
    let peer = InfoBackend::peer_profile(
        h.state().controller.session_backend(),
        session_peers::USER_MIRA,
    )
    .expect("USER_MIRA profile");
    let expected_peer_id = peer.id.0;
    let expected_title = peer.title.clone();
    let qr = PeerQrBoxState::from_profile(&peer);
    let colors = tg_app::settings::theme_bridge::LiveThemeColors::default();
    h.state_mut()
        .controller
        .box_host_mut()
        .push(&ctx, BoxKind::PeerQr(qr), &colors);
    step_frames(&mut h, 6);

    h.get_by_label("Share Theme").click();
    step_frames(&mut h, 8);

    assert!(AppThemesHost::is_open(&ctx));
    assert_eq!(AppThemesHost::active_tab(&ctx), ThemesTab::ChatThemes);
    assert_eq!(
        AppThemesHost::share_theme_target_peer(&ctx),
        Some((expected_peer_id, expected_title))
    );

    // At 390px width, the overlay must clamp within the viewport so the Close button is reachable.
    let close_btn = h
        .get_all_by_label("Close")
        .last()
        .expect("AppThemesHost Close button");
    assert!(
        close_btn.rect().right() <= MOBILE_SIZE.x,
        "AppThemesHost Close button must fit inside 390px viewport (rect={:?})",
        close_btn.rect()
    );
    close_btn.click();
    step_frames(&mut h, 4);
    assert!(
        !AppThemesHost::is_open(&ctx),
        "Close button on AppThemesHost must remain inside 390px viewport and close the overlay"
    );
}
