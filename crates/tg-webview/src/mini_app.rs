//! Mini App panel state machine and command router.
//!
//! Port of the UI-framework-independent panel logic from
//! `Telegram/SourceFiles/ui/chat/attach/attach_bot_webview.cpp`:
//! - Main & secondary button state, visibility rules, and layout position
//! - Back button, settings button, closing behavior confirmation, fullscreen state
//! - Header / background / bottom-bar color overrides
//! - Clipboard-read (`10s`) and external-link (`3s` one-shot) interaction gates
//! - Automatic desktop-unsupported event replies (sensors, location, biometry, secure storage,
//!   home screen)
//! - Origin enforcement (`same_origin`) and action dispatch for the hosting UI layer (`tg-app`).

use std::time::{Duration, Instant};

use serde_json::Value;
use tg_theme::{Color, Palette};

use crate::bridge::{
    AccessRequestStatus, BiometryInfo, ColorChangeParams, FileDownloadParams, FileDownloadStatus,
    HomeScreenStatus, HostEvent, InlineChatType, InvokeCustomMethodParams, MainButtonParams,
    MiniAppEvent, OpenLinkParams, OpenPopupParams, OpenTgLinkParams, PhoneRequestStatus,
    PreparedMessageParams, RequestChatParams, SafeAreaInsets, SecondaryButtonParams,
    SecondaryButtonPosition, SetEmojiStatusParams, ShareToStoryParams, ThemeChangedPayload,
    origins_match, parse_hex_color, parse_mini_app_event,
};
use crate::dialog::{PopupArgs, PopupButton, PopupButtonType};
use crate::error::Result;
use crate::interface::WebMessage;

/// Duration after user interaction during which clipboard reads are permitted
/// (`kClipboardReadTimeout` = 10s).
pub const CLIPBOARD_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Duration after user interaction during which `web_app_open_link` opens without confirmation
/// (`kOpenLinkTimeout` = 3s, one-shot per interaction).
pub const OPEN_LINK_TIMEOUT: Duration = Duration::from_secs(3);

/// Resolved UI state of a Mini App main or secondary bottom action button.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MiniAppButtonState {
    /// Whether the button is visible (`is_visible && (!text.trim().is_empty() || icon != 0)`).
    pub is_visible: bool,
    /// Whether clicks are enabled.
    pub is_active: bool,
    /// Whether the inline loading spinner is active.
    pub is_progress_visible: bool,
    /// Whether the shine animation is enabled.
    pub has_shine_effect: bool,
    /// Optional custom emoji document ID (`0` when absent).
    pub icon_custom_emoji_id: u64,
    /// Display text.
    pub text: String,
    /// Resolved background color (`None` = use theme `windowBgActive`).
    pub color: Option<Color>,
    /// Resolved foreground text color (`None` = use theme `windowFgActive`).
    pub text_color: Option<Color>,
}

impl MiniAppButtonState {
    fn apply(&mut self, params: &MainButtonParams) {
        let icon_id = params
            .icon_custom_emoji_id
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let visible = params.is_visible && (!params.text.trim().is_empty() || icon_id != 0);
        self.is_visible = visible;
        self.is_active = params.is_active;
        self.is_progress_visible = params.is_progress_visible;
        self.has_shine_effect = params.has_shine_effect;
        self.icon_custom_emoji_id = icon_id;
        self.text = params.text.clone();
        self.color = params.color.as_deref().and_then(parse_hex_color);
        self.text_color = params.text_color.as_deref().and_then(parse_hex_color);
    }
}

/// Configuration for [`MiniAppController`].
#[derive(Debug, Clone, Default)]
pub struct MiniAppPanelConfig {
    /// Initial URL loaded into the Mini App webview.
    pub initial_url: String,
    /// When `true`, bridge messages from origins other than `initial_url` are rejected.
    pub same_origin: bool,
    /// Whether this bot is allowed to call `web_app_read_text_from_clipboard`.
    pub allow_clipboard_read: bool,
    /// Initial fullscreen state.
    pub fullscreen: bool,
}

