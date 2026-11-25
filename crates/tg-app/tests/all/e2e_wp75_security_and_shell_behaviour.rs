//! WP-75 (audit #8): live behaviour and shell parity fixes, with before/after captures.
//!
//! Covers the window-level parts of the package: the bot `start` link bottom START bar, the
//! clickable global media search tabs, Forward opening the chosen chat with the forward bar,
//! the broadcast channel pull-to-next bar hidden at rest, and the frameless title bar without a
//! separator line.

use std::path::PathBuf;
use std::sync::Mutex;

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::window::platform_shell::PlatformWindowShell;
use tg_app::window::section::SectionShow;
use tg_platform::frameless::FakeWindowFrameBackend;
use tg_platform::taskbar_buttons::FakeTaskbarButtonsBackend;

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, apply_deterministic_style, build_wgpu_harness, click_at,
    right_click_at, settle_layout, step_frames, wait_for_menu_item, wait_until,
};

const DESKTOP_SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const MOBILE_SIZE: Vec2 = Vec2::new(390.0, 844.0);

static WGPU_WP75_LOCK: Mutex<()> = Mutex::new(());

fn snap_dir() -> PathBuf {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp75"));
    std::fs::create_dir_all(&dir).expect("create snap dir");
    dir
}

fn snap_stage() -> String {
    std::env::var("RUSTGRAM_WP75_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned())
}

fn save_png<S>(harness: &mut egui_kittest::Harness<'static, S>, name: &str) {
    let img = harness.render().expect("wgpu render");
    img.save(snap_dir().join(format!("{name}.png")))
        .expect("save png");
}

fn open_peer(harness: &mut egui_kittest::Harness<'_, AppTestState>, peer_id: PeerId) {
    harness
        .state_mut()
        .controller
        .session_mut()
        .expect("session")
        .show_peer_history(peer_id, SectionShow::clear_stack(), MsgId::SHOW_AT_END);
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        "switching active peer did not settle",
        |h| {
            !h.state().controller.is_animating()
                && h.state_mut()
                    .controller
                    .main_widget_mut()
                    .history_mut()
                    .active_peer()
                    == peer_id
        },
    );
    settle_layout(harness);
}

fn open_forward_box_from_last_message(harness: &mut egui_kittest::Harness<'_, AppTestState>) {
    let compose_rect = harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .last_compose_rect();
    let msg_pos = egui::Pos2::new(compose_rect.center().x, compose_rect.top() - 36.0);
    right_click_at(harness, msg_pos);
    wait_for_menu_item(harness, "Forward");
    let forward_item_pos = harness.get_by_label("Forward").rect().center();
    click_at(harness, forward_item_pos);
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        "clicking Forward must open the Forward to... box",
        |h| {
            matches!(
                h.state_mut().controller.box_host_mut().top_kind(),
                Some(tg_app::boxes::BoxKind::Share(share)) if !share.message_ids.is_empty()
            )
        },
    );
    settle_layout(harness);
}

/// Clicks the right-most node labelled `label` (the box row sits right of the chat list).
fn click_rightmost_label(harness: &mut egui_kittest::Harness<'_, AppTestState>, label: &str) {
    let pos = harness
        .query_all_by_label(label)
        .map(|n| n.rect().center())
        .max_by(|a, b| a.x.total_cmp(&b.x))
        .expect("label present");
    click_at(harness, pos);
}

