//! Live application host (`LiveAppHost`) driving real `tg-session` lifecycle in `egui`:
//! credential validation (`RUSTGRAM_API_ID`/`RUSTGRAM_API_HASH`), `tdata` passcode unlock,
//! session resume, real `IntroWidget::with_rpc_client` over `MtprotoRpc`, and the unified
//! desktop workspace ([`LiveActiveWorkspace`]) powered by [`WindowController`].
//!
//! Port origin: `Telegram/SourceFiles/core/application.cpp`,
//! `Telegram/SourceFiles/main/main_domain.cpp`, and `Telegram/SourceFiles/window/window_controller.cpp`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{
    Align, CentralPanel, Color32, CornerRadius, Frame, Id, Key, Layout, Modifiers, Order, Pos2,
    Rect, RichText, Sense, Stroke, Ui, UiBuilder, Vec2, ViewportCommand,
};
use tg_api::{ApiCredentials, LoginStep};
use tg_core::domain::Environment;
use tg_core::{CoreSettings, GlobalConfig};
use tg_data::UserId;
use tg_session::{
    AccountManager, ENV_API_HASH, ENV_API_ID, SessionEvent, SessionRuntime, SessionRuntimeConfig,
    StoredSessionSettings, WorkingDirOpenOptions, load_api_credentials_from_env,
    load_api_credentials_from_vars,
};
use tg_stories::StoriesStore;
use tg_ui::theme::ThemeManager;
use tracing::{info, warn};

use crate::business::BusinessScreenHost;
use crate::calls::AppCallsHost;
use crate::core_types::erased_rpc::ErasedRpcClient;
use crate::history::inline_media::default_hardware_audio_enabled;
use crate::intro::{IntroAction, IntroWidget};
use crate::live::bridge::RuntimeBridgedRpc;
use crate::live::convert::now_unix_i32;
use crate::live::multi_account::{MultiAccountFrameAction, MultiAccountWorkspaceHost};
use crate::live::session::{LiveSession, start_mtproto_erased_on};
use crate::live::settings_sync::{
    core_window_position_to_shell, persist_all_settings_to_tdata, shell_window_position_to_core,
};
use crate::notifications::AppNotifyTrayHost;
use crate::notifications::settings::NotifySettingsStore;
use crate::payments::PaymentsScreenHost;
use crate::session_api::AppSession;
use crate::settings::local_passcode::{
    AutoLockController, LocalPasscodeCommand, PasscodeVerifierFn,
};
use crate::stand_in::backend::Backend;
use crate::stand_in::settings::{ShellSettings, WindowPosition};
use crate::stand_in::strings::TgLangStrings;
use crate::stand_in::theme::Palette;
use crate::stand_in::types::{
    AuthUser, CodeSentInfo, CodeVerifyOutcome, ConnectionStatus, DialogEntry, FilterId,
    FolderFilter, MessageItem, PeerId, PeerSummary,
};
use crate::startup::updater::UpdateSnapshot;
use crate::stories::AppStoriesHost;
use crate::stories::backend::StoriesBackend;
use crate::webview::AppWebViewHost;
use crate::webview::backend::{
    ClipboardAccess, ExternalOpener, PlatformExternalOpener, WebViewRequest,
};
use crate::webview::platform_win::PlatformClipboard;
use crate::webview::theme::build_webview_theme_params;
use crate::window::controller::{RootScreen, WebViewHostRequest, WindowController};

/// High-level state discriminant of [`LiveAppHost`] (inspectable in tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveHostState {
    /// `RUSTGRAM_API_ID` or `RUSTGRAM_API_HASH` is missing or invalid.
    MissingCredentials,
    /// Opening `tdata` or initializing the Tokio runtime failed.
    BootstrapError,
    /// `tdata` is protected by a local passcode that must be entered first.
    PasscodeRequired,
    /// Showing the real `IntroWidget` login flow over `MtprotoRpc`.
    Intro,
    /// Authenticated multi-column workspace backed by [`LiveSession`].
    Active,
}

/// Minimal `Backend` shim passed to `IntroWidget::show` while unauthenticated in live mode.
///
/// `IntroWidget` itself drives all real auth RPCs through its `RpcClient` (`RuntimeBridgedRpc<MtprotoRpc>`).
struct IntroStatusShim {
    connection_status: ConnectionStatus,
    proxy_enabled: bool,
}

impl Default for IntroStatusShim {
    fn default() -> Self {
        Self {
            connection_status: ConnectionStatus::Connected,
            proxy_enabled: false,
        }
    }
}

impl Backend for IntroStatusShim {
    fn current_user(&self) -> Option<&AuthUser> {
        None
    }
    fn send_login_code(&mut self, _phone: &str) -> Result<CodeSentInfo, String> {
        Err("Use IntroWidget RPC login.".to_owned())
    }
    fn verify_login_code(&mut self, _code: &str) -> Result<CodeVerifyOutcome, String> {
        Err("Use IntroWidget RPC login.".to_owned())
    }
    fn verify_cloud_password(&mut self, _password: &str) -> Result<AuthUser, String> {
        Err("Use IntroWidget RPC login.".to_owned())
    }
    fn logout(&mut self) {}
    fn folders(&self) -> Vec<FolderFilter> {
        Vec::new()
    }
    fn dialogs(&self, _folder: FilterId, _query: &str) -> Vec<DialogEntry> {
        Vec::new()
    }
    fn peer_summary(&self, _peer_id: PeerId) -> Option<PeerSummary> {
        None
    }
    fn history_slice(&self, _peer_id: PeerId) -> Vec<MessageItem> {
        Vec::new()
    }
    fn send_message(&mut self, _peer_id: PeerId, _text: &str) -> Result<MessageItem, String> {
        Err("Not signed in.".to_owned())
    }
    fn mark_read(&mut self, _peer_id: PeerId) {}
    fn toggle_mute(&mut self, _peer_id: PeerId) -> bool {
        false
    }
    fn connection_status(&self) -> ConnectionStatus {
        self.connection_status
    }
    fn proxy_enabled(&self) -> bool {
        self.proxy_enabled
    }
    fn set_connection_status(&mut self, status: ConnectionStatus) {
        self.connection_status = status;
    }
    fn set_proxy_enabled(&mut self, enabled: bool) {
        self.proxy_enabled = enabled;
    }
    fn retry_connection(&mut self) {}
}

struct NoopExternalOpener;

impl ExternalOpener for NoopExternalOpener {
    fn open_url(&self, _url: &str) -> Result<(), String> {
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

/// Complete live-mode desktop workspace hosting the shared [`WindowController`],
/// [`AppCallsHost`], [`AppStoriesHost`], [`AppNotifyTrayHost`], [`AppWebViewHost`],
/// and live Business / Payments overlays over one [`LiveSession`].
///
/// Generic over `C: RpcClient` so integration tests can drive the exact same active
/// workspace over an in-process fake RPC client without touching the network.
pub struct LiveActiveWorkspace {
    controller: WindowController,
    live_session: LiveSession,
    applied_theme_gen: u64,
    calls_host: AppCallsHost,
    stories_host: AppStoriesHost,
    notify_tray: Option<AppNotifyTrayHost>,
    webview_host: AppWebViewHost,
    business_host: Option<BusinessScreenHost>,
    business_open: bool,
    payments_host: Option<PaymentsScreenHost>,
    payments_open: bool,
    platform_overlays_enabled: bool,
    web_proxy_carrier: crate::live::web_proxy_carrier::WebProxyCarrierHost,
    /// Stories lists loading on a background thread (see [`spawn_stories_bootstrap`]).
    pending_stories: Option<std::sync::mpsc::Receiver<StoriesStore>>,
}

/// Loads the stories lists (`stories.getAllStories` and read states) on a background thread, the
/// way tdesktop's `Data::Stories` preloads them asynchronously: those requests take hundreds of
/// milliseconds and used to block building the workspace before the first frame. Returns `None`
/// when the thread cannot be spawned, and the caller then loads them in place.
fn spawn_stories_bootstrap(
    live_session: &LiveSession,
    self_story_peer: tg_data::PeerId,
    egui_ctx: Option<&egui::Context>,
) -> Option<std::sync::mpsc::Receiver<StoriesStore>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut session = live_session.clone();
    let ctx = egui_ctx.cloned();
    std::thread::Builder::new()
        .name("stories-bootstrap".to_owned())
        .spawn(move || {
            let mut store = StoriesStore::new(self_story_peer);
            session.bootstrap_stories_store(&mut store, now_unix_i32());
            let _ = tx.send(store);
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        })
        .ok()
        .map(|_| rx)
}

impl LiveActiveWorkspace {
    /// Builds a full live desktop workspace wired to `live_session`, including tray/notifications,
    /// calls host, stories host, webview host, and live overlay screens.
    #[must_use]
    pub fn new(
        live_session: LiveSession,
        settings: ShellSettings,
        settings_path: PathBuf,
        egui_ctx: Option<&egui::Context>,
    ) -> Self {
        Self::new_internal(live_session, settings, settings_path, egui_ctx, true)
    }

    /// Builds a full live desktop workspace without creating an individual OS tray icon
    /// (used by [`MultiAccountWorkspaceHost`], which owns a single shared tray & notification host
    /// across all running accounts).
    #[must_use]
    pub fn new_without_tray(
        live_session: LiveSession,
        settings: ShellSettings,
        settings_path: PathBuf,
        egui_ctx: Option<&egui::Context>,
    ) -> Self {
        Self::new_internal(live_session, settings, settings_path, egui_ctx, false)
    }

    fn new_internal(
        mut live_session: LiveSession,
        settings: ShellSettings,
        settings_path: PathBuf,
        egui_ctx: Option<&egui::Context>,
        with_tray: bool,
    ) -> Self {
        if let Some(ctx) = egui_ctx {
            live_session.set_egui_ctx(ctx.clone());
        }
        let enable_hw_audio = default_hardware_audio_enabled();
        let calls_host =
            AppCallsHost::with_backend(live_session.create_calls_backend(), enable_hw_audio)
                .with_demo_shortcuts(false);

        let self_story_peer = StoriesBackend::self_peer_id(&live_session);
        let mut stories_store = StoriesStore::new(self_story_peer);
        let pending_stories = spawn_stories_bootstrap(&live_session, self_story_peer, egui_ctx);
        if pending_stories.is_none() {
            live_session.bootstrap_stories_store(&mut stories_store, now_unix_i32());
        }
        let stories_host =
            AppStoriesHost::with_backend(stories_store, live_session.create_stories_backend());

        let notify_tray = with_tray.then(|| {
            AppNotifyTrayHost::with_backend(
                live_session.create_notify_backend(),
                NotifySettingsStore::default(),
                Box::new(|_| {}),
            )
        });
        let webview_host = AppWebViewHost::with_dependencies(
            live_session.create_webview_backend(),
            Box::new(PlatformExternalOpener),
            Box::new(PlatformClipboard),
        );

        let live_session_clone = live_session.clone();
        let mut controller = WindowController::with_session(live_session, settings, settings_path);
        if let Some(ctx) = egui_ctx {
            controller.theme_manager_mut().apply_to_egui(ctx);
            if with_tray {
                crate::live::export::install_live_export_hooks(
                    ctx,
                    controller.theme_mode(),
                    &live_session_clone,
                );
                crate::live::themes::install_live_themes_hooks(ctx, &live_session_clone);
            }
        }
        let applied_theme_gen = controller.theme_generation();

        Self {
            controller,
            live_session: live_session_clone,
            applied_theme_gen,
            calls_host,
            stories_host,
            notify_tray,
            webview_host,
            business_host: None,
            business_open: false,
            payments_host: None,
            payments_open: false,
            platform_overlays_enabled: true,
            web_proxy_carrier: crate::live::web_proxy_carrier::WebProxyCarrierHost::new(),
            pending_stories,
        }
    }

    /// Builds a deterministic headless live workspace for tests (no OS tray icon, no hardware
    /// audio streams, no OS external browser launch).
    #[must_use]
    pub fn new_headless(
        mut live_session: LiveSession,
        settings: ShellSettings,
        settings_path: PathBuf,
        bootstrap_stories: bool,
    ) -> Self {
        let calls_host = AppCallsHost::with_backend(live_session.create_calls_backend(), false)
            .with_demo_shortcuts(false);

        let self_story_peer = StoriesBackend::self_peer_id(&live_session);
        let mut stories_store = StoriesStore::new(self_story_peer);
        if bootstrap_stories {
            live_session.bootstrap_stories_store(&mut stories_store, now_unix_i32());
        } else {
            live_session.attach_stories_updates_listener();
        }
        let stories_host =
            AppStoriesHost::with_backend(stories_store, live_session.create_stories_backend());

        let webview_host = AppWebViewHost::with_dependencies(
            live_session.create_webview_backend(),
            Box::new(NoopExternalOpener),
            Box::new(NoopClipboard),
        );

        let live_session_clone = live_session.clone();
        let controller = WindowController::with_session(live_session, settings, settings_path);
        Self {
            controller,
            live_session: live_session_clone,
            applied_theme_gen: u64::MAX,
            calls_host,
            stories_host,
            notify_tray: None,
            webview_host,
            business_host: None,
            business_open: false,
            payments_host: None,
            payments_open: false,
            platform_overlays_enabled: false,
            web_proxy_carrier: crate::live::web_proxy_carrier::WebProxyCarrierHost::new(),
            pending_stories: None,
        }
    }

