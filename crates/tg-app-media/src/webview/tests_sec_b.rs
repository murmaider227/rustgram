//! Security review #1 (SEC-B) regression tests for `tg-app-media::webview`:
//! - B1: Mini App location events go through same-origin and close checks (forged/foreign rejected)
//! - B2: Instant View `handle_web_message` checks sender `source_url` against `IV_ORIGIN`
//! - B5: WebView2 profile folder is per-account under the working directory (never `%TEMP%`)
//! - B6: `web_app_request_file_download` sanitizes filenames to last safe component & never overwrites

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tempfile::TempDir;
use tg_app_core::core_types::misc::account_bot_storage_dir;
use tg_iv::{MemoryMediaStore, Page};
use tg_platform::geolocation::{
    FakeGeoLocationProvider, GeoLocation, GeoLocationAccuracy, GeoPoint,
};
use tg_theme::Palette as TgPalette;
use tg_webview::WebMessage;

use crate::webview::backend::{
    AppWebViewEvent, BotInfo, ClipboardAccess, ExternalOpener, WebViewLaunchInfo,
};
use crate::webview::instant_view::{IV_ORIGIN, InstantViewWindow};
use crate::webview::mini_app::{
    ActiveMiniAppModal, MiniAppWindow, resolve_safe_bot_download_path,
    sanitize_bot_download_filename,
};
use crate::webview::mock::MockWebViewBackend;

#[derive(Default)]
struct RecordingOpener {
    urls: Mutex<Vec<String>>,
}

impl RecordingOpener {
    fn urls(&self) -> Vec<String> {
        self.urls.lock().expect("urls lock").clone()
    }
}

impl ExternalOpener for RecordingOpener {
    fn open_url(&self, url: &str) -> Result<(), String> {
        self.urls.lock().expect("urls lock").push(url.to_owned());
        Ok(())
    }

    fn open_mailto(&self, address: &str) -> Result<(), String> {
        self.urls
            .lock()
            .expect("urls lock")
            .push(format!("mailto:{address}"));
        Ok(())
    }
}

struct NoopClipboard;

impl ClipboardAccess for NoopClipboard {
    fn read_text(&self) -> Option<String> {
        None
    }

    fn write_text(&self, _text: &str) -> bool {
        true
    }
}

fn sample_launch(bot_id: u64, url: &str) -> WebViewLaunchInfo {
    WebViewLaunchInfo {
        bot: BotInfo {
            id: bot_id,
            username: format!("secb_bot_{bot_id}"),
            title: "SecB Test Bot".to_owned(),
            verified: false,
            in_attach_menu: false,
            in_main_menu: false,
            allow_clipboard_read: false,
            is_game: false,
        },
        url: url.to_owned(),
        query_id: 0,
        same_origin: true,
        fullscreen: false,
        local_routes: None,
    }
}