/// Current observable UI state of a Mini App panel (for `tg-ui` / `tg-app` rendering).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MiniAppPanelState {
    /// Whether the Mini App signaled `web_app_ready`.
    pub ready: bool,
    /// Whether the Mini App requested `web_app_expand`.
    pub expanded: bool,
    /// Primary bottom button state.
    pub main_button: MiniAppButtonState,
    /// Secondary bottom button state.
    pub secondary_button: MiniAppButtonState,
    /// Position of the secondary button relative to the main button.
    pub secondary_position: SecondaryButtonPosition,
    /// Header back button visibility.
    pub back_button_visible: bool,
    /// Panel menu settings item visibility.
    pub settings_button_visible: bool,
    /// Whether closing the panel should prompt for confirmation.
    pub close_needs_confirmation: bool,
    /// Whether vertical swipe-to-close is enabled.
    pub allow_vertical_swipe: bool,
    /// Whether screen orientation is locked.
    pub orientation_locked: bool,
    /// Current fullscreen mode.
    pub fullscreen: bool,
    /// Custom header color override (resolved from hex or palette key).
    pub header_color: Option<Color>,
    /// Custom body background color override.
    pub background_color: Option<Color>,
    /// Custom bottom bar color override.
    pub bottom_bar_color: Option<Color>,
    /// Whether closing has been requested.
    pub close_requested: bool,
}

/// Side-effect action emitted by [`MiniAppController`] for the host application to execute.
#[derive(Debug, Clone, PartialEq)]
pub enum MiniAppAction {
    /// Send a typed [`HostEvent`] back into the webview.
    PostEvent(HostEvent),
    /// Send a dynamic `viewport_changed` event (evaluating `window.innerHeight` in JS).
    SendDynamicViewport,
    /// Close the Mini App panel immediately.
    ClosePanel,
    /// Prompt user before closing because `close_needs_confirmation` is enabled.
    ConfirmClosePanel,
    /// Forward `web_app_data_send` bytes to the bot API.
    SendData(String),
    /// Switch to inline query mode with `(chat_types, query)`.
    SwitchInlineQuery {
        /// Allowed target chat types.
        chat_types: Vec<InlineChatType>,
        /// Query string.
        query: String,
    },
    /// Open a local Telegram link (`https://t.me<path_full>`).
    OpenTgLink {
        /// Full `https://t.me...` URL.
        url: String,
        /// Whether to force request.
        force_request: bool,
    },
    /// Open an external link (`needs_confirmation` is `false` when within [`OPEN_LINK_TIMEOUT`]).
    OpenExternalLink {
        /// Link parameters.
        params: OpenLinkParams,
        /// Whether the host should show a confirmation popup before opening.
        needs_confirmation: bool,
    },
    /// Open a Telegram payment invoice by slug.
    OpenInvoice {
        /// Invoice slug.
        slug: String,
    },
    /// Show a modal popup (`web_app_open_popup`).
    ShowPopup(PopupArgs),
    /// Show the "QR scanning not supported on desktop" informational popup.
    ShowScanQrUnsupported,
    /// Read clipboard text and reply with `clipboard_text_received` (`allowed` indicates gate).
    ReadClipboard {
        /// Echoed request ID.
        req_id: Value,
        /// `true` if clipboard permission + recent user interaction passed.
        allowed: bool,
    },
    /// Prompt/check write access for this bot.
    RequestWriteAccess,
    /// Prompt to share the user's phone number with this bot.
    RequestPhone,
    /// Invoke a Bot WebView custom method (e.g. CloudStorage).
    InvokeCustomMethod(InvokeCustomMethodParams),
    /// Share media to Telegram Stories.
    ShareToStory(ShareToStoryParams),
    /// Send a prepared inline message.
    SendPreparedMessage(PreparedMessageParams),
    /// Open chat selector for `web_app_request_chat`.
    RequestChat(RequestChatParams),
    /// Prompt to set custom emoji status.
    SetEmojiStatus {
        /// Parsed custom emoji ID.
        custom_emoji_id: u64,
        /// Expiration duration in seconds (`0` for indefinite).
        duration_secs: i32,
    },
    /// Prompt for emoji status access.
    RequestEmojiStatusAccess,
    /// Prompt to download a file.
    DownloadFile(FileDownloadParams),
    /// Save or delete a key in per-bot local device storage.
    DeviceStorageSave {
        /// Request ID.
        req_id: Value,
        /// Storage key.
        key: String,
        /// `Some(value)` to save, `None` to remove.
        value: Option<String>,
    },
    /// Read a key from per-bot local device storage.
    DeviceStorageGet {
        /// Request ID.
        req_id: Value,
        /// Storage key.
        key: String,
    },
    /// Clear all keys in per-bot local device storage.
    DeviceStorageClear {
        /// Request ID.
        req_id: Value,
    },
    /// Report verified age (`0` if verification did not pass).
    VerifyAge(i32),
    /// Share HTML5 game score.
    ShareGameScore,
}

