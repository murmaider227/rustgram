//! Telegram Mini App (`window.Telegram.WebView` / `window.TelegramWebviewProxy`) bridge protocol.
//!
//! Port of the Mini App message parsing, event types, payload validation, color helpers, and
//! host-to-page event dispatch in
//! `Telegram/SourceFiles/ui/chat/attach/attach_bot_webview.cpp`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tg_theme::{Color, Palette, WebViewThemeParams, web_view_params};
use url::Url;

use crate::dialog::{PopupButton, PopupButtonType};
use crate::error::{Result, WebViewError};

/// Maximum incoming native bridge payload size in bytes (`kMaxNativeMessageBytes` = 64 MiB).
pub const MAX_NATIVE_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// External shell envelope marker (`kExternalMessageType`).
pub const EXTERNAL_MESSAGE_TYPE: &str = "tdesktop_external_bot_webapp";

/// Lightness threshold for computing button ripple highlight/darken direction (`kLightnessThreshold`).
const LIGHTNESS_THRESHOLD: i32 = 128;

/// HSV value delta for button ripple color (`kLightnessDelta`).
const LIGHTNESS_DELTA: i32 = 32;

/// Parsed raw bridge message before typed event decoding.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeMessage {
    /// Normalized sender origin (`scheme://host:port`), if available.
    pub origin: String,
    /// Event command name (e.g. `"web_app_ready"`, `"web_app_setup_main_button"`).
    pub command: String,
    /// Parsed JSON object payload (empty object if the event carries no arguments).
    pub arguments: Map<String, Value>,
}

/// Named palette color keys accepted by `web_app_set_header_color`, `web_app_set_background_color`,
/// and `web_app_set_bottom_bar_color`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedColorKey {
    /// Primary background (`windowBg`).
    BgColor,
    /// Secondary background (`boxDividerBg`).
    SecondaryBgColor,
    /// Bottom bar background (`windowBg`).
    BottomBarBgColor,
}

impl NamedColorKey {
    /// Resolves this named color key against a [`Palette`].
    #[must_use]
    pub fn resolve(self, palette: &Palette) -> Color {
        match self {
            Self::BgColor | Self::BottomBarBgColor => palette.window_bg,
            Self::SecondaryBgColor => palette.box_divider_bg,
        }
    }
}

/// Color specification sent by Mini App header/background/bottom-bar color commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ColorSpec {
    /// Explicit `#RRGGBB` hex color.
    Hex(String),
    /// Named theme palette key.
    Named(NamedColorKey),
}

/// Payload for `web_app_set_header_color`, `web_app_set_background_color`, and
/// `web_app_set_bottom_bar_color`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorChangeParams {
    /// Optional explicit `#RRGGBB` color string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Optional palette key (`bg_color`, `secondary_bg_color`, `bottom_bar_bg_color`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_key: Option<NamedColorKey>,
}

impl ColorChangeParams {
    /// Resolves the effective color against `palette`, following `attach_bot_webview.cpp`:
    /// explicit `#RRGGBB` hex wins over `color_key`; invalid/missing returns `None` (reset to theme).
    #[must_use]
    pub fn resolve(&self, palette: &Palette) -> Option<Color> {
        if let Some(hex) = self.color.as_deref()
            && let Some(c) = parse_hex_color(hex)
        {
            return Some(c);
        }
        self.color_key.map(|k| k.resolve(palette))
    }
}

/// Position of the secondary bottom button relative to the main button (`ParsePosition`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecondaryButtonPosition {
    /// To the left of the main button (default).
    #[default]
    Left,
    /// Above the main button.
    Top,
    /// To the right of the main button.
    Right,
    /// Below the main button.
    Bottom,
}

/// Payload for `web_app_setup_main_button`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MainButtonParams {
    /// Whether the button should be visible (only shown if text or custom emoji is non-empty).
    #[serde(default)]
    pub is_visible: bool,
    /// Whether the button responds to clicks.
    #[serde(default)]
    pub is_active: bool,
    /// Button label text.
    #[serde(default)]
    pub text: String,
    /// Optional `#RRGGBB` button background color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Optional `#RRGGBB` button text color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
    /// Whether to show a loading spinner inside the button.
    #[serde(default)]
    pub is_progress_visible: bool,
    /// Whether to display a shine animation effect.
    #[serde(default)]
    pub has_shine_effect: bool,
    /// Optional custom emoji document ID as a decimal string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_custom_emoji_id: Option<String>,
}

/// Payload for `web_app_setup_secondary_button`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecondaryButtonParams {
    /// Shared button fields.
    #[serde(flatten)]
    pub button: MainButtonParams,
    /// Relative position next to or stacked with the main button.
    #[serde(default)]
    pub position: SecondaryButtonPosition,
}

/// Simple visibility toggle payload (`web_app_setup_back_button`, `web_app_setup_settings_button`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisibilityParams {
    /// Whether the UI element is visible.
    #[serde(default)]
    pub is_visible: bool,
}

/// Payload for `web_app_close`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseParams {
    /// Optional hint to return to the previous view rather than fully dismissing.
    #[serde(default)]
    pub return_back: bool,
}

/// Payload for `web_app_data_send`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataSendParams {
    /// Arbitrary UTF-8 payload string (up to 4096 bytes per Telegram Bot API).
    #[serde(default)]
    pub data: String,
}

/// Allowed inline query target chat types (`switchInlineQueryMessage` in `attach_bot_webview.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InlineChatType {
    /// Private user chats.
    Users,
    /// Bot chats.
    Bots,
    /// Group and supergroup chats.
    Groups,
    /// Broadcast channels.
    Channels,
}

/// Payload for `web_app_switch_inline_query`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchInlineQueryParams {
    /// Inline query text to insert.
    pub query: String,
    /// Optional filter of target chat types.
    #[serde(default)]
    pub chat_types: Vec<InlineChatType>,
}

/// Payload for `web_app_open_link`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenLinkParams {
    /// External HTTP(S) URL to open.
    pub url: String,
    /// Whether to attempt opening inside Instant View.
    #[serde(default)]
    pub try_instant_view: bool,
    /// Optional preferred browser name hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub try_browser: Option<String>,
}

/// Payload for `web_app_open_tg_link`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTgLinkParams {
    /// Path relative to `https://t.me` (e.g. `"/telegram"` or `"/+inviteHash"`).
    pub path_full: String,
    /// Optional force-request flag.
    #[serde(default)]
    pub force_request: bool,
}

/// Payload for `web_app_open_invoice`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenInvoiceParams {
    /// Invoice slug to open in the Telegram payment flow.
    pub slug: String,
}

/// Button type inside `web_app_open_popup`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiniAppPopupButtonType {
    /// Custom default button.
    #[default]
    Default,
    /// Standard "OK" button.
    Ok,
    /// Standard "Close" button.
    Close,
    /// Standard "Cancel" button.
    Cancel,
    /// Destructive red button.
    Destructive,
}

impl From<MiniAppPopupButtonType> for PopupButtonType {
    fn from(value: MiniAppPopupButtonType) -> Self {
        match value {
            MiniAppPopupButtonType::Default => Self::Default,
            MiniAppPopupButtonType::Ok => Self::Ok,
            MiniAppPopupButtonType::Close => Self::Close,
            MiniAppPopupButtonType::Cancel => Self::Cancel,
            MiniAppPopupButtonType::Destructive => Self::Destructive,
        }
    }
}

/// Single button inside `web_app_open_popup`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MiniAppPopupButton {
    /// Button identifier returned in `popup_closed`.
    #[serde(default)]
    pub id: String,
    /// Semantic button type.
    #[serde(rename = "type")]
    pub button_type: MiniAppPopupButtonType,
    /// Optional custom label text.
    #[serde(default)]
    pub text: String,
}

impl From<&MiniAppPopupButton> for PopupButton {
    fn from(btn: &MiniAppPopupButton) -> Self {
        Self {
            id: btn.id.clone(),
            text: btn.text.clone(),
            button_type: btn.button_type.into(),
        }
    }
}

/// Payload for `web_app_open_popup`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPopupParams {
    /// Optional popup title.
    #[serde(default)]
    pub title: String,
    /// Popup body message (must be non-empty).
    pub message: String,
    /// Popup action buttons (must be non-empty).
    #[serde(default)]
    pub buttons: Vec<MiniAppPopupButton>,
}

/// Payload for `web_app_open_scan_qr_popup`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenScanQrPopupParams {
    /// Optional prompt text displayed above the QR scanner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Impact style for `web_app_trigger_haptic_feedback`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactStyle {
    /// Light collision impact.
    Light,
    /// Medium collision impact.
    Medium,
    /// Heavy collision impact.
    Heavy,
    /// Rigid collision impact.
    Rigid,
    /// Soft collision impact.
    Soft,
}

/// Notification feedback type for `web_app_trigger_haptic_feedback`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HapticNotificationType {
    /// Error feedback.
    Error,
    /// Success feedback.
    Success,
    /// Warning feedback.
    Warning,
}

/// Payload for `web_app_trigger_haptic_feedback`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HapticFeedbackParams {
    /// Physical collision impact.
    Impact {
        /// Impact intensity style.
        impact_style: ImpactStyle,
    },
    /// Task outcome notification.
    Notification {
        /// Outcome kind.
        notification_type: HapticNotificationType,
    },
    /// UI selection change tick.
    SelectionChange,
}

/// Payload for `web_app_read_text_from_clipboard`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipboardReadParams {
    /// Request identifier echoed in `clipboard_text_received`.
    pub req_id: Value,
}

/// Payload for `web_app_setup_closing_behavior`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosingBehaviorParams {
    /// Whether closing the Mini App requires user confirmation.
    #[serde(default)]
    pub need_confirmation: bool,
}

/// Payload for `web_app_setup_swipe_behavior`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwipeBehaviorParams {
    /// Whether vertical swipes may minimize or close the Mini App.
    #[serde(default = "default_true")]
    pub allow_vertical_swipe: bool,
}

