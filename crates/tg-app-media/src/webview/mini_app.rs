//! Bot Mini App panel window hosting `tg_webview::WebView` and `tg_webview::MiniAppController`.
//!
//! Port origin: `Telegram/SourceFiles/ui/chat/attach/attach_bot_webview.cpp` and
//! `Telegram/SourceFiles/inline_bots/bot_attach_web_view.cpp`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use egui::{
    Align, Align2, CornerRadius, CursorIcon, FontId, Id, Key, Layout, Modifiers, Panel, Pos2, Rect,
    RichText, Sense, Stroke, Ui, Vec2, ViewportBuilder, ViewportCommand, ViewportId,
};
use tg_lang::tr;
use tg_theme::Palette as TgPalette;
use tg_ui::effects::InfiniteRadialAnimation;
use tg_ui::fonts::{ensure_tdesktop_fonts, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::layers::{BoxButton, BoxButtonKind, BoxModal, LayerStack};
use tg_ui::stand_in::{
    BoxStyle, Colors, InfiniteRadialAnimationStyle, PopupMenuStyle, ToggleStyle,
};
use tg_ui::tg_style::{Icon as StyleIcon, st};
use tg_ui::widgets::buttons::IconKind;
use tg_ui::widgets::menu::{MenuItem, PopupMenuState};
use tg_webview::{
    AccessRequestStatus, DEFAULT_CUSTOM_SCHEME, FileDownloadStatus, HostEvent, MiniAppAction,
    MiniAppButtonState, MiniAppController, MiniAppEvent, MiniAppPanelConfig, OpenLinkParams,
    PhoneRequestStatus, PopupArgs, PopupButton, PopupButtonType, PopupFloodGuard, SafeAreaInsets,
    SecondaryButtonPosition, ThemeChangedPayload, ViewportParams, WebMessage, WebView,
    WebViewBounds, WebViewCallbacks, WebViewConfig, availability, format_raw_post_event_script,
    is_allowed_navigation_scheme, parse_mini_app_event, resolve_ripple_color,
    resolve_user_data_dir,
};

use crate::webview::backend::{
    AppWebViewEvent, BotInfo, ClipboardAccess, ExternalOpener, WebViewBackend, WebViewLaunchInfo,
};
use crate::webview::bot_dialogs::{
    BotDownloadsManager, BotLocalStorage, DownloadFilePrompt, PreparedMessagePrompt,
    render_download_file_confirm_body, render_prepared_message_confirm_body,
};
use crate::webview::platform_win::{
    client_size_for_hwnd, find_thread_window_by_title, win32_raw_handle,
};
use crate::webview::runtime_error::{
    ACTION_CLOSE_ERROR, ACTION_DOWNLOAD_WEBVIEW2, RuntimeErrorKind, push_runtime_error_box,
    render_runtime_error_body, render_unavailable_backdrop,
};
use crate::webview::strings;
use crate::webview::theme::{
    TgThemeColors, build_webview_theme_params, color_to_egui, compute_contrast_colors,
    egui_visuals_from_tg_palette,
};
use tg_app_core::core_types::platform_call::{
    PLATFORM_CALL_REPAINT_INTERVAL, PlatformCall, PlatformCallState,
};

/// Official Telegram Mini Apps Terms of Service URL.
pub const MINI_APPS_TERMS_URL: &str = "https://telegram.org/tos/mini-apps";

/// Height of the Bot Mini App panel header bar in logical pixels (`st::separatePanelTitleHeight`).
pub const MINI_APP_HEADER_HEIGHT: f32 = st::separate_panel_title_height.0 as f32;

/// Compact header height when in fullscreen mode.
pub const MINI_APP_FULLSCREEN_HEADER_HEIGHT: f32 = 40.0;

/// Default Bot Mini App panel width (`st::botWebViewPanelSize`).
pub const MINI_APP_DEFAULT_WIDTH: f32 = st::bot_web_view_panel_size.width.0 as f32;

/// Default Bot Mini App panel height (`st::botWebViewPanelSize`).
pub const MINI_APP_DEFAULT_HEIGHT: f32 = st::bot_web_view_panel_size.height.0 as f32;

/// Minimum panel width.
pub const MINI_APP_MIN_WIDTH: f32 = 340.0;

/// Minimum panel height.
pub const MINI_APP_MIN_HEIGHT: f32 = 420.0;

const MINI_APP_VIEWPORT_ID_STR: &str = "rustgram_bot_mini_app_panel";
/// Host events kept for [`MiniAppWindow::drain_emitted_host_events`] when nobody drains them.
const MAX_RECORDED_HOST_EVENTS: usize = 64;
const BUTTON_ROW_HEIGHT: f32 = 44.0;
const BUTTON_BAR_PAD_V: f32 = 10.0;
const BUTTON_BAR_PAD_H: f32 = 12.0;
const BUTTON_GAP: f32 = 8.0;
const TOAST_DURATION: Duration = Duration::from_secs(3);

const BOX_MINI_APP_POPUP: &str = "mini_app_popup";
const BOX_CONFIRM_CLOSE: &str = "mini_app_confirm_close";
const BOX_CONFIRM_EXT_LINK: &str = "mini_app_confirm_external_link";
const BOX_CONFIRM_REMOVE: &str = "mini_app_confirm_remove";
const BOX_CONFIRM_WRITE_ACCESS: &str = "mini_app_confirm_write_access";
const BOX_CONFIRM_PHONE: &str = "mini_app_confirm_phone";
const BOX_CONFIRM_LOCATION: &str = "mini_app_confirm_location";
const BOX_SCAN_QR_UNSUPPORTED: &str = "mini_app_scan_qr_unsupported";
const BOX_CONFIRM_DOWNLOAD_FILE: &str = "mini_app_confirm_download_file";
const BOX_CONFIRM_PREPARED_MESSAGE: &str = "mini_app_confirm_prepared_message";

/// Events delivered asynchronously from WebView2 COM callbacks to [`MiniAppWindow`].
#[derive(Debug, Clone)]
pub enum MiniAppBridgeEvent {
    /// Incoming Mini App JSON message (`window.Telegram.WebView.postEvent`).
    Message(WebMessage),
    /// User mouse or keyboard interaction inside the WebView2 surface.
    UserInteraction,
    /// Navigation started.
    NavigationStart,
    /// Navigation completed (`success`).
    NavigationDone(bool),
    /// DOM fullscreen element toggled.
    DomFullscreenChanged(bool),
    /// Local Telegram link intercepted during top-level navigation.
    LocalUri(String),
    /// WebView2 renderer process crashed.
    ProcessFailed,
}

/// Active modal dialog displayed inside [`MiniAppWindow`]'s `tg-ui` [`LayerStack`].
#[derive(Debug, Clone)]
pub enum ActiveMiniAppModal {
    /// `web_app_open_popup` custom alert/confirm dialog.
    WebAppPopup(PopupArgs),
    /// Closing confirmation when `close_needs_confirmation` is enabled.
    ConfirmClose,
    /// External link confirmation when outside the 3-second user interaction window.
    ConfirmExternalLink(OpenLinkParams),
    /// Confirmation before removing the bot from the attachment/side menu.
    ConfirmRemoveFromMenu,
    /// Confirmation for `web_app_request_write_access`.
    ConfirmWriteAccess,
    /// Confirmation for `web_app_request_phone`.
    ConfirmPhone,
    /// Per-bot consent before `web_app_request_location` reads the device location
    /// (`Panel::requestPermission(Geolocation)`, `lng_bot_allow_location`).
    ConfirmLocation,
    /// Confirmation for `web_app_request_file_download` (`inline_bot_downloads.cpp`).
    ConfirmDownloadFile(DownloadFilePrompt),
    /// Confirmation for `web_app_send_prepared_message` (`inline_bot_confirm_prepared.cpp`).
    ConfirmPreparedMessage(PreparedMessagePrompt),
    /// Informational notice for `web_app_open_scan_qr_popup`.
    ScanQrUnsupported(PopupArgs),
    /// Missing WebView2 runtime or crash error box.
    RuntimeError(RuntimeErrorKind),
}

/// Top-level Bot Mini App panel window controller and renderer.
pub struct MiniAppWindow {
    bot: BotInfo,
    launch: WebViewLaunchInfo,
    controller: MiniAppController,
    flood_guard: PopupFloodGuard,
    device_storage: HashMap<String, String>,
    bot_storage: BotLocalStorage,
    bot_storage_dir: Option<PathBuf>,
    bot_downloads: BotDownloadsManager,
    emitted_host_events: Vec<HostEvent>,
    webview: Option<WebView>,
    attached_hwnd: Option<isize>,
    last_bounds: Option<WebViewBounds>,
    last_viewport_height: Option<i32>,
    webview_hidden_for_overlay: bool,
    webview_init_attempted: bool,
    tx_events: mpsc::Sender<MiniAppBridgeEvent>,
    rx_events: mpsc::Receiver<MiniAppBridgeEvent>,
    popup_menu: PopupMenuState,
    layer_stack: LayerStack,
    active_modal: Option<ActiveMiniAppModal>,
    pending_modals: Vec<ActiveMiniAppModal>,
    runtime_error: Option<RuntimeErrorKind>,
    loading: bool,
    fullscreen_synced: bool,
    toast: Option<(String, Instant)>,
    user_data_root: PathBuf,
    open: bool,
    viewport_frames: u32,
    geo_provider: Arc<dyn tg_platform::geolocation::GeoLocationProvider>,
    /// Per-bot location consent for this panel: `None` = not asked yet.
    location_access: Option<bool>,
    /// Device location fix for `web_app_request_location`, in flight on a worker thread.
    location_pending: Option<PlatformCall<Option<tg_platform::geolocation::GeoLocation>>>,
    posted_raw_events: Vec<(String, serde_json::Value)>,
}

impl MiniAppWindow {
    /// Creates a new Bot Mini App panel window from a [`WebViewLaunchInfo`].
    #[must_use]
    pub fn new(launch: WebViewLaunchInfo) -> Self {
        let controller = MiniAppController::new(MiniAppPanelConfig {
            initial_url: launch.url.clone(),
            same_origin: launch.same_origin,
            allow_clipboard_read: launch.bot.allow_clipboard_read,
            fullscreen: launch.fullscreen,
        });
        let (tx_events, rx_events) = mpsc::channel();
        let fullscreen_synced = launch.fullscreen;
        let bot = launch.bot.clone();
        let user_data_root = std::env::temp_dir().join("rustgram-webview");

        Self {
            bot,
            launch,
            controller,
            flood_guard: PopupFloodGuard::new(),
            device_storage: HashMap::new(),
            bot_storage: BotLocalStorage::new(),
            bot_storage_dir: None,
            bot_downloads: BotDownloadsManager::new(),
            emitted_host_events: Vec::new(),
            webview: None,
            attached_hwnd: None,
            last_bounds: None,
            last_viewport_height: None,
            webview_hidden_for_overlay: false,
            webview_init_attempted: false,
            tx_events,
            rx_events,
            popup_menu: PopupMenuState::new(),
            layer_stack: LayerStack::new(),
            active_modal: None,
            pending_modals: Vec::new(),
            runtime_error: None,
            loading: true,
            fullscreen_synced,
            toast: None,
            user_data_root,
            open: true,
            viewport_frames: 0,
            geo_provider: tg_app_core::core_types::geo::default_geolocation_provider(),
            location_access: None,
            location_pending: None,
            posted_raw_events: Vec::new(),
        }
    }

    /// Overrides the OS geolocation provider used for `web_app_check_location` /
    /// `web_app_request_location` (inject `FakeGeoLocationProvider` in tests).
    pub fn set_geo_provider(
        &mut self,
        provider: Arc<dyn tg_platform::geolocation::GeoLocationProvider>,
    ) {
        self.geo_provider = provider;
    }

    /// Returns raw host-to-page JSON events emitted directly by this panel (observable in tests).
    #[must_use]
    pub fn posted_raw_events(&self) -> &[(String, serde_json::Value)] {
        &self.posted_raw_events
    }

    /// Creates a Mini App panel displaying an explicit runtime error (e.g. missing WebView2).
    #[must_use]
    pub fn with_runtime_error(launch: WebViewLaunchInfo, error: RuntimeErrorKind) -> Self {
        let mut win = Self::new(launch);
        win.runtime_error = Some(error.clone());
        win.pending_modals
            .push(ActiveMiniAppModal::RuntimeError(error));
        win.webview_init_attempted = true;
        win.loading = false;
        win
    }

    /// Attaches a per-account `tdata` storage directory for persisting [`BotLocalStorage`]
    /// and immediately loads any saved keys for this bot.
    #[must_use]
    pub fn with_bot_storage_dir(mut self, dir: PathBuf) -> Self {
        self.set_bot_storage_dir(Some(dir));
        self
    }

    /// Updates the per-account `tdata` storage directory and reloads this bot's saved keys.
    pub fn set_bot_storage_dir(&mut self, dir: Option<PathBuf>) {
        self.bot_storage_dir = dir;
        if let Some(ref storage_dir) = self.bot_storage_dir {
            if let Err(err) = self.bot_storage.load_bot_from_dir(self.bot.id, storage_dir) {
                tracing::warn!(bot_id = self.bot.id, %err, "Failed to load persisted BotLocalStorage");
            } else {
                self.device_storage.clear();
                for key in self.bot_storage.keys(self.bot.id) {
                    if let Some(val) = self.bot_storage.read(self.bot.id, &key) {
                        self.device_storage.insert(key, val.to_owned());
                    }
                }
            }
        }
    }

    /// Returns the configured per-account `tdata` storage directory for [`BotLocalStorage`], if any.
    #[must_use]
    pub fn bot_storage_dir(&self) -> Option<&std::path::Path> {
        self.bot_storage_dir.as_deref()
    }

    /// Returns the current in-memory [`BotLocalStorage`] cache for this Mini App panel.
    #[must_use]
    pub const fn bot_storage(&self) -> &BotLocalStorage {
        &self.bot_storage
    }

    /// Drains host events dispatched to the Mini App page (also recorded when headless/no WebView2).
    pub fn drain_emitted_host_events(&mut self) -> Vec<HostEvent> {
        std::mem::take(&mut self.emitted_host_events)
    }

    /// Promotes the next queued modal into `active_modal` without requiring a rendered egui frame
    /// (useful in headless / unit tests before calling [`Self::resolve_active_modal`]).
    pub fn promote_pending_modal_headless(&mut self) -> Option<&ActiveMiniAppModal> {
        if self.active_modal.is_none() && !self.pending_modals.is_empty() {
            let next = self.pending_modals.remove(0);
            self.active_modal = Some(next);
        }
        self.active_modal.as_ref()
    }

    /// Selects a target recipient chat for an active or queued `ConfirmPreparedMessage` modal.
    pub fn set_prepared_message_recipient(
        &mut self,
        peer_id: tg_app_core::stand_in::types::PeerId,
        display_name: impl Into<String>,
    ) -> bool {
        let name = display_name.into();
        if let Some(ActiveMiniAppModal::ConfirmPreparedMessage(prompt)) = self.active_modal.as_mut()
        {
            prompt.recipient = Some((peer_id, name));
            return true;
        }
        for modal in &mut self.pending_modals {
            if let ActiveMiniAppModal::ConfirmPreparedMessage(prompt) = modal {
                prompt.recipient = Some((peer_id, name));
                return true;
            }
        }
        false
    }

    /// Returns whether this Mini App panel is still open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Returns metadata for the bot hosting this Mini App.
    #[must_use]
    pub const fn bot(&self) -> &BotInfo {
        &self.bot
    }

    /// Returns the underlying [`MiniAppController`].
    #[must_use]
    pub const fn controller(&self) -> &MiniAppController {
        &self.controller
    }

    /// Mutable reference to the underlying [`MiniAppController`] (for tests & direct dispatch).
    pub fn controller_mut(&mut self) -> &mut MiniAppController {
        &mut self.controller
    }

    /// Returns the currently active modal, if any.
    #[must_use]
    pub const fn active_modal(&self) -> Option<&ActiveMiniAppModal> {
        self.active_modal.as_ref()
    }

    /// Returns the modals queued behind the active one.
    #[must_use]
    pub fn pending_modals(&self) -> &[ActiveMiniAppModal] {
        &self.pending_modals
    }

    /// Promotes the next queued modal to active without rendering (headless tests).
    #[doc(hidden)]
    pub fn promote_next_modal_headless(&mut self) -> bool {
        if self.active_modal.is_none() && !self.pending_modals.is_empty() {
            self.active_modal = Some(self.pending_modals.remove(0));
            return true;
        }
        false
    }

    /// Returns the per-bot location consent given in this panel (`None` = not asked yet).
    #[must_use]
    pub const fn location_access(&self) -> Option<bool> {
        self.location_access
    }

    /// Returns the attached Win32 `HWND`, if any.
    #[must_use]
    pub const fn attached_hwnd(&self) -> Option<isize> {
        self.attached_hwnd
    }

    /// Returns the OS window title for this Mini App panel.
    #[must_use]
    pub fn window_title(&self) -> String {
        format!("{} — {}", self.bot.title, strings::MINI_APP_TITLE_SUFFIX)
    }

    /// Closes the panel immediately and releases the WebView2 controller.
    pub fn close_immediately(&mut self) {
        self.open = false;
        if let Some(wv) = self.webview.take() {
            wv.close();
        }
    }

    /// Requests closing the panel, honoring `close_needs_confirmation` (`web_app_setup_closing_behavior`).
    pub fn request_close(&mut self) -> bool {
        match self.controller.on_window_close_requested() {
            MiniAppAction::ClosePanel => {
                self.close_immediately();
                true
            }
            MiniAppAction::ConfirmClosePanel => {
                self.enqueue_modal(ActiveMiniAppModal::ConfirmClose);
                false
            }
            _ => false,
        }
    }

    /// Pushes updated `tg-theme` palette parameters to the WebView2 host and the Mini App JS bridge.
    pub fn apply_theme(&mut self, palette: &TgPalette) {
        let action = self.controller.on_palette_changed(palette);
        if let Some(wv) = &self.webview {
            let params = build_webview_theme_params(palette);
            let _ = wv.update_theme(&params);
        }
        if let MiniAppAction::PostEvent(ev) = action {
            self.post_host_event(&ev);
        }
    }

    /// Computes content safe-area insets matching `Panel::sendContentSafeArea` in `attach_bot_webview.cpp`.
    #[must_use]
    pub fn compute_content_safe_area(&self) -> SafeAreaInsets {
        SafeAreaInsets {
            top: if self.controller.state().fullscreen {
                MINI_APP_FULLSCREEN_HEADER_HEIGHT as i32
            } else {
                0
            },
            right: 0,
            bottom: 0,
            left: 0,
        }
    }

    /// Computes the logical pixel height needed for the bottom action buttons bar.
    #[must_use]
    pub fn bottom_bar_height(&self) -> f32 {
        let st = self.controller.state();
        let has_main = st.main_button.is_visible;
        let has_sec = st.secondary_button.is_visible;
        match (has_main, has_sec) {
            (false, false) => 0.0,
            (true, false) | (false, true) => BUTTON_ROW_HEIGHT + BUTTON_BAR_PAD_V * 2.0,
            (true, true) => match st.secondary_position {
                SecondaryButtonPosition::Left | SecondaryButtonPosition::Right => {
                    BUTTON_ROW_HEIGHT + BUTTON_BAR_PAD_V * 2.0
                }
                SecondaryButtonPosition::Top | SecondaryButtonPosition::Bottom => {
                    BUTTON_ROW_HEIGHT * 2.0 + BUTTON_GAP + BUTTON_BAR_PAD_V * 2.0
                }
            },
        }
    }

    /// Processes a raw incoming [`WebMessage`] through [`MiniAppController`] and executes resulting actions.
    pub fn handle_web_message(
        &mut self,
        msg: &WebMessage,
        palette: &TgPalette,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        clipboard: &dyn ClipboardAccess,
        now: Instant,
    ) -> Vec<AppWebViewEvent> {
        if let Ok((_, event)) = parse_mini_app_event(&msg.text, &msg.source_url) {
            match event {
                MiniAppEvent::WebAppCheckLocation => {
                    let status = self.geo_provider.status();
                    let payload = tg_app_core::core_types::geo::mini_app_location_checked_json(
                        status,
                        self.location_access,
                    );
                    self.post_raw_json_event("location_checked", payload);
                    return Vec::new();
                }
                MiniAppEvent::WebAppRequestLocation => {
                    self.request_location_for_page();
                    return Vec::new();
                }
                MiniAppEvent::WebAppOpenLocationSettings => {
                    let _ = external.open_url("ms-settings:privacy-location");
                    return vec![AppWebViewEvent::OpenExternalUrl {
                        url: "ms-settings:privacy-location".to_owned(),
                    }];
                }
                _ => {}
            }
        }
        match self.controller.handle_web_message(msg, palette, now) {
            Ok(actions) => self.handle_actions(actions, backend, external, clipboard, now),
            Err(err) => {
                tracing::debug!(%err, "Ignored invalid Mini App bridge message");
                Vec::new()
            }
        }
    }

    /// Executes a batch of [`MiniAppAction`]s produced by [`MiniAppController`].
    pub fn handle_actions(
        &mut self,
        actions: Vec<MiniAppAction>,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        clipboard: &dyn ClipboardAccess,
        now: Instant,
    ) -> Vec<AppWebViewEvent> {
        let mut events = Vec::new();
        for action in actions {
            match action {
                MiniAppAction::PostEvent(ev) => {
                    // Replace default zero content safe area with our fullscreen-aware inset.
                    let adjusted = match ev {
                        HostEvent::ContentSafeAreaChanged(_) => {
                            HostEvent::ContentSafeAreaChanged(self.compute_content_safe_area())
                        }
                        other => other,
                    };
                    self.post_host_event(&adjusted);
                }
                MiniAppAction::SendDynamicViewport => {
                    self.send_viewport();
                }
                MiniAppAction::ClosePanel => {
                    self.close_immediately();
                }
                MiniAppAction::ConfirmClosePanel => {
                    self.enqueue_modal(ActiveMiniAppModal::ConfirmClose);
                }
                MiniAppAction::SendData(data) => {
                    let button_text = self.controller.state().main_button.text.clone();
                    let _ = backend.send_web_app_data(self.bot.id, &button_text, &data);
                    self.show_toast(format!(
                        "{}{}{}{}",
                        strings::SENT_BYTES_PREFIX,
                        data.len(),
                        strings::SENT_BYTES_TO,
                        self.bot.username
                    ));
                    events.push(AppWebViewEvent::BotDataSent {
                        bot_id: self.bot.id,
                        data,
                    });
                }
                MiniAppAction::SwitchInlineQuery { chat_types, query } => {
                    events.push(AppWebViewEvent::SwitchInlineQuery {
                        bot_id: self.bot.id,
                        chat_types,
                        query,
                    });
                }
                MiniAppAction::OpenTgLink { url, .. } => {
                    events.push(AppWebViewEvent::OpenTelegramLink { uri: url });
                }
                MiniAppAction::OpenExternalLink {
                    params,
                    needs_confirmation,
                } => {
                    if !is_allowed_navigation_scheme(&params.url, None) {
                        continue;
                    }
                    if needs_confirmation {
                        self.enqueue_modal(ActiveMiniAppModal::ConfirmExternalLink(params));
                    } else {
                        events.extend(self.execute_external_link(&params, external));
                    }
                }
                MiniAppAction::OpenInvoice { slug } => {
                    events.push(AppWebViewEvent::OpenInvoice { slug });
                }
                MiniAppAction::ShowPopup(args) => {
                    if self.flood_guard.try_begin(now, args.ignore_flood_check) {
                        self.enqueue_modal(ActiveMiniAppModal::WebAppPopup(args));
                    } else {
                        // Throttled by flood guard -> immediately inform page popup closed.
                        self.post_host_event(&HostEvent::PopupClosed { button_id: None });
                    }
                }
                MiniAppAction::ShowScanQrUnsupported => {
                    self.enqueue_modal(ActiveMiniAppModal::ScanQrUnsupported(
                        MiniAppController::scan_qr_unsupported_popup(),
                    ));
                }
                MiniAppAction::ReadClipboard { req_id, allowed } => {
                    let data = if allowed { clipboard.read_text() } else { None };
                    self.post_host_event(&HostEvent::ClipboardTextReceived { req_id, data });
                }
                MiniAppAction::RequestWriteAccess => {
                    self.enqueue_modal(ActiveMiniAppModal::ConfirmWriteAccess);
                }
                MiniAppAction::RequestPhone => {
                    self.enqueue_modal(ActiveMiniAppModal::ConfirmPhone);
                }
                MiniAppAction::InvokeCustomMethod(req) => {
                    match backend.invoke_custom_method(self.bot.id, &req.method, req.params) {
                        Ok(result) => {
                            self.post_host_event(&HostEvent::CustomMethodInvoked {
                                req_id: req.req_id,
                                result: Some(result),
                                error: None,
                            });
                        }
                        Err(err) => {
                            self.post_host_event(&HostEvent::CustomMethodInvoked {
                                req_id: req.req_id,
                                result: None,
                                error: Some(err),
                            });
                        }
                    }
                }
                MiniAppAction::DeviceStorageSave { req_id, key, value } => {
                    if self.bot_storage.write(self.bot.id, &key, value.clone()) {
                        match value {
                            Some(v) => {
                                self.device_storage.insert(key, v);
                            }
                            None => {
                                self.device_storage.remove(&key);
                            }
                        }
                        if let Some(ref dir) = self.bot_storage_dir
                            && let Err(err) = self.bot_storage.save_bot_to_dir(self.bot.id, dir)
                        {
                            tracing::warn!(bot_id = self.bot.id, %err, "Failed to persist BotLocalStorage");
                        }
                        self.post_host_event(&HostEvent::DeviceStorageKeySaved { req_id });
                    } else {
                        self.post_host_event(&HostEvent::DeviceStorageFailed {
                            req_id,
                            error: "QUOTA_EXCEEDED".to_owned(),
                        });
                    }
                }
                MiniAppAction::DeviceStorageGet { req_id, key } => {
                    let value = self
                        .bot_storage
                        .read(self.bot.id, &key)
                        .map(str::to_owned)
                        .or_else(|| self.device_storage.get(&key).cloned());
                    self.post_host_event(&HostEvent::DeviceStorageKeyReceived {
                        req_id,
                        value: value.into(),
                    });
                }
                MiniAppAction::DeviceStorageClear { req_id } => {
                    self.bot_storage.clear(self.bot.id);
                    self.device_storage.clear();
                    if let Some(ref dir) = self.bot_storage_dir
                        && let Err(err) = BotLocalStorage::delete_bot_from_dir(self.bot.id, dir)
                    {
                        tracing::warn!(bot_id = self.bot.id, %err, "Failed to clear persisted BotLocalStorage");
                    }
                    self.post_host_event(&HostEvent::DeviceStorageCleared { req_id });
                }
                MiniAppAction::DownloadFile(params) => {
                    self.enqueue_modal(ActiveMiniAppModal::ConfirmDownloadFile(
                        DownloadFilePrompt {
                            bot_name: self.bot.title.clone(),
                            file_name: params.file_name,
                            url: params.url,
                        },
                    ));
                }
                MiniAppAction::SendPreparedMessage(params) => {
                    match backend.get_prepared_inline_message(self.bot.id, &params.id) {
                        Ok(info) => {
                            let title = if info.preview_title.is_empty() {
                                self.bot.title.clone()
                            } else {
                                info.preview_title
                            };
                            let text = if info.preview_text.is_empty() {
                                format!("Prepared message #{}", info.prepared_id)
                            } else {
                                info.preview_text
                            };
                            let candidates = backend
                                .prepared_message_recipients(self.bot.id, &info.allowed_peer_types);
                            self.enqueue_modal(ActiveMiniAppModal::ConfirmPreparedMessage(
                                PreparedMessagePrompt {
                                    bot_id: self.bot.id,
                                    bot_name: self.bot.username.clone(),
                                    prepared_id: info.prepared_id,
                                    query_id: info.query_id,
                                    result_id: info.result_id,
                                    preview_title: title,
                                    preview_text: text,
                                    allowed_peer_types: info.allowed_peer_types,
                                    candidates,
                                    recipient: None,
                                },
                            ));
                        }
                        Err(err) => {
                            let code = if err.code.is_empty() {
                                "UNKNOWN_ERROR".to_owned()
                            } else {
                                err.code.clone()
                            };
                            self.post_host_event(&HostEvent::PreparedMessageFailed { error: code });
                            self.show_toast(format!(
                                "Prepared message unavailable: {}",
                                err.message
                            ));
                        }
                    }
                }
                MiniAppAction::ShareGameScore => {
                    let url = format!("https://t.me/{}", self.bot.username);
                    events.push(AppWebViewEvent::ShareLink {
                        url,
                        title: self.bot.title.clone(),
                    });
                }
                MiniAppAction::ShareToStory(_)
                | MiniAppAction::RequestChat(_)
                | MiniAppAction::SetEmojiStatus { .. }
                | MiniAppAction::RequestEmojiStatusAccess
                | MiniAppAction::VerifyAge(_) => {}
            }
        }
        events
    }

    /// Executes a Bot Panel header menu action (`"settings"`, `"reload"`, `"share"`,
    /// `"terms"`, `"remove_from_menu"`).
    pub fn handle_menu_action(
        &mut self,
        action_id: &str,
        external: &dyn ExternalOpener,
        now: Instant,
    ) -> Vec<AppWebViewEvent> {
        match action_id {
            "settings" => {
                let action = self.controller.on_settings_button_clicked(now);
                if let MiniAppAction::PostEvent(ev) = action {
                    self.post_host_event(&ev);
                }
                Vec::new()
            }
            "reload" => {
                self.loading = true;
                if let Some(wv) = &self.webview {
                    let _ = wv.reload();
                }
                self.show_toast(strings::RELOADING_MINI_APP.to_owned());
                Vec::new()
            }
            "share" => {
                let url = format!("https://t.me/{}", self.bot.username);
                self.show_toast(format!("{}{}", strings::SHARING_PREFIX, self.bot.username));
                vec![AppWebViewEvent::ShareLink {
                    url,
                    title: self.bot.title.clone(),
                }]
            }
            "terms" => {
                let _ = external.open_url(MINI_APPS_TERMS_URL);
                vec![AppWebViewEvent::OpenExternalUrl {
                    url: MINI_APPS_TERMS_URL.to_owned(),
                }]
            }
            "remove_from_menu" => {
                self.enqueue_modal(ActiveMiniAppModal::ConfirmRemoveFromMenu);
                Vec::new()
            }
            "toggle_fullscreen" => {
                let next_fs = !self.controller.state().fullscreen;
                let event = if next_fs {
                    tg_webview::MiniAppEvent::WebAppRequestFullscreen
                } else {
                    tg_webview::MiniAppEvent::WebAppExitFullscreen
                };
                let actions = self
                    .controller
                    .handle_event(event, &TgPalette::default(), now);
                for act in actions {
                    if let MiniAppAction::PostEvent(ev) = act {
                        let adjusted = match ev {
                            HostEvent::ContentSafeAreaChanged(_) => {
                                HostEvent::ContentSafeAreaChanged(self.compute_content_safe_area())
                            }
                            other => other,
                        };
                        self.post_host_event(&adjusted);
                    }
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Resolves the currently open modal box when the user clicks `action_id` (or `"__close__"`).
    pub fn resolve_active_modal(
        &mut self,
        action_id: &str,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        now: Instant,
    ) -> Vec<AppWebViewEvent> {
        let Some(modal) = self.active_modal.take() else {
            return Vec::new();
        };
        match modal {
            ActiveMiniAppModal::WebAppPopup(_) => {
                self.flood_guard.finish(now);
                let button_id = if action_id == "__close__" {
                    None
                } else {
                    self.controller.record_user_interaction(now);
                    Some(action_id.to_owned())
                };
                self.post_host_event(&HostEvent::PopupClosed { button_id });
                Vec::new()
            }
            ActiveMiniAppModal::ConfirmClose => {
                if action_id == "close_anyway" {
                    self.close_immediately();
                }
                Vec::new()
            }
            ActiveMiniAppModal::ConfirmExternalLink(params) => {
                if action_id == "open" {
                    self.execute_external_link(&params, external)
                } else {
                    Vec::new()
                }
            }
            ActiveMiniAppModal::ConfirmRemoveFromMenu => {
                if action_id == "remove" {
                    let _ = backend.remove_bot_from_menu(self.bot.id);
                    let bot_id = self.bot.id;
                    self.close_immediately();
                    return vec![
                        AppWebViewEvent::BotRemovedFromMenu { bot_id },
                        AppWebViewEvent::Toast {
                            message: tr::lng_bot_remove_from_menu_done(),
                        },
                    ];
                }
                Vec::new()
            }
            ActiveMiniAppModal::ConfirmWriteAccess => {
                let allowed = action_id == "allow";
                self.post_host_event(&HostEvent::WriteAccessRequested {
                    status: if allowed {
                        AccessRequestStatus::Allowed
                    } else {
                        AccessRequestStatus::Cancelled
                    },
                });
                Vec::new()
            }
            ActiveMiniAppModal::ConfirmLocation => {
                let allowed = action_id == "allow";
                self.location_access = Some(allowed);
                if allowed {
                    self.controller.record_user_interaction(now);
                    self.post_current_location();
                } else {
                    self.post_raw_json_event(
                        "location_requested",
                        tg_app_core::core_types::geo::mini_app_location_requested_json(None),
                    );
                }
                Vec::new()
            }
            ActiveMiniAppModal::ConfirmPhone => {
                let sent = action_id == "share";
                self.post_host_event(&HostEvent::PhoneRequested {
                    status: if sent {
                        PhoneRequestStatus::Sent
                    } else {
                        PhoneRequestStatus::Cancelled
                    },
                });
                Vec::new()
            }
            ActiveMiniAppModal::ConfirmDownloadFile(prompt) => {
                if action_id == "download" {
                    let dest = self
                        .user_data_root
                        .join("downloads")
                        .join(&prompt.file_name);
                    let _ = self.bot_downloads.start(
                        self.bot.id,
                        prompt.url.clone(),
                        dest.display().to_string(),
                    );
                    self.post_host_event(&HostEvent::FileDownloadRequested {
                        status: FileDownloadStatus::Downloading,
                    });
                    self.show_toast(format!("Downloading {}…", prompt.file_name));
                } else {
                    self.post_host_event(&HostEvent::FileDownloadRequested {
                        status: FileDownloadStatus::Cancelled,
                    });
                }
                Vec::new()
            }
            ActiveMiniAppModal::ConfirmPreparedMessage(prompt) => {
                if action_id == "send" {
                    let Some(recipient_peer) = prompt.recipient.as_ref().map(|(pid, _)| *pid)
                    else {
                        // The box stays open until a chat is chosen, as the C++ preview box
                        // only sends after `PrepareChooseRecipientBox` picks a thread.
                        self.show_toast("Choose a chat to send the message to.".to_owned());
                        self.pending_modals
                            .insert(0, ActiveMiniAppModal::ConfirmPreparedMessage(prompt));
                        return Vec::new();
                    };
                    match backend.send_prepared_inline_message(
                        prompt.bot_id,
                        recipient_peer,
                        prompt.query_id,
                        &prompt.result_id,
                    ) {
                        Ok(()) => {
                            self.post_host_event(&HostEvent::PreparedMessageSent);
                            self.show_toast(format!(
                                "Shared prepared message from {}",
                                prompt.bot_name
                            ));
                            return vec![AppWebViewEvent::PreparedMessageSent {
                                bot_id: prompt.bot_id,
                                prepared_id: prompt.prepared_id,
                                query_id: prompt.query_id,
                            }];
                        }
                        Err(err) => {
                            self.post_host_event(&HostEvent::PreparedMessageFailed {
                                error: "MESSAGE_SEND_FAILED".to_owned(),
                            });
                            self.show_toast(format!(
                                "Failed to share prepared message: {}",
                                err.message
                            ));
                        }
                    }
                } else {
                    self.post_host_event(&HostEvent::PreparedMessageFailed {
                        error: "USER_DECLINED".to_owned(),
                    });
                }
                Vec::new()
            }
            ActiveMiniAppModal::ScanQrUnsupported(_) => Vec::new(),
            ActiveMiniAppModal::RuntimeError(err) => {
                if action_id == ACTION_DOWNLOAD_WEBVIEW2
                    && let Some(url) = err.download_url()
                {
                    let _ = external.open_url(url);
                    return vec![AppWebViewEvent::OpenExternalUrl {
                        url: url.to_owned(),
                    }];
                }
                if action_id == ACTION_CLOSE_ERROR || action_id == "__close__" {
                    self.close_immediately();
                }
                Vec::new()
            }
        }
    }

    fn execute_external_link(
        &self,
        params: &OpenLinkParams,
        external: &dyn ExternalOpener,
    ) -> Vec<AppWebViewEvent> {
        if params.try_instant_view {
            vec![AppWebViewEvent::OpenInstantView {
                url: params.url.clone(),
                webpage_id: None,
                fragment: None,
            }]
        } else {
            let _ = external.open_url(&params.url);
            vec![AppWebViewEvent::OpenExternalUrl {
                url: params.url.clone(),
            }]
        }
    }

    fn enqueue_modal(&mut self, modal: ActiveMiniAppModal) {
        self.pending_modals.push(modal);
    }

    fn post_host_event(&mut self, event: &HostEvent) {
        if self.emitted_host_events.len() >= MAX_RECORDED_HOST_EVENTS {
            self.emitted_host_events.remove(0);
        }
        self.emitted_host_events.push(event.clone());
        if let Some(wv) = &self.webview {
            let origin = self.launch.same_origin.then_some(self.launch.url.as_str());
            let _ = wv.post_host_event(event, origin);
        }
    }

    /// Answers `web_app_request_location`: asks for per-bot consent first, then reads the device
    /// location through the injected [`tg_platform::geolocation::GeoLocationProvider`].
    fn request_location_for_page(&mut self) {
        use tg_platform::geolocation::GeolocationStatus;
        let os_blocked = matches!(
            self.geo_provider.status(),
            GeolocationStatus::Disabled | GeolocationStatus::NotAvailable
        );
        match self.location_access {
            _ if os_blocked => self.post_raw_json_event(
                "location_requested",
                tg_app_core::core_types::geo::mini_app_location_requested_json(None),
            ),
            Some(true) => self.post_current_location(),
            Some(false) => self.post_raw_json_event(
                "location_requested",
                tg_app_core::core_types::geo::mini_app_location_requested_json(None),
            ),
            None => {
                let already_asking =
                    matches!(self.active_modal, Some(ActiveMiniAppModal::ConfirmLocation))
                        || self
                            .pending_modals
                            .iter()
                            .any(|m| matches!(m, ActiveMiniAppModal::ConfirmLocation));
                if !already_asking {
                    self.enqueue_modal(ActiveMiniAppModal::ConfirmLocation);
                }
            }
        }
    }

    /// Starts the device location fix on a worker thread; [`Self::poll_pending_location`] posts
    /// `location_requested` once it arrives. A request while one is running shares its answer.
    fn post_current_location(&mut self) {
        if self.location_pending.is_some() {
            return;
        }
        let provider = Arc::clone(&self.geo_provider);
        self.location_pending = Some(PlatformCall::spawn(
            "geolocation-miniapp",
            None,
            move || provider.resolve_current_exact_location().ok().flatten(),
        ));
    }

    /// Returns `true` while a device location fix for the page is running.
    #[must_use]
    pub const fn is_location_pending(&self) -> bool {
        self.location_pending.is_some()
    }

    /// Posts `location_requested` once the device location fix finishes.
    ///
    /// Returns `true` when an answer was posted this call.
    pub fn poll_pending_location(&mut self) -> bool {
        let Some(call) = self.location_pending.as_ref() else {
            return false;
        };
        let loc = match call.poll() {
            PlatformCallState::Pending => return false,
            PlatformCallState::Done(loc) => loc,
            PlatformCallState::Lost => None,
        };
        self.location_pending = None;
        let payload = tg_app_core::core_types::geo::mini_app_location_requested_json(loc);
        self.post_raw_json_event("location_requested", payload);
        true
    }

    /// Blocks up to `timeout` for the device location fix, then posts it (tests only).
    #[doc(hidden)]
    pub fn wait_pending_location(&mut self, timeout: Duration) -> bool {
        let Some(call) = self.location_pending.as_ref() else {
            return false;
        };
        let loc = match call.wait(timeout) {
            PlatformCallState::Pending => return false,
            PlatformCallState::Done(loc) => loc,
            PlatformCallState::Lost => None,
        };
        self.location_pending = None;
        let payload = tg_app_core::core_types::geo::mini_app_location_requested_json(loc);
        self.post_raw_json_event("location_requested", payload);
        true
    }

    fn post_raw_json_event(&mut self, event_name: &str, payload: serde_json::Value) {
        self.posted_raw_events
            .push((event_name.to_owned(), payload.clone()));
        if let Some(wv) = &self.webview {
            let origin = self.launch.same_origin.then_some(self.launch.url.as_str());
            if let Ok(encoded) = serde_json::to_string(&payload) {
                let script = format_raw_post_event_script(event_name, Some(&encoded), origin);
                let _ = wv.eval(&script);
            }
        }
    }

    fn send_viewport(&self) {
        if let Some(wv) = &self.webview {
            let origin = self.launch.same_origin.then_some(self.launch.url.as_str());
            if let Some(h) = self.last_viewport_height {
                let _ = wv.post_host_event(
                    &HostEvent::ViewportChanged(ViewportParams {
                        height: f64::from(h),
                        is_state_stable: true,
                        is_expanded: true,
                    }),
                    origin,
                );
            } else {
                let _ = wv.post_dynamic_viewport(origin);
            }
        }
    }

    fn show_toast(&mut self, message: String) {
        self.toast = Some((message, Instant::now()));
    }

    /// Renders this Mini App panel as a separate top-level `eframe` viewport.
    pub fn show_viewport(
        &mut self,
        ctx: &egui::Context,
        palette: &TgPalette,
        is_night: bool,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        clipboard: &dyn ClipboardAccess,
    ) -> Vec<AppWebViewEvent> {
        if !self.open {
            return Vec::new();
        }
        let now = Instant::now();
        let mut events = self.drain_bridge_events(palette, backend, external, clipboard, now);
        if !self.poll_pending_location() && self.location_pending.is_some() {
            ctx.request_repaint_after(PLATFORM_CALL_REPAINT_INTERVAL);
        }
        let title = self.window_title();
        let viewport_id = ViewportId::from_hash_of(MINI_APP_VIEWPORT_ID_STR);
        let builder = ViewportBuilder::default()
            .with_title(title.clone())
            .with_inner_size(Vec2::new(MINI_APP_DEFAULT_WIDTH, MINI_APP_DEFAULT_HEIGHT))
            .with_min_inner_size(Vec2::new(MINI_APP_MIN_WIDTH, MINI_APP_MIN_HEIGHT));

        ctx.show_viewport_immediate(viewport_id, builder, |vp_ui, _class| {
            let vp_ctx = vp_ui.ctx().clone();
            vp_ctx.set_visuals(egui_visuals_from_tg_palette(palette, is_night));

            // Handle OS close request, honoring closing confirmation.
            if vp_ctx.input(|i| i.viewport().close_requested()) && !self.request_close() {
                vp_ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            }

            // Sync fullscreen state to viewport.
            let target_fs = self.controller.state().fullscreen;
            if target_fs != self.fullscreen_synced {
                vp_ctx.send_viewport_cmd(ViewportCommand::Fullscreen(target_fs));
                self.fullscreen_synced = target_fs;
            }

            self.viewport_frames = self.viewport_frames.saturating_add(1);

            // Ctrl+W / Esc close shortcut when no modal box is intercepting Escape.
            let close_shortcut = vp_ctx.input(|i| {
                i.modifiers.matches_logically(Modifiers::COMMAND) && i.key_pressed(Key::W)
            });
            if close_shortcut {
                let _ = self.request_close();
            }

            events.extend(
                self.render_window_contents(
                    vp_ui, palette, &title, backend, external, clipboard, now,
                ),
            );
        });

        events
    }

    /// Renders the Bot Mini App header, bottom buttons, `tg-ui` popups, and WebView2 child HWND.
    #[allow(clippy::too_many_arguments)]
    pub fn render_window_contents(
        &mut self,
        ui: &mut Ui,
        palette: &TgPalette,
        window_title: &str,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        clipboard: &dyn ClipboardAccess,
        now: Instant,
    ) -> Vec<AppWebViewEvent> {
        let ctx = ui.ctx().clone();
        ensure_tdesktop_fonts(&ctx);
        let mut events = Vec::new();
        let colors = TgThemeColors(palette);

        let header_bg_tg = self
            .controller
            .state()
            .header_color
            .unwrap_or(palette.window_bg);
        let header_h = if self.controller.state().fullscreen {
            MINI_APP_FULLSCREEN_HEADER_HEIGHT
        } else {
            MINI_APP_HEADER_HEIGHT
        };

        // 1. Bot Mini App Header Bar (Back/Close, Title + Verified badge, Menu, Close).
        Panel::top("tg_mini_app_header")
            .exact_size(header_h)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.fill(color_to_egui(header_bg_tg)))
            .show(ui, |ui| {
                events.extend(self.render_header(ui, palette, header_bg_tg, now));
            });

        // 2. Bottom Main & Secondary Action Buttons bar driven by `MiniAppController::state`.
        let bottom_h = self.bottom_bar_height();
        if bottom_h > 0.0 {
            let bottom_bg_tg = self
                .controller
                .state()
                .bottom_bar_color
                .or(self.controller.state().background_color)
                .unwrap_or(palette.window_bg);
            Panel::bottom("tg_mini_app_bottom_bar")
                .exact_size(bottom_h)
                .show_separator_line(false)
                .frame(egui::Frame::NONE.fill(color_to_egui(bottom_bg_tg)))
                .show(ui, |ui| {
                    self.render_bottom_buttons(ui, palette, bottom_bg_tg, now);
                });
        }

        // 3. Central WebView area (or missing-runtime backdrop).
        let body_bg_tg = self
            .controller
            .state()
            .background_color
            .unwrap_or(palette.window_bg);
        let mut content_rect = Rect::NOTHING;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(color_to_egui(body_bg_tg)))
            .show(ui, |ui| {
                content_rect = ui.max_rect();
                if let Some(err) = &self.runtime_error {
                    render_unavailable_backdrop(ui, content_rect, palette, err);
                }
            });

        // 4. Bot panel dropdown menu (`tg-ui` PopupMenu).
        let menu_items = self.build_menu_items();
        let menu_style = PopupMenuStyle::default_popup(&colors);
        let toggle_style = ToggleStyle::default_toggle(&colors);
        if let Some(action_id) = self.popup_menu.show(
            &ctx,
            Id::unique("tg_mini_app_menu"),
            &menu_items,
            &menu_style,
            &toggle_style,
        ) {
            events.extend(self.handle_menu_action(&action_id, external, now));
        }

        // 5. Promote next queued modal into `tg-ui` LayerStack when idle.
        if self.active_modal.is_none() && !self.pending_modals.is_empty() {
            let next = self.pending_modals.remove(0);
            self.push_modal_to_layer_stack(&ctx, palette, &next);
            self.active_modal = Some(next);
        }

        // 6. Render `tg-ui` LayerStack modal boxes (`web_app_open_popup`, closing confirmation, etc.).
        let box_style = BoxStyle::default_box(&colors);
        let active_snapshot = self.active_modal.clone();
        let bot_title = self.bot.title.clone();
        let bot_in_main_menu = self.bot.in_main_menu;
        let mut inline_download_clicked = false;
        let mut chosen_recipient = None;
        let layer_event = self.layer_stack.show(
            &ctx,
            ctx.viewport_rect(),
            &box_style,
            &colors,
            |_box_id, ui| match &active_snapshot {
                Some(ActiveMiniAppModal::ConfirmPreparedMessage(prompt)) => {
                    chosen_recipient = render_prepared_message_confirm_body(ui, palette, prompt);
                }
                Some(modal) => {
                    inline_download_clicked = render_mini_app_modal_body(
                        ui,
                        palette,
                        modal,
                        &bot_title,
                        bot_in_main_menu,
                    );
                }
                None => {}
            },
        );
        if let Some((peer, title)) = chosen_recipient {
            self.set_prepared_message_recipient(peer, title);
        }
        if inline_download_clicked
            && let Some(ActiveMiniAppModal::RuntimeError(err)) = &self.active_modal
            && let Some(url) = err.download_url()
        {
            let _ = external.open_url(url);
            events.push(AppWebViewEvent::OpenExternalUrl {
                url: url.to_owned(),
            });
        }
        if let Some(ev) = layer_event {
            self.layer_stack.clear(&ctx, &box_style);
            events.extend(self.resolve_active_modal(&ev.action_id, backend, external, now));
        }

        // 7. Transient toast overlay.
        self.render_toast(&ctx, palette);

        // 8. Sync native WebView2 child HWND.
        let overlay_active = self.popup_menu.is_open()
            || !self.layer_stack.is_empty()
            || self.active_modal.is_some();
        self.sync_native_webview(&ctx, window_title, content_rect, palette, overlay_active);

        let _ = clipboard;
        events
    }

    fn render_header(
        &mut self,
        ui: &mut Ui,
        palette: &TgPalette,
        header_bg: tg_theme::Color,
        now: Instant,
    ) -> Vec<AppWebViewEvent> {
        let colors = TgThemeColors(palette);
        let contrast = compute_contrast_colors(header_bg);
        let bar_rect = ui.max_rect();
        let back_visible = self.controller.state().back_button_visible;

        ui.horizontal_centered(|ui| {
            ui.add_space(6.0);

            // Left control: Back button (when visible) or Close button.
            if back_visible {
                if header_icon_button(
                    ui,
                    st::separate_panel_back.icon,
                    &colors,
                    contrast,
                    strings::BACK_TOOLTIP,
                )
                .clicked()
                {
                    let act = self.controller.on_back_button_clicked(now);
                    if let MiniAppAction::PostEvent(ev) = act {
                        self.post_host_event(&ev);
                    }
                }
            } else if header_icon_button(
                ui,
                st::separate_panel_close.icon,
                &colors,
                contrast,
                &tr::lng_close(),
            )
            .clicked()
            {
                let _ = self.request_close();
            }

            ui.add_space(4.0);

            // Bot title + verified check badge + `@username` subtitle.
            ui.vertical(|ui| {
                ui.add_space(if self.controller.state().fullscreen {
                    3.0
                } else {
                    6.0
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    ui.label(
                        RichText::new(&self.bot.title)
                            .font(font_id_from_style(st::separate_panel_title_font))
                            .strong()
                            .color(contrast.text),
                    );
                    if self.bot.verified {
                        let (badge_rect, _) =
                            ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                        paint_style_icon(
                            ui.painter(),
                            badge_rect,
                            st::dialogs_verified_icon,
                            &colors,
                            None,
                        );
                    }
                });
                if !self.controller.state().fullscreen {
                    ui.label(
                        RichText::new(format!("@{}", self.bot.username))
                            .size(11.5)
                            .color(contrast.muted),
                    );
                }
            });

            // Right controls: Close (when Back is shown on left) + Menu (`...`).
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(6.0);
                if back_visible
                    && header_icon_button(
                        ui,
                        st::separate_panel_close.icon,
                        &colors,
                        contrast,
                        &tr::lng_close(),
                    )
                    .clicked()
                {
                    let _ = self.request_close();
                }

                let menu_resp = header_icon_button(
                    ui,
                    st::separate_panel_menu.icon,
                    &colors,
                    contrast,
                    &tr::lng_bot_menu_button(),
                );
                if menu_resp.clicked() {
                    if self.popup_menu.is_open() {
                        self.popup_menu.close();
                    } else {
                        let anchor = Pos2::new(
                            menu_resp.rect.right() - 210.0,
                            menu_resp.rect.bottom() + 4.0,
                        );
                        let menu_style = PopupMenuStyle::default_popup(&colors);
                        self.popup_menu.open_at(ui.ctx(), anchor, &menu_style);
                    }
                }
            });
        });

        // Subtle 1px divider below header.
        ui.painter().line_segment(
            [
                Pos2::new(bar_rect.min.x, bar_rect.max.y - 0.5),
                Pos2::new(bar_rect.max.x, bar_rect.max.y - 0.5),
            ],
            Stroke::new(1.0, contrast.divider),
        );

        Vec::new()
    }

    fn render_bottom_buttons(
        &mut self,
        ui: &mut Ui,
        palette: &TgPalette,
        bottom_bg: tg_theme::Color,
        now: Instant,
    ) {
        let bar_rect = ui.max_rect();
        let contrast = compute_contrast_colors(bottom_bg);

        // Top 1px separator line.
        ui.painter().line_segment(
            [
                Pos2::new(bar_rect.min.x, bar_rect.min.y + 0.5),
                Pos2::new(bar_rect.max.x, bar_rect.min.y + 0.5),
            ],
            Stroke::new(1.0, contrast.divider),
        );

        let main_st = self.controller.state().main_button.clone();
        let sec_st = self.controller.state().secondary_button.clone();
        let sec_pos = self.controller.state().secondary_position;

        let inner_w = (bar_rect.width() - BUTTON_BAR_PAD_H * 2.0).max(100.0);
        let mut clicked_main = false;
        let mut clicked_secondary = false;

        ui.add_space(BUTTON_BAR_PAD_V);
        ui.horizontal(|ui| {
            ui.add_space(BUTTON_BAR_PAD_H);
            match (main_st.is_visible, sec_st.is_visible) {
                (true, false) => {
                    if paint_mini_app_button(ui, &main_st, inner_w, palette, true).clicked() {
                        clicked_main = true;
                    }
                }
                (false, true) => {
                    if paint_mini_app_button(ui, &sec_st, inner_w, palette, false).clicked() {
                        clicked_secondary = true;
                    }
                }
                (true, true) => match sec_pos {
                    SecondaryButtonPosition::Left => {
                        let half = ((inner_w - BUTTON_GAP) * 0.5).max(40.0);
                        ui.spacing_mut().item_spacing.x = BUTTON_GAP;
                        if paint_mini_app_button(ui, &sec_st, half, palette, false).clicked() {
                            clicked_secondary = true;
                        }
                        if paint_mini_app_button(ui, &main_st, half, palette, true).clicked() {
                            clicked_main = true;
                        }
                    }
                    SecondaryButtonPosition::Right => {
                        let half = ((inner_w - BUTTON_GAP) * 0.5).max(40.0);
                        ui.spacing_mut().item_spacing.x = BUTTON_GAP;
                        if paint_mini_app_button(ui, &main_st, half, palette, true).clicked() {
                            clicked_main = true;
                        }
                        if paint_mini_app_button(ui, &sec_st, half, palette, false).clicked() {
                            clicked_secondary = true;
                        }
                    }
                    SecondaryButtonPosition::Top => {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = BUTTON_GAP;
                            if paint_mini_app_button(ui, &sec_st, inner_w, palette, false).clicked()
                            {
                                clicked_secondary = true;
                            }
                            if paint_mini_app_button(ui, &main_st, inner_w, palette, true).clicked()
                            {
                                clicked_main = true;
                            }
                        });
                    }
                    SecondaryButtonPosition::Bottom => {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = BUTTON_GAP;
                            if paint_mini_app_button(ui, &main_st, inner_w, palette, true).clicked()
                            {
                                clicked_main = true;
                            }
                            if paint_mini_app_button(ui, &sec_st, inner_w, palette, false).clicked()
                            {
                                clicked_secondary = true;
                            }
                        });
                    }
                },
                (false, false) => {}
            }
        });

        if clicked_main
            && let Some(MiniAppAction::PostEvent(ev)) = self.controller.on_main_button_clicked(now)
        {
            self.post_host_event(&ev);
        }
        if clicked_secondary
            && let Some(MiniAppAction::PostEvent(ev)) =
                self.controller.on_secondary_button_clicked(now)
        {
            self.post_host_event(&ev);
        }
    }

    /// Builds the Bot Mini App header dropdown menu items (`attach_bot_webview.cpp:665-756`).
    #[must_use]
    pub fn build_menu_items(&self) -> Vec<MenuItem> {
        let mut items = Vec::new();
        if self.controller.state().settings_button_visible {
            items.push(
                MenuItem::action("settings", tr::lng_bot_settings()).with_icon(IconKind::Settings),
            );
        }
        items.push(
            MenuItem::action("reload", tr::lng_bot_reload_page()).with_icon(IconKind::Restore),
        );
        items.push(MenuItem::action("share", tr::lng_iv_share()).with_icon(IconKind::Send));
        let fs_label = if self.controller.state().fullscreen {
            strings::EXIT_FULLSCREEN
        } else {
            strings::FULLSCREEN
        };
        items.push(MenuItem::action("toggle_fullscreen", fs_label).with_icon(IconKind::Maximize));
        items.push(MenuItem::action("terms", tr::lng_bot_terms()).with_icon(IconKind::Info));

        if self.bot.in_attach_menu || self.bot.in_main_menu {
            items.push(MenuItem::Separator);
            let remove_label = if self.bot.in_main_menu {
                tr::lng_bot_remove_from_side_menu()
            } else {
                tr::lng_bot_remove_from_menu()
            };
            items.push(
                MenuItem::action("remove_from_menu", remove_label)
                    .with_icon(IconKind::Trash)
                    .attention(),
            );
        }
        items
    }

    fn push_modal_to_layer_stack(
        &mut self,
        ctx: &egui::Context,
        palette: &TgPalette,
        modal: &ActiveMiniAppModal,
    ) {
        let colors = TgThemeColors(palette);
        let style = BoxStyle::default_box(&colors);
        match modal {
            ActiveMiniAppModal::WebAppPopup(args) => {
                let title = if args.title.is_empty() {
                    self.bot.title.clone()
                } else {
                    args.title.clone()
                };
                let mut box_modal = BoxModal::new(BOX_MINI_APP_POPUP, title).width(340.0);
                for btn in &args.buttons {
                    box_modal = box_modal.add_button(popup_button_to_box_button(btn));
                }
                self.layer_stack.push(ctx, box_modal, &style);
            }
            ActiveMiniAppModal::ConfirmClose => {
                let box_modal = BoxModal::new(BOX_CONFIRM_CLOSE, tr::lng_bot_close_warning_title())
                    .width(340.0)
                    .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                    .add_button(BoxButton::attention(
                        "close_anyway",
                        tr::lng_bot_close_warning_sure(),
                    ));
                self.layer_stack.push(ctx, box_modal, &style);
            }
            ActiveMiniAppModal::ConfirmExternalLink(_) => {
                let box_modal = BoxModal::new(BOX_CONFIRM_EXT_LINK, tr::lng_open_this_link())
                    .width(360.0)
                    .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                    .add_button(BoxButton::default_btn("open", tr::lng_open_link()));
                self.layer_stack.push(ctx, box_modal, &style);
            }
            ActiveMiniAppModal::ConfirmRemoveFromMenu => {
                let title = if self.bot.in_main_menu {
                    tr::lng_bot_remove_from_side_menu()
                } else {
                    tr::lng_bot_remove_from_menu()
                };
                let box_modal = BoxModal::new(BOX_CONFIRM_REMOVE, title)
                    .width(340.0)
                    .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                    .add_button(BoxButton::attention("remove", tr::lng_box_remove()));
                self.layer_stack.push(ctx, box_modal, &style);
            }
            ActiveMiniAppModal::ConfirmWriteAccess => {
                let box_modal =
                    BoxModal::new(BOX_CONFIRM_WRITE_ACCESS, tr::lng_bot_allow_write_title())
                        .width(340.0)
                        .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                        .add_button(BoxButton::default_btn(
                            "allow",
                            tr::lng_bot_allow_write_confirm(),
                        ));
                self.layer_stack.push(ctx, box_modal, &style);
            }
            ActiveMiniAppModal::ConfirmLocation => {
                let box_modal = BoxModal::new(BOX_CONFIRM_LOCATION, self.bot.title.clone())
                    .width(340.0)
                    .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                    .add_button(BoxButton::default_btn(
                        "allow",
                        tr::lng_bot_allow_permission_confirm(),
                    ));
                self.layer_stack.push(ctx, box_modal, &style);
            }
            ActiveMiniAppModal::ConfirmPhone => {
                let box_modal = BoxModal::new(BOX_CONFIRM_PHONE, strings::SHARE_PHONE_TITLE)
                    .width(340.0)
                    .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                    .add_button(BoxButton::default_btn(
                        "share",
                        tr::lng_bot_share_phone_confirm(),
                    ));
                self.layer_stack.push(ctx, box_modal, &style);
            }
            ActiveMiniAppModal::ConfirmDownloadFile(_) => {
                let box_modal = BoxModal::new(BOX_CONFIRM_DOWNLOAD_FILE, "Download File")
                    .width(360.0)
                    .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                    .add_button(BoxButton::default_btn("download", "Download"));
                self.layer_stack.push(ctx, box_modal, &style);
            }
            ActiveMiniAppModal::ConfirmPreparedMessage(_) => {
                let box_modal =
                    BoxModal::new(BOX_CONFIRM_PREPARED_MESSAGE, "Share Prepared Message")
                        .width(380.0)
                        .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                        .add_button(BoxButton::default_btn("send", "Send"));
                self.layer_stack.push(ctx, box_modal, &style);
            }
            ActiveMiniAppModal::ScanQrUnsupported(args) => {
                let mut box_modal = BoxModal::new(
                    BOX_SCAN_QR_UNSUPPORTED,
                    strings::QR_SCANNER_UNAVAILABLE_TITLE,
                )
                .width(340.0);
                for btn in &args.buttons {
                    box_modal = box_modal.add_button(popup_button_to_box_button(btn));
                }
                self.layer_stack.push(ctx, box_modal, &style);
            }
            ActiveMiniAppModal::RuntimeError(err) => {
                push_runtime_error_box(ctx, &mut self.layer_stack, palette, err, false);
            }
        }
    }

    fn drain_bridge_events(
        &mut self,
        palette: &TgPalette,
        backend: &mut dyn WebViewBackend,
        external: &dyn ExternalOpener,
        clipboard: &dyn ClipboardAccess,
        now: Instant,
    ) -> Vec<AppWebViewEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = self.rx_events.try_recv() {
            match ev {
                MiniAppBridgeEvent::UserInteraction => {
                    self.controller.record_user_interaction(now);
                }
                MiniAppBridgeEvent::Message(msg) => {
                    events.extend(
                        self.handle_web_message(&msg, palette, backend, external, clipboard, now),
                    );
                }
                MiniAppBridgeEvent::NavigationStart => {
                    self.loading = true;
                }
                MiniAppBridgeEvent::NavigationDone(_ok) => {
                    self.loading = false;
                    self.send_viewport();
                }
                MiniAppBridgeEvent::DomFullscreenChanged(fs) => {
                    let event = if fs {
                        tg_webview::MiniAppEvent::WebAppRequestFullscreen
                    } else {
                        tg_webview::MiniAppEvent::WebAppExitFullscreen
                    };
                    let actions = self.controller.handle_event(event, palette, now);
                    events.extend(self.handle_actions(actions, backend, external, clipboard, now));
                }
                MiniAppBridgeEvent::LocalUri(uri) => {
                    events.push(AppWebViewEvent::OpenTelegramLink { uri });
                }
                MiniAppBridgeEvent::ProcessFailed => {
                    if let Some(wv) = self.webview.take() {
                        wv.close();
                    }
                    let err = RuntimeErrorKind::ProcessCrashed;
                    self.runtime_error = Some(err.clone());
                    self.enqueue_modal(ActiveMiniAppModal::RuntimeError(err));
                }
            }
        }
        events
    }

    fn sync_native_webview(
        &mut self,
        ctx: &egui::Context,
        window_title: &str,
        content_rect: Rect,
        palette: &TgPalette,
        overlay_active: bool,
    ) {
        if self.runtime_error.is_some() {
            return;
        }

        if !self.webview_init_attempted && self.viewport_frames >= 2 {
            self.try_attach_webview(ctx, window_title, content_rect, palette);
        }

        let Some(wv) = &self.webview else {
            return;
        };

        if overlay_active != self.webview_hidden_for_overlay {
            let _ = wv.set_visible(!overlay_active);
            self.webview_hidden_for_overlay = overlay_active;
        }

        if let Some(bounds) = self.compute_physical_bounds(ctx, content_rect)
            && self.last_bounds != Some(bounds)
        {
            let _ = wv.set_bounds(bounds);
            self.last_bounds = Some(bounds);
            let logical_h = (content_rect.height().round() as i32).max(1);
            if self.last_viewport_height != Some(logical_h) {
                self.last_viewport_height = Some(logical_h);
                self.send_viewport();
            }
        }
    }

    /// Locates the Win32 `HWND` for this Mini App viewport and embeds WebView2.
    pub fn try_attach_webview(
        &mut self,
        ctx: &egui::Context,
        window_title: &str,
        content_rect: Rect,
        palette: &TgPalette,
    ) {
        self.webview_init_attempted = true;

        let avail = availability();
        if let Some(err) = RuntimeErrorKind::from_availability(&avail) {
            self.runtime_error = Some(err.clone());
            self.enqueue_modal(ActiveMiniAppModal::RuntimeError(err));
            return;
        }

        let Some(hwnd) = find_thread_window_by_title(window_title) else {
            self.webview_init_attempted = false;
            return;
        };
        self.attached_hwnd = Some(hwnd);

        let Some(raw_handle) = win32_raw_handle(hwnd) else {
            let err = RuntimeErrorKind::InitFailed {
                details: strings::PARENT_HANDLE_UNAVAILABLE.to_owned(),
            };
            self.runtime_error = Some(err.clone());
            self.enqueue_modal(ActiveMiniAppModal::RuntimeError(err));
            return;
        };

        let bounds = self
            .compute_physical_bounds(ctx, content_rect)
            .unwrap_or_else(|| WebViewBounds::new(0, 48, 420, 632));

        let tx_msg = self.tx_events.clone();
        let tx_int = self.tx_events.clone();
        let tx_start = self.tx_events.clone();
        let tx_done = self.tx_events.clone();
        let tx_fs = self.tx_events.clone();
        let tx_nav = self.tx_events.clone();
        let tx_fail = self.tx_events.clone();
        let ctx_msg = ctx.clone();
        let ctx_done = ctx.clone();
        let ctx_fs = ctx.clone();
        let ctx_nav = ctx.clone();
        let ctx_fail = ctx.clone();

        let user_data_path =
            resolve_user_data_dir(&self.user_data_root, &format!("bot_{}", self.bot.id));
        let config = WebViewConfig {
            opaque_bg: palette.window_bg,
            user_data_path: Some(user_data_path),
            debug: cfg!(debug_assertions),
            initial_bounds: Some(bounds),
            custom_scheme: DEFAULT_CUSTOM_SCHEME.to_owned(),
            scheme_router: self.launch.local_routes.clone(),
            callbacks: WebViewCallbacks {
                on_message: Some(Arc::new(move |msg| {
                    let _ = tx_msg.send(MiniAppBridgeEvent::Message(msg));
                    ctx_msg.request_repaint();
                })),
                on_interaction: Some(Arc::new(move || {
                    let _ = tx_int.send(MiniAppBridgeEvent::UserInteraction);
                })),
                on_navigation_policy: Some(Arc::new(move |uri, _new_window| {
                    if is_local_telegram_uri(uri) {
                        let _ = tx_nav.send(MiniAppBridgeEvent::LocalUri(uri.to_owned()));
                        ctx_nav.request_repaint();
                        false
                    } else {
                        true
                    }
                })),
                on_navigation_start: Some(Arc::new(move || {
                    let _ = tx_start.send(MiniAppBridgeEvent::NavigationStart);
                })),
                on_navigation_done: Some(Arc::new(move |ok| {
                    let _ = tx_done.send(MiniAppBridgeEvent::NavigationDone(ok));
                    ctx_done.request_repaint();
                })),
                on_fullscreen_changed: Some(Arc::new(move |fs| {
                    let _ = tx_fs.send(MiniAppBridgeEvent::DomFullscreenChanged(fs));
                    ctx_fs.request_repaint();
                })),
                on_process_failed: Some(Arc::new(move || {
                    let _ = tx_fail.send(MiniAppBridgeEvent::ProcessFailed);
                    ctx_fail.request_repaint();
                })),
                ..WebViewCallbacks::default()
            },
            ..WebViewConfig::default()
        };

        match WebView::create_from_raw(Some(raw_handle), config) {
            Ok(wv) => {
                let _ = wv.install_mini_app_bridge();
                let params = build_webview_theme_params(palette);
                let _ = wv.update_theme(&params);
                let _ = wv.navigate(&self.launch.url);
                let _ = wv.post_host_event(
                    &HostEvent::ThemeChanged(ThemeChangedPayload::from_palette(palette)),
                    None,
                );
                self.last_bounds = Some(bounds);
                self.last_viewport_height = Some((content_rect.height().round() as i32).max(1));
                self.webview = Some(wv);
            }
            Err(err) => {
                let kind = RuntimeErrorKind::from_webview_error(&err);
                self.runtime_error = Some(kind.clone());
                self.enqueue_modal(ActiveMiniAppModal::RuntimeError(kind));
            }
        }
    }

    fn compute_physical_bounds(
        &self,
        ctx: &egui::Context,
        content_rect: Rect,
    ) -> Option<WebViewBounds> {
        let ppp = ctx.pixels_per_point();
        let left_px = (content_rect.min.x * ppp).round() as i32;
        let top_px = (content_rect.min.y * ppp).round() as i32;
        let bottom_px = (self.bottom_bar_height() * ppp).round() as i32;
        let (w_px, h_px) = if let Some(hwnd) = self.attached_hwnd
            && let Some((cw, ch)) = client_size_for_hwnd(hwnd)
        {
            ((cw - left_px).max(1), (ch - top_px - bottom_px).max(1))
        } else {
            (
                (content_rect.width() * ppp).round().max(1.0) as i32,
                (content_rect.height() * ppp).round().max(1.0) as i32,
            )
        };
        Some(WebViewBounds::new(left_px, top_px, w_px, h_px))
    }

    fn render_toast(&mut self, ctx: &egui::Context, palette: &TgPalette) {
        let Some((msg, shown_at)) = &self.toast else {
            return;
        };
        if shown_at.elapsed() >= TOAST_DURATION {
            self.toast = None;
            return;
        }
        egui::Area::new(Id::unique("tg_mini_app_toast"))
            .order(egui::Order::Tooltip)
            .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -76.0))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(color_to_egui(palette.toast_bg))
                    .corner_radius(CornerRadius::same(16))
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(msg)
                                .size(12.5)
                                .color(color_to_egui(palette.toast_fg)),
                        );
                    });
            });
    }
}