    /// Returns a shared reference to the inner [`WindowController`].
    #[must_use]
    pub const fn controller(&self) -> &WindowController {
        &self.controller
    }

    /// Connects profile data directories (`iv_cache`, `video_drafts`, `exports`) and native file
    /// dialog wiring to this workspace's [`AppWebViewHost`] and [`WindowController`].
    pub fn apply_profile_data_paths(&mut self, working_dir: &std::path::Path) {
        let _ = crate::startup::apply_profile_data_paths_native(
            working_dir,
            &mut self.webview_host,
            &mut self.controller,
        );
    }

    /// Returns a shared reference to the underlying [`LiveSession`].
    #[must_use]
    pub const fn live_session(&self) -> &LiveSession {
        &self.live_session
    }

    /// Returns a mutable reference to the underlying [`LiveSession`].
    pub fn live_session_mut(&mut self) -> &mut LiveSession {
        &mut self.live_session
    }

    /// Returns a mutable reference to the inner [`WindowController`].
    pub fn controller_mut(&mut self) -> &mut WindowController {
        &mut self.controller
    }

    /// Returns a shared reference to the live [`AppCallsHost`].
    #[must_use]
    pub const fn calls_host(&self) -> &AppCallsHost {
        &self.calls_host
    }

    /// Returns a mutable reference to the live [`AppCallsHost`].
    pub fn calls_host_mut(&mut self) -> &mut AppCallsHost {
        &mut self.calls_host
    }

    /// Returns a shared reference to the live [`AppStoriesHost`].
    #[must_use]
    pub const fn stories_host(&self) -> &AppStoriesHost {
        &self.stories_host
    }

    /// Returns a mutable reference to the live [`AppStoriesHost`].
    pub fn stories_host_mut(&mut self) -> &mut AppStoriesHost {
        &mut self.stories_host
    }

    /// Returns the hidden Web proxy carrier host.
    pub fn web_proxy_carrier_mut(
        &mut self,
    ) -> &mut crate::live::web_proxy_carrier::WebProxyCarrierHost {
        &mut self.web_proxy_carrier
    }

    /// Returns a mutable reference to the live [`AppWebViewHost`].
    pub fn webview_host_mut(&mut self) -> &mut AppWebViewHost {
        &mut self.webview_host
    }

    /// Returns a mutable reference to the live [`AppNotifyTrayHost`], when enabled.
    pub fn notify_tray_mut(&mut self) -> Option<&mut AppNotifyTrayHost> {
        self.notify_tray.as_mut()
    }

    /// Returns `true` when the live Payments / Stars / Gifts / Passport overlay is open.
    #[must_use]
    pub const fn payments_open(&self) -> bool {
        self.payments_open
    }

    /// Closes the live Payments / Stars / Gifts / Passport overlay if open.
    pub fn close_payments(&mut self) {
        self.payments_open = false;
    }

    /// Returns a shared reference to the live [`PaymentsScreenHost`], if initialized.
    #[must_use]
    pub fn payments_host(&self) -> Option<&PaymentsScreenHost> {
        self.payments_host.as_ref()
    }

    /// Returns a mutable reference to the live [`PaymentsScreenHost`], if initialized.
    pub fn payments_host_mut(&mut self) -> Option<&mut PaymentsScreenHost> {
        self.payments_host.as_mut()
    }

    /// Returns `true` when the live Telegram Business overlay is open.
    #[must_use]
    pub const fn business_open(&self) -> bool {
        self.business_open
    }

    fn poll_pending_stories(&mut self) {
        let Some(rx) = self.pending_stories.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(store) => {
                *self.stories_host.store_mut() = store;
                self.pending_stories = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.pending_stories = None,
        }
    }

    /// Renders one complete frame of the live workspace, returning `true` if logout was requested.
    pub fn show(&mut self, ctx: &egui::Context, ui: &mut Ui) -> bool {
        self.live_session.set_egui_ctx(ctx.clone());

        let current_gen = self.controller.theme_generation();
        if self.applied_theme_gen != current_gen {
            self.controller.theme_manager_mut().apply_to_egui(ctx);
            self.applied_theme_gen = current_gen;
        }

        let mut request_logout = false;

        // 1. Drain high-level SessionEvents (ForceLogout, SyncError, Ready).
        let session_events = self.live_session.bridge_state().drain_session_events();
        for ev in session_events {
            match ev {
                SessionEvent::ForceLogout { notification_type } => {
                    self.controller.show_toast(format!(
                        "Session signed out by server ({notification_type})."
                    ));
                    request_logout = true;
                }
                SessionEvent::SyncError(err) => {
                    self.controller.show_toast(format!("Sync error: {err}"));
                }
                SessionEvent::Ready => {
                    self.live_session.refresh_cached_views();
                }
                _ => {}
            }
        }

        // 2. Install the background-loaded stories lists once ready, then drain asynchronous live
        //    story updates into the StoriesStore (they stay queued until the lists are installed).
        self.poll_pending_stories();
        if self.pending_stories.is_none() {
            let _ = self
                .live_session
                .drain_story_updates(self.stories_host.store_mut(), now_unix_i32());
        }

        // 3. Dispatch pre-queued deep-link overlays (e.g. IPC `tg://call?slug=`) and render Calls.
        if !self.controller.is_passcode_locked() {
            self.dispatch_deep_link_overlay_requests(ctx);
        }
        self.calls_host
            .set_passcode_locked(self.controller.is_passcode_locked());
        self.calls_host.update_and_render(ui);

        // 3b. Feed freshly decoded live RTMP/HLS stream frames into the viewer before painting.
        let pre_stream_now_ms = self.controller.elapsed_ms();
        {
            let live_handle = self.live_session.clone();
            crate::live::video_stream_loop::sync_live_video_stream(
                &live_handle,
                self.controller.media_overlay_mut(),
                pre_stream_now_ms,
            );
        }

        // 4. Render the unified WindowController + MainWidget + embedded StoriesStrip. Nothing is
        //    marked read while a host overlay covers the chat (SEC3-A A2).
        let host_overlay_open = self.stories_host.has_open_window()
            || self.business_open
            || self.payments_open
            || crate::wallet::AppWalletHost::is_open(ctx);
        self.controller
            .main_widget_mut()
            .history_mut()
            .inner_mut()
            .set_host_overlay_open(host_overlay_open);
        CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            self.controller
                .show_with_stories(ui, Some(&mut self.stories_host));
        });

        let locked = self.controller.is_passcode_locked();
        self.calls_host.set_passcode_locked(locked);
        if locked {
            self.close_for_passcode_lock(ctx);
        } else {
            self.webview_host.set_passcode_locked(false);
        }
        if self.controller.take_open_recent_calls_request() && !locked {
            self.calls_host.open_recent_calls();
        }
        if let Some((peer_id, video)) = self.controller.take_start_call_request()
            && !locked
        {
            self.calls_host.start_outgoing_call(peer_id, video);
        }
        if !locked {
            self.dispatch_deep_link_overlay_requests(ctx);
        }
        if self.controller.take_logout_requested() {
            request_logout = true;
        }

        // 5. Dispatch queued Instant View / Mini App / External URL requests.
        self.dispatch_webview_requests();

        // 6. Drive notifications / tray host & poll notification chat opens.
        let palette = self.controller.palette();
        let theme_mode = self.controller.theme_mode();
        if let Some(notify_tray) = self.notify_tray.as_mut() {
            notify_tray.set_active_peer(self.controller.active_focused_peer());
            notify_tray.update_and_render(
                ctx,
                &palette,
                self.controller.root_screen() == RootScreen::PasscodeLock,
            );
        }
        self.controller.poll_notification_opens(Some(ctx));

        // 7. Render WebView / Instant View host and route events back to WindowController.
        self.webview_host.update_and_render(ctx, theme_mode);
        for ev in self.webview_host.take_events() {
            self.controller.handle_webview_event(ev, ctx);
        }
        if self
            .web_proxy_carrier
            .pump(self.live_session.proxy_controller())
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        // 8. Live keyboard overlays: Statistics (Ctrl+Shift+T), Business (Ctrl+Shift+B),
        //    Payments/Passport (Ctrl+Shift+P), plus Export & Theme Studio. None of them (nor
        //    their shortcuts) run while the passcode lock is shown.
        if locked {
            return request_logout;
        }
        self.update_and_render_live_overlays(ctx, &palette, theme_mode);

        // 9. Tick StoriesStore timers and render fullscreen StoryViewerOverlay when open.
        self.stories_host.update_and_render(ctx, &palette);
        if let Some(mut stream_desc) = self.stories_host.viewer_mut().take_pending_video_stream() {
            let now_ms = self.controller.elapsed_ms();
            let info_res = crate::live::dice_pack::fetch_video_stream_info(
                &self.live_session,
                stream_desc.call_id,
                stream_desc.access_hash,
            );
            if let Ok(ref info) = info_res
                && info.stream_dc_id > 0
            {
                stream_desc.stream_dc_id = info.stream_dc_id;
            }
            self.controller
                .media_overlay_mut()
                .open_video_stream(stream_desc, now_ms);
            if let Some(viewer) = self.controller.media_overlay_mut().video_stream_mut() {
                match info_res {
                    Ok(info) => {
                        viewer.update_metadata(
                            Some(info.title),
                            info.participants_count.max(0) as u32,
                            false,
                        );
                    }
                    Err(err) if err.contains("has ended") => viewer.mark_finished(),
                    Err(_) => {}
                }
            }
        }
        if self.stories_host.take_open_premium_request() {
            self.open_live_payments_premium();
        }
        if self.stories_host.take_update_app_request() {
            self.controller.request_update_check();
        }

        // 9b. Ensure any live video stream opened during this frame starts polling immediately
        //     (or stops immediately if the overlay was closed).
        let post_stream_now_ms = self.controller.elapsed_ms();
        {
            let live_handle = self.live_session.clone();
            crate::live::video_stream_loop::sync_live_video_stream(
                &live_handle,
                self.controller.media_overlay_mut(),
                post_stream_now_ms,
            );
        }