fn default_true() -> bool {
    true
}

/// Payload for `web_app_toggle_orientation_lock`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrientationLockParams {
    /// Whether orientation should be locked.
    #[serde(default)]
    pub locked: bool,
}

/// Payload for `web_app_invoke_custom_method`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvokeCustomMethodParams {
    /// Request identifier echoed in `custom_method_invoked`.
    pub req_id: Value,
    /// Custom method name (e.g. `"saveStorageValue"`, `"getStorageValues"`).
    #[serde(default)]
    pub method: String,
    /// Method parameters object.
    #[serde(default)]
    pub params: Value,
}

/// Widget link embedded in `web_app_share_to_story`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoryWidgetLink {
    /// HTTPS URL attached to the story widget.
    #[serde(default)]
    pub url: String,
    /// Optional display label for the widget link.
    #[serde(default)]
    pub name: String,
}

/// Payload for `web_app_share_to_story`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareToStoryParams {
    /// Media image/video URL.
    #[serde(default)]
    pub media_url: String,
    /// Optional caption text.
    #[serde(default)]
    pub text: String,
    /// Optional clickable story link widget (only `https://` URLs are retained by tdesktop).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget_link: Option<StoryWidgetLink>,
}

/// Payload for `web_app_send_prepared_message`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedMessageParams {
    /// Prepared inline message ID.
    #[serde(default)]
    pub id: String,
}

/// Payload for `web_app_request_chat`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestChatParams {
    /// Request identifier echoed in `requested_chat_sent` / `requested_chat_failed`.
    #[serde(default)]
    pub req_id: String,
}

/// Payload for `web_app_set_emoji_status`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SetEmojiStatusParams {
    /// Custom emoji document ID (decimal string).
    #[serde(default)]
    pub custom_emoji_id: String,
    /// Optional expiration duration in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
}

/// Payload for `web_app_request_file_download`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDownloadParams {
    /// File URL to download.
    #[serde(default)]
    pub url: String,
    /// Suggested file name.
    #[serde(default)]
    pub file_name: String,
}

/// Payload for sensor start commands (`web_app_start_accelerometer`, `web_app_start_gyroscope`,
/// `web_app_start_device_orientation`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensorStartParams {
    /// Requested sampling interval in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_rate: Option<u32>,
    /// Whether absolute orientation is requested (device orientation only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need_absolute: Option<bool>,
}

/// Payload for `web_app_biometry_request_access` / `web_app_biometry_request_auth`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BiometryReasonParams {
    /// Optional user-facing reason shown in biometric prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Payload for `web_app_biometry_update_token`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BiometryUpdateTokenParams {
    /// New token to store (`None` or empty clears the token).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Optional prompt reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Payload for `web_app_device_storage_save_key` and `web_app_secure_storage_save_key`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageSaveKeyParams {
    /// Request identifier echoed in response events.
    #[serde(default)]
    pub req_id: Value,
    /// Storage key (must be a JSON string; kept as `Value` to preserve `KEY_INVALID` validation).
    #[serde(default)]
    pub key: Value,
    /// Storage value (`Value::Null` deletes the key; non-string fails with `VALUE_INVALID`).
    #[serde(default)]
    pub value: Value,
}

/// Payload for `web_app_device_storage_get_key`, `web_app_secure_storage_get_key`,
/// and `web_app_secure_storage_restore_key`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageGetKeyParams {
    /// Request identifier echoed in response events.
    #[serde(default)]
    pub req_id: Value,
    /// Storage key.
    #[serde(default)]
    pub key: Value,
}

/// Payload for `web_app_device_storage_clear` and `web_app_secure_storage_clear`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageClearParams {
    /// Request identifier echoed in response events.
    #[serde(default)]
    pub req_id: Value,
}

/// Payload for `web_app_verify_age`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct VerifyAgeParams {
    /// Whether age verification succeeded.
    #[serde(default)]
    pub passed: bool,
    /// Detected age value (floored to integer when `passed` is `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age: Option<f64>,
}

/// Payload for `iframe_ready`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IframeReadyParams {
    /// Whether the Mini App supports `reload_iframe`.
    #[serde(default)]
    pub reload_supported: bool,
}

/// Typed incoming event sent from a Telegram Mini App to the Rustgram host.
///
/// Covers all events handled in `Telegram/SourceFiles/ui/chat/attach/attach_bot_webview.cpp`
/// plus standard Telegram WebApp SDK events (`web_app_ready`, `web_app_expand`,
/// `web_app_trigger_haptic_feedback`, `web_app_biometry_*`, etc.).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "eventType", content = "eventData", rename_all = "snake_case")]
pub enum MiniAppEvent {
    /// `web_app_ready`: Mini App finished initial rendering and is ready to display.
    WebAppReady,
    /// `web_app_expand`: Mini App requests expanding to full available height.
    WebAppExpand,
    /// `web_app_close`: Mini App requests closing its panel.
    WebAppClose(CloseParams),
    /// `web_app_data_send`: Mini App sends service data back to the bot.
    WebAppDataSend(DataSendParams),
    /// `web_app_switch_inline_query`: Mini App opens inline query in a chosen chat.
    WebAppSwitchInlineQuery(SwitchInlineQueryParams),
    /// `web_app_setup_main_button`: Configures the primary bottom action button.
    WebAppSetupMainButton(MainButtonParams),
    /// `web_app_setup_secondary_button`: Configures the secondary bottom action button.
    WebAppSetupSecondaryButton(SecondaryButtonParams),
    /// `web_app_setup_back_button`: Shows or hides the header back button.
    WebAppSetupBackButton(VisibilityParams),
    /// `web_app_setup_settings_button`: Shows or hides the settings entry in the panel menu.
    WebAppSetupSettingsButton(VisibilityParams),
    /// `web_app_request_theme`: Requests an immediate `theme_changed` event.
    WebAppRequestTheme,
    /// `web_app_request_viewport`: Requests an immediate `viewport_changed` event.
    WebAppRequestViewport,
    /// `web_app_request_safe_area`: Requests an immediate `safe_area_changed` event.
    WebAppRequestSafeArea,
    /// `web_app_request_content_safe_area`: Requests an immediate `content_safe_area_changed` event.
    WebAppRequestContentSafeArea,
    /// `web_app_request_fullscreen`: Requests entering fullscreen mode.
    WebAppRequestFullscreen,
    /// `web_app_exit_fullscreen`: Requests exiting fullscreen mode.
    WebAppExitFullscreen,
    /// `web_app_set_header_color`: Overrides panel title/header background color.
    WebAppSetHeaderColor(ColorChangeParams),
    /// `web_app_set_background_color`: Overrides panel body background color.
    WebAppSetBackgroundColor(ColorChangeParams),
    /// `web_app_set_bottom_bar_color`: Overrides bottom button bar background color.
    WebAppSetBottomBarColor(ColorChangeParams),
    /// `web_app_trigger_haptic_feedback`: Requests haptic vibration feedback (no-op on desktop).
    WebAppTriggerHapticFeedback(HapticFeedbackParams),
    /// `web_app_open_link`: Opens an external HTTP(S) URL (or Instant View).
    WebAppOpenLink(OpenLinkParams),
    /// `web_app_open_tg_link`: Opens an internal `t.me` / `tg://` link.
    WebAppOpenTgLink(OpenTgLinkParams),
    /// `web_app_open_invoice`: Opens a Telegram payment invoice by slug.
    WebAppOpenInvoice(OpenInvoiceParams),
    /// `web_app_open_popup`: Opens a native alert/confirmation popup with custom buttons.
    WebAppOpenPopup(OpenPopupParams),
    /// `web_app_open_scan_qr_popup`: Requests QR code scanner popup (shows unsupported notice on desktop).
    WebAppOpenScanQrPopup(OpenScanQrPopupParams),
    /// `web_app_close_scan_qr_popup`: Dismisses an active QR scanner popup.
    WebAppCloseScanQrPopup,
    /// `web_app_read_text_from_clipboard`: Requests clipboard text (gated by recent user interaction).
    WebAppReadTextFromClipboard(ClipboardReadParams),
    /// `web_app_request_write_access`: Requests permission for the bot to message the user.
    WebAppRequestWriteAccess,
    /// `web_app_request_phone`: Requests sharing the user's phone number with the bot.
    WebAppRequestPhone,
    /// `web_app_invoke_custom_method`: Invokes a Bot WebView custom method (e.g. CloudStorage).
    WebAppInvokeCustomMethod(InvokeCustomMethodParams),
    /// `web_app_setup_closing_behavior`: Enables or disables close confirmation dialog.
    WebAppSetupClosingBehavior(ClosingBehaviorParams),
    /// `web_app_setup_swipe_behavior`: Configures vertical swipe gesture behavior.
    WebAppSetupSwipeBehavior(SwipeBehaviorParams),
    /// `web_app_toggle_orientation_lock`: Locks or unlocks screen orientation.
    WebAppToggleOrientationLock(OrientationLockParams),
    /// `web_app_share_to_story`: Opens the story editor pre-filled with media/caption/widget.
    WebAppShareToStory(ShareToStoryParams),
    /// `web_app_send_prepared_message`: Sends a prepared inline message by ID.
    WebAppSendPreparedMessage(PreparedMessageParams),
    /// `web_app_request_chat`: Requests selecting a chat (`req_id`).
    WebAppRequestChat(RequestChatParams),
    /// `web_app_set_emoji_status`: Prompts the user to set a custom emoji status.
    WebAppSetEmojiStatus(SetEmojiStatusParams),
    /// `web_app_request_emoji_status_access`: Prompts for permission to update emoji status.
    WebAppRequestEmojiStatusAccess,
    /// `web_app_request_file_download`: Prompts to download a file from `url`.
    WebAppRequestFileDownload(FileDownloadParams),
    /// `web_app_check_home_screen`: Checks home screen shortcut status (unsupported on desktop).
    WebAppCheckHomeScreen,
    /// `web_app_add_to_home_screen`: Requests adding a home screen shortcut.
    WebAppAddToHomeScreen,
    /// `web_app_start_accelerometer`: Starts accelerometer sensor (unsupported on desktop).
    WebAppStartAccelerometer(SensorStartParams),
    /// `web_app_stop_accelerometer`: Stops accelerometer sensor.
    WebAppStopAccelerometer,
    /// `web_app_start_device_orientation`: Starts device orientation sensor (unsupported on desktop).
    WebAppStartDeviceOrientation(SensorStartParams),
    /// `web_app_stop_device_orientation`: Stops device orientation sensor.
    WebAppStopDeviceOrientation,
    /// `web_app_start_gyroscope`: Starts gyroscope sensor (unsupported on desktop).
    WebAppStartGyroscope(SensorStartParams),
    /// `web_app_stop_gyroscope`: Stops gyroscope sensor.
    WebAppStopGyroscope,
    /// `web_app_check_location`: Checks geolocation availability (unavailable on desktop).
    WebAppCheckLocation,
    /// `web_app_request_location`: Requests current device location.
    WebAppRequestLocation,
    /// `web_app_open_location_settings`: Requests opening OS location settings.
    WebAppOpenLocationSettings,
    /// `web_app_biometry_get_info`: Queries biometric auth capability (unavailable on desktop).
    WebAppBiometryGetInfo,
    /// `web_app_biometry_request_access`: Requests biometric authentication permission.
    WebAppBiometryRequestAccess(BiometryReasonParams),
    /// `web_app_biometry_request_auth`: Requests biometric authentication challenge.
    WebAppBiometryRequestAuth(BiometryReasonParams),
    /// `web_app_biometry_update_token`: Updates or clears the biometry-protected token.
    WebAppBiometryUpdateToken(BiometryUpdateTokenParams),
    /// `web_app_biometry_open_settings`: Opens OS biometry settings.
    WebAppBiometryOpenSettings,
    /// `web_app_device_storage_save_key`: Saves or removes a key in local per-bot device storage.
    WebAppDeviceStorageSaveKey(StorageSaveKeyParams),
    /// `web_app_device_storage_get_key`: Reads a key from local per-bot device storage.
    WebAppDeviceStorageGetKey(StorageGetKeyParams),
    /// `web_app_device_storage_clear`: Clears all keys in local per-bot device storage.
    WebAppDeviceStorageClear(StorageClearParams),
    /// `web_app_secure_storage_save_key`: Secure storage write (unsupported on desktop).
    WebAppSecureStorageSaveKey(StorageSaveKeyParams),
    /// `web_app_secure_storage_get_key`: Secure storage read (unsupported on desktop).
    WebAppSecureStorageGetKey(StorageGetKeyParams),
    /// `web_app_secure_storage_restore_key`: Secure storage restore (unsupported on desktop).
    WebAppSecureStorageRestoreKey(StorageGetKeyParams),
    /// `web_app_secure_storage_clear`: Secure storage clear (unsupported on desktop).
    WebAppSecureStorageClear(StorageClearParams),
    /// `web_app_verify_age`: Reports age verification completion.
    WebAppVerifyAge(VerifyAgeParams),
    /// `iframe_ready`: Signals iframe web app readiness.
    IframeReady(IframeReadyParams),
    /// `iframe_will_reload`: Signals imminent iframe reload.
    IframeWillReload,
    /// `share_score`: Legacy Telegram HTML5 game event to share high score.
    ShareScore,
    /// Forward-compatible catch-all for unrecognized event names.
    Unknown {
        /// Raw event type string.
        event_type: String,
        /// Raw JSON payload object.
        event_data: Map<String, Value>,
    },
}