#[test]
#[ignore = "captures before/after PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wp75_snapshots() {
    let _guard = WGPU_WP75_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = snap_stage();

    for (size_label, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        // 1. `tg://resolve?domain=<bot>&start=<token>`: tdesktop opens the bot chat with the
        //    bottom START bar in place of the composer.
        {
            let mut h = build_wgpu_harness(AppTestState::main_workspace(), size);
            settle_layout(&mut h);
            h.state_mut()
                .controller
                .open_link("tg://resolve?domain=catalog_demo_bot&start=promo");
            step_frames(&mut h, 20);
            settle_layout(&mut h);
            save_png(&mut h, &format!("start_link_{size_label}_{stage}"));
        }

        // 2. Global search: the Photos media tab is clickable and shows global photo results.
        {
            let mut h = build_wgpu_harness(AppTestState::main_workspace(), size);
            settle_layout(&mut h);
            if size.x < 640.0
                && let Some(sess) = h.state_mut().controller.session_mut()
            {
                sess.show_back_from_stack(SectionShow::forward());
            }
            {
                let dlgs = h.state_mut().controller.main_widget_mut().dialogs_mut();
                dlgs.search_active = true;
                dlgs.search_query = String::new();
            }
            step_frames(&mut h, 6);
            if let Some(node) = h.query_by_label("Photos") {
                let pos = node.rect().center();
                click_at(&mut h, pos);
            }
            step_frames(&mut h, 10);
            settle_layout(&mut h);
            save_png(&mut h, &format!("search_photos_{size_label}_{stage}"));
        }

        // 3. Forward: picking a chat in "Forward to..." opens it with the forward bar.
        {
            let mut h = build_wgpu_harness(AppTestState::main_workspace(), size);
            settle_layout(&mut h);
            open_peer(&mut h, session_peers::USER_THEO);
            open_forward_box_from_last_message(&mut h);
            save_png(&mut h, &format!("forward_box_{size_label}_{stage}"));
            click_rightmost_label(&mut h, "Mira Vance");
            step_frames(&mut h, 20);
            settle_layout(&mut h);
            save_png(&mut h, &format!("forward_pick_{size_label}_{stage}"));
        }

        // 4. Broadcast channel at rest: no pull-to-next-channel bar above the bottom bar.
        {
            let mut h = build_wgpu_harness(AppTestState::main_workspace(), size);
            settle_layout(&mut h);
            open_peer(&mut h, session_peers::CHANNEL_DEV_LOG);
            step_frames(&mut h, 10);
            save_png(&mut h, &format!("channel_bottom_{size_label}_{stage}"));
        }

        // 5. Frameless title bar over the main window: no separator stroke below it.
        {
            let state = AppTestState::main_workspace();
            let shell = PlatformWindowShell::with_backends(
                FakeWindowFrameBackend::new(),
                Some(FakeTaskbarButtonsBackend::new()),
            );
            let mut h = egui_kittest::HarnessBuilder::default()
                .with_size(size)
                .with_step_dt(crate::common::TEST_STEP_DT)
                .with_render_cursor(false)
                .allow_missing_glyphs()
                .with_accessibility_check(false)
                .wgpu()
                .with_context_setup(|ctx| {
                    tg_ui::fonts::register_tdesktop_fonts(ctx);
                    apply_deterministic_style(ctx);
                })
                .build_ui_state(
                    |ui,
                     (state, shell): &mut (
                        AppTestState,
                        PlatformWindowShell<FakeWindowFrameBackend, FakeTaskbarButtonsBackend>,
                    )| {
                        let palette = state.controller.palette();
                        shell.show_frameless_title_bar(ui, "Rustgram", &palette, false, false);
                        state.show(ui);
                    },
                    (state, shell),
                );
            step_frames(&mut h, 12);
            save_png(&mut h, &format!("title_bar_{size_label}_{stage}"));
        }
    }
}

const WORKSPACE: Vec2 = Vec2::new(1280.0, 820.0);

#[test]
fn wp75_instant_view_fallback_opens_external_url_exactly_once() {
    let mut h = crate::common::build_harness(AppTestState::main_with_webview(), WORKSPACE);
    settle_layout(&mut h);
    let url = "https://example.org/wp75-no-instant-view";
    {
        let state = h.state_mut();
        let host = state.webview_host.as_mut().expect("webview host");
        host.open_instant_view(url, None, None);
        let events = host.take_events();
        let ctx = egui::Context::default();
        for ev in events {
            state.controller.handle_webview_event(ev, &ctx);
        }
    }
    // The next frames dispatch any queued webview request back into the host.
    step_frames(&mut h, 4);
    let opened = h.state().opened_external_urls.lock().expect("lock").clone();
    assert_eq!(
        opened,
        vec![url.to_owned()],
        "the Instant View fallback must open the external URL exactly once"
    );
}