        request_logout
    }

    /// Closes every window that could show account content over the passcode lock: Mini App and
    /// Instant View windows, the story viewer, the Business / Payments overlays and the wallet
    /// (`Application::closeAdditionalWindows`, `application.cpp:197-203, 1432-1440`). The
    /// controller closes its own boxes, media viewer, PiP and round video.
    pub fn close_for_passcode_lock(&mut self, ctx: &egui::Context) {
        self.webview_host.set_passcode_locked(true);
        self.stories_host.close_for_passcode_lock();
        self.business_open = false;
        self.payments_open = false;
        crate::wallet::AppWalletHost::close(ctx);
        let now_ms = self.controller.elapsed_ms();
        crate::live::video_stream_loop::sync_live_video_stream(
            &self.live_session,
            self.controller.media_overlay_mut(),
            now_ms,
        );
    }

    /// Returns `true` when any window that must not show over the lock is open (for tests).
    #[must_use]
    pub fn has_additional_windows_open(&self) -> bool {
        self.webview_host.instant_view().is_some()
            || self.webview_host.mini_app().is_some()
            || self.stories_host.has_open_window()
            || self.business_open
            || self.payments_open
    }

    /// Flushes shell settings and shuts down auxiliary hosts cleanly on exit or logout.
    pub fn on_exit(&mut self) {
        crate::live::video_stream_loop::stop_live_video_stream(&self.live_session);
        self.calls_host.on_exit();
        self.webview_host.on_exit();
        self.web_proxy_carrier.close();
        if let Some(notify_tray) = self.notify_tray.as_mut() {
            notify_tray.on_exit();
        }
        self.live_session.stop_all_prolong_web_views();
        self.controller.flush_settings();
    }

    /// Opens the live Payments overlay on `PaymentsScreen::Premium` (read-only plan preview).
    fn open_live_payments_premium(&mut self) {
        if self.payments_host.is_none() {
            let payments = self.live_session.create_payments_backend();
            let passport = self.live_session.create_passport_backend();
            self.payments_host = Some(PaymentsScreenHost::new(payments, passport));
        }
        if let Some(host) = self.payments_host.as_mut() {
            host.select_screen(crate::payments::PaymentsScreen::Premium);
        }
        self.payments_open = true;
    }

    fn dispatch_deep_link_overlay_requests(&mut self, ctx: &egui::Context) {
        let requests = self.controller.take_deep_link_overlay_requests();
        for req in requests {
            if crate::live::themes::dispatch_themes_deep_link_overlay(ctx, &req) {
                continue;
            }
            match req {
                crate::window::deep_links::DeepLinkOverlayRequest::OpenPayments {
                    screen,
                    slug,
                } => {
                    if self.payments_host.is_none() {
                        let payments = self.live_session.create_payments_backend();
                        let passport = self.live_session.create_passport_backend();
                        self.payments_host = Some(PaymentsScreenHost::new(payments, passport));
                    }
                    if let Some(host) = self.payments_host.as_mut()
                        && let Err(err) = host.open_deep_link_request(screen, slug.as_deref())
                    {
                        self.controller.show_toast(err);
                    }
                    self.payments_open = true;
                }
                crate::window::deep_links::DeepLinkOverlayRequest::OpenBusiness => {
                    if self.business_host.is_none() {
                        let backend = self.live_session.create_business_backend();
                        match BusinessScreenHost::try_new(backend) {
                            Ok(host) => self.business_host = Some(host),
                            Err(err) => {
                                self.controller
                                    .show_toast(format!("Could not load Telegram Business: {err}"));
                                continue;
                            }
                        }
                    }
                    self.business_open = true;
                }
                crate::window::deep_links::DeepLinkOverlayRequest::OpenStory {
                    peer_id,
                    story_id,
                } => {
                    let data_peer = crate::live::convert::to_data_peer(peer_id);
                    if !self.stories_host.open_story_deep_link(data_peer, story_id)
                        && !self.stories_host.open_story_by_id(data_peer, story_id)
                    {
                        self.controller
                            .show_toast("Story is unavailable or expired.");
                    }
                }
                crate::window::deep_links::DeepLinkOverlayRequest::JoinCall {
                    peer_id,
                    slug_or_hash,
                } => {
                    self.calls_host
                        .handle_join_call_deep_link(peer_id, slug_or_hash);
                }
                crate::window::deep_links::DeepLinkOverlayRequest::OpenTheme { .. }
                | crate::window::deep_links::DeepLinkOverlayRequest::OpenBackground { .. }
                | crate::window::deep_links::DeepLinkOverlayRequest::OpenShareTheme { .. } => {}
                crate::window::deep_links::DeepLinkOverlayRequest::OpenWallet { action } => {
                    crate::live::wallet::handle_wallet_deep_link(
                        ctx,
                        &action,
                        &mut self.controller,
                    );
                }
            }
        }
    }

    fn dispatch_webview_requests(&mut self) {
        let requests = self.controller.take_webview_requests();
        if requests.is_empty() {
            return;
        }
        let theme_params_json =
            build_webview_theme_params(self.controller.theme_manager().palette()).json;
        for req in requests {
            match req {
                WebViewHostRequest::OpenInstantView {
                    url,
                    webpage_id,
                    fragment,
                } => {
                    self.webview_host
                        .open_instant_view(&url, webpage_id, fragment.as_deref());
                }
                WebViewHostRequest::OpenMiniApp {
                    bot_id,
                    start_param,
                } => {
                    let bot_u64 = bot_id.max(0) as u64;
                    let url_override = self
                        .controller
                        .session_backend_mut()
                        .take_pending_mini_app_url(PeerId::from_user(bot_u64));
                    self.webview_host.open_mini_app(WebViewRequest {
                        bot_id: bot_u64,
                        start_param,
                        url_override,
                        fullscreen: false,
                        theme_params_json: theme_params_json.clone(),
                    });
                }
                WebViewHostRequest::OpenExternalUrl(url) => {
                    if self.platform_overlays_enabled {
                        let _ = PlatformExternalOpener.open_url(&url);
                    }
                }
            }
        }
    }

    fn update_and_render_live_overlays(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        theme_mode: crate::stand_in::theme::ThemeMode,
    ) {
        let (stats_pressed, business_pressed, payments_pressed, esc_pressed) = ctx.input(|i| {
            let chord = i
                .modifiers
                .matches_logically(Modifiers::COMMAND | Modifiers::SHIFT);
            (
                chord && i.key_pressed(Key::T),
                chord && i.key_pressed(Key::B),
                chord && i.key_pressed(Key::P),
                i.key_pressed(Key::Escape),
            )
        });

        // Ctrl+Shift+T: open live Statistics screen for the currently active peer.
        if stats_pressed {
            if let Some(peer_id) = self.controller.active_focused_peer() {
                self.controller.open_statistics_for_peer(peer_id, false);
            } else {
                self.controller
                    .show_toast("Select a channel, supergroup, or bot chat to view statistics.");
            }
        }

        // Ctrl+Shift+B: toggle live Telegram Business & Premium overlay.
        if business_pressed {
            self.business_open = !self.business_open;
            if self.business_open && self.business_host.is_none() {
                let backend = self.live_session.create_business_backend();
                match BusinessScreenHost::try_new(backend) {
                    Ok(host) => self.business_host = Some(host),
                    Err(err) => {
                        self.business_open = false;
                        self.controller
                            .show_toast(format!("Could not load Telegram Business: {err}"));
                    }
                }
            }
        }
        if self.business_open
            && let Some(host) = self.business_host.as_mut()
            && render_modal_overlay_panel(
                ctx,
                "tg_live_business_overlay",
                palette,
                esc_pressed,
                460.0,
                640.0,
                440.0,
                |ui| host.show(ui, palette, true),
            )
        {
            self.business_open = false;
        }
        if self
            .business_host
            .as_mut()
            .is_some_and(BusinessScreenHost::take_open_payments_premium)
        {
            self.business_open = false;
            self.open_live_payments_premium();
        }

        // Ctrl+Shift+P: toggle live Payments / Stars / Star Gifts / Passport overlay.
        if payments_pressed {
            self.payments_open = !self.payments_open;
            if self.payments_open && self.payments_host.is_none() {
                let payments = self.live_session.create_payments_backend();
                let passport = self.live_session.create_passport_backend();
                self.payments_host = Some(PaymentsScreenHost::new(payments, passport));
            }
        }
        if self.payments_open
            && let Some(host) = self.payments_host.as_mut()
            && render_modal_overlay_panel(
                ctx,
                "tg_live_payments_overlay",
                palette,
                esc_pressed,
                440.0,
                620.0,
                420.0,
                |ui| host.show(ui, palette, true),
            )
        {
            self.payments_open = false;
        }

        if self.platform_overlays_enabled {
            crate::live::export::ensure_live_export_hooks(ctx, theme_mode, &self.live_session);
            crate::live::themes::ensure_live_themes_hooks(ctx, &self.live_session);
            crate::export::AppExportHost::update_and_render(ctx, palette, theme_mode);
            crate::wallet::AppWalletHost::update_and_render(ctx, palette, theme_mode);
            crate::themes::AppThemesHost::hydrate_default_background_emojis(
                ctx,
                &self.live_session,
            );
            crate::themes::AppThemesHost::update_and_render(
                ctx,
                palette.layer_backdrop,
                palette.divider,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_modal_overlay_panel(
    ctx: &egui::Context,
    id_prefix: &'static str,
    palette: &Palette,
    esc_pressed: bool,
    min_w: f32,
    max_w: f32,
    min_h: f32,
    render_body: impl FnOnce(&mut Ui) -> bool,
) -> bool {
    let screen_rect = ctx.content_rect();
    let panel_w = (screen_rect.width() * 0.62).clamp(min_w, max_w);
    let panel_h = (screen_rect.height() - 48.0).max(min_h);
    let panel_pos = Pos2::new(
        screen_rect.center().x - panel_w / 2.0,
        screen_rect.top() + 24.0,
    );

    let mut close_requested = esc_pressed;
    egui::Area::new(Id::unique((id_prefix, "backdrop")))
        .order(Order::Middle)
        .fixed_pos(screen_rect.min)
        .show(ctx, |ui| {
            let (_, resp) = ui.allocate_exact_size(screen_rect.size(), Sense::click());
            ui.painter()
                .rect_filled(screen_rect, CornerRadius::ZERO, palette.layer_backdrop);
            if resp.clicked() {
                close_requested = true;
            }
        });

    egui::Area::new(Id::unique((id_prefix, "panel")))
        .order(Order::Foreground)
        .fixed_pos(panel_pos)
        .show(ctx, |ui| {
            let panel_rect = Rect::from_min_size(panel_pos, Vec2::new(panel_w, panel_h));
            ui.painter()
                .rect_filled(panel_rect, CornerRadius::same(10), palette.window_bg);
            ui.painter().rect_stroke(
                panel_rect,
                CornerRadius::same(10),
                Stroke::new(1.0, palette.divider),
                egui::StrokeKind::Inside,
            );
            let inner = panel_rect.shrink(2.0);
            ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
                ui.set_clip_rect(inner);
                if render_body(ui) {
                    close_requested = true;
                }
            });
        });

    close_requested
}

type ManagerSlot = Arc<Mutex<Option<AccountManager>>>;

/// Mirrors the active workspace's passcode lock onto every account (the lock is app-wide in
/// tdesktop, `application.cpp:1433`): locking one locks all of them and closes their additional
/// windows; unlocking unlocks all. Returns the new app-wide lock state.
pub fn sync_app_wide_passcode_lock(
    host: &mut MultiAccountWorkspaceHost,
    app_locked: bool,
    ctx: &egui::Context,
) -> bool {
    let active_slot = host.active_slot();
    let active_locked = host
        .active_workspace()
        .is_some_and(|ws| ws.controller().is_passcode_locked());
    if active_locked == app_locked {
        return app_locked;
    }
    for slot in host.slot_indices() {
        if slot == active_slot {
            continue;
        }
        if let Some(entry) = host.account_mut(slot) {
            let ws = &mut entry.workspace;
            if active_locked {
                ws.controller_mut().lock_for_app_wide_passcode();
                ws.close_for_passcode_lock(ctx);
            } else {
                ws.controller_mut().clear_passcode_lock();
            }
        }
    }
    ctx.request_repaint();
    active_locked
}

/// Undoes an account switch (Main Menu, tray menu, cross-account notification click) or an
/// "Add Account" request made during a frame that started locked, and reports no action.
pub fn refuse_account_switch_while_locked(
    host: &mut MultiAccountWorkspaceHost,
    was_locked: bool,
    slot_before: i32,
    action: MultiAccountFrameAction,
) -> MultiAccountFrameAction {
    if !was_locked {
        return action;
    }
    if host.active_slot() != slot_before {
        host.switch_to(slot_before);
    }
    match action {
        MultiAccountFrameAction::SwitchedAccount(_) | MultiAccountFrameAction::AddAccount => {
            MultiAccountFrameAction::None
        }
        other => other,
    }
}

fn with_manager<R>(slot: &ManagerSlot, f: impl FnOnce(&mut AccountManager) -> R) -> Option<R> {
    let mut guard = slot.lock().ok()?;
    let mgr = guard.as_mut()?;
    Some(f(mgr))
}

fn take_manager(slot: &ManagerSlot) -> Option<AccountManager> {
    slot.lock().ok()?.take()
}

fn put_manager(slot: &ManagerSlot, manager: AccountManager) {
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(manager);
    }
}

fn make_passcode_verifier(manager: &ManagerSlot) -> PasscodeVerifierFn {
    let mgr = Arc::clone(manager);
    Arc::new(move |candidate: &str| {
        mgr.lock().is_ok_and(|guard| {
            guard
                .as_ref()
                .is_some_and(|m| m.check_local_passcode(candidate.as_bytes()))
        })
    })
}

#[allow(clippy::too_many_arguments)]
fn configure_account_workspace_from_tdata(
    ws: &mut LiveActiveWorkspace,
    stored_session: &StoredSessionSettings,
    core_settings: &CoreSettings,
    global_config: &GlobalConfig,
    has_passcode: bool,
    verifier: Option<PasscodeVerifierFn>,
    notify_tray: Option<&mut AppNotifyTrayHost>,
) -> u64 {
    ws.live_session_mut().apply_stored_session_settings(
        stored_session,
        (
            core_settings.autoplay.videos,
            core_settings.autoplay.gifs,
            core_settings.autoplay.round_videos,
        ),
    );
    ws.controller_mut()
        .set_external_passcode_verifier(verifier.clone());
    if !global_config.working_dir.as_os_str().is_empty() {
        let tdata_dir = global_config.working_dir.join("tdata");
        ws.live_session().attach_webview_tdata_dir(tdata_dir, None);
        ws.apply_profile_data_paths(&global_config.working_dir);
    }
    ws.live_session()
        .set_platform_translation_live(core_settings.use_platform_translation);

    let settings_backend = Box::new(ws.live_session().handle());
    {
        let main = ws.controller_mut().main_widget_mut();
        let ctrl = main.ensure_settings(settings_backend);
        ctrl.seed_from_tdata(
            core_settings.clone(),
            global_config.clone(),
            has_passcode,
            verifier,
        );
        main.apply_core_settings(core_settings);
    }

    let live_handle = ws.live_session().handle();
    if let Some(notify_tray) = notify_tray {
        live_handle.sync_notify_settings_store(
            notify_tray.settings_store_mut(),
            i64::from(now_unix_i32()),
        );
    }
    ws.live_session().notify_settings_version()
}

struct LiveIntroPhase {
    rt: tokio::runtime::Runtime,
    manager: ManagerSlot,
    active_index: i32,
    intro: IntroWidget,
    shim: IntroStatusShim,
    /// Pre-login Proxy Settings & `AddProxyBox` modal opened by `IntroAction::OpenSettings`.
    proxy_modal: crate::live::shortcuts_wiring::IntroProxyModal,
    /// When adding an additional account from an already signed-in session, keeps existing
    /// accounts' `SessionRuntime`s running in the background and allows cancelling back.
    background_host: Option<Box<MultiAccountWorkspaceHost>>,
    /// Slot index to reactivate if the user cancels "Add Account".
    return_to_slot: Option<i32>,
}