/// Write-access / emoji-status-access reply status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessRequestStatus {
    /// User granted access.
    Allowed,
    /// User declined or cancelled.
    Cancelled,
}

/// Phone sharing reply status (`replyRequestPhone`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhoneRequestStatus {
    /// Phone contact was shared.
    Sent,
    /// User cancelled.
    Cancelled,
}

/// Home screen shortcut status (`home_screen_checked`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeScreenStatus {
    /// Platform does not support home screen shortcuts.
    Unsupported,
    /// Status cannot be determined.
    Unknown,
    /// Shortcut is present.
    Added,
    /// Shortcut is not present.
    Missed,
}

/// File download confirmation status (`file_download_requested`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileDownloadStatus {
    /// Download started.
    Downloading,
    /// User cancelled download.
    Cancelled,
}

/// Insets in CSS pixels (`safe_area_changed` and `content_safe_area_changed`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafeAreaInsets {
    /// Top inset in CSS pixels.
    pub top: i32,
    /// Right inset in CSS pixels.
    pub right: i32,
    /// Bottom inset in CSS pixels.
    pub bottom: i32,
    /// Left inset in CSS pixels.
    pub left: i32,
}

/// Payload for `viewport_changed`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ViewportParams {
    /// Visible webview height in CSS pixels.
    pub height: f64,
    /// Whether the viewport height is currently stable (not mid-resize animation).
    pub is_state_stable: bool,
    /// Whether the Mini App panel is expanded.
    pub is_expanded: bool,
}

/// Payload for `biometry_info_received`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BiometryInfo {
    /// Whether biometric authentication is available on this device.
    pub available: bool,
    /// Biometry hardware type (`"finger"`, `"face"`, `"unknown"`), omitted when unavailable.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub biometry_type: Option<String>,
    /// Whether access has been requested previously.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_requested: Option<bool>,
    /// Whether access was granted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_granted: Option<bool>,
    /// Whether a biometric token is stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_saved: Option<bool>,
    /// Stable device identifier for biometry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

/// Payload for `theme_changed` (`{ "theme_params": { "bg_color": "#ffffff", ... } }`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeChangedPayload {
    /// Map of Telegram Mini App CSS color keys (`bg_color`, `text_color`, etc.) to `#rrggbb`.
    pub theme_params: BTreeMap<String, String>,
}

impl ThemeChangedPayload {
    /// Builds a `theme_changed` payload directly from a `tg-theme` [`Palette`].
    #[must_use]
    pub fn from_palette(palette: &Palette) -> Self {
        Self::from_web_view_params(&web_view_params(palette))
    }

    /// Builds a `theme_changed` payload from [`WebViewThemeParams`].
    #[must_use]
    pub fn from_web_view_params(params: &WebViewThemeParams) -> Self {
        let theme_params =
            serde_json::from_str::<BTreeMap<String, String>>(&params.json).unwrap_or_default();
        Self { theme_params }
    }
}