#[test]
fn wp75_keyboard_and_menu_mini_app_buttons_ask_unverified_bots_first() {
    let mut h = crate::common::build_harness(AppTestState::main_with_webview(), WORKSPACE);
    settle_layout(&mut h);
    h.state_mut()
        .controller
        .mock_session_mut()
        .set_peer_verified(session_peers::BOT_CATALOG, false);
    open_peer(&mut h, session_peers::BOT_CATALOG);
    let log_len = |h: &egui_kittest::Harness<'_, AppTestState>| {
        h.state()
            .controller
            .mock_session()
            .webview_requests_log()
            .len()
    };
    let before = log_len(&h);

    // 1. Inline keyboard WebView button of an unverified, untrusted bot -> confirmOpen box.
    h.get_by_label("Open Store App").click();
    wait_until(
        &mut h,
        DEFAULT_MAX_FRAMES,
        "keyboard WebView button must ask first",
        |h| {
            matches!(
                h.state_mut().controller.box_host_mut().top_kind(),
                Some(tg_app::boxes::BoxKind::MiniAppOpenConfirm(_))
            )
        },
    );
    assert_eq!(log_len(&h), before, "no Mini App request before consent");

    // 2. Launch -> the Mini App opens and the bot becomes trusted.
    let launch = tg_lang::tr::lng_view_button_bot_app();
    crate::common::wait_for_label_settled(&mut h, &launch);
    h.get_by_label(&launch).click();
    wait_until(
        &mut h,
        DEFAULT_MAX_FRAMES,
        "launch opens the Mini App",
        |h| log_len(h) == before + 1,
    );
    assert!(
        h.state()
            .controller
            .is_peer_trusted_open_webview(session_peers::BOT_CATALOG)
    );

    // 3. The bot menu WebApp button of the now trusted bot opens directly.
    h.state_mut()
        .controller
        .mock_session_mut()
        .set_bot_menu_button(
            session_peers::BOT_CATALOG,
            tg_bots::commands::BotMenuButton::WebApp {
                text: "Open Shop".to_owned(),
                url: "https://shop.example.org/menu-webapp".to_owned(),
            },
        );
    crate::common::wait_for_label_settled(&mut h, "Open Shop");
    h.get_by_label("Open Shop").click();
    wait_until(
        &mut h,
        DEFAULT_MAX_FRAMES,
        "trusted bot menu button opens without a box",
        |h| log_len(h) == before + 2,
    );
    assert!(h.state().controller.box_host().is_empty());
}

#[test]
fn wp75_bot_menu_button_of_untrusted_bot_asks_first() {
    let mut h = crate::common::build_harness(AppTestState::main_with_webview(), WORKSPACE);
    settle_layout(&mut h);
    h.state_mut()
        .controller
        .mock_session_mut()
        .set_peer_verified(session_peers::BOT_CATALOG, false);
    h.state_mut()
        .controller
        .mock_session_mut()
        .set_bot_menu_button(
            session_peers::BOT_CATALOG,
            tg_bots::commands::BotMenuButton::WebApp {
                text: "Open Shop".to_owned(),
                url: "https://shop.example.org/menu-webapp".to_owned(),
            },
        );
    open_peer(&mut h, session_peers::BOT_CATALOG);
    let before = h
        .state()
        .controller
        .mock_session()
        .webview_requests_log()
        .len();
    crate::common::wait_for_label_settled(&mut h, "Open Shop");
    h.get_by_label("Open Shop").click();
    wait_until(
        &mut h,
        DEFAULT_MAX_FRAMES,
        "bot menu WebApp button must ask first",
        |h| {
            matches!(
                h.state_mut().controller.box_host_mut().top_kind(),
                Some(tg_app::boxes::BoxKind::MiniAppOpenConfirm(_))
            )
        },
    );
    assert_eq!(
        h.state()
            .controller
            .mock_session()
            .webview_requests_log()
            .len(),
        before
    );
}

