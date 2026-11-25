//! Security review #3 (SEC3-A, A5): a Mini App may open the OS location settings at most once per
//! window, and only after one of its location requests was denied (tdesktop has no handler for
//! `web_app_open_location_settings`).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use tg_platform::geolocation::{
    FakeGeoLocationProvider, GeoLocation, GeoLocationAccuracy, GeoPoint, GeolocationStatus,
};
use tg_theme::Palette as TgPalette;
use tg_webview::WebMessage;

use crate::webview::backend::{
    AppWebViewEvent, BotInfo, ClipboardAccess, ExternalOpener, WebViewLaunchInfo,
};
use crate::webview::mini_app::MiniAppWindow;
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

const PAGE: &str = "https://trusted.bot.example/mini";

fn page_message(event: &str) -> WebMessage {
    WebMessage {
        text: format!(r#"["{event}", {{}}]"#),
        source_url: PAGE.to_owned(),
    }
}

struct Page {
    win: MiniAppWindow,
    backend: MockWebViewBackend,
    opener: RecordingOpener,
}

impl Page {
    fn new(geo: FakeGeoLocationProvider) -> Self {
        let mut win = MiniAppWindow::new(sample_launch(93, PAGE));
        win.set_geo_provider(Arc::new(geo));
        Self {
            win,
            backend: MockWebViewBackend::new(),
            opener: RecordingOpener::default(),
        }
    }

    fn send(&mut self, event: &str) -> Vec<AppWebViewEvent> {
        self.win.handle_web_message(
            &page_message(event),
            &TgPalette::default(),
            &mut self.backend,
            &self.opener,
            &NoopClipboard,
            Instant::now(),
        )
    }

    fn settings_opened(&self) -> usize {
        self.opener
            .urls()
            .iter()
            .filter(|u| u.as_str() == tg_platform::url_policy::WINDOWS_LOCATION_SETTINGS_URI)
            .count()
    }
}

fn located() -> FakeGeoLocationProvider {
    FakeGeoLocationProvider::with_location(GeoLocation {
        point: GeoPoint::new(51.5, -0.1),
        accuracy: GeoLocationAccuracy::Exact,
        horizontal_accuracy_meters: Some(5.0),
    })
}

#[test]
fn a5_location_settings_without_a_denied_request_are_dropped() {
    let mut page = Page::new(located());
    for _ in 0..5 {
        let evs = page.send("web_app_open_location_settings");
        assert!(evs.is_empty(), "no event for a dropped request: {evs:?}");
    }
    assert_eq!(
        page.settings_opened(),
        0,
        "a page cannot open Settings on its own"
    );
}

#[test]
fn a5_location_settings_open_once_after_the_user_denies_location() {
    let mut page = Page::new(located());
    let _ = page.send("web_app_request_location");
    assert!(page.win.promote_next_modal_headless());
    let _ = page
        .win
        .resolve_active_modal("deny", &mut page.backend, &page.opener, Instant::now());
    assert_eq!(page.win.location_access(), Some(false));

    let _ = page.send("web_app_open_location_settings");
    assert_eq!(page.settings_opened(), 1, "opened once after the denial");
    for _ in 0..5 {
        let _ = page.send("web_app_request_location");
        let _ = page.send("web_app_open_location_settings");
    }
    assert_eq!(page.settings_opened(), 1, "never again for this window");
}

#[test]
fn a5_location_settings_open_once_when_the_os_blocks_location() {
    let mut page = Page::new(FakeGeoLocationProvider::unavailable(
        GeolocationStatus::Disabled,
    ));
    let _ = page.send("web_app_request_location");
    let _ = page.send("web_app_open_location_settings");
    let _ = page.send("web_app_open_location_settings");
    assert_eq!(page.settings_opened(), 1);
}