/// Stateful controller implementing tdesktop's `Ui::BotWebView::Panel` bridge logic.
#[derive(Debug, Clone)]
pub struct MiniAppController {
    config: MiniAppPanelConfig,
    state: MiniAppPanelState,
    header_color_spec: Option<ColorChangeParams>,
    background_color_spec: Option<ColorChangeParams>,
    bottom_bar_color_spec: Option<ColorChangeParams>,
    last_user_interaction: Option<Instant>,
    open_link_interaction: Option<Instant>,
}

impl MiniAppController {
    /// Creates a new Mini App controller from `config`.
    #[must_use]
    pub fn new(config: MiniAppPanelConfig) -> Self {
        let fullscreen = config.fullscreen;
        Self {
            config,
            state: MiniAppPanelState {
                allow_vertical_swipe: true,
                fullscreen,
                ..MiniAppPanelState::default()
            },
            header_color_spec: None,
            background_color_spec: None,
            bottom_bar_color_spec: None,
            last_user_interaction: None,
            open_link_interaction: None,
        }
    }

    /// Returns the current panel UI state.
    #[must_use]
    pub fn state(&self) -> &MiniAppPanelState {
        &self.state
    }

    /// Returns the panel configuration.
    #[must_use]
    pub fn config(&self) -> &MiniAppPanelConfig {
        &self.config
    }

    /// Records a user mouse click or key press inside the webview or panel chrome.
    pub fn record_user_interaction(&mut self, now: Instant) {
        self.last_user_interaction = Some(now);
    }

    /// Returns `true` if `web_app_open_link` can open immediately without confirmation
    /// (consumes the current interaction timestamp, matching `Panel::allowOpenLink`).
    pub fn consume_open_link_interaction(&mut self, now: Instant) -> bool {
        let Some(last) = self.last_user_interaction else {
            return false;
        };
        if self.open_link_interaction == Some(last) {
            return false;
        }
        if now.saturating_duration_since(last) > OPEN_LINK_TIMEOUT {
            return false;
        }
        self.open_link_interaction = Some(last);
        true
    }

    /// Returns `true` if `web_app_read_text_from_clipboard` is currently authorized.
    #[must_use]
    pub fn allow_clipboard_query(&self, now: Instant) -> bool {
        if !self.config.allow_clipboard_read {
            return false;
        }
        self.last_user_interaction
            .is_some_and(|last| now.saturating_duration_since(last) <= CLIPBOARD_READ_TIMEOUT)
    }

    /// Updates theme colors from a new [`Palette`] and returns the `theme_changed` action.
    pub fn on_palette_changed(&mut self, palette: &Palette) -> MiniAppAction {
        if let Some(spec) = &self.header_color_spec {
            self.state.header_color = spec.resolve(palette);
        }
        if let Some(spec) = &self.background_color_spec {
            self.state.background_color = spec.resolve(palette);
        }
        if let Some(spec) = &self.bottom_bar_color_spec {
            self.state.bottom_bar_color = spec.resolve(palette);
        }
        MiniAppAction::PostEvent(HostEvent::ThemeChanged(ThemeChangedPayload::from_palette(
            palette,
        )))
    }