#[test]
fn wp75_start_link_opens_bot_chat_with_start_bar() {
    let mut h = crate::common::build_harness(AppTestState::main_workspace(), WORKSPACE);
    settle_layout(&mut h);
    let handle = h.state().controller.session_handle();
    h.state_mut()
        .controller
        .open_link("tg://resolve?domain=catalog_demo_bot&start=wp75promo");
    wait_until(
        &mut h,
        DEFAULT_MAX_FRAMES,
        "the start link must open the bot chat with the START bar",
        |h| {
            let history = h.state_mut().controller.main_widget_mut().history_mut();
            history.active_peer() == session_peers::BOT_CATALOG && history.is_bot_start_shown()
        },
    );
    assert!(h.state().controller.box_host().is_empty(), "no START box");
    assert!(
        handle.started_bots().is_empty(),
        "nothing sent before START"
    );

    h.get_by_label(&tg_lang::tr::lng_bot_start().to_uppercase())
        .click();
    wait_until(
        &mut h,
        DEFAULT_MAX_FRAMES,
        "START sends messages.startBot with the token and restores the composer",
        |h| {
            !h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .is_bot_start_shown()
        },
    );
    let started = handle.started_bots();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].start_param, "wp75promo");
}

#[test]
fn wp75_search_media_tabs_switch_and_load_global_media() {
    use tg_app::dialogs::search::SearchStripMedia;
    let mut h = crate::common::build_harness(AppTestState::main_workspace(), WORKSPACE);
    settle_layout(&mut h);
    {
        let dlgs = h.state_mut().controller.main_widget_mut().dialogs_mut();
        dlgs.search_active = true;
        dlgs.search_query = String::new();
    }
    step_frames(&mut h, 4);
    for (label, media) in [
        (tg_lang::tr::lng_all_photos(), SearchStripMedia::Photos),
        (tg_lang::tr::lng_all_videos(), SearchStripMedia::Videos),
        (tg_lang::tr::lng_all_links(), SearchStripMedia::Links),
        (tg_lang::tr::lng_all_files(), SearchStripMedia::Files),
        (tg_lang::tr::lng_all_music(), SearchStripMedia::Music),
        (tg_lang::tr::lng_all_voice(), SearchStripMedia::Voice),
    ] {
        // The strip scrolls horizontally like `st::dialogsTabsScroll` when the column is
        // narrower than the ten labels: scroll it so the tab is inside the column.
        let column_right = h
            .get_by_role_and_label(egui::accesskit::Role::Tab, "Chats")
            .rect()
            .left()
            + 380.0;
        if h.get_by_role_and_label(egui::accesskit::Role::Tab, &label)
            .rect()
            .right()
            > column_right
        {
            let anchor = h
                .get_by_role_and_label(egui::accesskit::Role::Tab, "Chats")
                .rect()
                .center();
            crate::common::hover_at(&mut h, anchor);
            h.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(-400.0, 0.0),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Unknown,
            });
            step_frames(&mut h, 4);
        }
        h.get_by_role_and_label(egui::accesskit::Role::Tab, &label)
            .click();
        step_frames(&mut h, 3);
        let state = &h
            .state_mut()
            .controller
            .main_widget_mut()
            .dialogs_mut()
            .search_state;
        assert_eq!(
            state.media_tab,
            Some(media),
            "{label} tab must become active"
        );
        if media == SearchStripMedia::Photos {
            assert!(
                !state.media_results.entries.is_empty(),
                "Photos must list global photo results"
            );
            assert!(
                state
                    .media_results
                    .entries
                    .iter()
                    .all(|e| matches!(e.item, tg_app::info::backend::MediaItem::Photo { .. }))
            );
        }
    }
    // A scope tab switches back to chat results.
    let anchor = h
        .get_by_role_and_label(egui::accesskit::Role::Tab, &tg_lang::tr::lng_all_voice())
        .rect()
        .center();
    crate::common::hover_at(&mut h, anchor);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: Vec2::new(800.0, 0.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: egui::Modifiers::NONE,
    });
    step_frames(&mut h, 4);
    h.get_by_role_and_label(egui::accesskit::Role::Tab, "Chats")
        .click();
    step_frames(&mut h, 3);
    assert_eq!(
        h.state_mut()
            .controller
            .main_widget_mut()
            .dialogs_mut()
            .search_state
            .media_tab,
        None
    );
}