struct LiveActivePhase {
    rt: tokio::runtime::Runtime,
    manager: ManagerSlot,
    host: MultiAccountWorkspaceHost,
    auto_lock: AutoLockController,
    last_persisted_core: CoreSettings,
    last_persisted_global: GlobalConfig,
    last_persisted_sessions: BTreeMap<i32, StoredSessionSettings>,
    last_notify_sync_versions: BTreeMap<i32, u64>,
    last_window_pos: WindowPosition,
    /// App-wide passcode lock (`Core::Application::_passcodeLock`): every account is locked
    /// together and account switching is refused while it is set.
    app_locked: bool,
}

impl LiveActivePhase {
    fn from_existing_host(
        rt: tokio::runtime::Runtime,
        manager: ManagerSlot,
        mut host: MultiAccountWorkspaceHost,
    ) -> Self {
        if let Some(dir) = with_manager(&manager, |mgr| mgr.working_dir().to_path_buf())
            && let Some(ws) = host.active_workspace_mut()
        {
            ws.controller_mut().install_restore_windows(&dir);
        }
        let slots = host.slot_indices();
        let (core, global, stored_sessions) = with_manager(&manager, |mgr| {
            let core = mgr.core_settings().clone();
            let global = mgr.global_config().clone();
            let mut sessions = BTreeMap::new();
            for &slot in &slots {
                let stored = mgr.load_session_settings(slot).unwrap_or_default();
                sessions.insert(slot, stored);
            }
            (core, global, sessions)
        })
        .unwrap_or_else(|| {
            (
                CoreSettings::default(),
                GlobalConfig::default(),
                BTreeMap::new(),
            )
        });

        let mut notify_versions = BTreeMap::new();
        for &slot in &slots {
            if let Some(entry) = host.account(slot) {
                notify_versions.insert(
                    slot,
                    entry.workspace.live_session().notify_settings_version(),
                );
            }
        }
        let last_window_pos = host.settings().window_position;

        Self {
            rt,
            manager,
            host,
            auto_lock: AutoLockController::new(),
            last_persisted_core: core,
            last_persisted_global: global,
            last_persisted_sessions: stored_sessions,
            last_notify_sync_versions: notify_versions,
            last_window_pos,
            app_locked: false,
        }
    }

    /// Renders one frame; while the app is locked an account switch (Main Menu, tray menu or a
    /// notification click from another account) is undone and reported as no action.
    fn show_frame(&mut self, ctx: &egui::Context, ui: &mut Ui) -> MultiAccountFrameAction {
        let was_locked = self.app_locked;
        let slot_before = self.host.active_slot();
        let action = self.host.show(ctx, ui);
        let action =
            refuse_account_switch_while_locked(&mut self.host, was_locked, slot_before, action);
        self.tick_settings_and_passcode(ctx);
        self.app_locked = sync_app_wide_passcode_lock(&mut self.host, self.app_locked, ctx);
        action
    }