    /// Handles a user clicking the window close button.
    pub fn on_window_close_requested(&mut self) -> MiniAppAction {
        if self.state.close_needs_confirmation {
            MiniAppAction::ConfirmClosePanel
        } else {
            self.state.close_requested = true;
            MiniAppAction::ClosePanel
        }
    }

    /// Handles a user clicking the main bottom button.
    pub fn on_main_button_clicked(&mut self, now: Instant) -> Option<MiniAppAction> {
        if self.state.main_button.is_visible && self.state.main_button.is_active {
            self.record_user_interaction(now);
            Some(MiniAppAction::PostEvent(HostEvent::MainButtonPressed))
        } else {
            None
        }
    }

    /// Handles a user clicking the secondary bottom button.
    pub fn on_secondary_button_clicked(&mut self, now: Instant) -> Option<MiniAppAction> {
        if self.state.secondary_button.is_visible && self.state.secondary_button.is_active {
            self.record_user_interaction(now);
            Some(MiniAppAction::PostEvent(HostEvent::SecondaryButtonPressed))
        } else {
            None
        }
    }

    /// Handles a user clicking the header back button.
    pub fn on_back_button_clicked(&mut self, now: Instant) -> MiniAppAction {
        self.record_user_interaction(now);
        MiniAppAction::PostEvent(HostEvent::BackButtonPressed)
    }

    /// Handles a user clicking Settings in the panel menu.
    pub fn on_settings_button_clicked(&mut self, now: Instant) -> MiniAppAction {
        self.record_user_interaction(now);
        MiniAppAction::PostEvent(HostEvent::SettingsButtonPressed)
    }

    /// Parses and handles a raw [`WebMessage`] received from the WebView2 bridge.
    pub fn handle_web_message(
        &mut self,
        message: &WebMessage,
        palette: &Palette,
        now: Instant,
    ) -> Result<Vec<MiniAppAction>> {
        if self.state.close_requested {
            return Ok(Vec::new());
        }
        let (native, event) = parse_mini_app_event(&message.text, &message.source_url)?;
        if self.config.same_origin
            && !self.config.initial_url.is_empty()
            && !origins_match(&native.origin, &self.config.initial_url)
        {
            return Err(crate::error::WebViewError::InvalidBridgeMessage(format!(
                "origin mismatch for '{}': '{}' != '{}'",
                native.command, native.origin, self.config.initial_url
            )));
        }
        Ok(self.handle_event(event, palette, now))
    }