/// Typed outgoing event sent from Rustgram host to the Mini App page.
///
/// Mirrors every `postEvent(...)` call in `Telegram/SourceFiles/ui/chat/attach/attach_bot_webview.cpp`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "eventType", content = "eventData", rename_all = "snake_case")]
pub enum HostEvent {
    /// `theme_changed`: Sends updated palette colors.
    ThemeChanged(ThemeChangedPayload),
    /// `viewport_changed`: Sends viewport height and expansion state.
    ViewportChanged(ViewportParams),
    /// `main_button_pressed`: User clicked the primary bottom button.
    MainButtonPressed,
    /// `secondary_button_pressed`: User clicked the secondary bottom button.
    SecondaryButtonPressed,
    /// `back_button_pressed`: User clicked the header back button.
    BackButtonPressed,
    /// `settings_button_pressed`: User selected Settings in the panel menu.
    SettingsButtonPressed,
    /// `invoice_closed`: Payment flow finished for `slug`.
    InvoiceClosed {
        /// Invoice slug.
        slug: String,
        /// Outcome (`"paid"`, `"cancelled"`, `"failed"`, `"pending"`).
        status: String,
    },
    /// `popup_closed`: Modal popup closed; `button_id` is `None` when dismissed without clicking.
    PopupClosed {
        /// Clicked button ID, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        button_id: Option<String>,
    },
    /// `qr_text_received`: QR scanner decoded a payload.
    QrTextReceived {
        /// Decoded QR text.
        data: String,
    },
    /// `scan_qr_popup_closed`: QR scanner popup dismissed.
    ScanQrPopupClosed,
    /// `clipboard_text_received`: Reply to `web_app_read_text_from_clipboard`.
    ClipboardTextReceived {
        /// Echoed request ID.
        req_id: Value,
        /// Clipboard text, or `None` if read was denied/expired.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
    },
    /// `write_access_requested`: Reply to `web_app_request_write_access`.
    WriteAccessRequested {
        /// `"allowed"` or `"cancelled"`.
        status: AccessRequestStatus,
    },
    /// `phone_requested`: Reply to `web_app_request_phone`.
    PhoneRequested {
        /// `"sent"` or `"cancelled"`.
        status: PhoneRequestStatus,
    },
    /// `custom_method_invoked`: Reply to `web_app_invoke_custom_method`.
    CustomMethodInvoked {
        /// Echoed request ID.
        req_id: Value,
        /// Result value on success.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        /// Error string on failure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// `fullscreen_changed`: Fullscreen state toggled.
    FullscreenChanged {
        /// Current fullscreen state.
        is_fullscreen: bool,
    },
    /// `fullscreen_failed`: Entering fullscreen failed.
    FullscreenFailed {
        /// Error code (e.g. `"UNSUPPORTED"`).
        error: String,
    },
    /// `safe_area_changed`: Hardware safe-area insets updated.
    SafeAreaChanged(SafeAreaInsets),
    /// `content_safe_area_changed`: Content safe-area insets updated (accounts for title overlay).
    ContentSafeAreaChanged(SafeAreaInsets),
    /// `home_screen_checked`: Reply to `web_app_check_home_screen`.
    HomeScreenChecked {
        /// Current status (`HomeScreenStatus::Unsupported` on desktop).
        status: HomeScreenStatus,
    },
    /// `accelerometer_failed`: Sensor unavailable.
    AccelerometerFailed {
        /// Error code (`"UNSUPPORTED"`).
        error: String,
    },
    /// `device_orientation_failed`: Sensor unavailable.
    DeviceOrientationFailed {
        /// Error code (`"UNSUPPORTED"`).
        error: String,
    },
    /// `gyroscope_failed`: Sensor unavailable.
    GyroscopeFailed {
        /// Error code (`"UNSUPPORTED"`).
        error: String,
    },
    /// `location_checked`: Reply to `web_app_check_location`.
    LocationChecked {
        /// Whether geolocation is available (`false` on desktop).
        available: bool,
    },
    /// `location_requested`: Reply to `web_app_request_location`.
    LocationRequested {
        /// Whether geolocation succeeded (`false` on desktop).
        available: bool,
    },
    /// `biometry_info_received`: Reply to `web_app_biometry_get_info`.
    BiometryInfoReceived(BiometryInfo),
    /// `biometry_auth_requested`: Reply to `web_app_biometry_request_auth`.
    BiometryAuthRequested {
        /// `"authorized"` or `"failed"`.
        status: String,
        /// Optional stored token on authorization.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
    },
    /// `biometry_token_updated`: Reply to `web_app_biometry_update_token`.
    BiometryTokenUpdated {
        /// `"updated"`, `"removed"`, or `"failed"`.
        status: String,
    },
    /// `emoji_status_set`: User confirmed setting custom emoji status.
    EmojiStatusSet,
    /// `emoji_status_failed`: Setting emoji status failed or was cancelled.
    EmojiStatusFailed {
        /// Error code (e.g. `"SUGGESTED_EMOJI_INVALID"`, `"DURATION_INVALID"`, `"USER_DECLINED"`).
        error: String,
    },
    /// `emoji_status_access_requested`: Reply to `web_app_request_emoji_status_access`.
    EmojiStatusAccessRequested {
        /// `"allowed"` or `"cancelled"`.
        status: AccessRequestStatus,
    },
    /// `file_download_requested`: Reply to `web_app_request_file_download`.
    FileDownloadRequested {
        /// `"downloading"` or `"cancelled"`.
        status: FileDownloadStatus,
    },
    /// `prepared_message_sent`: Prepared inline message was sent.
    PreparedMessageSent,
    /// `prepared_message_failed`: Sending prepared inline message failed.
    PreparedMessageFailed {
        /// Error string.
        error: String,
    },
    /// `requested_chat_sent`: User selected a chat for `req_id`.
    RequestedChatSent {
        /// Request identifier.
        req_id: String,
    },
    /// `requested_chat_failed`: Chat selection failed or was cancelled.
    RequestedChatFailed {
        /// Request identifier.
        req_id: String,
        /// Error string.
        error: String,
    },
    /// `device_storage_key_saved`: Reply to `web_app_device_storage_save_key`.
    DeviceStorageKeySaved {
        /// Echoed request ID.
        req_id: Value,
    },
    /// `device_storage_key_received`: Reply to `web_app_device_storage_get_key`.
    DeviceStorageKeyReceived {
        /// Echoed request ID.
        req_id: Value,
        /// Stored string value or `Value::Null` when absent.
        value: Value,
    },
    /// `device_storage_cleared`: Reply to `web_app_device_storage_clear`.
    DeviceStorageCleared {
        /// Echoed request ID.
        req_id: Value,
    },
    /// `device_storage_failed`: Failure reply for device storage commands.
    DeviceStorageFailed {
        /// Echoed request ID.
        req_id: Value,
        /// Error code (`"KEY_INVALID"`, `"VALUE_INVALID"`, `"QUOTA_EXCEEDED"`).
        error: String,
    },
    /// `secure_storage_failed`: Failure reply for secure storage commands (`"UNSUPPORTED"`).
    SecureStorageFailed {
        /// Echoed request ID.
        req_id: Value,
        /// Error code (`"UNSUPPORTED"`).
        error: String,
    },
    /// `activated`: Mini App window gained focus (Bot API 8.0+).
    Activated,
    /// `deactivated`: Mini App window lost focus (Bot API 8.0+).
    Deactivated,
    /// `visibility_changed`: Mini App visibility changed.
    VisibilityChanged {
        /// Whether the webview is visible.
        is_visible: bool,
    },
    /// `reload_iframe`: Instructs an iframe Mini App to reload.
    ReloadIframe,
}

impl HostEvent {
    /// Returns the wire event name and optional JSON payload for `TelegramGameProxy.receiveEvent`.
    #[must_use]
    pub fn to_wire_parts(&self) -> (&'static str, Option<Value>) {
        match self {
            Self::ThemeChanged(p) => ("theme_changed", serde_json::to_value(p).ok()),
            Self::ViewportChanged(p) => ("viewport_changed", serde_json::to_value(p).ok()),
            Self::MainButtonPressed => ("main_button_pressed", None),
            Self::SecondaryButtonPressed => ("secondary_button_pressed", None),
            Self::BackButtonPressed => ("back_button_pressed", None),
            Self::SettingsButtonPressed => ("settings_button_pressed", None),
            Self::InvoiceClosed { slug, status } => (
                "invoice_closed",
                Some(serde_json::json!({ "slug": slug, "status": status })),
            ),
            Self::PopupClosed { button_id } => (
                "popup_closed",
                button_id
                    .as_ref()
                    .map(|id| serde_json::json!({ "button_id": id })),
            ),
            Self::QrTextReceived { data } => (
                "qr_text_received",
                Some(serde_json::json!({ "data": data })),
            ),
            Self::ScanQrPopupClosed => ("scan_qr_popup_closed", None),
            Self::ClipboardTextReceived { req_id, data } => {
                let mut obj = Map::new();
                obj.insert("req_id".to_owned(), req_id.clone());
                if let Some(text) = data {
                    obj.insert("data".to_owned(), Value::String(text.clone()));
                }
                ("clipboard_text_received", Some(Value::Object(obj)))
            }
            Self::WriteAccessRequested { status } => (
                "write_access_requested",
                serde_json::to_value(serde_json::json!({ "status": status })).ok(),
            ),
            Self::PhoneRequested { status } => (
                "phone_requested",
                serde_json::to_value(serde_json::json!({ "status": status })).ok(),
            ),
            Self::CustomMethodInvoked {
                req_id,
                result,
                error,
            } => {
                let mut obj = Map::new();
                obj.insert("req_id".to_owned(), req_id.clone());
                if let Some(res) = result {
                    obj.insert("result".to_owned(), res.clone());
                }
                if let Some(err) = error {
                    obj.insert("error".to_owned(), Value::String(err.clone()));
                }
                ("custom_method_invoked", Some(Value::Object(obj)))
            }
            Self::FullscreenChanged { is_fullscreen } => (
                "fullscreen_changed",
                Some(serde_json::json!({ "is_fullscreen": is_fullscreen })),
            ),
            Self::FullscreenFailed { error } => (
                "fullscreen_failed",
                Some(serde_json::json!({ "error": error })),
            ),
            Self::SafeAreaChanged(insets) => {
                ("safe_area_changed", serde_json::to_value(insets).ok())
            }
            Self::ContentSafeAreaChanged(insets) => (
                "content_safe_area_changed",
                serde_json::to_value(insets).ok(),
            ),
            Self::HomeScreenChecked { status } => (
                "home_screen_checked",
                serde_json::to_value(serde_json::json!({ "status": status })).ok(),
            ),
            Self::AccelerometerFailed { error } => (
                "accelerometer_failed",
                Some(serde_json::json!({ "error": error })),
            ),
            Self::DeviceOrientationFailed { error } => (
                "device_orientation_failed",
                Some(serde_json::json!({ "error": error })),
            ),
            Self::GyroscopeFailed { error } => (
                "gyroscope_failed",
                Some(serde_json::json!({ "error": error })),
            ),
            Self::LocationChecked { available } => (
                "location_checked",
                Some(serde_json::json!({ "available": available })),
            ),
            Self::LocationRequested { available } => (
                "location_requested",
                Some(serde_json::json!({ "available": available })),
            ),
            Self::BiometryInfoReceived(info) => {
                ("biometry_info_received", serde_json::to_value(info).ok())
            }
            Self::BiometryAuthRequested { status, token } => {
                let mut obj = Map::new();
                obj.insert("status".to_owned(), Value::String(status.clone()));
                if let Some(tok) = token {
                    obj.insert("token".to_owned(), Value::String(tok.clone()));
                }
                ("biometry_auth_requested", Some(Value::Object(obj)))
            }
            Self::BiometryTokenUpdated { status } => (
                "biometry_token_updated",
                Some(serde_json::json!({ "status": status })),
            ),
            Self::EmojiStatusSet => ("emoji_status_set", None),
            Self::EmojiStatusFailed { error } => (
                "emoji_status_failed",
                Some(serde_json::json!({ "error": error })),
            ),
            Self::EmojiStatusAccessRequested { status } => (
                "emoji_status_access_requested",
                serde_json::to_value(serde_json::json!({ "status": status })).ok(),
            ),
            Self::FileDownloadRequested { status } => (
                "file_download_requested",
                serde_json::to_value(serde_json::json!({ "status": status })).ok(),
            ),
            Self::PreparedMessageSent => ("prepared_message_sent", None),
            Self::PreparedMessageFailed { error } => (
                "prepared_message_failed",
                Some(serde_json::json!({ "error": error })),
            ),
            Self::RequestedChatSent { req_id } => (
                "requested_chat_sent",
                Some(serde_json::json!({ "req_id": req_id })),
            ),
            Self::RequestedChatFailed { req_id, error } => (
                "requested_chat_failed",
                Some(serde_json::json!({ "req_id": req_id, "error": error })),
            ),
            Self::DeviceStorageKeySaved { req_id } => (
                "device_storage_key_saved",
                Some(serde_json::json!({ "req_id": req_id })),
            ),
            Self::DeviceStorageKeyReceived { req_id, value } => (
                "device_storage_key_received",
                Some(serde_json::json!({ "req_id": req_id, "value": value })),
            ),
            Self::DeviceStorageCleared { req_id } => (
                "device_storage_cleared",
                Some(serde_json::json!({ "req_id": req_id })),
            ),
            Self::DeviceStorageFailed { req_id, error } => (
                "device_storage_failed",
                Some(serde_json::json!({ "req_id": req_id, "error": error })),
            ),
            Self::SecureStorageFailed { req_id, error } => (
                "secure_storage_failed",
                Some(serde_json::json!({ "req_id": req_id, "error": error })),
            ),
            Self::Activated => ("activated", None),
            Self::Deactivated => ("deactivated", None),
            Self::VisibilityChanged { is_visible } => (
                "visibility_changed",
                Some(serde_json::json!({ "is_visible": is_visible })),
            ),
            Self::ReloadIframe => ("reload_iframe", None),
        }
    }
}

/// Returns `true` if `s` is tdesktop's no-arguments sentinel (`""` or `"\"\""`).
#[must_use]
fn is_no_arguments_sentinel(s: &str) -> bool {
    s.is_empty() || s == "\"\""
}

/// Validates and parses an event data JSON value (object, null, empty sentinel, or JSON string
/// encoding an object), matching `CanParseArguments` / `ParseArguments` in `attach_bot_webview.cpp:200-232`.
fn parse_event_arguments(value: &Value) -> Result<Map<String, Value>> {
    match value {
        Value::Null => Ok(Map::new()),
        Value::Object(obj) => Ok(obj.clone()),
        Value::String(s) => {
            if is_no_arguments_sentinel(s) {
                return Ok(Map::new());
            }
            let parsed: Value = serde_json::from_str(s).map_err(|e| {
                WebViewError::InvalidBridgeMessage(format!("invalid eventData JSON string: {e}"))
            })?;
            match parsed {
                Value::Object(obj) => Ok(obj),
                _ => Err(WebViewError::InvalidBridgeMessage(
                    "stringified eventData is not a JSON object".to_owned(),
                )),
            }
        }
        _ => Err(WebViewError::InvalidBridgeMessage(
            "eventData must be an object, JSON string, or null".to_owned(),
        )),
    }
}

/// Returns `true` if `command` belongs to the internal shell namespace (`shell_*`, `tdesktop_shell_*`).
#[must_use]
pub fn is_shell_namespace_command(command: &str) -> bool {
    command.starts_with("shell_") || command.starts_with("tdesktop_shell_")
}

/// Returns `true` if `origin` is the exact trusted external-shell origin
/// (`https://web.telegram.org` / `https://web.telegram.org:443` with no userinfo, path, query, or fragment).
///
/// Exact port of `IsExternalShellOrigin` (`attach_bot_webview.cpp:121-131`).
#[must_use]
pub fn is_external_shell_origin(origin: &str) -> bool {
    let trimmed = origin.trim();
    let Ok(parsed) = Url::parse(trimmed) else {
        return false;
    };
    if parsed.scheme() != "https"
        || !parsed
            .host_str()
            .is_some_and(|h| h.eq_ignore_ascii_case("web.telegram.org"))
        || parsed.port().unwrap_or(443) != 443
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return false;
    }
    // `url::Url` normalizes empty path on https to "/"; reject any explicit path in the input.
    let Some(after_scheme) = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("HTTPS://"))
        .or_else(|| {
            trimmed
                .get(..8)
                .filter(|p| p.eq_ignore_ascii_case("https://"))
                .map(|_| &trimmed[8..])
        })
    else {
        return false;
    };
    !after_scheme.contains('/')
}