    fn tick_settings_and_passcode(&mut self, ctx: &egui::Context) {
        // 1. Drain and apply any local passcode set/change/remove command from Settings UI.
        let passcode_cmd = self
            .host
            .active_workspace_mut()
            .and_then(|ws| ws.controller_mut().main_widget_mut().settings_mut())
            .and_then(|s| s.take_passcode_command());
        if let Some(cmd) = passcode_cmd {
            self.apply_passcode_command(cmd);
        }

        // 2. Keep CoreSettings::window_position aligned with live window geometry.
        if let Some(ws) = self.host.active_workspace_mut() {
            let shell_pos = ws.controller().settings().window_position;
            if shell_pos != self.last_window_pos {
                self.last_window_pos = shell_pos;
                if let Some(settings_ctrl) = ws.controller_mut().main_widget_mut().settings_mut() {
                    let scale = settings_ctrl.global_config.config_scale;
                    settings_ctrl.core_settings.window_position =
                        shell_window_position_to_core(shell_pos, scale);
                }
            }
        }

        // 3. Persist modified CoreSettings, GlobalConfig, and per-account StoredSessionSettings.
        let active_slot = self.host.active_slot();
        let snapshot = self
            .host
            .active_workspace()
            .and_then(|ws| ws.controller().main_widget().settings())
            .map(|s| (s.core_settings.clone(), s.global_config.clone()));

        let all_slots = self.host.slot_indices();
        let mut dirty_sessions: Vec<(i32, StoredSessionSettings)> = Vec::new();
        for &slot in &all_slots {
            let Some(entry) = self.host.account(slot) else {
                continue;
            };
            if entry.workspace.live_session().take_session_settings_dirty() {
                let base = self
                    .last_persisted_sessions
                    .get(&slot)
                    .cloned()
                    .unwrap_or_default();
                let next = entry
                    .workspace
                    .live_session()
                    .collect_stored_session_settings(&base);
                if next != base {
                    dirty_sessions.push((slot, next));
                }
            }
        }

        if let Some((core, global)) = snapshot {
            let core_changed = core != self.last_persisted_core;
            let global_changed = global != self.last_persisted_global;

            if core_changed {
                if let Some(notify_tray) = self.host.notify_tray_mut() {
                    notify_tray.apply_core_settings(&core);
                }
                self.host.set_all_accounts_unread_prefs(
                    core.include_muted_counter,
                    core.count_unread_messages,
                );
                for &slot in &all_slots {
                    if let Some(entry) = self.host.account(slot) {
                        let live = entry.workspace.live_session();
                        live.set_platform_translation_live(core.use_platform_translation);
                    }
                }
            }

            if core_changed || global_changed {
                // Propagate domain-wide CoreSettings & GlobalConfig to inactive account workspaces
                // so switching accounts never reverts recent settings changes.
                for &slot in &all_slots {
                    if slot == active_slot {
                        continue;
                    }
                    if let Some(entry) = self.host.account_mut(slot) {
                        let main = entry.workspace.controller_mut().main_widget_mut();
                        if let Some(settings_ctrl) = main.settings_mut() {
                            settings_ctrl.core_settings = core.clone();
                            settings_ctrl.global_config = global.clone();
                        }
                        if core_changed {
                            main.apply_core_settings(&core);
                        }
                    }
                }
            }

            if core_changed || global_changed || !dirty_sessions.is_empty() {
                let active_session = dirty_sessions
                    .iter()
                    .find(|(slot, _)| *slot == active_slot)
                    .map(|(_, ss)| ss);
                let persist_res = with_manager(&self.manager, |mgr| -> tg_session::Result<()> {
                    persist_all_settings_to_tdata(
                        mgr,
                        active_slot,
                        &core,
                        &global,
                        active_session,
                    )?;
                    for (slot, ss) in &dirty_sessions {
                        if *slot != active_slot {
                            mgr.save_session_settings(*slot, ss)?;
                        }
                    }
                    Ok(())
                });
                match persist_res {
                    Some(Ok(())) => {
                        self.last_persisted_core = core;
                        self.last_persisted_global = global;
                        for (slot, ss) in dirty_sessions {
                            self.last_persisted_sessions.insert(slot, ss);
                        }
                    }
                    Some(Err(err)) => {
                        warn!(error = %err, "failed to persist settings to tdata");
                    }
                    None => {}
                }
            }
        } else if !dirty_sessions.is_empty() {
            let persist_res = with_manager(&self.manager, |mgr| -> tg_session::Result<()> {
                for (slot, ss) in &dirty_sessions {
                    mgr.save_session_settings(*slot, ss)?;
                }
                Ok(())
            });
            if let Some(Ok(())) = persist_res {
                for (slot, ss) in dirty_sessions {
                    self.last_persisted_sessions.insert(slot, ss);
                }
            } else if let Some(Err(err)) = persist_res {
                warn!(error = %err, "failed to persist session settings to tdata");
            }
        }

        // 4. Sync scope defaults & per-peer notification exceptions into shared NotifySettingsStore.
        let mut changed_notify_handles = Vec::new();
        for &slot in &all_slots {
            let Some(entry) = self.host.account(slot) else {
                continue;
            };
            let notify_ver = entry.workspace.live_session().notify_settings_version();
            if self.last_notify_sync_versions.get(&slot).copied() != Some(notify_ver) {
                changed_notify_handles.push((
                    slot,
                    notify_ver,
                    entry.workspace.live_session().handle(),
                ));
            }
        }
        if !changed_notify_handles.is_empty() {
            let now_unix = i64::from(now_unix_i32());
            if let Some(notify_tray) = self.host.notify_tray_mut() {
                for (_, _, live_handle) in &changed_notify_handles {
                    live_handle
                        .sync_notify_settings_store(notify_tray.settings_store_mut(), now_unix);
                }
            }
            for (slot, notify_ver, _) in changed_notify_handles {
                self.last_notify_sync_versions.insert(slot, notify_ver);
            }
        }

        // 5. Evaluate OS idle time for automatic local passcode lock (`Application::checkAutoLock`).
        let Some((has_passcode, is_locked, auto_lock_secs)) =
            self.host.active_workspace().map(|ws| {
                let has_passcode = ws.controller().has_local_passcode();
                let is_locked = ws.controller().root_screen() == RootScreen::PasscodeLock;
                let auto_lock_secs = ws
                    .controller()
                    .main_widget()
                    .settings()
                    .map(|s| s.core_settings.auto_lock)
                    .unwrap_or(self.last_persisted_core.auto_lock);
                (has_passcode, is_locked, auto_lock_secs)
            })
        else {
            return;
        };

        if self.auto_lock.check_system_idle(
            has_passcode,
            is_locked,
            auto_lock_secs,
            crate::clock::now(),
        ) {
            if let Some(ws) = self.host.active_workspace_mut() {
                ws.controller_mut().setup_passcode_lock();
            }
            ctx.request_repaint();
        } else if has_passcode && !is_locked && auto_lock_secs > 0 {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn apply_passcode_command(&mut self, cmd: LocalPasscodeCommand) {
        let Some((result, success_toast)) = with_manager(&self.manager, |mgr| match cmd {
            LocalPasscodeCommand::Set {
                current,
                new_passcode,
            } => {
                let is_change = current.is_some();
                let res = mgr.set_local_passcode(
                    current.as_deref().map(str::as_bytes),
                    new_passcode.as_bytes(),
                );
                let msg = if is_change {
                    "Local passcode changed."
                } else {
                    "Local passcode enabled."
                };
                (res, msg)
            }
            LocalPasscodeCommand::Remove { current } => (
                mgr.remove_local_passcode(current.as_bytes()),
                "Local passcode turned off.",
            ),
        }) else {
            return;
        };

        match result {
            Ok(()) => {
                let (has_passcode, updated_core) = with_manager(&self.manager, |m| {
                    (m.has_local_passcode(), m.core_settings().clone())
                })
                .unwrap_or((false, self.last_persisted_core.clone()));
                let verifier = has_passcode.then(|| make_passcode_verifier(&self.manager));
                for slot in self.host.slot_indices() {
                    if let Some(entry) = self.host.account_mut(slot) {
                        entry
                            .workspace
                            .controller_mut()
                            .set_external_passcode_verifier(verifier.clone());
                        if let Some(settings_ctrl) = entry
                            .workspace
                            .controller_mut()
                            .main_widget_mut()
                            .settings_mut()
                        {
                            settings_ctrl.core_settings.system_unlock_enabled =
                                updated_core.system_unlock_enabled;
                            settings_ctrl
                                .sync_local_passcode_from_tdata(has_passcode, verifier.clone());
                            settings_ctrl.local_passcode.clear_manage_session();
                        }
                    }
                }
                self.last_persisted_core.system_unlock_enabled = updated_core.system_unlock_enabled;
                self.auto_lock.reset();
                if let Some(ws) = self.host.active_workspace_mut() {
                    ws.controller_mut().show_toast(success_toast);
                }
            }
            Err(err) => {
                if let Some(ws) = self.host.active_workspace_mut() {
                    let still_enabled = ws.controller().has_local_passcode();
                    if let Some(settings_ctrl) =
                        ws.controller_mut().main_widget_mut().settings_mut()
                    {
                        settings_ctrl
                            .local_passcode
                            .apply_external_error(err.to_string(), still_enabled);
                    }
                }
            }
        }
    }

    fn flush_settings_to_tdata(&mut self) {
        let (mut core, global) = self
            .host
            .active_workspace()
            .and_then(|ws| ws.controller().main_widget().settings())
            .map(|s| (s.core_settings.clone(), s.global_config.clone()))
            .unwrap_or_else(|| {
                (
                    self.last_persisted_core.clone(),
                    self.last_persisted_global.clone(),
                )
            });
        let shell_pos = self.host.settings().window_position;
        core.window_position = shell_window_position_to_core(shell_pos, global.config_scale);

        let slots = self.host.slot_indices();
        let mut collected: Vec<(i32, StoredSessionSettings)> = Vec::with_capacity(slots.len());
        for slot in slots {
            if let Some(entry) = self.host.account(slot) {
                let base = self
                    .last_persisted_sessions
                    .get(&slot)
                    .cloned()
                    .unwrap_or_default();
                let ss = entry
                    .workspace
                    .live_session()
                    .collect_stored_session_settings(&base);
                collected.push((slot, ss));
            }
        }

        let active_slot = self.host.active_slot();
        let active_ss = collected
            .iter()
            .find(|(slot, _)| *slot == active_slot)
            .map(|(_, ss)| ss);
        let persist_res = with_manager(&self.manager, |mgr| -> tg_session::Result<()> {
            persist_all_settings_to_tdata(mgr, active_slot, &core, &global, active_ss)?;
            for (slot, ss) in &collected {
                if *slot != active_slot {
                    mgr.save_session_settings(*slot, ss)?;
                }
            }
            Ok(())
        });
        match persist_res {
            Some(Ok(())) => {
                self.last_persisted_core = core;
                self.last_persisted_global = global;
                for (slot, ss) in collected {
                    self.last_persisted_sessions.insert(slot, ss);
                }
            }
            Some(Err(err)) => {
                warn!(error = %err, "failed to flush settings to tdata");
            }
            None => {}
        }
    }
}

enum LivePhase {
    MissingCredentials {
        message: String,
    },
    BootstrapError {
        message: String,
    },
    PasscodeRequired {
        rt: tokio::runtime::Runtime,
        manager: Box<AccountManager>,
        passcode_input: String,
        error: Option<String>,
    },
    Intro(Box<LiveIntroPhase>),
    Active(Box<LiveActivePhase>),
}

/// Top-level live-mode coordinator owned by [`crate::app::TgforkApp`] when launched with
/// `--live` or `RUSTGRAM_LIVE=1`.
pub struct LiveAppHost {
    egui_ctx: Option<egui::Context>,
    theme_manager: ThemeManager,
    applied_theme_gen: u64,
    strings: TgLangStrings,
    settings: ShellSettings,
    settings_path: PathBuf,
    working_dir: PathBuf,
    data_name: String,
    phase: LivePhase,
    update_snapshot: UpdateSnapshot,
    widget_helper_path: Option<PathBuf>,
    window_hwnd: usize,
    native_hwnd: isize,
    /// Activations received before `tdata` was unlocked, replayed once an account is active.
    queued_activations: Vec<crate::startup::ipc::IncomingActivation>,
}

impl LiveAppHost {
    /// Bootstraps live mode reading `RUSTGRAM_API_ID` and `RUSTGRAM_API_HASH` from the process environment.
    #[must_use]
    pub fn bootstrap(
        egui_ctx: Option<egui::Context>,
        settings: ShellSettings,
        settings_path: PathBuf,
        working_dir: PathBuf,
        data_name: String,
    ) -> Self {
        let creds_res = load_api_credentials_from_env();
        Self::bootstrap_with_credentials_result(
            egui_ctx,
            settings,
            settings_path,
            working_dir,
            data_name,
            creds_res,
        )
    }

    /// Bootstraps live mode from a [`LiveBootstrapPrefetch`] started before the window was created
    /// (falls back to [`Self::bootstrap`] if the prefetch thread died).
    #[must_use]
    pub fn bootstrap_prefetched(
        egui_ctx: Option<egui::Context>,
        settings: ShellSettings,
        settings_path: PathBuf,
        working_dir: PathBuf,
        data_name: String,
        prefetch: LiveBootstrapPrefetch,
    ) -> Self {
        let Some((creds_res, prepared)) = prefetch.wait() else {
            return Self::bootstrap(egui_ctx, settings, settings_path, working_dir, data_name);
        };
        Self::bootstrap_with_parts(
            egui_ctx,
            settings,
            settings_path,
            working_dir,
            data_name,
            creds_res,
            prepared,
        )
    }

    /// Bootstraps live mode using explicit raw environment variable values (for deterministic tests).
    #[must_use]
    pub fn bootstrap_with_env_vars(
        egui_ctx: Option<egui::Context>,
        settings: ShellSettings,
        settings_path: PathBuf,
        working_dir: PathBuf,
        data_name: String,
        api_id_raw: Option<&str>,
        api_hash_raw: Option<&str>,
    ) -> Self {
        let creds_res = load_api_credentials_from_vars(api_id_raw, api_hash_raw);
        Self::bootstrap_with_credentials_result(
            egui_ctx,
            settings,
            settings_path,
            working_dir,
            data_name,
            creds_res,
        )
    }

    fn bootstrap_with_credentials_result(
        egui_ctx: Option<egui::Context>,
        settings: ShellSettings,
        settings_path: PathBuf,
        working_dir: PathBuf,
        data_name: String,
        creds_res: tg_session::Result<ApiCredentials>,
    ) -> Self {
        Self::bootstrap_with_parts(
            egui_ctx,
            settings,
            settings_path,
            working_dir,
            data_name,
            creds_res,
            None,
        )
    }

    fn bootstrap_with_parts(
        egui_ctx: Option<egui::Context>,
        settings: ShellSettings,
        settings_path: PathBuf,
        working_dir: PathBuf,
        data_name: String,
        creds_res: tg_session::Result<ApiCredentials>,
        prepared: Option<PreparedAccountManager>,
    ) -> Self {
        let embedded = settings.resolved_embedded_type();
        let accent = settings.resolved_accent_color();
        let mut theme_manager = ThemeManager::with_embedded(embedded);
        if let Some(c) = accent {
            theme_manager.set_embedded(embedded, Some(c));
        }
        if let Some(ctx) = egui_ctx.as_ref() {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            theme_manager.apply_to_egui(ctx);
        }
        let applied_theme_gen = theme_manager.generation();

        let phase = match creds_res {
            Err(err) => LivePhase::MissingCredentials {
                message: format_missing_credentials_message(&err.to_string()),
            },
            Ok(credentials) => match prepared {
                Some(prepared) => finish_account_manager_phase(
                    egui_ctx.clone(),
                    &settings,
                    settings_path.clone(),
                    prepared,
                ),
                None => init_account_manager_phase(
                    egui_ctx.clone(),
                    &settings,
                    settings_path.clone(),
                    working_dir.clone(),
                    data_name.clone(),
                    credentials,
                ),
            },
        };

        Self {
            egui_ctx,
            theme_manager,
            applied_theme_gen,
            strings: TgLangStrings::new(),
            settings,
            settings_path,
            working_dir,
            data_name,
            phase,
            update_snapshot: UpdateSnapshot::default(),
            widget_helper_path: None,
            window_hwnd: 0,
            native_hwnd: 0,
            queued_activations: Vec::new(),
        }
    }

    /// Returns the current high-level live host state.
    #[must_use]
    pub fn state(&self) -> LiveHostState {
        match self.phase {
            LivePhase::MissingCredentials { .. } => LiveHostState::MissingCredentials,
            LivePhase::BootstrapError { .. } => LiveHostState::BootstrapError,
            LivePhase::PasscodeRequired { .. } => LiveHostState::PasscodeRequired,
            LivePhase::Intro(_) => LiveHostState::Intro,
            LivePhase::Active(_) => LiveHostState::Active,
        }
    }

    /// Returns the diagnostic message when in `MissingCredentials` or `BootstrapError` state.
    #[must_use]
    pub fn status_message(&self) -> Option<&str> {
        match &self.phase {
            LivePhase::MissingCredentials { message } | LivePhase::BootstrapError { message } => {
                Some(message.as_str())
            }
            _ => None,
        }
    }

    /// Returns the active color palette derived from `ThemeManager`.
    #[must_use]
    pub fn palette(&self) -> Palette {
        if let LivePhase::Active(active) = &self.phase
            && let Some(ws) = active.host.active_workspace()
        {
            ws.controller().palette()
        } else {
            Palette::from_theme_manager(&self.theme_manager)
        }
    }

    /// Updates the native window handle (`HWND` on Windows, `0` otherwise) used for
    /// screenshot protection synchronization on the active account workspace.
    pub fn set_native_hwnd(&mut self, hwnd: isize) {
        self.native_hwnd = hwnd;
        if let LivePhase::Active(active) = &mut self.phase {
            active.host.set_native_hwnd(hwnd);
        }
    }

    /// Updates the update-banner snapshot forwarded from `StartupContext`.
    pub fn set_update_snapshot(&mut self, snapshot: UpdateSnapshot) {
        if let LivePhase::Active(active) = &mut self.phase {
            active.host.set_update_snapshot(snapshot.clone());
        }
        self.update_snapshot = snapshot;
    }

    /// Forwards the startup-resolved `rustgram-widget-helper.exe` path to the active workspaces.
    pub fn set_widget_helper_path(&mut self, path: PathBuf) {
        if let LivePhase::Active(active) = &mut self.phase {
            active.host.set_widget_helper_path(path.clone());
        }
        self.widget_helper_path = Some(path);
    }

    /// Sets the parent main-window Win32 `HWND` for native passkey dialogs.
    pub fn set_window_hwnd(&mut self, hwnd: usize) {
        if self.window_hwnd == hwnd {
            return;
        }
        self.window_hwnd = hwnd;
        self.sync_window_hwnd_to_phase();
    }

    fn sync_window_hwnd_to_phase(&mut self) {
        match &mut self.phase {
            LivePhase::Active(active) => active.host.set_window_hwnd(self.window_hwnd),
            LivePhase::Intro(intro) => intro.intro.set_passkey_window_hwnd(self.window_hwnd),
            _ => {}
        }
    }

    /// Opens Settings directly (`-tosettings` CLI flag) when in the active live workspace.
    pub fn open_settings(&mut self) {
        if let LivePhase::Active(active) = &mut self.phase
            && let Some(ws) = active.host.active_workspace_mut()
        {
            ws.controller_mut().open_settings();
        }
    }

    /// Forwards a single-instance IPC activation (`tg://` links, `-sendpath`) to the active workspace
    /// (or opens a `tg://proxy`/`tg://socks` link in the pre-login Intro proxy dialog).
    pub fn handle_incoming_activation(
        &mut self,
        activation: crate::startup::ipc::IncomingActivation,
        ctx: &egui::Context,
    ) {
        match &mut self.phase {
            LivePhase::Active(active) => {
                if let Some(ws) = active.host.active_workspace_mut() {
                    ws.controller_mut()
                        .handle_incoming_activation(activation, ctx);
                }
            }
            LivePhase::PasscodeRequired { .. } => {
                self.queued_activations.push(activation);
            }
            LivePhase::Intro(intro_phase) => {
                if let Some(url) = activation.urls.first() {
                    let current =
                        with_manager(&intro_phase.manager, |m| m.proxy_settings().clone())
                            .unwrap_or_default();
                    let _ = intro_phase.proxy_modal.open_from_url(url, current);
                    ctx.request_repaint();
                }
            }
            _ => {}
        }
    }

    /// Drains any pending UI-originated updater action from the active live workspace.
    pub fn take_update_action(&mut self) -> Option<crate::startup::updater::UpdateUiAction> {
        if let LivePhase::Active(active) = &mut self.phase
            && let Some(ws) = active.host.active_workspace_mut()
        {
            ws.controller_mut().take_update_action()
        } else {
            None
        }
    }

    /// Drains any pending `(auto_update, install_beta)` change from the active live workspace.
    pub fn take_update_prefs_change(&mut self) -> Option<(bool, bool)> {
        if let LivePhase::Active(active) = &mut self.phase
            && let Some(ws) = active.host.active_workspace_mut()
        {
            ws.controller_mut().take_update_prefs_change()
        } else {
            None
        }
    }

    /// Displays a transient toast notification in the active live workspace.
    pub fn show_toast(&mut self, message: impl Into<String>) {
        if let LivePhase::Active(active) = &mut self.phase
            && let Some(ws) = active.host.active_workspace_mut()
        {
            ws.controller_mut().show_toast(message);
        }
    }

    /// Records updated window geometry into shell settings.
    pub fn note_window_position(&mut self, pos: WindowPosition) {
        self.settings.window_position = pos.clamped();
        if let LivePhase::Active(active) = &mut self.phase {
            active.host.note_window_position(pos);
        }
    }

    /// Returns whether the OS native window frame is enabled (`CoreSettings::native_window_frame`).
    #[must_use]
    pub fn native_window_frame(&self) -> bool {
        if let LivePhase::Active(active) = &self.phase
            && let Some(ws) = active.host.active_workspace()
        {
            return ws.controller().native_window_frame();
        }
        self.settings.native_window_frame
    }

    /// Returns whether the active live workspace currently uses a dark theme.
    #[must_use]
    pub fn is_dark_theme(&self) -> bool {
        if let LivePhase::Active(active) = &self.phase
            && let Some(ws) = active.host.active_workspace()
        {
            return ws.controller().theme_mode() == crate::stand_in::theme::ThemeMode::Night;
        }
        self.settings.theme_mode == crate::stand_in::theme::ThemeMode::Night
    }

    /// Returns `true` when the live host is locked behind a local passcode screen.
    #[must_use]
    pub fn is_passcode_locked(&self) -> bool {
        match &self.phase {
            LivePhase::PasscodeRequired { .. } => true,
            LivePhase::Active(active) => active
                .host
                .active_workspace()
                .is_some_and(|ws| ws.controller().root_screen() == RootScreen::PasscodeLock),
            _ => false,
        }
    }

    /// Returns the active [`Palette`] for rendering the custom frameless title bar.
    #[must_use]
    pub fn current_palette(&self) -> Palette {
        if let LivePhase::Active(active) = &self.phase
            && let Some(ws) = active.host.active_workspace()
        {
            return ws.controller().palette();
        }
        Palette::from_theme_manager(&self.theme_manager)
    }

    /// Returns the active workspace's unified [`crate::media::player_bar::AudioPlayerBar`], if any.
    #[must_use]
    pub fn active_audio_player_bar(&self) -> Option<&crate::media::player_bar::AudioPlayerBar> {
        if let LivePhase::Active(active) = &self.phase
            && let Some(ws) = active.host.active_workspace()
        {
            return Some(ws.controller().main_widget().audio_player_bar());
        }
        None
    }

    /// Returns mutable access to the active workspace's unified
    /// [`crate::media::player_bar::AudioPlayerBar`], if any.
    pub fn active_audio_player_bar_mut(
        &mut self,
    ) -> Option<&mut crate::media::player_bar::AudioPlayerBar> {
        if let LivePhase::Active(active) = &mut self.phase
            && let Some(ws) = active.host.active_workspace_mut()
        {
            return Some(ws.controller_mut().main_widget_mut().audio_player_bar_mut());
        }
        None
    }

    /// Renders one frame of the live host inside `ui`.
    pub fn show(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        if self.egui_ctx.is_none() {
            self.egui_ctx = Some(ctx.clone());
        }

        let mut retry_bootstrap = false;
        let mut unlocked_passcode: Option<String> = None;
        let mut completed_login: Option<tg_tl::types::User> = None;
        let mut cancel_add_account_to_slot: Option<i32> = None;
        let mut active_action = MultiAccountFrameAction::None;

        if !matches!(self.phase, LivePhase::Active(_)) {
            let current_gen = self.theme_manager.generation();
            if self.applied_theme_gen != current_gen {
                self.theme_manager.apply_to_egui(ctx);
                self.applied_theme_gen = current_gen;
            }
        }

        match &mut self.phase {
            LivePhase::MissingCredentials { message } => {
                let palette = Palette::from_theme_manager(&self.theme_manager);
                render_status_screen(
                    ui,
                    &palette,
                    "Live Mode Requires Telegram API Credentials",
                    message,
                    true,
                    &mut retry_bootstrap,
                );
            }
            LivePhase::BootstrapError { message } => {
                let palette = Palette::from_theme_manager(&self.theme_manager);
                render_status_screen(
                    ui,
                    &palette,
                    "Could Not Start Live Session",
                    message,
                    true,
                    &mut retry_bootstrap,
                );
            }
            LivePhase::PasscodeRequired {
                passcode_input,
                error,
                ..
            } => {
                let palette = Palette::from_theme_manager(&self.theme_manager);
                render_passcode_unlock_screen(
                    ui,
                    &palette,
                    passcode_input,
                    error.as_deref(),
                    &mut unlocked_passcode,
                );
            }
            LivePhase::Intro(intro_phase) => {
                let palette = Palette::from_theme_manager(&self.theme_manager);
                if let Some(bg) = intro_phase.background_host.as_mut() {
                    if let Some(target_slot) = bg.poll_background_only(ctx) {
                        cancel_add_account_to_slot = Some(target_slot);
                    }
                    let back_label = format!("Back to {}", bg.active_display_name());
                    egui::Panel::top("tg_add_account_cancel_bar")
                        .frame(
                            Frame::NONE
                                .fill(palette.window_bg_over)
                                .inner_margin(egui::Margin::symmetric(12, 6)),
                        )
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if tg_app_core::ui_icons::back_button(ui, &back_label).clicked() {
                                    cancel_add_account_to_slot =
                                        intro_phase.return_to_slot.or(Some(bg.active_slot()));
                                }
                                ui.add_space(8.0);
                                ui.label(
                                    RichText::new(tg_lang::tr::lng_menu_add_account())
                                        .size(13.0)
                                        .color(palette.window_sub_text_fg),
                                );
                            });
                        });
                }
                let mut open_proxy_settings = false;
                CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
                    match intro_phase
                        .intro
                        .show(ui, &mut intro_phase.shim, &palette, &self.strings)
                    {
                        Some(IntroAction::SignedIn(_)) => {
                            if let Some(user) = intro_phase.intro.passkey_authorized_user() {
                                completed_login = Some(user.clone());
                            } else if let LoginStep::Authorized { user, .. } =
                                intro_phase.intro.login_step()
                            {
                                completed_login = Some((**user).clone());
                            }
                        }
                        Some(IntroAction::OpenSettings) => open_proxy_settings = true,
                        None => {}
                    }
                });
                let open_with = open_proxy_settings.then(|| {
                    with_manager(&intro_phase.manager, |mgr| mgr.proxy_settings().clone())
                        .unwrap_or_default()
                });
                let manager = &intro_phase.manager;
                if let Some(enabled) = crate::live::shortcuts_wiring::drive_intro_proxy_modal(
                    &mut intro_phase.proxy_modal,
                    open_with,
                    ctx,
                    &palette,
                    &self.theme_manager,
                    |next| {
                        with_manager(manager, |mgr| mgr.set_proxy_settings(next))
                            .unwrap_or(Ok(()))
                            .map_err(|err| err.to_string())
                    },
                ) {
                    intro_phase.shim.set_proxy_enabled(enabled);
                }
            }
            LivePhase::Active(active) => {
                if !self.queued_activations.is_empty()
                    && let Some(ws) = active.host.active_workspace_mut()
                {
                    for activation in std::mem::take(&mut self.queued_activations) {
                        ws.controller_mut()
                            .handle_incoming_activation(activation, ctx);
                    }
                }
                with_manager(&active.manager, |mgr| {
                    crate::live::wallet::sync_live_wallets_for_host(ctx, &active.host, mgr);
                });
                active_action = active.show_frame(ctx, ui);
            }
        }

        if retry_bootstrap {
            let creds = load_api_credentials_from_env();
            self.phase = match creds {
                Err(err) => LivePhase::MissingCredentials {
                    message: format_missing_credentials_message(&err.to_string()),
                },
                Ok(credentials) => init_account_manager_phase(
                    self.egui_ctx.clone(),
                    &self.settings,
                    self.settings_path.clone(),
                    self.working_dir.clone(),
                    self.data_name.clone(),
                    credentials,
                ),
            };
            self.sync_window_hwnd_to_phase();
            if let LivePhase::Active(active) = &mut self.phase {
                active.host.set_native_hwnd(self.native_hwnd);
            }
            ctx.request_repaint();
            return;
        }

        if let Some(passcode) = unlocked_passcode {
            self.try_unlock_passcode(&passcode);
            ctx.request_repaint();
            return;
        }

        if let Some(target_slot) = cancel_add_account_to_slot {
            self.cancel_add_account(target_slot);
            ctx.request_repaint();
            return;
        }

        if let Some(user) = completed_login {
            self.transition_intro_to_active(&user);
            ctx.request_repaint();
            return;
        }

        match active_action {
            MultiAccountFrameAction::None => {}
            MultiAccountFrameAction::SwitchedAccount(target_slot) => {
                if let LivePhase::Active(active) = &mut self.phase {
                    if let Some(Err(err)) =
                        with_manager(&active.manager, |mgr| mgr.switch_account(target_slot))
                    {
                        warn!(target_slot, error = %err, "failed to persist active account switch");
                    }
                    active.last_window_pos = active.host.settings().window_position;
                    let mode = active
                        .host
                        .active_workspace()
                        .map_or(self.settings.theme_mode, |ws| ws.controller().theme_mode());
                    let next_session = active.host.active_workspace().map(|ws| ws.live_session());
                    crate::live::export::sync_live_export_hooks(Some(ctx), mode, next_session);
                    crate::live::themes::sync_live_themes_hooks(Some(ctx), next_session);
                }
                ctx.request_repaint();
            }
            MultiAccountFrameAction::AddAccount => {
                self.begin_add_account();
                ctx.request_repaint();
            }
            MultiAccountFrameAction::LogoutAccount(slot) => {
                self.perform_logout(slot);
                ctx.request_repaint();
            }
        }
    }

    /// Flushes persisted account auth keys, shuts down the live runtimes, and saves shell settings.
    pub fn on_exit(&mut self) {
        let placeholder = LivePhase::BootstrapError {
            message: "Shutting down.".to_owned(),
        };
        let prev = std::mem::replace(&mut self.phase, placeholder);
        match prev {
            LivePhase::Active(mut active) => {
                if let Some(ctx) = self.egui_ctx.as_ref() {
                    crate::live::export::clear_live_export_hooks(ctx, self.settings.theme_mode);
                    crate::live::themes::clear_live_themes_hooks(ctx);
                }
                active.flush_settings_to_tdata();
                with_manager(&active.manager, |mgr| {
                    sync_host_premium_to_manager(&active.host, mgr);
                });
                active.host.on_exit();
                self.settings = active.host.settings().clone();
                if let Some(Err(err)) =
                    with_manager(&active.manager, AccountManager::save_all_accounts)
                {
                    warn!(error = %err, "failed to save live accounts on exit");
                }
                drop(active.host);
                with_manager(&active.manager, AccountManager::shutdown);
                active.rt.shutdown_timeout(Duration::from_secs(2));
            }
            LivePhase::Intro(mut intro_phase) => {
                if let Some(mut bg) = intro_phase.background_host.take() {
                    with_manager(&intro_phase.manager, |mgr| {
                        sync_host_premium_to_manager(&bg, mgr);
                    });
                    bg.on_exit();
                    self.settings = bg.settings().clone();
                    drop(bg);
                }
                let _ = self.settings.save_to(&self.settings_path);
                with_manager(&intro_phase.manager, |mgr| {
                    let _ = mgr.save_all_accounts();
                    mgr.shutdown();
                });
                intro_phase.rt.shutdown_timeout(Duration::from_secs(2));
            }
            LivePhase::PasscodeRequired {
                mut manager, rt, ..
            } => {
                let _ = self.settings.save_to(&self.settings_path);
                manager.shutdown();
                rt.shutdown_timeout(Duration::from_secs(2));
            }
            LivePhase::MissingCredentials { .. } | LivePhase::BootstrapError { .. } => {
                let _ = self.settings.save_to(&self.settings_path);
            }
        }
    }

    fn try_unlock_passcode(&mut self, passcode: &str) {
        let placeholder = LivePhase::BootstrapError {
            message: String::new(),
        };
        let prev = std::mem::replace(&mut self.phase, placeholder);
        let LivePhase::PasscodeRequired {
            rt, mut manager, ..
        } = prev
        else {
            self.phase = prev;
            return;
        };

        match manager.unlock(passcode.as_bytes()) {
            Ok(()) => {
                self.phase = build_phase_after_unlock(
                    self.egui_ctx.clone(),
                    &self.settings,
                    self.settings_path.clone(),
                    &self.update_snapshot,
                    rt,
                    *manager,
                );
                self.sync_window_hwnd_to_phase();
                if let LivePhase::Active(active) = &mut self.phase {
                    active.host.set_native_hwnd(self.native_hwnd);
                    if let Some(ref helper_path) = self.widget_helper_path {
                        active.host.set_widget_helper_path(helper_path.clone());
                    }
                }
            }
            Err(err) => {
                self.phase = LivePhase::PasscodeRequired {
                    rt,
                    manager,
                    passcode_input: String::new(),
                    error: Some(err.to_string()),
                };
            }
        }
    }

    fn begin_add_account(&mut self) {
        let placeholder = LivePhase::BootstrapError {
            message: String::new(),
        };
        let prev = std::mem::replace(&mut self.phase, placeholder);
        let LivePhase::Active(mut active) = prev else {
            self.phase = prev;
            return;
        };

        active.flush_settings_to_tdata();
        with_manager(&active.manager, |mgr| {
            sync_host_premium_to_manager(&active.host, mgr);
        });
        let return_slot = active.host.active_slot();
        let add_result = with_manager(&active.manager, |mgr| {
            let new_index = mgr.add_account(Environment::Production)?;
            let rpc = mgr.account(new_index).map(|a| a.rpc.clone());
            let credentials = mgr.credentials().clone();
            Ok::<_, tg_session::SessionError>((new_index, rpc, credentials))
        });

        match add_result {
            Some(Ok((new_index, Some(rpc), credentials))) => {
                let bridged_rpc = RuntimeBridgedRpc::new(rpc, active.rt.handle().clone());
                let mut intro = IntroWidget::with_rpc_client(credentials, bridged_rpc);
                intro.set_passkey_window_hwnd(self.window_hwnd);
                let mut shim = IntroStatusShim::default();
                if let Some(enabled) =
                    with_manager(&active.manager, |mgr| mgr.proxy_settings().is_enabled())
                {
                    shim.set_proxy_enabled(enabled);
                }
                self.phase = LivePhase::Intro(Box::new(LiveIntroPhase {
                    rt: active.rt,
                    manager: active.manager,
                    active_index: new_index,
                    intro,
                    shim,
                    proxy_modal: crate::live::shortcuts_wiring::IntroProxyModal::new(),
                    background_host: Some(Box::new(active.host)),
                    return_to_slot: Some(return_slot),
                }));
            }
            Some(Ok((_, None, _))) | None => {
                self.phase = LivePhase::Active(active);
            }
            Some(Err(err)) => {
                if let Some(ws) = active.host.active_workspace_mut() {
                    ws.controller_mut()
                        .show_toast(format!("Cannot add account: {err}"));
                }
                self.phase = LivePhase::Active(active);
            }
        }
    }

    fn cancel_add_account(&mut self, target_slot: i32) {
        let placeholder = LivePhase::BootstrapError {
            message: String::new(),
        };
        let prev = std::mem::replace(&mut self.phase, placeholder);
        let LivePhase::Intro(mut intro_phase) = prev else {
            self.phase = prev;
            return;
        };

        if let Some(mut host) = intro_phase.background_host.take() {
            let return_slot = if host.account(target_slot).is_some() {
                target_slot
            } else {
                intro_phase
                    .return_to_slot
                    .unwrap_or_else(|| host.active_slot())
            };
            if let Some(Err(err)) = with_manager(&intro_phase.manager, |mgr| {
                mgr.cancel_pending_account(return_slot)
            }) {
                warn!(return_slot, error = %err, "cancel_pending_account failed");
            }
            host.set_native_hwnd(self.native_hwnd);
            host.switch_to(return_slot);
            let mode = host
                .active_workspace()
                .map_or(self.settings.theme_mode, |ws| ws.controller().theme_mode());
            let next_session = host.active_workspace().map(|ws| ws.live_session());
            crate::live::export::sync_live_export_hooks(self.egui_ctx.as_ref(), mode, next_session);
            crate::live::themes::sync_live_themes_hooks(self.egui_ctx.as_ref(), next_session);
            self.phase = LivePhase::Active(Box::new(LiveActivePhase::from_existing_host(
                intro_phase.rt,
                intro_phase.manager,
                *host,
            )));
            self.sync_window_hwnd_to_phase();
        } else {
            self.phase = LivePhase::Intro(intro_phase);
        }
    }

    fn transition_intro_to_active(&mut self, user: &tg_tl::types::User) {
        let placeholder = LivePhase::BootstrapError {
            message: String::new(),
        };
        let prev = std::mem::replace(&mut self.phase, placeholder);
        let LivePhase::Intro(intro_phase) = prev else {
            self.phase = prev;
            return;
        };

        let login_info = with_manager(&intro_phase.manager, |mgr| {
            mgr.complete_login(intro_phase.active_index, user)?;
            let acc = mgr
                .account(intro_phase.active_index)
                .map(|a| (a.rpc.clone(), a.environment == Environment::Test));
            let core_settings = mgr.core_settings().clone();
            let global_config = mgr.global_config().clone();
            let has_passcode = mgr.has_local_passcode();
            let stored_session = mgr
                .load_session_settings(intro_phase.active_index)
                .unwrap_or_default();
            Ok::<_, tg_session::SessionError>((
                acc,
                core_settings,
                global_config,
                has_passcode,
                stored_session,
            ))
        });

        let (acc_info, core_settings, global_config, has_passcode, stored_session) =
            match login_info {
                Some(Ok(info)) => info,
                Some(Err(err)) => {
                    warn!(error = %err, "complete_login failed after IntroWidget authorization");
                    self.phase = LivePhase::BootstrapError {
                        message: format!("Failed to persist signed-in session to tdata: {err}"),
                    };
                    return;
                }
                None => {
                    self.phase = LivePhase::BootstrapError {
                        message: "AccountManager unavailable after login.".to_owned(),
                    };
                    return;
                }
            };

        let user_id = UserId::from_tl(user.id);
        let Some((rpc, is_test_mode)) = acc_info else {
            self.phase = LivePhase::BootstrapError {
                message: "Active account slot missing after login.".to_owned(),
            };
            return;
        };

        let runtime = match start_mtproto_erased_on(
            user_id,
            rpc,
            intro_phase.rt.handle().clone(),
            live_runtime_config(),
        ) {
            Ok(r) => Arc::new(r),
            Err(err) => {
                self.phase = LivePhase::BootstrapError {
                    message: format!("Failed to start live SessionRuntime: {err}"),
                };
                return;
            }
        };
        runtime.seed_self_user(user);

        let tdata_dir = with_manager(&intro_phase.manager, |mgr| mgr.working_dir().join("tdata"))
            .unwrap_or_else(|| self.working_dir.join("tdata"));
        let mut host = intro_phase.background_host.map(|b| *b).unwrap_or_else(|| {
            let mut effective_shell = self.settings.clone();
            if let Some(restored_pos) = core_window_position_to_shell(core_settings.window_position)
            {
                effective_shell.window_position = restored_pos;
            }
            let mut h = MultiAccountWorkspaceHost::new(effective_shell, self.settings_path.clone());
            h.set_update_snapshot(self.update_snapshot.clone());
            if let Some(ref helper_path) = self.widget_helper_path {
                h.set_widget_helper_path(helper_path.clone());
            }
            h.set_window_hwnd(self.window_hwnd);
            h
        });
        host.set_tdata_dir(tdata_dir);
        host.set_native_hwnd(self.native_hwnd);

        host.add_account(
            intro_phase.active_index,
            user_id,
            is_test_mode,
            user.premium,
            runtime,
            self.egui_ctx.as_ref(),
            true,
            true,
        );
        if let Some(ctx) = self.egui_ctx.as_ref() {
            with_manager(&intro_phase.manager, |mgr| {
                crate::live::wallet::install_live_wallet_for_account(
                    ctx,
                    intro_phase.active_index,
                    &host,
                    mgr,
                );
            });
        }

        let verifier = has_passcode.then(|| make_passcode_verifier(&intro_phase.manager));
        if let Some(entry) = host.account_mut(intro_phase.active_index) {
            configure_account_workspace_from_tdata(
                &mut entry.workspace,
                &stored_session,
                &core_settings,
                &global_config,
                has_passcode,
                verifier,
                None,
            );
        }
        let live_handle = host
            .account(intro_phase.active_index)
            .map(|e| e.workspace.live_session().handle());
        if let Some(notify_tray) = host.notify_tray_mut() {
            notify_tray.apply_core_settings(&core_settings);
            if let Some(handle) = live_handle {
                handle.sync_notify_settings_store(
                    notify_tray.settings_store_mut(),
                    i64::from(now_unix_i32()),
                );
            }
        }
        host.set_all_accounts_unread_prefs(
            core_settings.include_muted_counter,
            core_settings.count_unread_messages,
        );

        let mode = host
            .active_workspace()
            .map_or(self.settings.theme_mode, |ws| ws.controller().theme_mode());
        let next_session = host.active_workspace().map(|ws| ws.live_session());
        crate::live::export::sync_live_export_hooks(self.egui_ctx.as_ref(), mode, next_session);
        crate::live::themes::sync_live_themes_hooks(self.egui_ctx.as_ref(), next_session);

        self.phase = LivePhase::Active(Box::new(LiveActivePhase::from_existing_host(
            intro_phase.rt,
            intro_phase.manager,
            host,
        )));
    }

    fn perform_logout(&mut self, slot: i32) {
        let placeholder = LivePhase::BootstrapError {
            message: String::new(),
        };
        let prev = std::mem::replace(&mut self.phase, placeholder);
        let LivePhase::Active(mut active) = prev else {
            self.phase = prev;
            return;
        };

        active.flush_settings_to_tdata();
        with_manager(&active.manager, |mgr| {
            sync_host_premium_to_manager(&active.host, mgr);
        });
        self.settings = active.host.settings().clone();
        self.theme_manager.set_embedded(
            self.settings.resolved_embedded_type(),
            self.settings.resolved_accent_color(),
        );

        let remaining_slot = active.host.remove_account(slot);
        if let Some(ctx) = self.egui_ctx.as_ref() {
            crate::live::export::clear_live_export_hooks(ctx, self.settings.theme_mode);
            crate::live::themes::clear_live_themes_hooks(ctx);
        }
        active.last_persisted_sessions.remove(&slot);
        active.last_notify_sync_versions.remove(&slot);

        let Some(mut manager) = take_manager(&active.manager) else {
            self.phase = LivePhase::BootstrapError {
                message: "Could not reclaim AccountManager during logout.".to_owned(),
            };
            return;
        };

        let logout_res = active
            .rt
            .block_on(async { manager.logout_account(slot).await });
        if let Err(err) = logout_res {
            warn!(slot, error = %err, "logout_account returned error");
        }

        if let Some(fallback_slot) = remaining_slot {
            let target_slot = manager
                .active_index()
                .filter(|&idx| active.host.account(idx).is_some())
                .unwrap_or(fallback_slot);
            active.host.switch_to(target_slot);
            let _ = manager.switch_account(target_slot);
            put_manager(&active.manager, manager);
            active.last_window_pos = active.host.settings().window_position;
            let mode = active
                .host
                .active_workspace()
                .map_or(self.settings.theme_mode, |ws| ws.controller().theme_mode());
            let next_session = active.host.active_workspace().map(|ws| ws.live_session());
            crate::live::export::sync_live_export_hooks(self.egui_ctx.as_ref(), mode, next_session);
            crate::live::themes::sync_live_themes_hooks(self.egui_ctx.as_ref(), next_session);
            self.phase = LivePhase::Active(active);
        } else {
            drop(active.host);
            self.phase = build_phase_after_unlock(
                self.egui_ctx.clone(),
                &self.settings,
                self.settings_path.clone(),
                &self.update_snapshot,
                active.rt,
                manager,
            );
            self.sync_window_hwnd_to_phase();
        }
    }
}