/// Returns `true` if `uri` is a local Telegram/TON link that should be handed off to the app.
#[must_use]
pub fn is_local_telegram_uri(uri: &str) -> bool {
    let lower = uri.trim().to_ascii_lowercase();
    lower.starts_with("tg://")
        || lower.starts_with("ton://")
        || lower.starts_with("tonsite://")
        || lower.starts_with("https://t.me/")
        || lower.starts_with("http://t.me/")
}

fn popup_button_to_box_button(btn: &PopupButton) -> BoxButton {
    let label = if !btn.text.trim().is_empty() {
        btn.text.clone()
    } else {
        match btn.button_type {
            PopupButtonType::Ok => tr::lng_box_ok(),
            PopupButtonType::Close => tr::lng_close(),
            PopupButtonType::Cancel => tr::lng_cancel(),
            PopupButtonType::Default | PopupButtonType::Destructive => tr::lng_box_ok(),
        }
    };
    let kind = if btn.button_type == PopupButtonType::Destructive {
        BoxButtonKind::Attention
    } else {
        BoxButtonKind::Default
    };
    BoxButton {
        id: btn.id.clone(),
        label,
        kind,
    }
}

fn render_mini_app_modal_body(
    ui: &mut Ui,
    palette: &TgPalette,
    modal: &ActiveMiniAppModal,
    bot_title: &str,
    bot_in_main_menu: bool,
) -> bool {
    let colors = TgThemeColors(palette);
    let text_fg = colors.color("boxTextFg");
    match modal {
        ActiveMiniAppModal::WebAppPopup(args) => {
            ui.label(RichText::new(&args.text).size(14.0).color(text_fg));
            false
        }
        ActiveMiniAppModal::ScanQrUnsupported(_) => {
            ui.label(
                RichText::new(tr::lng_bot_no_scan_qr())
                    .size(14.0)
                    .color(text_fg),
            );
            false
        }
        ActiveMiniAppModal::ConfirmClose => {
            ui.label(
                RichText::new(tr::lng_bot_close_warning())
                    .size(14.0)
                    .color(text_fg),
            );
            false
        }
        ActiveMiniAppModal::ConfirmExternalLink(params) => {
            ui.label(
                RichText::new(strings::CONFIRM_EXTERNAL_LINK_BODY)
                    .size(14.0)
                    .color(text_fg),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(&params.url)
                    .size(12.5)
                    .color(colors.color("windowActiveTextFg")),
            );
            false
        }
        ActiveMiniAppModal::ConfirmRemoveFromMenu => {
            let body = if bot_in_main_menu {
                tr::lng_bot_remove_from_side_menu_sure(bot_title)
            } else {
                tr::lng_bot_remove_from_menu_sure(bot_title)
            };
            ui.label(RichText::new(body).size(14.0).color(text_fg));
            false
        }
        ActiveMiniAppModal::ConfirmWriteAccess => {
            ui.label(
                RichText::new(tr::lng_bot_allow_write())
                    .size(14.0)
                    .color(text_fg),
            );
            false
        }
        ActiveMiniAppModal::ConfirmLocation => {
            ui.label(
                RichText::new(tr::lng_bot_allow_location())
                    .size(14.0)
                    .color(text_fg),
            );
            false
        }
        ActiveMiniAppModal::ConfirmPhone => {
            ui.label(
                RichText::new(tr::lng_bot_share_phone())
                    .size(14.0)
                    .color(text_fg),
            );
            false
        }
        ActiveMiniAppModal::ConfirmDownloadFile(prompt) => {
            render_download_file_confirm_body(ui, palette, prompt);
            false
        }
        ActiveMiniAppModal::ConfirmPreparedMessage(prompt) => {
            let _ = render_prepared_message_confirm_body(ui, palette, prompt);
            false
        }
        ActiveMiniAppModal::RuntimeError(err) => render_runtime_error_body(ui, palette, err, None),
    }
}