/// Parses a raw webview bridge payload in either standard embedded mode (`external_shell = false`)
/// or external-shell mode (`external_shell = true`, requiring a verified `web.telegram.org` sender
/// and matching random `shell_token`).
///
/// Port of `ParseNativeMessage` (`attach_bot_webview.cpp:273-388`).
pub fn parse_native_message_with_shell(
    text: &str,
    source_url: &str,
    external_shell: bool,
    shell_token: Option<&str>,
) -> Result<NativeMessage> {
    if text.len() > MAX_NATIVE_MESSAGE_BYTES {
        return Err(WebViewError::InvalidBridgeMessage(format!(
            "payload too large ({} bytes)",
            text.len()
        )));
    }
    let document: Value = serde_json::from_str(text)?;
    if external_shell {
        let Value::Object(obj) = document else {
            return Err(WebViewError::InvalidBridgeMessage(
                "external payload is not an object".to_owned(),
            ));
        };
        if !is_external_shell_origin(&origin_from_url(source_url)) {
            return Err(WebViewError::InvalidBridgeMessage(
                "bad external sender".to_owned(),
            ));
        }
        if obj.get("type").and_then(Value::as_str) != Some(EXTERNAL_MESSAGE_TYPE) {
            return Err(WebViewError::InvalidBridgeMessage(
                "bad external type".to_owned(),
            ));
        }
        let is_shell_source = match obj.get("source").and_then(Value::as_str) {
            Some("webapp") => false,
            Some("shell") => true,
            _ => {
                return Err(WebViewError::InvalidBridgeMessage(
                    "bad external source".to_owned(),
                ));
            }
        };
        let expected_token = shell_token.filter(|t| !t.is_empty());
        let provided_token = obj.get("token").and_then(Value::as_str);
        if expected_token.is_none() || provided_token != expected_token {
            return Err(WebViewError::InvalidBridgeMessage(
                "bad external token".to_owned(),
            ));
        }
        let origin_str = obj.get("origin").and_then(Value::as_str).ok_or_else(|| {
            WebViewError::InvalidBridgeMessage("missing external origin".to_owned())
        })?;
        let origin = if is_shell_source {
            if !is_external_shell_origin(origin_str) {
                return Err(WebViewError::InvalidBridgeMessage(
                    "bad shell origin".to_owned(),
                ));
            }
            String::new()
        } else {
            let normalized = origin_from_url(origin_str);
            if normalized.is_empty() {
                return Err(WebViewError::InvalidBridgeMessage(
                    "bad webapp origin".to_owned(),
                ));
            }
            normalized
        };
        let Some(Value::String(command)) = obj.get("eventType") else {
            return Err(WebViewError::InvalidBridgeMessage(
                "object payload missing string 'eventType'".to_owned(),
            ));
        };
        if command.is_empty() {
            return Err(WebViewError::InvalidBridgeMessage(
                "empty eventType".to_owned(),
            ));
        }
        let arguments = match obj.get("eventData") {
            None | Some(Value::Null) => Map::new(),
            Some(val) => parse_event_arguments(val)?,
        };
        let shell_cmd = is_shell_namespace_command(command);
        if is_shell_source && !shell_cmd {
            return Err(WebViewError::InvalidBridgeMessage(format!(
                "non-shell command '{command}' rejected from shell"
            )));
        }
        if !is_shell_source && shell_cmd {
            return Err(WebViewError::InvalidBridgeMessage(format!(
                "shell command '{command}' rejected from webapp"
            )));
        }
        return Ok(NativeMessage {
            origin,
            command: command.clone(),
            arguments,
        });
    }

    let Value::Array(list) = document else {
        return Err(WebViewError::InvalidBridgeMessage(
            "legacy payload is not an array".to_owned(),
        ));
    };
    let Some(Value::String(command)) = list.first() else {
        return Err(WebViewError::InvalidBridgeMessage(
            "array payload missing string eventType at index 0".to_owned(),
        ));
    };
    if command.is_empty() {
        return Err(WebViewError::InvalidBridgeMessage(
            "empty eventType".to_owned(),
        ));
    }
    if is_shell_namespace_command(command) {
        return Err(WebViewError::InvalidBridgeMessage(format!(
            "shell command '{command}' rejected from webapp"
        )));
    }
    let arguments = match list.get(1) {
        None | Some(Value::Null) => Map::new(),
        Some(val) => parse_event_arguments(val)?,
    };
    Ok(NativeMessage {
        origin: origin_from_url(source_url),
        command: command.clone(),
        arguments,
    })
}

/// Parses a raw webview bridge payload in embedded WebView mode (`[eventType, eventData]` array
/// format only; sender origin is derived strictly from WebView2's `source_url`).
///
/// Port of `ParseNativeMessage` (`attach_bot_webview.cpp:273-388`).
pub fn parse_native_message(text: &str, source_url: &str) -> Result<NativeMessage> {
    parse_native_message_with_shell(text, source_url, false, None)
}

/// Decodes a raw bridge JSON payload directly into a typed [`MiniAppEvent`].
pub fn parse_mini_app_event(text: &str, source_url: &str) -> Result<(NativeMessage, MiniAppEvent)> {
    parse_mini_app_event_with_shell(text, source_url, false, None)
}

/// Decodes a raw bridge JSON payload directly into a typed [`MiniAppEvent`], supporting
/// external-shell verification when `external_shell` is `true`.
pub fn parse_mini_app_event_with_shell(
    text: &str,
    source_url: &str,
    external_shell: bool,
    shell_token: Option<&str>,
) -> Result<(NativeMessage, MiniAppEvent)> {
    let native = parse_native_message_with_shell(text, source_url, external_shell, shell_token)?;
    let event = decode_mini_app_event(&native.command, &native.arguments)?;
    Ok((native, event))
}