    /// Handles an already-parsed [`MiniAppEvent`], updating internal panel state and returning
    /// any actions for the host to perform.
    pub fn handle_event(
        &mut self,
        event: MiniAppEvent,
        palette: &Palette,
        now: Instant,
    ) -> Vec<MiniAppAction> {
        if self.state.close_requested {
            return Vec::new();
        }
        match event {
            MiniAppEvent::WebAppReady => {
                self.state.ready = true;
                Vec::new()
            }
            MiniAppEvent::WebAppExpand => {
                self.state.expanded = true;
                vec![MiniAppAction::SendDynamicViewport]
            }
            MiniAppEvent::WebAppClose(_) => {
                self.state.close_requested = true;
                vec![MiniAppAction::ClosePanel]
            }
            MiniAppEvent::WebAppDataSend(p) => {
                if p.data.is_empty() {
                    self.state.close_requested = true;
                    vec![MiniAppAction::ClosePanel]
                } else {
                    vec![MiniAppAction::SendData(p.data)]
                }
            }
            MiniAppEvent::WebAppSwitchInlineQuery(p) => vec![MiniAppAction::SwitchInlineQuery {
                chat_types: p.chat_types,
                query: p.query,
            }],
            MiniAppEvent::WebAppSetupMainButton(params) => {
                let was_visible = self.state.main_button.is_visible;
                self.state.main_button.apply(&params);
                if self.state.main_button.is_visible != was_visible {
                    vec![MiniAppAction::SendDynamicViewport]
                } else {
                    Vec::new()
                }
            }
            MiniAppEvent::WebAppSetupSecondaryButton(SecondaryButtonParams {
                button,
                position,
            }) => {
                let was_visible = self.state.secondary_button.is_visible;
                self.state.secondary_button.apply(&button);
                self.state.secondary_position = position;
                if self.state.secondary_button.is_visible != was_visible {
                    vec![MiniAppAction::SendDynamicViewport]
                } else {
                    Vec::new()
                }
            }
            MiniAppEvent::WebAppSetupBackButton(v) => {
                self.state.back_button_visible = v.is_visible;
                Vec::new()
            }
            MiniAppEvent::WebAppSetupSettingsButton(v) => {
                self.state.settings_button_visible = v.is_visible;
                Vec::new()
            }
            MiniAppEvent::WebAppRequestTheme => vec![MiniAppAction::PostEvent(
                HostEvent::ThemeChanged(ThemeChangedPayload::from_palette(palette)),
            )],
            MiniAppEvent::WebAppRequestViewport => vec![MiniAppAction::SendDynamicViewport],
            MiniAppEvent::WebAppRequestSafeArea => vec![MiniAppAction::PostEvent(
                HostEvent::SafeAreaChanged(SafeAreaInsets::default()),
            )],
            MiniAppEvent::WebAppRequestContentSafeArea => vec![MiniAppAction::PostEvent(
                HostEvent::ContentSafeAreaChanged(SafeAreaInsets::default()),
            )],
            MiniAppEvent::WebAppRequestFullscreen => {
                self.state.fullscreen = true;
                vec![
                    MiniAppAction::PostEvent(HostEvent::FullscreenChanged {
                        is_fullscreen: true,
                    }),
                    MiniAppAction::PostEvent(HostEvent::SafeAreaChanged(SafeAreaInsets::default())),
                    MiniAppAction::PostEvent(HostEvent::ContentSafeAreaChanged(
                        SafeAreaInsets::default(),
                    )),
                ]
            }
            MiniAppEvent::WebAppExitFullscreen => {
                self.state.fullscreen = false;
                vec![
                    MiniAppAction::PostEvent(HostEvent::FullscreenChanged {
                        is_fullscreen: false,
                    }),
                    MiniAppAction::PostEvent(HostEvent::SafeAreaChanged(SafeAreaInsets::default())),
                    MiniAppAction::PostEvent(HostEvent::ContentSafeAreaChanged(
                        SafeAreaInsets::default(),
                    )),
                ]
            }
            MiniAppEvent::WebAppSetHeaderColor(spec) => {
                self.state.header_color = spec.resolve(palette);
                self.header_color_spec = Some(spec);
                Vec::new()
            }
            MiniAppEvent::WebAppSetBackgroundColor(spec) => {
                self.state.background_color = spec.resolve(palette);
                self.background_color_spec = Some(spec);
                Vec::new()
            }
            MiniAppEvent::WebAppSetBottomBarColor(spec) => {
                self.state.bottom_bar_color = spec.resolve(palette);
                self.bottom_bar_color_spec = Some(spec);
                Vec::new()
            }
            MiniAppEvent::WebAppTriggerHapticFeedback(_) => Vec::new(),
            MiniAppEvent::WebAppOpenLink(params) => {
                let needs_confirmation = !self.consume_open_link_interaction(now);
                vec![MiniAppAction::OpenExternalLink {
                    params,
                    needs_confirmation,
                }]
            }
            MiniAppEvent::WebAppOpenTgLink(OpenTgLinkParams {
                path_full,
                force_request,
            }) => {
                if path_full.is_empty() {
                    self.state.close_requested = true;
                    vec![MiniAppAction::ClosePanel]
                } else {
                    vec![MiniAppAction::OpenTgLink {
                        url: format!("https://t.me{path_full}"),
                        force_request,
                    }]
                }
            }
            MiniAppEvent::WebAppOpenInvoice(p) => {
                if p.slug.is_empty() {
                    self.state.close_requested = true;
                    vec![MiniAppAction::ClosePanel]
                } else {
                    vec![MiniAppAction::OpenInvoice { slug: p.slug }]
                }
            }
            MiniAppEvent::WebAppOpenPopup(OpenPopupParams {
                title,
                message,
                buttons,
            }) => vec![MiniAppAction::ShowPopup(PopupArgs {
                title,
                text: message,
                value: None,
                buttons: buttons.iter().map(PopupButton::from).collect(),
                ignore_flood_check: false,
            })],
            MiniAppEvent::WebAppOpenScanQrPopup(_) => vec![MiniAppAction::ShowScanQrUnsupported],
            MiniAppEvent::WebAppCloseScanQrPopup => Vec::new(),
            MiniAppEvent::WebAppReadTextFromClipboard(p) => {
                let allowed = self.allow_clipboard_query(now);
                if allowed {
                    vec![MiniAppAction::ReadClipboard {
                        req_id: p.req_id,
                        allowed: true,
                    }]
                } else {
                    vec![MiniAppAction::PostEvent(HostEvent::ClipboardTextReceived {
                        req_id: p.req_id,
                        data: None,
                    })]
                }
            }
            MiniAppEvent::WebAppRequestWriteAccess => vec![MiniAppAction::RequestWriteAccess],
            MiniAppEvent::WebAppRequestPhone => vec![MiniAppAction::RequestPhone],
            MiniAppEvent::WebAppInvokeCustomMethod(p) => {
                vec![MiniAppAction::InvokeCustomMethod(p)]
            }
            MiniAppEvent::WebAppSetupClosingBehavior(p) => {
                self.state.close_needs_confirmation = p.need_confirmation;
                Vec::new()
            }
            MiniAppEvent::WebAppSetupSwipeBehavior(p) => {
                self.state.allow_vertical_swipe = p.allow_vertical_swipe;
                Vec::new()
            }
            MiniAppEvent::WebAppToggleOrientationLock(p) => {
                self.state.orientation_locked = p.locked;
                Vec::new()
            }
            MiniAppEvent::WebAppShareToStory(p) => vec![MiniAppAction::ShareToStory(p)],
            MiniAppEvent::WebAppSendPreparedMessage(p) => {
                vec![MiniAppAction::SendPreparedMessage(p)]
            }
            MiniAppEvent::WebAppRequestChat(p) => {
                if p.req_id.is_empty() {
                    Vec::new()
                } else {
                    vec![MiniAppAction::RequestChat(p)]
                }
            }
            MiniAppEvent::WebAppSetEmojiStatus(SetEmojiStatusParams {
                custom_emoji_id,
                duration,
            }) => {
                let id = custom_emoji_id.parse::<u64>().unwrap_or(0);
                let dur = duration.map(|d| d.round() as i64).unwrap_or(0);
                if id == 0 {
                    vec![MiniAppAction::PostEvent(HostEvent::EmojiStatusFailed {
                        error: "SUGGESTED_EMOJI_INVALID".to_owned(),
                    })]
                } else if dur < 0 || dur > i64::from(i32::MAX) {
                    vec![MiniAppAction::PostEvent(HostEvent::EmojiStatusFailed {
                        error: "DURATION_INVALID".to_owned(),
                    })]
                } else {
                    vec![MiniAppAction::SetEmojiStatus {
                        custom_emoji_id: id,
                        duration_secs: dur as i32,
                    }]
                }
            }
            MiniAppEvent::WebAppRequestEmojiStatusAccess => {
                vec![MiniAppAction::RequestEmojiStatusAccess]
            }
            MiniAppEvent::WebAppRequestFileDownload(p) => {
                if p.url.is_empty() || p.file_name.is_empty() {
                    self.state.close_requested = true;
                    vec![MiniAppAction::ClosePanel]
                } else {
                    vec![MiniAppAction::DownloadFile(p)]
                }
            }
            MiniAppEvent::WebAppCheckHomeScreen => {
                vec![MiniAppAction::PostEvent(HostEvent::HomeScreenChecked {
                    status: HomeScreenStatus::Unsupported,
                })]
            }
            MiniAppEvent::WebAppStartAccelerometer(_) => {
                vec![MiniAppAction::PostEvent(HostEvent::AccelerometerFailed {
                    error: "UNSUPPORTED".to_owned(),
                })]
            }
            MiniAppEvent::WebAppStartDeviceOrientation(_) => vec![MiniAppAction::PostEvent(
                HostEvent::DeviceOrientationFailed {
                    error: "UNSUPPORTED".to_owned(),
                },
            )],
            MiniAppEvent::WebAppStartGyroscope(_) => {
                vec![MiniAppAction::PostEvent(HostEvent::GyroscopeFailed {
                    error: "UNSUPPORTED".to_owned(),
                })]
            }
            MiniAppEvent::WebAppCheckLocation => {
                vec![MiniAppAction::PostEvent(HostEvent::LocationChecked {
                    available: false,
                })]
            }
            MiniAppEvent::WebAppRequestLocation => {
                vec![MiniAppAction::PostEvent(HostEvent::LocationRequested {
                    available: false,
                })]
            }
            MiniAppEvent::WebAppBiometryGetInfo => vec![MiniAppAction::PostEvent(
                HostEvent::BiometryInfoReceived(BiometryInfo {
                    available: false,
                    ..BiometryInfo::default()
                }),
            )],
            MiniAppEvent::WebAppBiometryRequestAccess(_)
            | MiniAppEvent::WebAppBiometryRequestAuth(_) => {
                vec![MiniAppAction::PostEvent(HostEvent::BiometryAuthRequested {
                    status: "failed".to_owned(),
                    token: None,
                })]
            }
            MiniAppEvent::WebAppBiometryUpdateToken(_) => {
                vec![MiniAppAction::PostEvent(HostEvent::BiometryTokenUpdated {
                    status: "failed".to_owned(),
                })]
            }
            MiniAppEvent::WebAppDeviceStorageSaveKey(p) => match p.key.as_str() {
                None => vec![MiniAppAction::PostEvent(HostEvent::DeviceStorageFailed {
                    req_id: p.req_id,
                    error: "KEY_INVALID".to_owned(),
                })],
                Some(key) => match p.value {
                    Value::Null => vec![MiniAppAction::DeviceStorageSave {
                        req_id: p.req_id,
                        key: key.to_owned(),
                        value: None,
                    }],
                    Value::String(val) => vec![MiniAppAction::DeviceStorageSave {
                        req_id: p.req_id,
                        key: key.to_owned(),
                        value: Some(val),
                    }],
                    _ => vec![MiniAppAction::PostEvent(HostEvent::DeviceStorageFailed {
                        req_id: p.req_id,
                        error: "VALUE_INVALID".to_owned(),
                    })],
                },
            },
            MiniAppEvent::WebAppDeviceStorageGetKey(p) => match p.key.as_str() {
                None => vec![MiniAppAction::PostEvent(HostEvent::DeviceStorageFailed {
                    req_id: p.req_id,
                    error: "KEY_INVALID".to_owned(),
                })],
                Some(key) => vec![MiniAppAction::DeviceStorageGet {
                    req_id: p.req_id,
                    key: key.to_owned(),
                }],
            },
            MiniAppEvent::WebAppDeviceStorageClear(p) => {
                vec![MiniAppAction::DeviceStorageClear { req_id: p.req_id }]
            }
            MiniAppEvent::WebAppSecureStorageSaveKey(p) => {
                vec![MiniAppAction::PostEvent(HostEvent::SecureStorageFailed {
                    req_id: p.req_id,
                    error: "UNSUPPORTED".to_owned(),
                })]
            }
            MiniAppEvent::WebAppSecureStorageGetKey(p)
            | MiniAppEvent::WebAppSecureStorageRestoreKey(p) => {
                vec![MiniAppAction::PostEvent(HostEvent::SecureStorageFailed {
                    req_id: p.req_id,
                    error: "UNSUPPORTED".to_owned(),
                })]
            }
            MiniAppEvent::WebAppSecureStorageClear(p) => {
                vec![MiniAppAction::PostEvent(HostEvent::SecureStorageFailed {
                    req_id: p.req_id,
                    error: "UNSUPPORTED".to_owned(),
                })]
            }
            MiniAppEvent::WebAppVerifyAge(p) => {
                let age = if p.passed {
                    p.age.map(|a| a.floor() as i32).unwrap_or(0)
                } else {
                    0
                };
                vec![MiniAppAction::VerifyAge(age)]
            }
            MiniAppEvent::ShareScore => vec![MiniAppAction::ShareGameScore],
            MiniAppEvent::WebAppAddToHomeScreen
            | MiniAppEvent::WebAppStopAccelerometer
            | MiniAppEvent::WebAppStopDeviceOrientation
            | MiniAppEvent::WebAppStopGyroscope
            | MiniAppEvent::WebAppOpenLocationSettings
            | MiniAppEvent::WebAppBiometryOpenSettings
            | MiniAppEvent::IframeReady(_)
            | MiniAppEvent::IframeWillReload
            | MiniAppEvent::Unknown { .. } => Vec::new(),
        }
    }