fn header_icon_button(
    ui: &mut Ui,
    icon: StyleIcon,
    colors: &TgThemeColors<'_>,
    contrast: crate::webview::theme::ContrastColors,
    tooltip: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .circle_filled(rect.center(), 17.0, contrast.control_hover);
    }
    paint_style_icon(ui.painter(), rect, icon, colors, Some(contrast.control));
    resp.on_hover_text(tooltip)
}

fn paint_mini_app_button(
    ui: &mut Ui,
    state: &MiniAppButtonState,
    width: f32,
    palette: &TgPalette,
    is_main: bool,
) -> egui::Response {
    let bg_tg = state.color.unwrap_or(if is_main {
        palette.window_bg_active
    } else {
        palette.window_bg_over
    });
    let fg_tg = state.text_color.unwrap_or(if is_main {
        palette.window_fg_active
    } else {
        palette.window_active_text_fg
    });
    let hover_tg = resolve_ripple_color(bg_tg);

    let sense = if state.is_active {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, BUTTON_ROW_HEIGHT), sense);
    if state.is_active && resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let bg = if !state.is_active {
        color_to_egui(bg_tg).gamma_multiply(0.55)
    } else if resp.hovered() || resp.is_pointer_button_down_on() {
        color_to_egui(hover_tg)
    } else {
        color_to_egui(bg_tg)
    };
    let fg = if state.is_active {
        color_to_egui(fg_tg)
    } else {
        color_to_egui(fg_tg).gamma_multiply(0.75)
    };

    ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        &state.text,
        FontId::proportional(14.0),
        fg,
    );

    if state.is_progress_visible {
        let colors = TgThemeColors(palette);
        let mut rad_st = InfiniteRadialAnimationStyle::box_loading(&colors);
        rad_st.color = fg;
        let spin_rect = Rect::from_center_size(
            Pos2::new(rect.max.x - 22.0, rect.center().y),
            Vec2::splat(18.0),
        );
        InfiniteRadialAnimation::new(rad_st).paint(ui.ctx(), ui.painter(), spin_rect);
    }

    resp
}