/// Decodes `(command, arguments)` into a typed [`MiniAppEvent`].
pub fn decode_mini_app_event(
    command: &str,
    arguments: &Map<String, Value>,
) -> Result<MiniAppEvent> {
    let obj_val = || Value::Object(arguments.clone());
    let parse_obj = |val: Value| -> Result<Value> { Ok(val) };
    let _ = &parse_obj;

    let event = match command {
        "web_app_ready" => MiniAppEvent::WebAppReady,
        "web_app_expand" => MiniAppEvent::WebAppExpand,
        "web_app_close" => MiniAppEvent::WebAppClose(serde_json::from_value(obj_val())?),
        "web_app_data_send" => MiniAppEvent::WebAppDataSend(serde_json::from_value(obj_val())?),
        "web_app_switch_inline_query" => {
            if !arguments.contains_key("query") {
                return Err(WebViewError::InvalidBridgeMessage(
                    "missing 'query' in web_app_switch_inline_query".to_owned(),
                ));
            }
            let query = arguments
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            // Port of switchInlineQueryMessage (attach_bot_webview.cpp:2746-2764):
            // if any chat_type is invalid, clear chat_types rather than failing.
            let mut chat_types = Vec::new();
            if let Some(arr) = arguments.get("chat_types").and_then(Value::as_array) {
                for item in arr {
                    let parsed = item.as_str().and_then(|s| match s {
                        "users" => Some(InlineChatType::Users),
                        "bots" => Some(InlineChatType::Bots),
                        "groups" => Some(InlineChatType::Groups),
                        "channels" => Some(InlineChatType::Channels),
                        _ => None,
                    });
                    if let Some(ct) = parsed {
                        chat_types.push(ct);
                    } else {
                        chat_types.clear();
                        break;
                    }
                }
            }
            MiniAppEvent::WebAppSwitchInlineQuery(SwitchInlineQueryParams { query, chat_types })
        }
        "web_app_setup_main_button" => {
            MiniAppEvent::WebAppSetupMainButton(serde_json::from_value(obj_val())?)
        }
        "web_app_setup_secondary_button" => {
            let button: MainButtonParams = serde_json::from_value(obj_val())?;
            let position = match arguments.get("position").and_then(Value::as_str) {
                Some("top") => SecondaryButtonPosition::Top,
                Some("right") => SecondaryButtonPosition::Right,
                Some("bottom") => SecondaryButtonPosition::Bottom,
                _ => SecondaryButtonPosition::Left,
            };
            MiniAppEvent::WebAppSetupSecondaryButton(SecondaryButtonParams { button, position })
        }
        "web_app_setup_back_button" => {
            MiniAppEvent::WebAppSetupBackButton(serde_json::from_value(obj_val())?)
        }
        "web_app_setup_settings_button" => {
            MiniAppEvent::WebAppSetupSettingsButton(serde_json::from_value(obj_val())?)
        }
        "web_app_request_theme" => MiniAppEvent::WebAppRequestTheme,
        "web_app_request_viewport" => MiniAppEvent::WebAppRequestViewport,
        "web_app_request_safe_area" => MiniAppEvent::WebAppRequestSafeArea,
        "web_app_request_content_safe_area" => MiniAppEvent::WebAppRequestContentSafeArea,
        "web_app_request_fullscreen" => MiniAppEvent::WebAppRequestFullscreen,
        "web_app_exit_fullscreen" => MiniAppEvent::WebAppExitFullscreen,
        "web_app_set_header_color" => {
            MiniAppEvent::WebAppSetHeaderColor(parse_color_change(arguments))
        }
        "web_app_set_background_color" => {
            MiniAppEvent::WebAppSetBackgroundColor(parse_color_change(arguments))
        }
        "web_app_set_bottom_bar_color" => {
            MiniAppEvent::WebAppSetBottomBarColor(parse_color_change(arguments))
        }
        "web_app_trigger_haptic_feedback" => {
            MiniAppEvent::WebAppTriggerHapticFeedback(serde_json::from_value(obj_val())?)
        }
        "web_app_open_link" => MiniAppEvent::WebAppOpenLink(serde_json::from_value(obj_val())?),
        "web_app_open_tg_link" => {
            MiniAppEvent::WebAppOpenTgLink(serde_json::from_value(obj_val())?)
        }
        "web_app_open_invoice" => {
            MiniAppEvent::WebAppOpenInvoice(serde_json::from_value(obj_val())?)
        }
        "web_app_open_popup" => {
            let params: OpenPopupParams = serde_json::from_value(obj_val())?;
            if params.message.is_empty() {
                return Err(WebViewError::InvalidBridgeMessage(
                    "empty 'message' in web_app_open_popup".to_owned(),
                ));
            }
            if params.buttons.is_empty() {
                return Err(WebViewError::InvalidBridgeMessage(
                    "empty 'buttons' in web_app_open_popup".to_owned(),
                ));
            }
            MiniAppEvent::WebAppOpenPopup(params)
        }
        "web_app_open_scan_qr_popup" => {
            MiniAppEvent::WebAppOpenScanQrPopup(serde_json::from_value(obj_val())?)
        }
        "web_app_close_scan_qr_popup" => MiniAppEvent::WebAppCloseScanQrPopup,
        "web_app_read_text_from_clipboard" => {
            let Some(req_id) = arguments.get("req_id").cloned() else {
                return Err(WebViewError::InvalidBridgeMessage(
                    "missing 'req_id' in web_app_read_text_from_clipboard".to_owned(),
                ));
            };
            MiniAppEvent::WebAppReadTextFromClipboard(ClipboardReadParams { req_id })
        }
        "web_app_request_write_access" => MiniAppEvent::WebAppRequestWriteAccess,
        "web_app_request_phone" => MiniAppEvent::WebAppRequestPhone,
        "web_app_invoke_custom_method" => {
            if !arguments.contains_key("req_id") {
                return Err(WebViewError::InvalidBridgeMessage(
                    "missing 'req_id' in web_app_invoke_custom_method".to_owned(),
                ));
            }
            MiniAppEvent::WebAppInvokeCustomMethod(serde_json::from_value(obj_val())?)
        }
        "web_app_setup_closing_behavior" => {
            MiniAppEvent::WebAppSetupClosingBehavior(serde_json::from_value(obj_val())?)
        }
        "web_app_setup_swipe_behavior" => {
            MiniAppEvent::WebAppSetupSwipeBehavior(serde_json::from_value(obj_val())?)
        }
        "web_app_toggle_orientation_lock" => {
            MiniAppEvent::WebAppToggleOrientationLock(serde_json::from_value(obj_val())?)
        }
        "web_app_share_to_story" => {
            let mut params: ShareToStoryParams = serde_json::from_value(obj_val())?;
            // Port of processShareToStory (attach_bot_webview.cpp:3076):
            // widget_link.url must start with https:// (case-insensitive).
            if let Some(widget) = params.widget_link.as_mut()
                && !widget
                    .url
                    .get(..8)
                    .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
            {
                widget.url.clear();
                widget.name.clear();
            }
            MiniAppEvent::WebAppShareToStory(params)
        }
        "web_app_send_prepared_message" => {
            MiniAppEvent::WebAppSendPreparedMessage(serde_json::from_value(obj_val())?)
        }
        "web_app_request_chat" => {
            MiniAppEvent::WebAppRequestChat(serde_json::from_value(obj_val())?)
        }
        "web_app_set_emoji_status" => {
            MiniAppEvent::WebAppSetEmojiStatus(serde_json::from_value(obj_val())?)
        }
        "web_app_request_emoji_status_access" => MiniAppEvent::WebAppRequestEmojiStatusAccess,
        "web_app_request_file_download" => {
            MiniAppEvent::WebAppRequestFileDownload(serde_json::from_value(obj_val())?)
        }
        "web_app_check_home_screen" => MiniAppEvent::WebAppCheckHomeScreen,
        "web_app_add_to_home_screen" => MiniAppEvent::WebAppAddToHomeScreen,
        "web_app_start_accelerometer" => {
            MiniAppEvent::WebAppStartAccelerometer(serde_json::from_value(obj_val())?)
        }
        "web_app_stop_accelerometer" => MiniAppEvent::WebAppStopAccelerometer,
        "web_app_start_device_orientation" => {
            MiniAppEvent::WebAppStartDeviceOrientation(serde_json::from_value(obj_val())?)
        }
        "web_app_stop_device_orientation" => MiniAppEvent::WebAppStopDeviceOrientation,
        "web_app_start_gyroscope" => {
            MiniAppEvent::WebAppStartGyroscope(serde_json::from_value(obj_val())?)
        }
        "web_app_stop_gyroscope" => MiniAppEvent::WebAppStopGyroscope,
        "web_app_check_location" => MiniAppEvent::WebAppCheckLocation,
        "web_app_request_location" => MiniAppEvent::WebAppRequestLocation,
        "web_app_open_location_settings" => MiniAppEvent::WebAppOpenLocationSettings,
        "web_app_biometry_get_info" => MiniAppEvent::WebAppBiometryGetInfo,
        "web_app_biometry_request_access" => {
            MiniAppEvent::WebAppBiometryRequestAccess(serde_json::from_value(obj_val())?)
        }
        "web_app_biometry_request_auth" => {
            MiniAppEvent::WebAppBiometryRequestAuth(serde_json::from_value(obj_val())?)
        }
        "web_app_biometry_update_token" => {
            MiniAppEvent::WebAppBiometryUpdateToken(serde_json::from_value(obj_val())?)
        }
        "web_app_biometry_open_settings" => MiniAppEvent::WebAppBiometryOpenSettings,
        "web_app_device_storage_save_key" => {
            MiniAppEvent::WebAppDeviceStorageSaveKey(serde_json::from_value(obj_val())?)
        }
        "web_app_device_storage_get_key" => {
            MiniAppEvent::WebAppDeviceStorageGetKey(serde_json::from_value(obj_val())?)
        }
        "web_app_device_storage_clear" => {
            MiniAppEvent::WebAppDeviceStorageClear(serde_json::from_value(obj_val())?)
        }
        "web_app_secure_storage_save_key" => {
            MiniAppEvent::WebAppSecureStorageSaveKey(serde_json::from_value(obj_val())?)
        }
        "web_app_secure_storage_get_key" => {
            MiniAppEvent::WebAppSecureStorageGetKey(serde_json::from_value(obj_val())?)
        }
        "web_app_secure_storage_restore_key" => {
            MiniAppEvent::WebAppSecureStorageRestoreKey(serde_json::from_value(obj_val())?)
        }
        "web_app_secure_storage_clear" => {
            MiniAppEvent::WebAppSecureStorageClear(serde_json::from_value(obj_val())?)
        }
        "web_app_verify_age" => MiniAppEvent::WebAppVerifyAge(serde_json::from_value(obj_val())?),
        "iframe_ready" => MiniAppEvent::IframeReady(serde_json::from_value(obj_val())?),
        "iframe_will_reload" => MiniAppEvent::IframeWillReload,
        "share_score" => MiniAppEvent::ShareScore,
        other => MiniAppEvent::Unknown {
            event_type: other.to_owned(),
            event_data: arguments.clone(),
        },
    };
    Ok(event)
}