    /// Helper creating the default unsupported-QR popup args matching `openScanQrPopup`.
    #[must_use]
    pub fn scan_qr_unsupported_popup() -> PopupArgs {
        PopupArgs {
            title: String::new(),
            text: "QR code scanning is not supported on desktop.".to_owned(),
            value: None,
            buttons: vec![PopupButton {
                id: "ok".to_owned(),
                text: "OK".to_owned(),
                button_type: PopupButtonType::Ok,
            }],
            ignore_flood_check: false,
        }
    }

    /// Helper creating a write-access reply event (`write_access_requested`).
    #[must_use]
    pub fn write_access_reply(allowed: bool) -> HostEvent {
        HostEvent::WriteAccessRequested {
            status: if allowed {
                AccessRequestStatus::Allowed
            } else {
                AccessRequestStatus::Cancelled
            },
        }
    }

    /// Helper creating an emoji-status-access reply event (`emoji_status_access_requested`).
    ///
    /// Port of `Panel::processEmojiStatusAccessRequest` (`attach_bot_webview.cpp:2851-2858`).
    #[must_use]
    pub fn emoji_status_access_reply(allowed: bool) -> HostEvent {
        HostEvent::EmojiStatusAccessRequested {
            status: if allowed {
                AccessRequestStatus::Allowed
            } else {
                AccessRequestStatus::Cancelled
            },
        }
    }