#[test]
fn wp75_forward_pick_opens_chat_with_forward_bar() {
    let mut h = crate::common::build_harness(AppTestState::main_workspace(), WORKSPACE);
    settle_layout(&mut h);
    open_peer(&mut h, session_peers::USER_THEO);
    open_forward_box_from_last_message(&mut h);
    let titles: Vec<String> = match h.state_mut().controller.box_host_mut().top_kind() {
        Some(tg_app::boxes::BoxKind::Share(share)) => {
            share.folder_tabs.iter().map(|t| t.title.clone()).collect()
        }
        _ => Vec::new(),
    };
    assert!(titles.len() > 1, "folder tabs expected, got {titles:?}");
    assert_eq!(titles[0], tg_lang::tr::lng_filters_all_short());

    click_rightmost_label(&mut h, "Mira Vance");
    wait_until(
        &mut h,
        DEFAULT_MAX_FRAMES,
        "picking a chat must open it with the forward bar",
        |h| {
            let history = h.state_mut().controller.main_widget_mut().history_mut();
            history.active_peer() == session_peers::USER_MIRA
                && matches!(
                    history.compose_mut().mode,
                    tg_app::history::compose::ComposeMode::Forward { from_peer, .. }
                        if from_peer == session_peers::USER_THEO
                )
        },
    );
    assert!(h.state().controller.box_host().is_empty());
}

#[test]
fn wp75_calls_row_opens_only_the_calls_box() {
    let mut h = crate::common::build_harness(AppTestState::main_workspace(), WORKSPACE);
    settle_layout(&mut h);
    h.state_mut().controller.open_main_menu();
    crate::common::wait_for_label_settled(&mut h, &tg_lang::tr::lng_menu_calls());
    h.get_by_label(&tg_lang::tr::lng_menu_calls()).click();
    step_frames(&mut h, 4);
    assert!(
        h.state_mut().controller.take_open_recent_calls_request(),
        "Calls must open the Recent Calls box"
    );
    assert!(
        h.state().controller.main_widget().settings().is_none(),
        "Calls must not open Settings > Calls"
    );
}

#[test]
fn wp75_set_emoji_status_opens_the_selector_for_premium() {
    use tg_app::chat_helpers::backend::ChatHelpersBackend;
    let mut h = crate::common::build_harness(AppTestState::main_workspace(), WORKSPACE);
    settle_layout(&mut h);
    h.state_mut()
        .controller
        .mock_session_mut()
        .set_premium(true);
    h.state_mut().controller.open_main_menu();
    let label = tg_lang::tr::lng_menu_set_status();
    crate::common::wait_for_label_settled(&mut h, &label);
    h.get_by_label(&label).click();
    step_frames(&mut h, 3);
    assert_eq!(
        tg_app::business::AppBusinessHost::open_screen(&h.ctx),
        Some(tg_app::business::BusinessScreen::EmojiStatus)
    );
}

#[test]
fn wp75_channel_pull_bar_hidden_at_rest_and_shown_on_overscroll() {
    let mut h = crate::common::build_harness(AppTestState::main_workspace(), WORKSPACE);
    settle_layout(&mut h);
    open_peer(&mut h, session_peers::CHANNEL_DEV_LOG);
    step_frames(&mut h, 4);
    let prompt = tg_lang::tr::lng_pull_next_channel();
    assert!(
        h.query_by_label_contains(&prompt).is_none(),
        "no pull-to-next-channel bar at rest"
    );
    let now = h.ctx.input(|i| i.time);
    h.state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .pull_next_channel_mut()
        .feed_bottom_overscroll(20.0, now, true);
    step_frames(&mut h, 1);
    assert!(
        h.query_by_label_contains(&prompt).is_some(),
        "over-scrolling past the bottom shows the bar"
    );
}

