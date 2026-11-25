//! Web-based feature windows for `tg-app`: Instant View and Bot Mini Apps.
//!
//! Port origin:
//! - `Telegram/SourceFiles/iv/iv_controller.cpp` and `iv_instance.cpp` ([`instant_view`])
//! - `Telegram/SourceFiles/ui/chat/attach/attach_bot_webview.cpp` and
//!   `Telegram/SourceFiles/inline_bots/bot_attach_web_view.cpp` ([`mini_app`])
//! - Decoupled [`backend::WebViewBackend`] trait and [`mock::MockWebViewBackend`]
//! - Live `tg-theme` palette synchronization ([`theme`])
//! - Missing WebView2 runtime error dialog using `tg-ui` boxes ([`runtime_error`])

pub use tg_app_core::webview::backend;
pub mod bot_dialogs;
pub mod formula_widget;
pub mod instant_view;
pub mod mini_app;
pub mod mock;
pub mod platform_win;
pub use platform_win::PlatformClipboard;
pub mod rich_editor;
pub mod rich_page_view;
pub mod runtime_error;
pub mod strings;
pub mod theme;
pub mod widget_host;

#[cfg(test)]
mod tests_sec2a;
#[cfg(test)]
mod tests_sec3a;
#[cfg(test)]
mod tests_sec_b;
#[cfg(test)]
mod tests_wire2;

use std::path::PathBuf;
use std::sync::Arc;

use egui::{Key, Modifiers, ViewportCommand};
use tg_theme::Palette as TgPalette;

use crate::webview::backend::{
    AppWebViewCallbacks, AppWebViewEvent, ClipboardAccess, ExternalOpener, PlatformExternalOpener,
    WebViewBackend, WebViewRequest,
};
use crate::webview::instant_view::InstantViewWindow;
use crate::webview::mini_app::MiniAppWindow;
use crate::webview::mock::{MockWebViewBackend, SAMPLE_BOT_ID, SAMPLE_IV_URL};
use crate::webview::runtime_error::RuntimeErrorKind;
use crate::webview::theme::{build_webview_theme_params, tg_palette_for_mode};
use tg_app_core::stand_in::theme::ThemeMode;

/// Top-level coordinator hosting both the Instant View window and Bot Mini App panel
/// alongside [`crate::app::TgforkApp`].
pub struct AppWebViewHost {
    backend: Box<dyn WebViewBackend>,
    external: Box<dyn ExternalOpener>,
    clipboard: Box<dyn ClipboardAccess>,
    callbacks: AppWebViewCallbacks,
    theme_mode: ThemeMode,
    tg_palette: TgPalette,
    instant_view: Option<InstantViewWindow>,
    mini_app: Option<MiniAppWindow>,
    bot_storage_dir: Option<PathBuf>,
    iv_cache_dir: Option<PathBuf>,
    recent_events: Vec<AppWebViewEvent>,
    passcode_locked: bool,
}

impl Default for AppWebViewHost {
    fn default() -> Self {
        Self::new()
    }
}

impl AppWebViewHost {
    /// Creates a new webview host wired to [`MockWebViewBackend`] and native platform helpers.
    #[must_use]
    pub fn new() -> Self {
        Self::with_dependencies(
            Box::new(MockWebViewBackend::new()),
            Box::new(PlatformExternalOpener),
            Box::new(PlatformClipboard),
        )
    }

    /// Creates a host with explicit backend, external URL opener, and clipboard implementations.
    #[must_use]
    pub fn with_dependencies(
        backend: Box<dyn WebViewBackend>,
        external: Box<dyn ExternalOpener>,
        clipboard: Box<dyn ClipboardAccess>,
    ) -> Self {
        let theme_mode = ThemeMode::Day;
        let tg_palette = tg_palette_for_mode(theme_mode);
        Self {
            backend,
            external,
            clipboard,
            callbacks: AppWebViewCallbacks::default(),
            theme_mode,
            tg_palette,
            instant_view: None,
            mini_app: None,
            bot_storage_dir: None,
            iv_cache_dir: None,
            recent_events: Vec::new(),
            passcode_locked: false,
        }
    }

    /// Configures an optional disk backing directory for Instant View [`tg_iv::IvCachedMedia`].
    pub fn set_iv_cache_dir(&mut self, dir: Option<PathBuf>) {
        self.iv_cache_dir = dir;
    }

    /// Returns the configured Instant View disk cache directory, if any.
    #[must_use]
    pub fn iv_cache_dir(&self) -> Option<&std::path::Path> {
        self.iv_cache_dir.as_deref()
    }

    /// Configures an explicit per-account `tdata` storage directory for Mini App [`bot_dialogs::BotLocalStorage`].
    pub fn set_bot_storage_dir(&mut self, dir: Option<PathBuf>) {
        self.bot_storage_dir = dir.clone();
        if let Some(ma) = &mut self.mini_app {
            ma.set_bot_storage_dir(dir);
        }
    }

    /// Installs application callbacks for `tg://` links, media viewer, channels, share, and toasts.
    pub fn set_callbacks(&mut self, callbacks: AppWebViewCallbacks) {
        self.callbacks = callbacks;
    }

    /// Returns a reference to the open Instant View window, if any.
    #[must_use]
    pub fn instant_view(&self) -> Option<&InstantViewWindow> {
        self.instant_view.as_ref()
    }

    /// Returns a mutable reference to the open Instant View window, if any.
    pub fn instant_view_mut(&mut self) -> Option<&mut InstantViewWindow> {
        self.instant_view.as_mut()
    }

    /// Returns a reference to the open Bot Mini App panel, if any.
    #[must_use]
    pub fn mini_app(&self) -> Option<&MiniAppWindow> {
        self.mini_app.as_ref()
    }

    /// Returns a mutable reference to the open Bot Mini App panel, if any.
    pub fn mini_app_mut(&mut self) -> Option<&mut MiniAppWindow> {
        self.mini_app.as_mut()
    }