    /// Helper creating a file-download reply event (`file_download_requested`).
    ///
    /// Port of `Panel::processDownloadFile` (`attach_bot_webview.cpp:3560-3564`).
    #[must_use]
    pub fn file_download_reply(downloading: bool) -> HostEvent {
        HostEvent::FileDownloadRequested {
            status: if downloading {
                FileDownloadStatus::Downloading
            } else {
                FileDownloadStatus::Cancelled
            },
        }
    }

    /// Helper creating a `requested_chat_sent` or `requested_chat_failed` reply event.
    ///
    /// Port of `Panel::processRequestChat` (`attach_bot_webview.cpp:2798-2809`).
    #[must_use]
    pub fn requested_chat_reply(req_id: impl Into<String>, error: Option<String>) -> HostEvent {
        let req_id = req_id.into();
        match error.filter(|e| !e.is_empty()) {
            None => HostEvent::RequestedChatSent { req_id },
            Some(error) => HostEvent::RequestedChatFailed { req_id, error },
        }
    }

    /// Helper creating a `prepared_message_sent` or `prepared_message_failed` reply event.
    ///
    /// Port of `Panel::processSendPreparedMessage` (`attach_bot_webview.cpp:2774-2782`).
    #[must_use]
    pub fn prepared_message_reply(error: Option<String>) -> HostEvent {
        match error.filter(|e| !e.is_empty()) {
            None => HostEvent::PreparedMessageSent,
            Some(error) => HostEvent::PreparedMessageFailed { error },
        }
    }

    /// Helper creating a `phone_requested` reply event (`attach_bot_webview.cpp:3146-3155`).
    #[must_use]
    pub fn phone_reply(sent: bool) -> HostEvent {
        HostEvent::PhoneRequested {
            status: if sent {
                PhoneRequestStatus::Sent
            } else {
                PhoneRequestStatus::Cancelled
            },
        }
    }
}
