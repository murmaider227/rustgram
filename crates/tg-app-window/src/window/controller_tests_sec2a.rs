//! Security review #2 (SEC2-A) regression tests for [`WindowController`]:
//! - L1: activations while locked open nothing (no box, no webview request) and replay on unlock;
//!   boxes, the media viewer, PiP and round video are closed and not rendered over the lock
//! - L4: a `?start=` bot link sends nothing until START is pressed
//! - L5: a first-time Mini App link shows the `confirmOpen` box
//! - L6: Mini App `OpenTelegramLink` events that do not route as Telegram links are dropped
//! - L7: `http(s)` URLs from the command line / pipe ask before opening
//! - L8: the Mini App location-settings event no longer shows "unsupported link"
//! - L10: `-tosettings` while locked waits for unlock

use std::path::PathBuf;

use egui::{Pos2, Rect, Vec2};
use tg_app_core::core_types::startup_types::IncomingActivation;
use tg_app_media::webview::backend::AppWebViewEvent;

use super::*;
use tg_app_info::boxes::mini_app_confirm::BotStartConfirmBoxState;

fn controller() -> WindowController {
    WindowController::with_session(
        MockSession::new(true),
        ShellSettings {
            local_passcode: Some("4242".to_owned()),
            ..ShellSettings::default()
        },
        std::env::temp_dir().join("rustgram_sec2a_controller_test.json"),
    )
}

fn run_frame(ctrl: &mut WindowController, ctx: &egui::Context) {
    let raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| ctrl.show(ui));
    out.textures_delta.clear();
}

fn temp_file(name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = dir.path().join(name);
    std::fs::write(&path, b"payload").expect("write temp file");
    (dir, path)
}

fn activation(urls: &[&str], send_paths: Vec<PathBuf>) -> IncomingActivation {
    IncomingActivation {
        urls: urls.iter().map(|u| (*u).to_owned()).collect(),
        send_paths,
        show: true,
        quit: false,
    }
}

#[test]
fn l1_activation_while_locked_opens_nothing_and_replays_after_unlock() {
    let ctx = egui::Context::default();
    let mut ctrl = controller();
    ctrl.setup_passcode_lock();
    assert!(ctrl.is_passcode_locked());

    let (_dir, file) = temp_file("photo.png");
    ctrl.handle_incoming_activation(
        activation(
            &[
                "tg://share?url=https%3A%2F%2Fexample.com&text=hi",
                "https://t.me/rustgram_log",
            ],
            vec![file.clone()],
        ),
        &ctx,
    );
    assert!(
        ctrl.box_host().is_empty(),
        "no ShareBox / link box over the lock screen"
    );
    assert!(ctrl.take_webview_requests().is_empty());
    assert_eq!(ctrl.queued_activation_count(), 1);

    // Mini App / external link requests from any other path are dropped while locked too.
    ctrl.open_link("https://example.com/");
    ctrl.open_mini_app_from_link(session_peers::BOT_CATALOG, None, None);
    run_frame(&mut ctrl, &ctx);
    assert!(ctrl.box_host().is_empty());
    assert!(ctrl.take_webview_requests().is_empty());
    assert!(ctrl.take_deep_link_overlay_requests().is_empty());

    ctrl.clear_passcode_lock();
    run_frame(&mut ctrl, &ctx);
    assert_eq!(
        ctrl.queued_activation_count(),
        0,
        "queue replayed on unlock"
    );
    assert!(
        matches!(ctrl.box_host().top_kind(), Some(BoxKind::Share(_))),
        "the -sendpath ShareBox appears after unlock"
    );
}

#[test]
fn l1_lock_closes_boxes_and_viewer_and_renders_none() {
    let ctx = egui::Context::default();
    let mut ctrl = controller();
    ctrl.box_host_mut().push_immediate(
        BoxKind::BotStartConfirm(BotStartConfirmBoxState::new(
            session_peers::BOT_CATALOG,
            "Catalog Bot",
            "x",
        )),
        &ThemeManager::default(),
    );
    assert!(!ctrl.box_host().is_empty());
    ctrl.setup_passcode_lock();
    assert!(ctrl.box_host().is_empty(), "lock closes open boxes");
    assert!(!ctrl.is_media_overlay_open());

    // A box pushed while locked (e.g. from a background event) is never shown.
    ctrl.box_host_mut().push_immediate(
        BoxKind::BotStartConfirm(BotStartConfirmBoxState::new(
            session_peers::BOT_CATALOG,
            "Catalog Bot",
            "y",
        )),
        &ThemeManager::default(),
    );
    run_frame(&mut ctrl, &ctx);
    assert!(ctrl.box_host().is_empty(), "boxes are cleared while locked");
}

#[test]
fn l2_app_wide_lock_locks_a_window_without_its_own_passcode() {
    let mut ctrl = WindowController::with_session(
        MockSession::new(true),
        ShellSettings::default(),
        std::env::temp_dir().join("rustgram_sec2a_controller_test2.json"),
    );
    assert!(!ctrl.has_local_passcode());
    ctrl.setup_passcode_lock();
    assert!(!ctrl.is_passcode_locked(), "own Ctrl+L needs a passcode");
    ctrl.lock_for_app_wide_passcode();
    assert!(
        ctrl.is_passcode_locked(),
        "app-wide lock applies to every account"
    );
    ctrl.clear_passcode_lock();
    assert!(!ctrl.is_passcode_locked());
}