#[test]
fn b1_mini_app_location_events_enforce_origin_and_close_checks() {
    let launch = sample_launch(91, "https://trusted.bot.example/mini");
    let mut win = MiniAppWindow::new(launch);
    let geo = Arc::new(FakeGeoLocationProvider::with_location(GeoLocation {
        point: GeoPoint::new(51.5, -0.1),
        accuracy: GeoLocationAccuracy::Exact,
        horizontal_accuracy_meters: Some(5.0),
    }));
    win.set_geo_provider(geo.clone());

    let palette = TgPalette::default();
    let mut backend = MockWebViewBackend::new();
    let opener = RecordingOpener::default();
    let clipboard = NoopClipboard;
    let now = Instant::now();

    // 1. Foreign origin sending location commands MUST be rejected (old code handled them before
    //    `controller.handle_web_message`!).
    let foreign_check = WebMessage {
        text: r#"["web_app_check_location", {}]"#.to_owned(),
        source_url: "https://evil.attacker.example/page".to_owned(),
    };
    let foreign_req = WebMessage {
        text: r#"["web_app_request_location", {}]"#.to_owned(),
        source_url: "https://evil.attacker.example/page".to_owned(),
    };
    let foreign_settings = WebMessage {
        text: r#"["web_app_open_location_settings", {}]"#.to_owned(),
        source_url: "https://evil.attacker.example/page".to_owned(),
    };
    assert!(
        win.handle_web_message(
            &foreign_check,
            &palette,
            &mut backend,
            &opener,
            &clipboard,
            now
        )
        .is_empty()
    );
    assert!(
        win.handle_web_message(
            &foreign_req,
            &palette,
            &mut backend,
            &opener,
            &clipboard,
            now
        )
        .is_empty()
    );
    assert!(
        win.handle_web_message(
            &foreign_settings,
            &palette,
            &mut backend,
            &opener,
            &clipboard,
            now
        )
        .is_empty()
    );
    assert!(
        win.posted_raw_events().is_empty(),
        "foreign origin must not receive location_checked"
    );
    assert!(
        win.pending_modals().is_empty(),
        "foreign origin must not trigger location consent prompt"
    );
    assert!(
        opener.urls().is_empty(),
        "foreign origin must not open ms-settings:privacy-location"
    );

    // 2. Forged-origin object payload is also rejected for location commands.
    let forged = WebMessage {
        text: r#"{"eventType":"web_app_open_location_settings","eventData":{},"origin":"https://trusted.bot.example"}"#
            .to_owned(),
        source_url: "https://evil.attacker.example/page".to_owned(),
    };
    assert!(
        win.handle_web_message(&forged, &palette, &mut backend, &opener, &clipboard, now)
            .is_empty()
    );
    assert!(opener.urls().is_empty());

    // 3. After `web_app_close` sets `close_requested`, even trusted-origin location events must be ignored.
    let close_msg = WebMessage {
        text: r#"["web_app_close", {}]"#.to_owned(),
        source_url: "https://trusted.bot.example/mini".to_owned(),
    };
    let _ = win.handle_web_message(&close_msg, &palette, &mut backend, &opener, &clipboard, now);
    assert!(!win.is_open());

    let post_close_settings = WebMessage {
        text: r#"["web_app_open_location_settings", {}]"#.to_owned(),
        source_url: "https://trusted.bot.example/mini".to_owned(),
    };
    let post_close_check = WebMessage {
        text: r#"["web_app_check_location", {}]"#.to_owned(),
        source_url: "https://trusted.bot.example/mini".to_owned(),
    };
    assert!(
        win.handle_web_message(
            &post_close_settings,
            &palette,
            &mut backend,
            &opener,
            &clipboard,
            now
        )
        .is_empty()
    );
    assert!(
        win.handle_web_message(
            &post_close_check,
            &palette,
            &mut backend,
            &opener,
            &clipboard,
            now
        )
        .is_empty()
    );
    assert!(win.posted_raw_events().is_empty());
    assert!(opener.urls().is_empty());
    assert_eq!(geo.resolve_call_count(), 0);
}

#[test]
fn b2_instant_view_window_rejects_bridge_messages_from_untrusted_sender() {
    let page = Page {
        id: 777,
        url: "https://example.org/post".to_owned(),
        ..Page::default()
    };
    let mut iv = InstantViewWindow::new(page, Arc::new(MemoryMediaStore::new()), false, None);
    let mut backend = MockWebViewBackend::new();
    let opener = RecordingOpener::default();
    let clipboard = NoopClipboard;

    // Untrusted sender trying to trigger external URL open or channel join via bridge.
    let evil_msg = WebMessage {
        text:
            r#"{"type":"link_click","href":"https://attacker.example/exploit","new_window":true}"#
                .to_owned(),
        source_url: "https://attacker.example/frame".to_owned(),
    };
    let evs = iv.handle_web_message(&evil_msg, &mut backend, &opener, &clipboard);
    assert!(evs.is_empty(), "untrusted IV sender must produce no events");
    assert!(
        opener.urls().is_empty(),
        "untrusted IV sender must not open external URLs"
    );

    // Prefix-confused sender is also rejected.
    let confused_msg = WebMessage {
        text: r#"{"type":"channel_join","channel_id":123,"username":"demo"}"#.to_owned(),
        source_url: format!("{IV_ORIGIN}.attacker.example/index.html"),
    };
    let evs2 = iv.handle_web_message(&confused_msg, &mut backend, &opener, &clipboard);
    assert!(evs2.is_empty());

    // Authentic IV origin succeeds.
    let valid_msg = WebMessage {
        text: r#"{"type":"link_click","href":"https://example.org/ok","new_window":true}"#
            .to_owned(),
        source_url: format!("{IV_ORIGIN}/index.html"),
    };
    let evs3 = iv.handle_web_message(&valid_msg, &mut backend, &opener, &clipboard);
    assert_eq!(
        evs3,
        vec![AppWebViewEvent::OpenExternalUrl {
            url: "https://example.org/ok".to_owned(),
        }]
    );
}