fn parse_color_change(arguments: &Map<String, Value>) -> ColorChangeParams {
    let color = arguments
        .get("color")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let color_key = arguments
        .get("color_key")
        .and_then(Value::as_str)
        .and_then(|key| match key {
            "bg_color" => Some(NamedColorKey::BgColor),
            "secondary_bg_color" => Some(NamedColorKey::SecondaryBgColor),
            "bottom_bar_bg_color" => Some(NamedColorKey::BottomBarBgColor),
            _ => None,
        });
    ColorChangeParams { color, color_key }
}

/// Returns the JavaScript init script injected into Mini App pages on document creation.
///
/// Installs:
/// - `window.TelegramWebviewProxy.postEvent(eventType, eventData)` (exact tdesktop contract from
///   `attach_bot_webview.cpp:2622-2629`, recognized by official `telegram-web-app.js`)
/// - `window.Telegram.WebView` shim (`postEvent`, `receiveEvent`, `onEvent`, `offEvent`) and
///   default `window.TelegramGameProxy.receiveEvent` dispatcher when not already defined by
///   `telegram-web-app.js`.
#[must_use]
pub fn mini_app_init_script() -> &'static str {
    r#"(function() {
window.TelegramWebviewProxy = {
    postEvent: function(eventType, eventData) {
        if (window.external && window.external.invoke) {
            window.external.invoke(JSON.stringify([eventType, eventData]));
        }
    }
};
window.Telegram = window.Telegram || {};
if (!window.Telegram.WebView) {
    const handlers = {};
    window.Telegram.WebView = {
        postEvent: function(eventType, callback, eventData) {
            if (typeof callback !== 'function' && eventData === undefined) {
                eventData = callback;
                callback = null;
            }
            window.TelegramWebviewProxy.postEvent(eventType, eventData === undefined ? '' : JSON.stringify(eventData));
            if (typeof callback === 'function') {
                callback();
            }
        },
        onEvent: function(eventType, callback) {
            if (!handlers[eventType]) handlers[eventType] = [];
            if (handlers[eventType].indexOf(callback) === -1) {
                handlers[eventType].push(callback);
            }
        },
        offEvent: function(eventType, callback) {
            const list = handlers[eventType];
            if (!list) return;
            const idx = list.indexOf(callback);
            if (idx !== -1) list.splice(idx, 1);
        },
        receiveEvent: function(eventType, eventData) {
            const list = handlers[eventType];
            if (!list) return;
            for (const fn of list.slice()) {
                try { fn(eventType, eventData); } catch (e) {}
            }
        }
    };
}
if (!window.TelegramGameProxy) {
    window.TelegramGameProxy = {
        receiveEvent: function(eventType, eventData) {
            if (window.Telegram && window.Telegram.WebView && window.Telegram.WebView.receiveEvent) {
                window.Telegram.WebView.receiveEvent(eventType, eventData);
            }
        }
    };
}
})();"#
}

/// Formats the JavaScript snippet that delivers a [`HostEvent`] into the page.
///
/// Exact port of `Panel::postEvent` (`attach_bot_webview.cpp:3896-3928`).
/// If `same_origin` is `Some(origin)`, guards delivery with `this.location.origin === ...`.
#[must_use]
pub fn format_post_event_script(event: &HostEvent, same_origin: Option<&str>) -> String {
    let (name, payload) = event.to_wire_parts();
    let encoded_data = payload.and_then(|v| serde_json::to_string(&v).ok());
    format_raw_post_event_script(name, encoded_data.as_deref(), same_origin)
}

/// Formats a `viewport_changed` event using `window.innerHeight` evaluated inside the page,
/// matching `Panel::sendViewport` (`attach_bot_webview.cpp:2658-2667`).
#[must_use]
pub fn format_dynamic_viewport_script(same_origin: Option<&str>) -> String {
    format_raw_post_event_script(
        "viewport_changed",
        Some("{ height: window.innerHeight, is_state_stable: true, is_expanded: true }"),
        same_origin,
    )
}

/// Formats a raw host-to-page event script with an optional JS expression or JSON payload.
#[must_use]
pub fn format_raw_post_event_script(
    event_name: &str,
    raw_js_data: Option<&str>,
    same_origin: Option<&str>,
) -> String {
    let origin_guard = same_origin
        .map(origin_check_script)
        .map(|check| format!("{check} && "))
        .unwrap_or_default();
    let encoded_name =
        serde_json::to_string(event_name).unwrap_or_else(|_| format!("\"{event_name}\""));
    let args = match raw_js_data {
        Some(data) if !data.is_empty() => format!("{encoded_name}, {data}"),
        _ => encoded_name,
    };
    format!(
        "if ({origin_guard}this.TelegramGameProxy) {{\n\
         this.TelegramGameProxy.receiveEvent({args});\n\
         }} else if ({origin_guard}this.Telegram && this.Telegram.WebView) {{\n\
         this.Telegram.WebView.receiveEvent({args});\n\
         }}"
    )
}

/// Normalizes a URL into `scheme://host:port` (lowercase, IPv6 bracketed, default ports filled).
///
/// Exact port of `OriginFromUrl` (`attach_bot_webview.cpp:147-162`).
#[must_use]
pub fn origin_from_url(raw: &str) -> String {
    let Ok(parsed) = Url::parse(raw) else {
        return String::new();
    };
    let scheme = parsed.scheme().to_ascii_lowercase();
    let Some(host_raw) = parsed.host_str() else {
        return String::new();
    };
    if scheme.is_empty() || host_raw.is_empty() {
        return String::new();
    }
    let port = parsed.port().or(match scheme.as_str() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    });
    let Some(port) = port else {
        return String::new();
    };
    let host = host_raw.to_ascii_lowercase();
    format!("{scheme}://{host}:{port}")
}

/// Returns `true` when `a` and `b` normalize to the same non-empty origin.
///
/// Exact port of `OriginsMatch` (`attach_bot_webview.cpp:164-173`).
#[must_use]
pub fn origins_match(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let norm_a = origin_from_url(a);
    let norm_b = origin_from_url(b);
    !norm_a.is_empty() && !norm_b.is_empty() && norm_a == norm_b
}

/// Builds the JS expression `this.location.origin === "<origin>"` with default ports stripped.
///
/// Exact port of `OriginCheckScript` (`attach_bot_webview.cpp:175-185`).
#[must_use]
pub fn origin_check_script(origin: &str) -> String {
    let normalized = if let Ok(mut parsed) = Url::parse(origin) {
        if (parsed.scheme() == "https" && parsed.port() == Some(443))
            || (parsed.scheme() == "http" && parsed.port() == Some(80))
        {
            let _ = parsed.set_port(None);
        }
        parsed.origin().ascii_serialization()
    } else {
        origin.to_owned()
    };
    let encoded = serde_json::to_string(&normalized).unwrap_or_else(|_| "\"\"".to_owned());
    format!("this.location.origin === {encoded}")
}

/// Parses a `#RRGGBB` hex color string into a [`Color`].
///
/// Exact port of `ParseColor` (`attach_bot_webview.cpp:920-941`).
#[must_use]
pub fn parse_hex_color(text: &str) -> Option<Color> {
    let bytes = text.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' {
        return None;
    }
    let hex_byte = |hi: u8, lo: u8| -> Option<u8> {
        let nibble = |b: u8| -> Option<u8> {
            match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(10 + (b - b'a')),
                b'A'..=b'F' => Some(10 + (b - b'A')),
                _ => None,
            }
        };
        Some(nibble(hi)? * 16 + nibble(lo)?)
    };
    let r = hex_byte(bytes[1], bytes[2])?;
    let g = hex_byte(bytes[3], bytes[4])?;
    let b = hex_byte(bytes[5], bytes[6])?;
    Some(Color::rgb(r, g, b))
}

/// Computes a contrasting ripple color for a Mini App button background.
///
/// Exact port of `ResolveRipple` (`attach_bot_webview.cpp:943-956`).
#[must_use]
pub fn resolve_ripple_color(background: Color) -> Color {
    let hsv = background.to_hsv();
    let adjusted = if hsv.v > LIGHTNESS_THRESHOLD {
        (hsv.v - LIGHTNESS_DELTA).max(0)
    } else {
        (hsv.v + LIGHTNESS_DELTA).min(255)
    };
    Color::from_hsv(hsv.h, hsv.s, adjusted, background.a)
}

// ============================================================================
// MTProto Web Proxy Hidden WebView Carrier Bridge Routing (`web_proxy_webview.cpp`)
// ============================================================================

/// Fixed 16-byte WebView2 profile isolation token for the hidden Web Proxy carrier
/// (`web_proxy_webview.cpp:138`).
pub const WEB_PROXY_STORAGE_TOKEN: [u8; 16] = [
    0xec, 0x5f, 0x15, 0xfe, 0x14, 0x86, 0x4f, 0xaa, 0xa0, 0x18, 0xd2, 0x70, 0xaa, 0x2a, 0x0d, 0xf8,
];