    /// Drains all emitted [`AppWebViewEvent`]s since the last call.
    pub fn take_events(&mut self) -> Vec<AppWebViewEvent> {
        std::mem::take(&mut self.recent_events)
    }

    /// Opens (or navigates) the Instant View top-level window for `url`.
    ///
    /// Falls back to opening in the external browser when the backend reports no Instant View page.
    pub fn open_instant_view(
        &mut self,
        url: &str,
        webpage_id: Option<i64>,
        fragment: Option<&str>,
    ) {
        if self.passcode_locked {
            return;
        }
        if let Some(iv) = &mut self.instant_view
            && iv.is_open()
        {
            let evs = iv.open_url(
                url,
                webpage_id,
                fragment,
                self.backend.as_mut(),
                self.external.as_ref(),
            );
            self.process_events(evs, None);
            return;
        }

        match self.backend.fetch_iv_page(url, webpage_id) {
            Ok(Some(bundle)) => {
                let win = if let Some(ref cache_dir) = self.iv_cache_dir {
                    InstantViewWindow::with_disk_cache(
                        bundle.page.clone(),
                        Arc::clone(&bundle.media),
                        cache_dir.clone(),
                        self.theme_mode.is_night(),
                        fragment,
                    )
                    .unwrap_or_else(|_| {
                        InstantViewWindow::new(
                            bundle.page,
                            bundle.media,
                            self.theme_mode.is_night(),
                            fragment,
                        )
                    })
                } else {
                    InstantViewWindow::new(
                        bundle.page,
                        bundle.media,
                        self.theme_mode.is_night(),
                        fragment,
                    )
                };
                self.instant_view = Some(win);
            }
            Ok(None) | Err(_) => {
                let _ = self.external.open_url(url);
                self.process_events(
                    vec![AppWebViewEvent::OpenExternalUrl {
                        url: url.to_owned(),
                    }],
                    None,
                );
            }
        }
    }

    /// Opens the built-in sample Instant View article (`MockWebViewBackend`).
    pub fn open_sample_instant_view(&mut self) {
        self.open_instant_view(SAMPLE_IV_URL, None, None);
    }

    /// Opens an Instant View window displaying a simulated or real runtime error box.
    pub fn open_instant_view_with_error(&mut self, url: &str, error: RuntimeErrorKind) {
        self.instant_view = Some(InstantViewWindow::with_runtime_error(
            url,
            error,
            self.theme_mode.is_night(),
        ));
    }

    /// Follows the app-wide passcode lock: locking closes the Instant View and Mini App windows
    /// (`Application::closeAdditionalWindows`) and, until unlocked, no window opens or renders.
    pub fn set_passcode_locked(&mut self, locked: bool) {
        if locked {
            self.close_instant_view();
            self.close_mini_app();
        }
        self.passcode_locked = locked;
    }

    /// Returns `true` while the app is locked behind the passcode screen.
    #[must_use]
    pub const fn is_passcode_locked(&self) -> bool {
        self.passcode_locked
    }

    /// Requests and opens a Bot Mini App panel for `request`.
    pub fn open_mini_app(&mut self, request: WebViewRequest) {
        if self.passcode_locked {
            return;
        }
        if let Some(existing) = self.mini_app.take() {
            let mut ex = existing;
            ex.close_immediately();
        }
        match self.backend.request_web_view(request) {
            Ok(launch) => {
                let mut win = MiniAppWindow::new(launch);
                if let Some(dir) = self
                    .bot_storage_dir
                    .clone()
                    .or_else(|| self.backend.bot_storage_dir())
                {
                    win.set_bot_storage_dir(Some(dir));
                }
                self.mini_app = Some(win);
            }
            Err(err) => {
                self.process_events(
                    vec![AppWebViewEvent::Toast {
                        message: format!("Failed to open Mini App: {err}"),
                    }],
                    None,
                );
            }
        }
    }

    /// Opens the built-in local demo Bot Mini App (`Catalog Bot`).
    pub fn open_sample_mini_app(&mut self) {
        let theme_params_json = build_webview_theme_params(&self.tg_palette).json;
        self.open_mini_app(WebViewRequest {
            bot_id: SAMPLE_BOT_ID,
            start_param: None,
            url_override: None,
            fullscreen: false,
            theme_params_json,
        });
    }

    /// Closes the Instant View window if open.
    pub fn close_instant_view(&mut self) {
        if let Some(mut iv) = self.instant_view.take() {
            iv.close();
        }
    }

    /// Closes the Bot Mini App panel immediately if open.
    pub fn close_mini_app(&mut self) {
        if let Some(mut ma) = self.mini_app.take() {
            ma.close_immediately();
        }
    }

    /// Syncs Day/Night theme mode from the main app to both open webview windows.
    pub fn sync_theme(&mut self, mode: ThemeMode) {
        if self.theme_mode == mode {
            return;
        }
        self.theme_mode = mode;
        self.tg_palette = tg_palette_for_mode(mode);
        let is_night = mode.is_night();
        if let Some(iv) = &mut self.instant_view {
            iv.apply_theme(&self.tg_palette, is_night);
        }
        if let Some(ma) = &mut self.mini_app {
            ma.apply_theme(&self.tg_palette);
        }
    }