fn sync_host_premium_to_manager(host: &MultiAccountWorkspaceHost, manager: &mut AccountManager) {
    for (slot, is_premium) in host.slot_premium_states() {
        if let Err(err) = manager.set_account_premium(slot, is_premium) {
            warn!(slot, is_premium, error = %err, "failed to sync account premium flag");
        }
    }
}

fn format_missing_credentials_message(reason: &str) -> String {
    format!(
        "Rustgram was started in live mode (`--live` / `RUSTGRAM_LIVE=1`), but Telegram API \
         credentials could not be loaded ({reason}).\n\n\
         Rustgram never embeds API credentials in the binary. Please obtain your own `api_id` \
         and `api_hash` from https://my.telegram.org and set both environment variables before \
         launching live mode:\n\n\
           {ENV_API_ID}=<your_numeric_api_id>\n\
           {ENV_API_HASH}=<your_api_hash>\n\n\
         Or launch without `--live` to run in offline demo mode."
    )
}

fn init_account_manager_phase(
    egui_ctx: Option<egui::Context>,
    settings: &ShellSettings,
    settings_path: PathBuf,
    working_dir: PathBuf,
    data_name: String,
    credentials: ApiCredentials,
) -> LivePhase {
    let prepared = prepare_account_manager(working_dir, data_name, credentials);
    finish_account_manager_phase(egui_ctx, settings, settings_path, prepared)
}