#[test]
fn l10_to_settings_while_locked_waits_for_unlock() {
    let ctx = egui::Context::default();
    let mut ctrl = controller();
    ctrl.setup_passcode_lock();
    ctrl.open_settings();
    assert_eq!(ctrl.queued_activation_count(), 1);
    assert!(ctrl.main_widget().settings().is_none());
    ctrl.clear_passcode_lock();
    run_frame(&mut ctrl, &ctx);
    assert_eq!(ctrl.queued_activation_count(), 0);
}

#[test]
fn l4_start_link_sends_nothing_until_start_is_pressed() {
    let ctx = egui::Context::default();
    let mut ctrl = controller();
    let handle = ctrl.session_handle();
    ctrl.open_link("https://t.me/catalog_demo_bot?start=ref42");
    assert!(
        handle.started_bots().is_empty(),
        "a link alone must not send /start"
    );
    // WP-75: no START box; the bot chat opens with the token behind the bottom START bar.
    assert!(
        ctrl.box_host().is_empty(),
        "no START box (7.4.6 uses the bottom bar)"
    );
    assert_eq!(
        ctrl.main_widget_mut()
            .history_mut()
            .bot_start_token(session_peers::BOT_CATALOG),
        Some("ref42")
    );
    assert_eq!(
        ctrl.session().map(|s| s.active_peer()),
        Some(session_peers::BOT_CATALOG)
    );
    run_frame(&mut ctrl, &ctx);
    assert!(
        handle.started_bots().is_empty(),
        "showing the START bar must not send /start"
    );
}

#[test]
fn l5_first_mini_app_link_asks_then_trusts_the_bot() {
    let ctx = egui::Context::default();
    let mut ctrl = controller();
    ctrl.open_link("https://t.me/catalog_demo_bot?startapp=promo");
    assert!(
        ctrl.take_webview_requests().is_empty(),
        "no Mini App request before consent"
    );
    let state = match ctrl.box_host().top_kind() {
        Some(BoxKind::MiniAppOpenConfirm(s)) => s.clone(),
        other => panic!("Mini App consent box expected, got {other:?}"),
    };
    assert_eq!(state.bot_id, session_peers::BOT_CATALOG);
    ctrl.box_host_mut().pop_immediate();

    ctrl.handle_box_event(
        BoxEvent::MiniAppOpenConfirmed {
            bot_id: state.bot_id,
            url: state.url,
            start_param: state.start_param,
        },
        &ctx,
    );
    assert!(ctrl.is_peer_trusted_open_webview(session_peers::BOT_CATALOG));
    assert_eq!(
        ctrl.take_webview_requests(),
        vec![WebViewHostRequest::OpenMiniApp {
            bot_id: session_peers::BOT_CATALOG.bare() as i64,
            start_param: Some("promo".to_owned()),
        }]
    );

    // Second link for a trusted bot opens directly.
    ctrl.open_link("https://t.me/catalog_demo_bot?startapp=again");
    assert!(ctrl.box_host().is_empty());
    assert_eq!(ctrl.take_webview_requests().len(), 1);
}

#[test]
fn l6_bot_local_link_that_is_not_telegram_is_dropped() {
    let ctx = egui::Context::default();
    let mut ctrl = controller();
    ctrl.handle_webview_event(
        AppWebViewEvent::OpenTelegramLink {
            uri: "https://t.me@evil.example/".to_owned(),
        },
        &ctx,
    );
    assert!(ctrl.take_webview_requests().is_empty());
    assert!(ctrl.box_host().is_empty());
}

#[test]
fn l7_http_url_from_command_line_asks_first() {
    let ctx = egui::Context::default();
    let mut ctrl = controller();
    ctrl.handle_incoming_activation(activation(&["https://example.com/login"], Vec::new()), &ctx);
    assert!(
        ctrl.take_webview_requests().is_empty(),
        "an IPC http(s) URL is never opened without a click"
    );
    assert!(matches!(
        ctrl.box_host().top_kind(),
        Some(BoxKind::HiddenLinkConfirm(_))
    ));
}

#[test]
fn l11_relative_send_paths_are_ignored() {
    let ctx = egui::Context::default();
    let mut ctrl = controller();
    ctrl.handle_incoming_activation(
        activation(
            &[],
            vec![PathBuf::from(r"..\secret.txt"), PathBuf::from("x.txt")],
        ),
        &ctx,
    );
    assert!(ctrl.box_host().is_empty(), "no ShareBox for relative paths");
}

#[test]
fn l8_location_settings_event_is_not_reported_as_unsupported() {
    let ctx = egui::Context::default();
    let mut ctrl = controller();
    ctrl.handle_webview_event(
        AppWebViewEvent::OpenExternalUrl {
            url: tg_platform::url_policy::WINDOWS_LOCATION_SETTINGS_URI.to_owned(),
        },
        &ctx,
    );
    assert_ne!(
        ctrl.layer_stack.toast_text(),
        Some(tg_lang::tr::lng_open_link_unsupported().as_str())
    );
}