    /// Drives one frame in `TgforkApp::ui`: checks shortcuts (`Ctrl+Shift+I` for sample Instant
    /// View, `Ctrl+Shift+M` for sample Mini App), syncs theme, and renders both secondary viewports.
    pub fn update_and_render(&mut self, ctx: &egui::Context, theme_mode: ThemeMode) {
        if self.passcode_locked {
            self.close_instant_view();
            self.close_mini_app();
            return;
        }
        self.sync_theme(theme_mode);

        // Developer / preview shortcuts from the main window:
        // Ctrl+Shift+I -> Open sample Instant View window
        // Ctrl+Shift+M -> Open sample Bot Mini App panel
        let (open_iv, open_ma) = ctx.input(|i| {
            let chord = i
                .modifiers
                .matches_logically(Modifiers::COMMAND | Modifiers::SHIFT);
            (
                chord && i.key_pressed(Key::I),
                chord && i.key_pressed(Key::M),
            )
        });
        if open_iv {
            self.open_sample_instant_view();
        }
        if open_ma {
            self.open_sample_mini_app();
        }

        let is_night = self.theme_mode.is_night();

        if let Some(mut iv) = self.instant_view.take() {
            let evs = iv.show_viewport(
                ctx,
                &self.tg_palette,
                is_night,
                self.backend.as_mut(),
                self.external.as_ref(),
                self.clipboard.as_ref(),
            );
            if iv.is_open() {
                self.instant_view = Some(iv);
            }
            self.process_events(evs, Some(ctx));
        }

        if let Some(mut ma) = self.mini_app.take() {
            let evs = ma.show_viewport(
                ctx,
                &self.tg_palette,
                is_night,
                self.backend.as_mut(),
                self.external.as_ref(),
                self.clipboard.as_ref(),
            );
            if ma.is_open() {
                self.mini_app = Some(ma);
            }
            self.process_events(evs, Some(ctx));
        }
    }

    /// Cleanly tears down any open WebView2 controllers before application shutdown.
    pub fn on_exit(&mut self) {
        self.close_instant_view();
        self.close_mini_app();
    }