#[test]
fn wp75_frameless_title_bar_has_no_separator_line() {
    let palette = tg_app::stand_in::theme::Palette::day();
    let shell = PlatformWindowShell::with_backends(
        FakeWindowFrameBackend::new(),
        Some(FakeTaskbarButtonsBackend::new()),
    );
    let mut h = egui_kittest::Harness::builder()
        .with_size(Vec2::new(800.0, 200.0))
        .build_ui_state(
            move |ui,
                  sh: &mut PlatformWindowShell<
                FakeWindowFrameBackend,
                FakeTaskbarButtonsBackend,
            >| {
                sh.show_frameless_title_bar(ui, "Rustgram", &palette, false, false);
            },
            shell,
        );
    h.run();
    let output = h.output();
    let full_width_lines = output
        .shapes
        .iter()
        .filter(|clipped| match &clipped.shape {
            egui::Shape::LineSegment { points, .. } => {
                (points[0].y - points[1].y).abs() < 0.01
                    && (points[1].x - points[0].x).abs() >= 790.0
            }
            _ => false,
        })
        .count();
    assert_eq!(
        full_width_lines, 0,
        "no separator stroke under the title bar"
    );
}

#[test]
fn wp75_demo_overlays_close_on_passcode_lock() {
    let ctx = egui::Context::default();
    tg_app::business::AppBusinessHost::open_overlay(&ctx);
    assert!(tg_app::business::AppBusinessHost::is_open(&ctx));
    tg_app::app::close_demo_overlays_for_passcode_lock(&ctx);
    assert!(!tg_app::business::AppBusinessHost::is_open(&ctx));
    assert!(!tg_app::statistics::AppStatisticsHost::is_open(&ctx));
    assert!(!tg_app::themes::AppThemesHost::is_open(&ctx));
}

#[test]
#[ignore = "captures before/after PNGs of the stage-3 additions under RUSTGRAM_SNAPSHOT_DIR"]
fn capture_wp75_addition_snapshots() {
    let _guard = WGPU_WP75_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = snap_stage();

    for (size_label, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        // Saved Messages: no "All Messages | My Notes" tabs.
        {
            let mut h = build_wgpu_harness(AppTestState::main_workspace(), size);
            settle_layout(&mut h);
            open_peer(&mut h, session_peers::SELF);
            step_frames(&mut h, 8);
            save_png(&mut h, &format!("saved_messages_{size_label}_{stage}"));
        }

        // 1:1 chat with the pointer over the last bubble: no reply / react / forward corner
        // buttons; narrow width shows the bubble width.
        {
            let mut h = build_wgpu_harness(AppTestState::main_workspace(), size);
            settle_layout(&mut h);
            open_peer(&mut h, session_peers::USER_MIRA);
            let compose_rect = h
                .state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .last_compose_rect();
            let pos = egui::Pos2::new(compose_rect.left() + 120.0, compose_rect.top() - 30.0);
            crate::common::hover_at(&mut h, pos);
            step_frames(&mut h, 10);
            save_png(&mut h, &format!("bubble_hover_{size_label}_{stage}"));

            // Message context menu with the reaction strip.
            right_click_at(&mut h, pos);
            step_frames(&mut h, 12);
            save_png(&mut h, &format!("message_menu_{size_label}_{stage}"));
        }

        // Global search with a query: results header and match highlight.
        {
            let mut h = build_wgpu_harness(AppTestState::main_workspace(), size);
            settle_layout(&mut h);
            if size.x < 640.0
                && let Some(sess) = h.state_mut().controller.session_mut()
            {
                sess.show_back_from_stack(SectionShow::forward());
            }
            {
                let dlgs = h.state_mut().controller.main_widget_mut().dialogs_mut();
                dlgs.search_active = true;
                dlgs.search_query = "layout".to_owned();
            }
            step_frames(&mut h, 10);
            save_png(&mut h, &format!("search_results_{size_label}_{stage}"));
        }
    }
}