#[test]
fn b5_mini_app_webview_profile_is_per_account_under_workdir_not_temp() {
    let workdir = TempDir::new().expect("temp workdir");
    let tdata = workdir.path().join("tdata");
    let acc1_dir = account_bot_storage_dir(&tdata, "user_1001");
    let acc2_dir = account_bot_storage_dir(&tdata, "user_2002");

    // Even default `MiniAppWindow::new` must NOT point into `%TEMP%\rustgram-webview`.
    let default_win = MiniAppWindow::new(sample_launch(42, "https://bot.example/app"));
    let temp_legacy = std::env::temp_dir().join("rustgram-webview");
    assert!(
        !default_win
            .webview_user_data_dir()
            .starts_with(&temp_legacy),
        "default WebView2 profile must not live under %TEMP%\\rustgram-webview, got {:?}",
        default_win.webview_user_data_dir()
    );

    // Same bot opened under two different accounts gets two distinct per-account profile folders
    // strictly inside the working directory.
    let win1 = MiniAppWindow::new(sample_launch(42, "https://bot.example/app"))
        .with_bot_storage_dir(acc1_dir.clone());
    let win2 = MiniAppWindow::new(sample_launch(42, "https://bot.example/app"))
        .with_bot_storage_dir(acc2_dir.clone());

    let p1 = win1.webview_user_data_dir();
    let p2 = win2.webview_user_data_dir();
    assert_ne!(p1, p2, "two accounts must never share a WebView2 profile");
    assert!(
        p1.starts_with(&acc1_dir),
        "account 1 profile must live under account 1 storage dir: {p1:?}"
    );
    assert!(
        p2.starts_with(&acc2_dir),
        "account 2 profile must live under account 2 storage dir: {p2:?}"
    );
    assert!(!p1.starts_with(&temp_legacy));
    assert!(!p2.starts_with(&temp_legacy));
}

#[test]
fn b6_bot_download_filename_sanitized_contained_and_non_overwriting() {
    // 1. Unit sanitization checks: traversal, absolute/UNC/drive paths, illegal chars, trailing
    //    dots, `.lnk`/`.scf`, and Windows DOS device names.
    assert_eq!(sanitize_bot_download_filename("../../evil.exe"), "evil.exe");
    assert_eq!(
        sanitize_bot_download_filename(r"C:\Windows\System32\cmd.exe"),
        "cmd.exe"
    );
    assert_eq!(
        sanitize_bot_download_filename(r"\\attacker\share\payload.bat"),
        "payload.bat"
    );
    assert_eq!(sanitize_bot_download_filename("C:sneaky.txt"), "sneaky.txt");
    assert_eq!(
        sanitize_bot_download_filename("shortcut.lnk"),
        "shortcut.lnk.download"
    );
    assert_eq!(
        sanitize_bot_download_filename("desk.SCF"),
        "desk.SCF.download"
    );
    assert_eq!(sanitize_bot_download_filename("CON.txt"), "_CON.txt");
    assert_eq!(sanitize_bot_download_filename("nul"), "_nul");
    assert_eq!(
        sanitize_bot_download_filename("report.pdf...  "),
        "report.pdf"
    );
    assert_eq!(sanitize_bot_download_filename("..."), "file");
    assert_eq!(
        sanitize_bot_download_filename("a<b>c:d|e?f*g\x07.txt"),
        "abcdefg.txt"
    );

    // 2. End-to-end download modal flow: traversal filename stays strictly inside downloads_dir
    //    and never overwrites an existing file.
    let workdir = TempDir::new().expect("temp workdir");
    let acc_dir = account_bot_storage_dir(&workdir.path().join("tdata"), "user_55");
    let mut win = MiniAppWindow::new(sample_launch(77, "https://bot.example/app"))
        .with_bot_storage_dir(acc_dir);
    let dl_dir = win.downloads_dir();
    std::fs::create_dir_all(&dl_dir).expect("create downloads dir");

    // Pre-create `invoice.pdf` so the download must pick `invoice (1).pdf` instead of overwriting.
    let existing = dl_dir.join("invoice.pdf");
    std::fs::write(&existing, b"original-important-data").expect("write existing");

    let palette = TgPalette::default();
    let mut backend = MockWebViewBackend::new();
    let opener = RecordingOpener::default();
    let clipboard = NoopClipboard;
    let now = Instant::now();

    let msg = WebMessage {
        text: r#"["web_app_request_file_download", {"url":"https://bot.example/dl","file_name":"..\\..\\invoice.pdf"}]"#
            .to_owned(),
        source_url: "https://bot.example/app".to_owned(),
    };
    let _ = win.handle_web_message(&msg, &palette, &mut backend, &opener, &clipboard, now);
    assert!(win.promote_next_modal_headless());
    assert!(matches!(
        win.active_modal(),
        Some(ActiveMiniAppModal::ConfirmDownloadFile(_))
    ));
    let _ = win.resolve_active_modal("download", &mut backend, &opener, now);

    let entries = win.bot_downloads().list(77);
    assert_eq!(entries.len(), 1);
    let chosen = PathBuf::from(&entries[0].path);
    assert_eq!(
        chosen.parent(),
        Some(dl_dir.as_path()),
        "download destination must remain strictly inside downloads_dir: {chosen:?}"
    );
    assert_eq!(
        chosen.file_name().and_then(|n| n.to_str()),
        Some("invoice (1).pdf"),
        "existing file must not be overwritten"
    );
    assert_eq!(
        resolve_safe_bot_download_path(&dl_dir, r"C:\evil\escape.lnk").unwrap(),
        dl_dir.join("escape.lnk.download")
    );
}