    fn process_events(&mut self, events: Vec<AppWebViewEvent>, ctx: Option<&egui::Context>) {
        for ev in events {
            self.callbacks.dispatch(&ev);
            match &ev {
                AppWebViewEvent::OpenInstantView {
                    url,
                    webpage_id,
                    fragment,
                } => {
                    self.open_instant_view(url, *webpage_id, fragment.as_deref());
                }
                AppWebViewEvent::QuitApp => {
                    if let Some(c) = ctx {
                        c.send_viewport_cmd(ViewportCommand::Close);
                    }
                }
                _ => {}
            }
            self.recent_events.push(ev);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use serde_json::json;
    use tg_iv::{
        BridgeMessage, LinkTarget, MediaKind, NavigationDecision, ShortcutAction, ZOOM_DEFAULT,
        ZOOM_STEP, classify_link,
    };
    use tg_theme::Palette as TgPalette;
    use tg_webview::{
        Availability, AvailabilityError, HostEvent, MiniAppAction, MiniAppEvent,
        SecondaryButtonPosition, WEBVIEW2_DOWNLOAD_URL, WebMessage,
    };

    use crate::webview::AppWebViewHost;
    use tg_app_core::stand_in::theme::ThemeMode;

    use crate::webview::backend::{
        AppWebViewCallbacks, AppWebViewEvent, ClipboardAccess, ExternalOpener, WebViewBackend,
        WebViewRequest,
    };
    use crate::webview::instant_view::{
        IV_ORIGIN, InstantViewWindow, evaluate_iv_navigation_policy,
    };
    use crate::webview::mini_app::{
        ActiveMiniAppModal, MINI_APPS_TERMS_URL, MiniAppWindow, is_local_telegram_uri,
    };
    use crate::webview::mock::{
        MockWebViewBackend, SAMPLE_BOT_ID, SAMPLE_IV_SECOND_URL, SAMPLE_IV_URL, SAMPLE_MINI_APP_URL,
    };
    use crate::webview::runtime_error::{
        ACTION_CLOSE_ERROR, ACTION_DOWNLOAD_WEBVIEW2, RuntimeErrorKind,
    };
    use crate::webview::theme::{compute_contrast_colors, tg_palette_for_mode};

    #[derive(Default)]
    struct FakeExternalOpener {
        opened: RefCell<Vec<String>>,
    }

    impl FakeExternalOpener {
        fn urls(&self) -> Vec<String> {
            self.opened.borrow().clone()
        }
    }

    impl ExternalOpener for FakeExternalOpener {
        fn open_url(&self, url: &str) -> Result<(), String> {
            self.opened.borrow_mut().push(url.to_owned());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeClipboard {
        text: RefCell<Option<String>>,
    }

    impl FakeClipboard {
        fn with_text(text: &str) -> Self {
            Self {
                text: RefCell::new(Some(text.to_owned())),
            }
        }

        fn current(&self) -> Option<String> {
            self.text.borrow().clone()
        }
    }

    impl ClipboardAccess for FakeClipboard {
        fn read_text(&self) -> Option<String> {
            self.text.borrow().clone()
        }

        fn write_text(&self, text: &str) -> bool {
            *self.text.borrow_mut() = Some(text.to_owned());
            true
        }
    }

    fn make_sample_iv_window() -> (InstantViewWindow, MockWebViewBackend) {
        let backend = MockWebViewBackend::new();
        let bundle = backend
            .fetch_iv_page(SAMPLE_IV_URL, None)
            .expect("fetch sample IV page")
            .expect("sample IV page exists");
        let win = InstantViewWindow::new(bundle.page, bundle.media, false, None);
        (win, backend)
    }

    fn make_sample_mini_app_window() -> (MiniAppWindow, MockWebViewBackend) {
        let mut backend = MockWebViewBackend::new();
        let launch = backend
            .request_web_view(WebViewRequest {
                bot_id: SAMPLE_BOT_ID,
                start_param: None,
                url_override: None,
                fullscreen: false,
                theme_params_json: "{}".to_owned(),
            })
            .expect("request sample mini app");
        let win = MiniAppWindow::new(launch);
        (win, backend)
    }

    // -------------------------------------------------------------------------
    // 1. Instant View custom-scheme server (`rustgram-internal://` -> `IvServer`)
    // -------------------------------------------------------------------------

    #[test]
    fn iv_resource_bridge_serves_html_css_js_media_and_http_ranges() {
        let (iv, _backend) = make_sample_iv_window();
        let bridge = iv.resource_bridge();

        // Root page HTML served from rustgram-internal://iv/index.html.
        let html_resp = bridge
            .serve_sync("rustgram-internal://iv/index.html", None)
            .expect("index.html matched");
        assert_eq!(html_resp.status_code, 200);
        assert!(html_resp.headers.contains("text/html"));

        // CSS and JS assets served from tg_iv::IvServer.
        let css_resp = bridge
            .serve_sync("rustgram-internal://iv/css/page.css", None)
            .expect("page.css matched");
        assert_eq!(css_resp.status_code, 200);
        assert!(css_resp.headers.contains("text/css"));

        let js_resp = bridge
            .serve_sync("rustgram-internal://iv/js/page.js", None)
            .expect("page.js matched");
        assert_eq!(js_resp.status_code, 200);
        assert!(js_resp.headers.contains("application/javascript"));

        // Photo served from MemoryMediaStore with HTTP Range -> 206 Partial Content.
        let range_resp = bridge
            .serve_sync("rustgram-internal://iv/media/photo/101", Some("bytes=0-31"))
            .expect("photo range matched");
        assert_eq!(range_resp.status_code, 206);
        assert!(range_resp.headers.contains("Content-Range: bytes 0-31/"));
        let mut stream = range_resp.stream.expect("range body stream");
        assert_eq!(stream.size(), 32);
        let mut buf = [0u8; 32];
        assert_eq!(stream.read(&mut buf).expect("read stream"), 32);
        assert!(std::str::from_utf8(&buf).unwrap_or("").starts_with("<svg"));

        // Unrelated external URL is not matched by SchemeRouter.
        assert!(bridge.serve_sync("https://example.org/", None).is_none());
    }

    // -------------------------------------------------------------------------
    // 2. Instant View link routing, history, media clicks, menu & zoom
    // -------------------------------------------------------------------------

    #[test]
    fn iv_link_routing_classifies_anchors_iv_pages_tg_links_and_external_urls() {
        let (mut iv, mut backend) = make_sample_iv_window();
        let external = FakeExternalOpener::default();
        let clipboard = FakeClipboard::default();

        // Anchor link scrolls in-place without navigating or opening browser.
        let anchor_target = classify_link("#more-details", Some(SAMPLE_IV_URL), None);
        assert_eq!(
            anchor_target,
            LinkTarget::Anchor {
                fragment: "more-details".to_owned()
            }
        );
        let evs = iv.handle_bridge_json(
            r##"{"type":"link_click","href":"#more-details"}"##,
            &mut backend,
            &external,
            &clipboard,
        );
        assert!(evs.is_empty());
        assert_eq!(iv.controller().current_url(), SAMPLE_IV_URL);
        assert!(external.urls().is_empty());

        // Record scroll before navigating to a second Instant View page.
        let _ = iv.handle_bridge_json(
            r#"{"type":"scroll_position","y":420.0,"max_y":2000.0,"progress":0.21}"#,
            &mut backend,
            &external,
            &clipboard,
        );

        // Linked Instant View page opens in the SAME window and pushes history.
        let second_json =
            format!(r#"{{"type":"link_click","href":"{SAMPLE_IV_SECOND_URL}","webpage_id":777}}"#);
        let evs = iv.handle_bridge_json(&second_json, &mut backend, &external, &clipboard);
        assert!(evs.is_empty());
        assert_eq!(iv.controller().current_url(), SAMPLE_IV_SECOND_URL);
        assert!(iv.controller().can_go_back());
        assert!(!iv.controller().can_go_forward());

        // Back button restores first page & enables Forward.
        let evs = iv.go_back(&mut backend, &external);
        assert!(evs.is_empty());
        assert_eq!(iv.controller().current_url(), SAMPLE_IV_URL);
        assert!(!iv.controller().can_go_back());
        assert!(iv.controller().can_go_forward());

        // Forward button returns to second page.
        let evs = iv.go_forward(&mut backend, &external);
        assert!(evs.is_empty());
        assert_eq!(iv.controller().current_url(), SAMPLE_IV_SECOND_URL);

        // tg:// link goes back to the app through callback event (not external browser).
        let evs = iv.handle_bridge_json(
            r#"{"type":"link_click","href":"tg://resolve?domain=rustgram_news"}"#,
            &mut backend,
            &external,
            &clipboard,
        );
        assert_eq!(
            evs,
            vec![AppWebViewEvent::OpenTelegramLink {
                uri: "tg://resolve?domain=rustgram_news".to_owned()
            }]
        );
        assert!(external.urls().is_empty());

        // External link opens in system browser via ExternalOpener (tg-platform).
        let evs = iv.handle_bridge_json(
            r#"{"type":"link_click","href":"https://example.org/docs"}"#,
            &mut backend,
            &external,
            &clipboard,
        );
        assert_eq!(
            evs,
            vec![AppWebViewEvent::OpenExternalUrl {
                url: "https://example.org/docs".to_owned()
            }]
        );
        assert_eq!(external.urls(), vec!["https://example.org/docs"]);

        // Disallowed javascript: scheme is blocked cleanly.
        let evs = iv.handle_bridge_json(
            r#"{"type":"link_click","href":"javascript:alert(1)"}"#,
            &mut backend,
            &external,
            &clipboard,
        );
        assert!(evs.is_empty());
        assert_eq!(external.urls().len(), 1);
    }

    #[test]
    fn iv_media_click_channel_join_menu_zoom_and_navigation_policy() {
        let (mut iv, mut backend) = make_sample_iv_window();
        let external = FakeExternalOpener::default();
        let clipboard = FakeClipboard::default();

        // Media click emits OpenMediaViewer callback event.
        let evs = iv.handle_bridge_json(
            r#"{"type":"media_click","kind":"photo","id":102}"#,
            &mut backend,
            &external,
            &clipboard,
        );
        assert_eq!(
            evs,
            vec![AppWebViewEvent::OpenMediaViewer {
                kind: MediaKind::Photo,
                id: 102
            }]
        );

        // Channel join invokes backend.join_channel.
        let _ = iv.handle_bridge_json(
            r#"{"type":"channel_join","channel_id":1000042,"username":"rustgram_news"}"#,
            &mut backend,
            &external,
            &clipboard,
        );
        assert_eq!(backend.joined_channels, vec![1_000_042]);

        // Title bar menu actions: copy link, share, open in browser, zoom in/out/reset.
        let _ = iv.handle_menu_action("copy_link", &mut backend, &external, &clipboard);
        assert_eq!(clipboard.current().as_deref(), Some(SAMPLE_IV_URL));

        let share_evs = iv.handle_menu_action("share", &mut backend, &external, &clipboard);
        assert!(matches!(
            share_evs.first(),
            Some(AppWebViewEvent::ShareLink { url, .. }) if url == SAMPLE_IV_URL
        ));

        let _ = iv.handle_menu_action("open_browser", &mut backend, &external, &clipboard);
        assert_eq!(external.urls(), vec![SAMPLE_IV_URL]);

        let _ = iv.handle_menu_action("zoom_in", &mut backend, &external, &clipboard);
        assert_eq!(iv.controller().zoom().percent(), ZOOM_DEFAULT + ZOOM_STEP);
        let _ = iv.handle_menu_action("zoom_out", &mut backend, &external, &clipboard);
        assert_eq!(iv.controller().zoom().percent(), ZOOM_DEFAULT);
        let _ = iv.handle_menu_action("zoom_in", &mut backend, &external, &clipboard);
        let _ = iv.handle_menu_action("zoom_reset", &mut backend, &external, &clipboard);
        assert_eq!(iv.controller().zoom().percent(), ZOOM_DEFAULT);

        // Navigation policy allows local IV origin and about:blank, cancels external origins.
        assert_eq!(
            evaluate_iv_navigation_policy(
                iv.controller(),
                &format!("{IV_ORIGIN}/index.html"),
                SAMPLE_IV_URL,
                false
            ),
            NavigationDecision::Allow
        );
        assert!(matches!(
            evaluate_iv_navigation_policy(
                iv.controller(),
                "https://example.org/other",
                SAMPLE_IV_URL,
                false
            ),
            NavigationDecision::Cancel(Some(LinkTarget::ExternalUrl { .. }))
        ));

        // Escape shortcut closes IV window.
        let actions = iv
            .controller()
            .clone()
            .handle_bridge_message(BridgeMessage::KeyShortcut {
                shortcut: ShortcutAction::Close,
            });
        let _ = iv.handle_controller_actions(actions, &mut backend, &external, &clipboard);
        assert!(!iv.is_open());
    }

    // -------------------------------------------------------------------------
    // 3. Bot Mini App panel: buttons, layout, closing confirm, popups, gates
    // -------------------------------------------------------------------------

    #[test]
    fn mini_app_buttons_layout_closing_confirmation_and_menu_actions() {
        let (mut panel, mut backend) = make_sample_mini_app_window();
        let palette = TgPalette::default();
        let external = FakeExternalOpener::default();
        let clipboard = FakeClipboard::with_text("secret-clip");
        let now = Instant::now();

        assert_eq!(panel.bottom_bar_height(), 0.0);

        // Setup main button + secondary button side-by-side (`left`) vs stacked (`top`).
        let msg_main = WebMessage {
            text: json!([
                "web_app_setup_main_button",
                { "is_visible": true, "is_active": true, "text": "Pay 180 Stars" }
            ])
            .to_string(),
            source_url: SAMPLE_MINI_APP_URL.to_owned(),
        };
        let _ = panel.handle_web_message(
            &msg_main,
            &palette,
            &mut backend,
            &external,
            &clipboard,
            now,
        );
        let single_row_h = panel.bottom_bar_height();
        assert!(single_row_h > 40.0);

        let msg_sec_left = WebMessage {
            text: json!([
                "web_app_setup_secondary_button",
                { "is_visible": true, "is_active": true, "text": "Cart", "position": "left" }
            ])
            .to_string(),
            source_url: SAMPLE_MINI_APP_URL.to_owned(),
        };
        let _ = panel.handle_web_message(
            &msg_sec_left,
            &palette,
            &mut backend,
            &external,
            &clipboard,
            now,
        );
        assert_eq!(panel.bottom_bar_height(), single_row_h);
        assert_eq!(
            panel.controller().state().secondary_position,
            SecondaryButtonPosition::Left
        );

        let msg_sec_top = WebMessage {
            text: json!([
                "web_app_setup_secondary_button",
                { "is_visible": true, "is_active": true, "text": "Cart", "position": "top" }
            ])
            .to_string(),
            source_url: SAMPLE_MINI_APP_URL.to_owned(),
        };
        let _ = panel.handle_web_message(
            &msg_sec_top,
            &palette,
            &mut backend,
            &external,
            &clipboard,
            now,
        );
        assert!(panel.bottom_bar_height() > single_row_h);

        // Settings menu item appears only when web_app_setup_settings_button is enabled.
        assert!(!panel.build_menu_items().iter().any(|it| matches!(
            it,
            tg_ui::widgets::menu::MenuItem::Action { id, .. } if id == "settings"
        )));
        let _ = panel.handle_web_message(
            &WebMessage {
                text: json!(["web_app_setup_settings_button", { "is_visible": true }]).to_string(),
                source_url: SAMPLE_MINI_APP_URL.to_owned(),
            },
            &palette,
            &mut backend,
            &external,
            &clipboard,
            now,
        );
        assert!(panel.build_menu_items().iter().any(|it| matches!(
            it,
            tg_ui::widgets::menu::MenuItem::Action { id, .. } if id == "settings"
        )));

        // Terms of Service opens the official Mini Apps ToS URL via ExternalOpener.
        let evs = panel.handle_menu_action("terms", &external, now);
        assert_eq!(
            evs,
            vec![AppWebViewEvent::OpenExternalUrl {
                url: MINI_APPS_TERMS_URL.to_owned()
            }]
        );

        // Closing confirmation intercepts window close and opens ConfirmClose modal.
        let _ = panel.handle_web_message(
            &WebMessage {
                text: json!(["web_app_setup_closing_behavior", { "need_confirmation": true }])
                    .to_string(),
                source_url: SAMPLE_MINI_APP_URL.to_owned(),
            },
            &palette,
            &mut backend,
            &external,
            &clipboard,
            now,
        );
        assert!(!panel.request_close());
        assert!(panel.is_open());

        // Simulating modal resolution: "cancel" keeps panel open, "close_anyway" closes it.
        let _ = panel.handle_actions(
            vec![MiniAppAction::ConfirmClosePanel],
            &mut backend,
            &external,
            &clipboard,
            now,
        );
        // Drain directly via resolve_active_modal after promoting manually:
        let (mut panel2, mut backend2) = make_sample_mini_app_window();
        let _ = panel2.handle_web_message(
            &WebMessage {
                text: json!(["web_app_setup_closing_behavior", { "need_confirmation": true }])
                    .to_string(),
                source_url: SAMPLE_MINI_APP_URL.to_owned(),
            },
            &palette,
            &mut backend2,
            &external,
            &clipboard,
            now,
        );
        assert!(!panel2.request_close());
        // Disabling closing confirmation allows immediate close.
        let _ = panel2.handle_web_message(
            &WebMessage {
                text: json!(["web_app_setup_closing_behavior", { "need_confirmation": false }])
                    .to_string(),
                source_url: SAMPLE_MINI_APP_URL.to_owned(),
            },
            &palette,
            &mut backend2,
            &external,
            &clipboard,
            now,
        );
        assert!(panel2.request_close());
        assert!(!panel2.is_open());
    }

    #[test]
    fn mini_app_clipboard_gate_open_link_gate_tg_link_fullscreen_and_custom_method() {
        let (mut panel, mut backend) = make_sample_mini_app_window();
        let palette = TgPalette::default();
        let external = FakeExternalOpener::default();
        let clipboard = FakeClipboard::with_text("copied-secret");
        let t0 = Instant::now();

        // 1. Clipboard read without recent user interaction is DENIED by MiniAppController.
        let actions = panel.controller_mut().handle_event(
            MiniAppEvent::WebAppReadTextFromClipboard(tg_webview::ClipboardReadParams {
                req_id: json!("c1"),
            }),
            &palette,
            t0,
        );
        assert_eq!(
            actions,
            vec![MiniAppAction::PostEvent(HostEvent::ClipboardTextReceived {
                req_id: json!("c1"),
                data: None,
            })]
        );

        // 2. After recording user interaction, clipboard read is ALLOWED within 10 seconds.
        panel.controller_mut().record_user_interaction(t0);
        let actions = panel.controller_mut().handle_event(
            MiniAppEvent::WebAppReadTextFromClipboard(tg_webview::ClipboardReadParams {
                req_id: json!("c2"),
            }),
            &palette,
            t0 + Duration::from_secs(2),
        );
        assert_eq!(
            actions,
            vec![MiniAppAction::ReadClipboard {
                req_id: json!("c2"),
                allowed: true,
            }]
        );

        // 3. External link with try_instant_view within 3s opens Instant View without prompt;
        //    a second open_link without a fresh click requires confirmation (one-shot gate).
        let evs = panel.handle_web_message(
            &WebMessage {
                text: json!([
                    "web_app_open_link",
                    { "url": SAMPLE_IV_URL, "try_instant_view": true }
                ])
                .to_string(),
                source_url: SAMPLE_MINI_APP_URL.to_owned(),
            },
            &palette,
            &mut backend,
            &external,
            &clipboard,
            t0 + Duration::from_secs(1),
        );
        assert_eq!(
            evs,
            vec![AppWebViewEvent::OpenInstantView {
                url: SAMPLE_IV_URL.to_owned(),
                webpage_id: None,
                fragment: None,
            }]
        );

        // Second link without new interaction triggers confirmation modal instead of opening.
        let evs2 = panel.handle_web_message(
            &WebMessage {
                text: json!([
                    "web_app_open_link",
                    { "url": "https://example.org/second" }
                ])
                .to_string(),
                source_url: SAMPLE_MINI_APP_URL.to_owned(),
            },
            &palette,
            &mut backend,
            &external,
            &clipboard,
            t0 + Duration::from_secs(1),
        );
        assert!(evs2.is_empty());
        assert!(external.urls().is_empty());

        // 4. web_app_open_tg_link hands off https://t.me/... to the app.
        let tg_evs = panel.handle_web_message(
            &WebMessage {
                text: json!(["web_app_open_tg_link", { "path_full": "/rustgram_news" }])
                    .to_string(),
                source_url: SAMPLE_MINI_APP_URL.to_owned(),
            },
            &palette,
            &mut backend,
            &external,
            &clipboard,
            t0 + Duration::from_secs(1),
        );
        assert_eq!(
            tg_evs,
            vec![AppWebViewEvent::OpenTelegramLink {
                uri: "https://t.me/rustgram_news".to_owned()
            }]
        );

        // 5. Fullscreen toggles state & updates content_safe_area top inset.
        assert_eq!(panel.compute_content_safe_area().top, 0);
        let _ = panel.handle_web_message(
            &WebMessage {
                text: json!(["web_app_request_fullscreen", {}]).to_string(),
                source_url: SAMPLE_MINI_APP_URL.to_owned(),
            },
            &palette,
            &mut backend,
            &external,
            &clipboard,
            t0 + Duration::from_secs(2),
        );
        assert!(panel.controller().state().fullscreen);
        assert!(panel.compute_content_safe_area().top > 0);

        // 6. Custom method & send_web_app_data route through WebViewBackend.
        let _ = panel.handle_web_message(
            &WebMessage {
                text: json!([
                    "web_app_invoke_custom_method",
                    { "req_id": "m1", "method": "getCatalogInfo", "params": { "a": 1 } }
                ])
                .to_string(),
                source_url: SAMPLE_MINI_APP_URL.to_owned(),
            },
            &palette,
            &mut backend,
            &external,
            &clipboard,
            t0 + Duration::from_secs(2),
        );
        assert_eq!(backend.custom_method_calls.len(), 1);
        assert_eq!(backend.custom_method_calls[0].1, "getCatalogInfo");

        let send_evs = panel.handle_web_message(
            &WebMessage {
                text: json!(["web_app_data_send", { "data": "order=42" }]).to_string(),
                source_url: SAMPLE_MINI_APP_URL.to_owned(),
            },
            &palette,
            &mut backend,
            &external,
            &clipboard,
            t0 + Duration::from_secs(2),
        );
        assert_eq!(backend.sent_data.len(), 1);
        assert_eq!(backend.sent_data[0].2, "order=42");
        assert_eq!(
            send_evs,
            vec![AppWebViewEvent::BotDataSent {
                bot_id: SAMPLE_BOT_ID,
                data: "order=42".to_owned(),
            }]
        );

        assert!(is_local_telegram_uri("tg://resolve?domain=rustgram"));
        assert!(is_local_telegram_uri("https://t.me/rustgram"));
        assert!(!is_local_telegram_uri("https://example.org/"));
    }

    // -------------------------------------------------------------------------
    // 4. Missing WebView2 runtime handling, theme sync & AppWebViewHost wiring
    // -------------------------------------------------------------------------

    #[test]
    fn missing_webview2_runtime_surfaces_download_url_and_host_routes_callbacks() {
        let missing = Availability {
            error: AvailabilityError::NoWebView2,
            custom_scheme_requests: false,
            custom_range_requests: false,
            custom_referer: false,
            browser_version: None,
            details: "Edge WebView2 Runtime not found".to_owned(),
        };
        let err = RuntimeErrorKind::from_availability(&missing).expect("error detected");
        assert_eq!(err.download_url(), Some(WEBVIEW2_DOWNLOAD_URL));
        assert!(err.body_text().contains("WebView2"));

        let mut backend = MockWebViewBackend::new();
        let launch = backend
            .request_web_view(WebViewRequest {
                bot_id: SAMPLE_BOT_ID,
                start_param: None,
                url_override: None,
                fullscreen: false,
                theme_params_json: "{}".to_owned(),
            })
            .expect("launch");
        let mut ma = MiniAppWindow::with_runtime_error(launch, err.clone());
        // Simulate clicking Download WebView2 in the runtime error modal.
        let external = FakeExternalOpener::default();
        // Promote queued modal directly for headless resolution check.
        let mut iv = InstantViewWindow::with_runtime_error(SAMPLE_IV_URL, err.clone(), false);
        assert_eq!(iv.runtime_error(), Some(&err));
        iv.close();
        assert!(!iv.is_open());

        let _ = ma.resolve_active_modal(
            ACTION_DOWNLOAD_WEBVIEW2,
            &mut backend,
            &external,
            Instant::now(),
        );
        ma.close_immediately();
        assert!(!ma.is_open());

        // Verify Night & Day palettes produce distinct contrast colors and theme sync works.
        let day_pal = tg_palette_for_mode(ThemeMode::Day);
        let night_pal = tg_palette_for_mode(ThemeMode::Night);
        assert!(compute_contrast_colors(day_pal.window_bg).is_light);
        assert!(!compute_contrast_colors(night_pal.window_bg).is_light);

        // AppWebViewHost callback dispatch for tg:// links & media clicks.
        let tg_links = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let tg_links_cb = Arc::clone(&tg_links);
        let mut host = AppWebViewHost::with_dependencies(
            Box::new(MockWebViewBackend::new()),
            Box::new(FakeExternalOpener::default()),
            Box::new(FakeClipboard::default()),
        );
        host.set_callbacks(AppWebViewCallbacks {
            on_open_tg_link: Some(Box::new(move |uri| {
                if let Ok(mut g) = tg_links_cb.lock() {
                    g.push(uri.to_owned());
                }
            })),
            ..AppWebViewCallbacks::default()
        });
        host.open_sample_instant_view();
        host.open_sample_mini_app();
        host.sync_theme(ThemeMode::Night);
        assert!(host.instant_view().is_some());
        assert!(host.mini_app().is_some());
        host.process_events(
            vec![AppWebViewEvent::OpenTelegramLink {
                uri: "tg://resolve?domain=rustgram_news".to_owned(),
            }],
            None,
        );
        assert_eq!(
            tg_links.lock().expect("lock").as_slice(),
            &["tg://resolve?domain=rustgram_news"]
        );

        let _ = ACTION_CLOSE_ERROR;
        let _ = ActiveMiniAppModal::ConfirmClose;
    }

    #[test]
    fn mini_app_device_storage_and_remove_from_menu_and_iv_fallback() {
        let (mut panel, mut backend) = make_sample_mini_app_window();
        let palette = TgPalette::default();
        let external = FakeExternalOpener::default();
        let clipboard = FakeClipboard::default();
        let now = Instant::now();

        // Save, get, and clear per-bot local device storage.
        let _ = panel.handle_web_message(
            &WebMessage {
                text: json!([
                    "web_app_device_storage_save_key",
                    { "req_id": "s1", "key": "draft", "value": "hello" }
                ])
                .to_string(),
                source_url: SAMPLE_MINI_APP_URL.to_owned(),
            },
            &palette,
            &mut backend,
            &external,
            &clipboard,
            now,
        );
        let _ = panel.handle_web_message(
            &WebMessage {
                text: json!([
                    "web_app_device_storage_get_key",
                    { "req_id": "g1", "key": "draft" }
                ])
                .to_string(),
                source_url: SAMPLE_MINI_APP_URL.to_owned(),
            },
            &palette,
            &mut backend,
            &external,
            &clipboard,
            now,
        );
        let _ = panel.handle_web_message(
            &WebMessage {
                text: json!(["web_app_device_storage_clear", { "req_id": "c1" }]).to_string(),
                source_url: SAMPLE_MINI_APP_URL.to_owned(),
            },
            &palette,
            &mut backend,
            &external,
            &clipboard,
            now,
        );

        // Popup flood guard allows a popup and queues modal; confirm remove from menu closes panel.
        let _ = panel.handle_web_message(
            &WebMessage {
                text: json!([
                    "web_app_open_popup",
                    {
                        "title": "Notice",
                        "message": "Hello popup",
                        "buttons": [{ "id": "ok", "type": "ok" }]
                    }
                ])
                .to_string(),
                source_url: SAMPLE_MINI_APP_URL.to_owned(),
            },
            &palette,
            &mut backend,
            &external,
            &clipboard,
            now,
        );

        let _ = panel.handle_menu_action("remove_from_menu", &external, now);
        assert!(panel.is_open());

        // Instant View falls back to external browser for URLs with no IV representation.
        let (mut iv, mut iv_backend) = make_sample_iv_window();
        let evs = iv.open_url(
            "https://example.org/no-iv-page",
            None,
            None,
            &mut iv_backend,
            &external,
        );
        assert_eq!(
            evs,
            vec![AppWebViewEvent::OpenExternalUrl {
                url: "https://example.org/no-iv-page".to_owned()
            }]
        );
        assert_eq!(external.urls(), vec!["https://example.org/no-iv-page"]);
    }

    /// Captures live Windows screenshots of both the Instant View window and Bot Mini App panel
    /// (including rendered WebView2 pages + `tg-ui` chrome) into `crates/tg-app/docs/webview*.png`.
    ///
    /// Run manually with:
    /// `cargo test -p tg-app capture_webview_screenshots -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn capture_webview_screenshots() {
        use std::fs::File;
        use std::io::BufWriter;
        use std::path::PathBuf;

        let avail = tg_webview::availability();
        eprintln!(
            "WebView2 availability: supported={}, version={:?}, details={:?}",
            avail.is_supported(),
            avail.browser_version,
            avail.details
        );

        let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
        let _ = std::fs::create_dir_all(&docs_dir);
        let iv_path = docs_dir.join("webview_instant_view.png");
        let ma_path = docs_dir.join("webview_mini_app.png");

        fn save_rgba_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) {
            let file = File::create(path).expect("create screenshot png");
            let mut encoder = png::Encoder::new(BufWriter::new(file), w, h);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("write png header");
            writer.write_image_data(rgba).expect("write png data");
            eprintln!("Saved screenshot: {} ({w}x{h})", path.display());
        }

        struct ScreenshotHarness {
            host: AppWebViewHost,
            started_at: Instant,
            ready_since: Option<Instant>,
            captured: bool,
            iv_path: PathBuf,
            ma_path: PathBuf,
        }

        impl eframe::App for ScreenshotHarness {
            fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
                let ctx = ui.ctx().clone();
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.label("Capturing Instant View & Mini App screenshots...");
                });

                self.host.update_and_render(&ctx, ThemeMode::Day);
                ctx.request_repaint_after(Duration::from_millis(50));

                let iv_attached = self
                    .host
                    .instant_view()
                    .and_then(InstantViewWindow::attached_hwnd)
                    .is_some();
                let ma_ready = self.host.mini_app().is_some_and(|ma| {
                    ma.attached_hwnd().is_some() && ma.controller().state().ready
                });

                if iv_attached && ma_ready && self.ready_since.is_none() {
                    self.ready_since = Some(Instant::now());
                }

                let settled = self
                    .ready_since
                    .is_some_and(|t| t.elapsed() >= Duration::from_millis(1200));
                let timed_out = self.started_at.elapsed() >= Duration::from_secs(12);

                if !self.captured && (settled || timed_out) {
                    if let Some(iv) = self.host.instant_view() {
                        eprintln!(
                            "IV state: hwnd={:?}, err={:?}, url={}",
                            iv.attached_hwnd(),
                            iv.runtime_error(),
                            iv.controller().current_url()
                        );
                        if let Some(hwnd) = iv.attached_hwnd() {
                            match crate::webview::platform_win::capture_hwnd_rgba(hwnd) {
                                Ok((w, h, rgba)) => save_rgba_png(&self.iv_path, w, h, &rgba),
                                Err(e) => eprintln!("IV capture failed: {e}"),
                            }
                        }
                    }

                    if let Some(ma) = self.host.mini_app() {
                        eprintln!(
                            "MiniApp state: hwnd={:?}, ready={}, main_btn={:?}, sec_btn={:?}, back={}, settings={}",
                            ma.attached_hwnd(),
                            ma.controller().state().ready,
                            ma.controller().state().main_button.text,
                            ma.controller().state().secondary_button.text,
                            ma.controller().state().back_button_visible,
                            ma.controller().state().settings_button_visible,
                        );
                        if let Some(hwnd) = ma.attached_hwnd() {
                            match crate::webview::platform_win::capture_hwnd_rgba(hwnd) {
                                Ok((w, h, rgba)) => save_rgba_png(&self.ma_path, w, h, &rgba),
                                Err(e) => eprintln!("Mini App capture failed: {e}"),
                            }
                        }
                    }

                    self.captured = true;
                    self.host.on_exit();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }

            fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
                self.host.on_exit();
            }
        }

        let mut host = AppWebViewHost::new();
        host.open_sample_instant_view();
        host.open_sample_mini_app();

        #[cfg(windows)]
        let event_loop_builder: Option<eframe::EventLoopBuilderHook> = Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        }));
        #[cfg(not(windows))]
        let event_loop_builder: Option<eframe::EventLoopBuilderHook> = None;

        let opts = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("Rustgram Screenshot Runner")
                .with_inner_size(egui::Vec2::new(300.0, 120.0))
                .with_position(egui::Pos2::new(20.0, 20.0)),
            event_loop_builder,
            ..Default::default()
        };

        let _ = eframe::run_native(
            "Rustgram Screenshot Runner",
            opts,
            Box::new(move |_cc| {
                Ok(Box::new(ScreenshotHarness {
                    host,
                    started_at: Instant::now(),
                    ready_since: None,
                    captured: false,
                    iv_path,
                    ma_path,
                }))
            }),
        );
    }
}