/// Live `SessionRuntime`s started before the window exists, keyed by account slot.
type EarlyRuntimes = BTreeMap<i32, Result<Arc<SessionRuntime<ErasedRpcClient>>, String>>;

/// The part of the live bootstrap that needs neither the window nor the `egui` context: the
/// Tokio runtime, opening and unlocking `tdata`, and starting each account's `SessionRuntime`
/// (which opens the MTProto connection).
enum PreparedAccountManager {
    Failed(String),
    PasscodeRequired {
        rt: tokio::runtime::Runtime,
        manager: AccountManager,
    },
    Unlocked {
        rt: tokio::runtime::Runtime,
        manager: AccountManager,
        early_runtimes: EarlyRuntimes,
    },
}

fn prepare_account_manager(
    working_dir: PathBuf,
    data_name: String,
    credentials: ApiCredentials,
) -> PreparedAccountManager {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            return PreparedAccountManager::Failed(format!(
                "Failed to start async Tokio runtime: {err}"
            ));
        }
    };
    crate::startup::profiler::mark("live_tokio_rt");

    let opts = WorkingDirOpenOptions::new(working_dir, credentials).with_data_name(data_name);
    let mut manager = match AccountManager::open(opts) {
        Ok(m) => m,
        Err(err) => {
            return PreparedAccountManager::Failed(format!(
                "Failed to open tdata working directory: {err}"
            ));
        }
    };
    crate::startup::profiler::mark("live_account_manager_open");

    match manager.needs_passcode() {
        Ok(true) => PreparedAccountManager::PasscodeRequired { rt, manager },
        Ok(false) => match manager.unlock(b"") {
            Ok(()) => {
                crate::startup::profiler::mark("live_account_manager_unlock");
                let early_runtimes = start_account_runtimes(&rt, &manager);
                PreparedAccountManager::Unlocked {
                    rt,
                    manager,
                    early_runtimes,
                }
            }
            Err(err) => PreparedAccountManager::Failed(format!("Failed to unlock tdata: {err}")),
        },
        Err(err) => {
            PreparedAccountManager::Failed(format!("Failed to inspect tdata passcode state: {err}"))
        }
    }
}