/// Maximum encoded script message size accepted from a hidden Web Proxy carrier (2 MiB).
pub const MAX_WEB_PROXY_CARRIER_MESSAGE_BYTES: usize = 2 * 1024 * 1024;

/// Returns the JavaScript bridge injected into a hidden Web Proxy carrier window
/// (`BridgeScript()` in `mtproto/web_proxy/web_proxy_webview.cpp:76-119`).
#[must_use]
pub const fn web_proxy_init_script() -> &'static str {
    r#"(()=>{
if(window!==window.top||Object.prototype.hasOwnProperty.call(window,'TelegramWebProxy'))return;
let receiver=null;
const send=value=>window.external.invoke(value);
const encode=value=>{
 const bytes=new Uint8Array(value),parts=[];
 for(let i=0;i<bytes.length;i+=32768)parts.push(String.fromCharCode(...bytes.subarray(i,i+32768)));
 return btoa(parts.join(''));
};
const decode=value=>{
 const binary=atob(value),bytes=new Uint8Array(binary.length);
 for(let i=0;i<binary.length;i++)bytes[i]=binary.charCodeAt(i);
 return bytes.buffer;
};
const bridge={
 postMessage(value){
  if(value instanceof ArrayBuffer)send('b'+encode(value));
  else if(typeof value==='string')send('c'+value);
  else send('f');
 },
 receive(sequence,value){
  try{
   if(typeof receiver!=='function')throw new Error();
   receiver({data:decode(value)});
   send('a'+sequence);
  }catch(error){send('f')}
 },
 receiveControl(sequence,value){
  try{
   if(typeof receiver!=='function')throw new Error();
   receiver({data:value});
   send('a'+sequence);
  }catch(error){send('f')}
 },
 get onmessage(){return receiver},
 set onmessage(value){receiver=typeof value==='function'?value:null}
};
Object.defineProperty(window,'TelegramWebProxy',{
 value:Object.freeze(bridge),configurable:false,writable:false
});
send('h');
})()"#
}

/// JavaScript snippet sent when closing an adopted Web Proxy carrier (`web_proxy_webview.cpp:215`).
#[must_use]
pub const fn web_proxy_close_script() -> &'static str {
    "window.TelegramWebProxy?.receiveControl(0,'{\"t\":\"close\"}')"
}

/// JavaScript heartbeat probe evaluated periodically in the hidden Web Proxy carrier
/// (`web_proxy_webview.cpp:170`).
#[must_use]
pub const fn web_proxy_heartbeat_probe_script() -> &'static str {
    "window.external?.invoke('h')"
}

/// Decoded carrier message emitted by the `window.TelegramWebProxy` bridge script.
#[derive(Debug, Clone, PartialEq)]
pub enum WebProxyBridgeMessage {
    /// Heartbeat ping `'h'`.
    Heartbeat,
    /// Bridge script error `'f'`.
    Failure,
    /// Outbound write sequence acknowledgement `'a<seq>'`.
    WriteAck(u64),
    /// JSON control message `'c<json>'` (e.g. `tproxy-android-init`, `status`, `close`).
    Control(Value),
    /// Base64-encoded binary relay frame payload `'b<base64>'`.
    BinaryBase64(String),
}

/// Parses a [`crate::interface::WebMessage`] from a hidden Web Proxy carrier into a typed
/// [`WebProxyBridgeMessage`].
pub fn parse_web_proxy_bridge_message(
    msg: &crate::interface::WebMessage,
) -> Result<WebProxyBridgeMessage> {
    if msg.text.is_empty() || msg.text.len() > MAX_WEB_PROXY_CARRIER_MESSAGE_BYTES {
        return Err(WebViewError::InvalidBridgeMessage(
            "web proxy carrier message is empty or exceeds 2 MiB limit".to_owned(),
        ));
    }
    let bytes = msg.text.as_bytes();
    match bytes[0] {
        b'h' if bytes.len() == 1 => Ok(WebProxyBridgeMessage::Heartbeat),
        b'f' if bytes.len() == 1 => Ok(WebProxyBridgeMessage::Failure),
        b'a' => {
            let seq = msg.text[1..].parse::<u64>().map_err(|_| {
                WebViewError::InvalidBridgeMessage("invalid web proxy ack sequence".to_owned())
            })?;
            Ok(WebProxyBridgeMessage::WriteAck(seq))
        }
        b'c' => {
            let val: Value = serde_json::from_str(&msg.text[1..]).map_err(|e| {
                WebViewError::InvalidBridgeMessage(format!("invalid web proxy control JSON: {e}"))
            })?;
            if !val.is_object() {
                return Err(WebViewError::InvalidBridgeMessage(
                    "web proxy control payload must be a JSON object".to_owned(),
                ));
            }
            Ok(WebProxyBridgeMessage::Control(val))
        }
        b'b' if bytes.len() > 1 => Ok(WebProxyBridgeMessage::BinaryBase64(
            msg.text[1..].to_owned(),
        )),
        _ => Err(WebViewError::InvalidBridgeMessage(
            "unrecognized web proxy carrier prefix".to_owned(),
        )),
    }
}

/// Builds a locked-down hidden [`crate::interface::WebViewConfig`] for a Web Proxy carrier
/// matching `WebviewCarrier` (`web_proxy_webview.cpp:132-182`).
///
/// - `WindowMode::Hidden`, `safe = true`, `debug = false`, `allow_third_party_cookies = false`
/// - `restricted_origin = Some("https://<host>")`
/// - `user_data_path = Some(<tdata_dir>/wvproxy)` and `user_data_token = WEB_PROXY_STORAGE_TOKEN`
/// - Navigation policy permits only exact navigation to `bridge_url` (no popups / new windows)
/// - Incoming `WebMessage`s are routed directly to `on_message`.
#[must_use]
pub fn build_web_proxy_carrier_config(
    host: &str,
    bridge_url: &str,
    tdata_dir: Option<std::path::PathBuf>,
    on_message: crate::interface::MessageHandler,
    on_navigation_done: Option<crate::interface::NavigationDoneHandler>,
) -> crate::interface::WebViewConfig {
    let expected_url = bridge_url.to_owned();
    let expected_parsed = Url::parse(&expected_url).ok();
    let user_data_path = tdata_dir.map(|root| root.join("wvproxy"));

    crate::interface::WebViewConfig {
        opaque_bg: Color::BLACK,
        user_data_path,
        user_data_token: WEB_PROXY_STORAGE_TOKEN.to_vec(),
        debug: false,
        safe: true,
        allow_third_party_cookies: false,
        mode: crate::interface::WindowMode::Hidden,
        window_style: crate::interface::WindowStyle::Default,
        initial_bounds: None,
        custom_scheme: crate::data_stream::DEFAULT_CUSTOM_SCHEME.to_owned(),
        scheme_router: None,
        restricted_origin: Some(format!("https://{host}")),
        callbacks: crate::interface::WebViewCallbacks {
            on_message: Some(on_message),
            on_navigation_policy: Some(std::sync::Arc::new(move |url, new_window| {
                if new_window {
                    return false;
                }
                match (&expected_parsed, Url::parse(url).ok()) {
                    (Some(exp), Some(actual)) => exp == &actual,
                    _ => url == expected_url,
                }
            })),
            on_navigation_done,
            ..crate::interface::WebViewCallbacks::default()
        },
    }
}

#[cfg(test)]
mod web_proxy_bridge_tests {
    use super::*;
    use crate::interface::WebMessage;

    #[test]
    fn parses_web_proxy_carrier_messages_and_locks_down_config() {
        let src = "https://proxy.example.com/app/#android=abc";
        assert_eq!(
            parse_web_proxy_bridge_message(&WebMessage {
                text: "h".to_owned(),
                source_url: src.to_owned(),
            })
            .unwrap(),
            WebProxyBridgeMessage::Heartbeat
        );
        assert_eq!(
            parse_web_proxy_bridge_message(&WebMessage {
                text: "a42".to_owned(),
                source_url: src.to_owned(),
            })
            .unwrap(),
            WebProxyBridgeMessage::WriteAck(42)
        );
        assert!(matches!(
            parse_web_proxy_bridge_message(&WebMessage {
                text: r#"c{"t":"tproxy-android-init","v":1,"nonce":"abc"}"#.to_owned(),
                source_url: src.to_owned(),
            })
            .unwrap(),
            WebProxyBridgeMessage::Control(_)
        ));
        assert_eq!(
            parse_web_proxy_bridge_message(&WebMessage {
                text: "bAAAA".to_owned(),
                source_url: src.to_owned(),
            })
            .unwrap(),
            WebProxyBridgeMessage::BinaryBase64("AAAA".to_owned())
        );
        assert!(
            parse_web_proxy_bridge_message(&WebMessage {
                text: "x".to_owned(),
                source_url: src.to_owned(),
            })
            .is_err()
        );

        let url = "https://proxy.example.com/app/?bridge=cap#android=abc";
        let cfg = build_web_proxy_carrier_config(
            "proxy.example.com",
            url,
            Some(std::path::PathBuf::from("tdata")),
            std::sync::Arc::new(|_| {}),
            None,
        );
        assert_eq!(cfg.mode, crate::interface::WindowMode::Hidden);
        assert!(cfg.safe);
        assert!(!cfg.debug);
        assert_eq!(
            cfg.restricted_origin.as_deref(),
            Some("https://proxy.example.com")
        );
        assert_eq!(cfg.user_data_token.as_slice(), &WEB_PROXY_STORAGE_TOKEN);
        let nav = cfg.callbacks.on_navigation_policy.unwrap();
        assert!(nav(url, false));
        assert!(!nav(url, true));
        assert!(!nav("https://other.example/app/", false));
    }
}