fn finish_account_manager_phase(
    egui_ctx: Option<egui::Context>,
    settings: &ShellSettings,
    settings_path: PathBuf,
    prepared: PreparedAccountManager,
) -> LivePhase {
    match prepared {
        PreparedAccountManager::Failed(message) => LivePhase::BootstrapError { message },
        PreparedAccountManager::PasscodeRequired { rt, manager } => LivePhase::PasscodeRequired {
            rt,
            manager: Box::new(manager),
            passcode_input: String::new(),
            error: None,
        },
        PreparedAccountManager::Unlocked {
            rt,
            manager,
            early_runtimes,
        } => build_phase_after_unlock_with(
            egui_ctx,
            settings,
            settings_path,
            &UpdateSnapshot::default(),
            rt,
            manager,
            early_runtimes,
        ),
    }
}

/// Authenticated account slots in `tdata` order.
fn authed_account_slots(manager: &AccountManager) -> Vec<i32> {
    manager
        .account_slots()
        .into_iter()
        .filter(|s| s.user_id.is_some())
        .map(|s| s.index)
        .collect()
}

/// The app's `SessionRuntime` settings. At start tdesktop sends `updates.getState` only and
/// requests the chat list from `stateDone` (`api/api_updates.cpp:258,480-489`); with a fresh state
/// a `getDifference` right after it has nothing to catch up, and the first chat-list page would
/// wait for that extra round trip.
fn live_runtime_config() -> SessionRuntimeConfig {
    SessionRuntimeConfig {
        run_get_difference_on_start: false,
        ..SessionRuntimeConfig::default()
    }
}

/// Starts a live `SessionRuntime` for every authenticated slot, so the MTProto connection, the
/// temporary auth key and its binding are under way before the window is created.
fn start_account_runtimes(rt: &tokio::runtime::Runtime, manager: &AccountManager) -> EarlyRuntimes {
    let mut runtimes = EarlyRuntimes::new();
    for slot in authed_account_slots(manager) {
        let Some(account) = manager.account(slot) else {
            continue;
        };
        let Some(user_id) = account.user_id else {
            continue;
        };
        let started = start_mtproto_erased_on(
            user_id,
            account.rpc.clone(),
            rt.handle().clone(),
            SessionRuntimeConfig {
                initial_self_user: account.self_user.clone(),
                ..live_runtime_config()
            },
        )
        .map(Arc::new)
        .map_err(|err| err.to_string());
        runtimes.insert(slot, started);
    }
    crate::startup::profiler::mark("live_start_mtproto");
    runtimes
}

/// Result of [`LiveBootstrapPrefetch`]: the API credentials from the environment and, when they
/// loaded, the prepared account manager.
type PrefetchedLive = (
    tg_session::Result<ApiCredentials>,
    Option<PreparedAccountManager>,
);

/// Live-mode startup work that runs on a background thread while `eframe` creates the window and
/// the GL context (see [`PreparedAccountManager`]).
pub struct LiveBootstrapPrefetch {
    rx: std::sync::mpsc::Receiver<PrefetchedLive>,
}

impl LiveBootstrapPrefetch {
    /// Starts the prefetch thread for `working_dir` / `data_name` with the API credentials from
    /// the environment. Returns `None` if the thread cannot be spawned.
    #[must_use]
    pub fn spawn(working_dir: PathBuf, data_name: String) -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("live-bootstrap".to_owned())
            .spawn(move || {
                let creds_res = load_api_credentials_from_env();
                let prepared = creds_res
                    .as_ref()
                    .ok()
                    .map(|c| prepare_account_manager(working_dir, data_name, c.clone()));
                let _ = tx.send((creds_res, prepared));
            })
            .ok()
            .map(|_| Self { rx })
    }

    fn wait(self) -> Option<PrefetchedLive> {
        self.rx.recv().ok()
    }
}

fn build_phase_after_unlock(
    egui_ctx: Option<egui::Context>,
    settings: &ShellSettings,
    settings_path: PathBuf,
    update_snapshot: &UpdateSnapshot,
    rt: tokio::runtime::Runtime,
    manager: AccountManager,
) -> LivePhase {
    build_phase_after_unlock_with(
        egui_ctx,
        settings,
        settings_path,
        update_snapshot,
        rt,
        manager,
        EarlyRuntimes::new(),
    )
}

fn build_phase_after_unlock_with(
    egui_ctx: Option<egui::Context>,
    settings: &ShellSettings,
    settings_path: PathBuf,
    update_snapshot: &UpdateSnapshot,
    rt: tokio::runtime::Runtime,
    manager: AccountManager,
    mut early_runtimes: EarlyRuntimes,
) -> LivePhase {
    let Some(active_index) = manager.active_index() else {
        return LivePhase::BootstrapError {
            message: "No active account slot found in tdata.".to_owned(),
        };
    };

    // Start a live SessionRuntime for EVERY authenticated account slot so inactive accounts
    // remain active in the background (unread counters + notifications), matching tdesktop.
    let authed_slots = authed_account_slots(&manager);

    if !authed_slots.is_empty() {
        let core_settings = manager.core_settings().clone();
        let global_config = manager.global_config().clone();
        let has_passcode = manager.has_local_passcode();

        let mut effective_shell = settings.clone();
        if let Some(restored_pos) = core_window_position_to_shell(core_settings.window_position) {
            effective_shell.window_position = restored_pos;
        }
        let last_window_pos = effective_shell.window_position;

        let mut host = MultiAccountWorkspaceHost::new(effective_shell, settings_path);
        host.set_tdata_dir(manager.working_dir().join("tdata"));
        host.set_update_snapshot(update_snapshot.clone());

        let target_active = if authed_slots.contains(&active_index) {
            active_index
        } else {
            authed_slots[0]
        };

        let mut stored_sessions: BTreeMap<i32, StoredSessionSettings> = BTreeMap::new();
        for &slot in &authed_slots {
            let Some(account) = manager.account(slot) else {
                continue;
            };
            let Some(user_id) = account.user_id else {
                continue;
            };
            info!(
                index = slot,
                user_id = user_id.0,
                main_dc = account.main_dc_id(),
                active = (slot == target_active),
                "resuming saved live session from tdata"
            );
            let started = early_runtimes.remove(&slot).unwrap_or_else(|| {
                start_mtproto_erased_on(
                    user_id,
                    account.rpc.clone(),
                    rt.handle().clone(),
                    live_runtime_config(),
                )
                .map(Arc::new)
                .map_err(|err| err.to_string())
            });
            let runtime = match started {
                Ok(r) => r,
                Err(err) => {
                    return LivePhase::BootstrapError {
                        message: format!(
                            "Failed to start live SessionRuntime for slot {slot}: {err}"
                        ),
                    };
                }
            };
            let is_premium = account.self_user.as_ref().is_some_and(|u| u.premium);
            if let Some(u) = account.self_user.as_ref() {
                runtime.seed_self_user(u);
            }
            stored_sessions.insert(
                slot,
                manager.load_session_settings(slot).unwrap_or_default(),
            );
            host.add_account(
                slot,
                user_id,
                account.environment == Environment::Test,
                is_premium,
                runtime,
                egui_ctx.as_ref(),
                slot == target_active,
                true,
            );
            if let Some(ctx) = egui_ctx.as_ref() {
                crate::live::wallet::install_live_wallet_for_account(ctx, slot, &host, &manager);
            }
        }

        let manager_slot: ManagerSlot = Arc::new(Mutex::new(Some(manager)));
        let verifier = has_passcode.then(|| make_passcode_verifier(&manager_slot));
        let mut last_notify_sync_versions: BTreeMap<i32, u64> = BTreeMap::new();
        let mut live_handles = Vec::new();
        for &slot in &authed_slots {
            let stored = stored_sessions.get(&slot).cloned().unwrap_or_default();
            if let Some(entry) = host.account_mut(slot) {
                let notify_ver = configure_account_workspace_from_tdata(
                    &mut entry.workspace,
                    &stored,
                    &core_settings,
                    &global_config,
                    has_passcode,
                    verifier.clone(),
                    None,
                );
                last_notify_sync_versions.insert(slot, notify_ver);
                live_handles.push(entry.workspace.live_session().handle());
            }
        }
        if let Some(notify_tray) = host.notify_tray_mut() {
            notify_tray.apply_core_settings(&core_settings);
            let now_unix = i64::from(now_unix_i32());
            for handle in live_handles {
                handle.sync_notify_settings_store(notify_tray.settings_store_mut(), now_unix);
            }
        }
        host.set_all_accounts_unread_prefs(
            core_settings.include_muted_counter,
            core_settings.count_unread_messages,
        );

        let mode = host
            .active_workspace()
            .map_or(settings.theme_mode, |ws| ws.controller().theme_mode());
        let active_session = host.active_workspace().map(|ws| ws.live_session());
        crate::live::export::sync_live_export_hooks(egui_ctx.as_ref(), mode, active_session);
        crate::live::themes::sync_live_themes_hooks(egui_ctx.as_ref(), active_session);

        return LivePhase::Active(Box::new(LiveActivePhase {
            rt,
            manager: manager_slot,
            host,
            auto_lock: AutoLockController::new(),
            last_persisted_core: core_settings,
            last_persisted_global: global_config,
            last_persisted_sessions: stored_sessions,
            last_notify_sync_versions,
            last_window_pos,
            app_locked: false,
        }));
    }

    let Some(account) = manager.account(active_index) else {
        return LivePhase::BootstrapError {
            message: format!("Account slot {active_index} is missing."),
        };
    };

    let credentials = manager.credentials().clone();
    let bridged_rpc = RuntimeBridgedRpc::new(account.rpc.clone(), rt.handle().clone());
    let intro = IntroWidget::with_rpc_client(credentials, bridged_rpc);
    let mut shim = IntroStatusShim::default();
    let initial_proxy_enabled = manager.proxy_settings().is_enabled();
    shim.set_proxy_enabled(initial_proxy_enabled);
    LivePhase::Intro(Box::new(LiveIntroPhase {
        rt,
        manager: Arc::new(Mutex::new(Some(manager))),
        active_index,
        intro,
        shim,
        proxy_modal: crate::live::shortcuts_wiring::IntroProxyModal::new(),
        background_host: None,
        return_to_slot: None,
    }))
}

fn render_status_screen(
    ui: &mut Ui,
    palette: &Palette,
    heading: &str,
    body: &str,
    show_retry: bool,
    retry_clicked: &mut bool,
) {
    CentralPanel::default()
        .frame(Frame::NONE.fill(palette.window_bg))
        .show(ui, |ui| {
            ui.with_layout(
                Layout::centered_and_justified(egui::Direction::TopDown),
                |ui| {
                    Frame::NONE
                        .fill(palette.window_bg_over)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(10.0)
                        .inner_margin(24.0)
                        .show(ui, |ui| {
                            ui.set_max_width(540.0);
                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new(heading)
                                        .size(20.0)
                                        .strong()
                                        .color(palette.window_fg),
                                );
                                ui.add_space(12.0);
                                ui.label(
                                    RichText::new(body)
                                        .size(13.5)
                                        .color(palette.window_sub_text_fg),
                                );
                                ui.add_space(18.0);
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.button("Quit").clicked() {
                                        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                                    }
                                    if show_retry && ui.button("Retry").clicked() {
                                        *retry_clicked = true;
                                    }
                                });
                            });
                        });
                },
            );
        });
}

fn render_passcode_unlock_screen(
    ui: &mut Ui,
    palette: &Palette,
    passcode_input: &mut String,
    error: Option<&str>,
    submitted: &mut Option<String>,
) {
    CentralPanel::default()
        .frame(Frame::NONE.fill(palette.window_bg))
        .show(ui, |ui| {
            ui.with_layout(Layout::centered_and_justified(egui::Direction::TopDown), |ui| {
                Frame::NONE
                    .fill(palette.window_bg_over)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(10.0)
                    .inner_margin(24.0)
                    .show(ui, |ui| {
                        ui.set_max_width(380.0);
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new("Enter Local Passcode")
                                    .size(19.0)
                                    .strong()
                                    .color(palette.window_fg),
                            );
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(
                                    "Your saved live session in tdata is protected by a local passcode.",
                                )
                                .size(13.0)
                                .color(palette.window_sub_text_fg),
                            );
                            ui.add_space(12.0);
                            let resp = ui.add(
                                egui::TextEdit::singleline(passcode_input)
                                    .password(true)
                                    .hint_text("Passcode")
                                    .desired_width(f32::INFINITY),
                            );
                            let enter =
                                resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if let Some(err) = error {
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(err)
                                        .size(12.5)
                                        .color(Color32::from_rgb(0xE0, 0x52, 0x52)),
                                );
                            }
                            ui.add_space(14.0);
                            if ui.button("Unlock").clicked() || enter {
                                *submitted = Some(passcode_input.clone());
                            }
                        });
                    });
            });
        });
}
