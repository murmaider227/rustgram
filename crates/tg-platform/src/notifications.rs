//! Native Windows toast notifications, COM/WinRT activation callbacks, and Focus Assist / Quiet Hours checks.
//!
//! Port of `Telegram/SourceFiles/platform/win/notifications_manager_win.cpp`,
//! `Telegram/SourceFiles/platform/win/windows_toast_activator.cpp`, and
//! `Telegram/SourceFiles/platform/win/windows_quiethours.idl`.
//!
//! # Activation Routing
//!
//! Rustgram supports both activation paths used by Telegram Desktop on Windows:
//! 1. **In-process WinRT events** (`ToastNotification::Activated`, `Dismissed`, `Failed`):
//!    delivers click, inline reply (`fastReply`), and `Mark as read` actions directly to the
//!    registered callback while the process is alive.
//! 2. **COM `INotificationActivationCallback` server** (`ToastActivator` with
//!    [`crate::app_id::TOAST_ACTIVATOR_CLSID_STR`]): registered via `CoRegisterClassObject` so
//!    Action Center activations (even after a toast times out to Action Center) also route
//!    back into the same callback. Optional HKCU COM registration is provided via
//!    [`register_toast_activator_com_in`].

use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::sync::Arc;

use crate::error::Result;
use crate::registration::RegistryRoot;

/// Maximum length of an activation argument or user-input string (`ToastActivation::String`).
pub const MAX_ACTIVATION_STRING_CHARS: usize = 16_384;

/// Chat / topic / session key identifying a group of notifications for per-chat clearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContextId {
    /// Account / session unique ID.
    pub session_id: u64,
    /// Peer ID (`UserId` / `ChatId` / `ChannelId` encoded as `i64`).
    pub peer_id: i64,
    /// Forum topic root message ID (`0` if not in a forum topic).
    pub topic_root_id: i32,
    /// Monoforum peer ID (`0` if unused).
    pub monoforum_peer_id: i64,
}

/// Unique identifier for a single message toast notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NotificationId {
    /// Owning session/chat/topic context.
    pub context: ContextId,
    /// Message ID within the chat.
    pub msg_id: i32,
}

/// Display options controlling which actions and fields appear on a toast.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisplayOptions {
    /// Omit the inline text input and Reply button.
    pub hide_reply_button: bool,
    /// Omit the "Mark as read" action button.
    pub hide_mark_as_read: bool,
    /// Hide sender name and avatar (privacy mode).
    pub hide_name_and_photo: bool,
}

/// Complete content of a native toast notification.
#[derive(Debug, Clone)]
pub struct ToastContent {
    /// Message identifier encoded into activation arguments and tracked for clearing.
    pub id: NotificationId,
    /// Primary title line (usually sender or chat name).
    pub title: String,
    /// Optional second line (e.g. group sender name or topic title).
    pub subtitle: Option<String>,
    /// Message body text.
    pub message: String,
    /// Optional local filesystem path to a circular avatar image (`appLogoOverride`).
    pub image_path: Option<PathBuf>,
    /// Optional local filesystem path to the send-button icon PNG (`imageUri`).
    pub send_icon_path: Option<PathBuf>,
    /// Placeholder text inside the `fastReply` input box.
    pub reply_placeholder: String,
    /// Label for the "Mark as read" action button.
    pub mark_as_read_label: String,
    /// Label for the "Send" inline reply button.
    pub send_label: String,
    /// Visibility options for actions and photo.
    pub options: DisplayOptions,
}

impl ToastContent {
    /// Create a simple toast with sensible defaults for button labels.
    pub fn simple(
        id: NotificationId,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            subtitle: None,
            message: message.into(),
            image_path: None,
            send_icon_path: None,
            reply_placeholder: "Write a message...".to_string(),
            mark_as_read_label: "Mark as read".to_string(),
            send_label: "Send".to_string(),
            options: DisplayOptions::default(),
        }
    }
}

/// Action chosen by the user when activating a toast notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastAction {
    /// Clicked the toast body to open the chat.
    Open,
    /// Submitted an inline reply via the `fastReply` input box.
    Reply(String),
    /// Clicked the "Mark as read" button.
    MarkAsRead,
}

/// Parsed activation event routed back to the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastActivationEvent {
    /// Process ID embedded when the toast was created.
    pub pid: u32,
    /// Notification identifier decoded from the launch/action arguments.
    pub id: NotificationId,
    /// Action triggered by the user.
    pub action: ToastAction,
    /// Any draft text typed into `fastReply` when the user clicked the toast body (`Open`).
    pub draft: Option<String>,
    /// Raw arguments string received from Windows.
    pub raw_args: String,
}

/// Windows `SHQueryUserNotificationState` result (`QUERY_USER_NOTIFICATION_STATE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserNotificationState {
    /// User is not present (`QUNS_NOT_PRESENT`, e.g. locked or screen saver).
    NotPresent,
    /// Full-screen application or presentation is active (`QUNS_BUSY`).
    Busy,
    /// Exclusive Direct3D full-screen app is running (`QUNS_RUNNING_D3D_FULL_SCREEN`).
    RunningD3dFullScreen,
    /// Windows Presentation Mode is active (`QUNS_PRESENTATION_MODE`).
    PresentationMode,
    /// Normal desktop accepting notifications (`QUNS_ACCEPTS_NOTIFICATIONS`).
    AcceptsNotifications,
    /// Quiet time / initial user logon grace period (`QUNS_QUIET_TIME`).
    QuietTime,
    /// Windows Store full-screen app is active (`QUNS_APP`).
    App,
    /// Unrecognized numeric state.
    Unknown(i32),
}

/// Windows 10/11 Focus Assist / Do Not Disturb state (`windows_quiethours.idl`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusAssistProfile {
    /// Focus Assist is off / unrestricted.
    Unrestricted,
    /// Priority-only mode; `app_allowed` is `true` if our AUMID is in the priority allowlist.
    PriorityOnly {
        /// Whether this application's AUMID is permitted through Priority Only.
        app_allowed: bool,
    },
    /// Alarms-only mode; all chat notifications are suppressed.
    AlarmsOnly,
    /// Focus Assist COM service is unavailable on this OS version.
    Unavailable,
}

/// Combined Windows notification suppression snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationSuppressionState {
    /// Result of `SHQueryUserNotificationState`.
    pub user_state: UserNotificationState,
    /// Result of `IQuietHoursSettings` / Focus Assist query.
    pub focus_assist: FocusAssistProfile,
    /// Legacy pre-RS4 Windows 10 quiet hours registry flag (`NOC_GLOBAL_SETTING_TOASTS_ENABLED == 0`).
    pub quiet_hours_registry: bool,
}

impl NotificationSuppressionState {
    /// Returns `true` if Focus Assist currently blocks notifications for this app.
    pub fn focus_assist_blocks(&self) -> bool {
        matches!(
            self.focus_assist,
            FocusAssistProfile::AlarmsOnly
                | FocusAssistProfile::PriorityOnly { app_allowed: false }
        )
    }

    /// Mirrors `Platform::Notifications::SkipToastForCustom()` in `notifications_manager_win.cpp`.
    pub fn should_skip_toast(&self, skip_in_focus: bool) -> bool {
        matches!(
            self.user_state,
            UserNotificationState::PresentationMode | UserNotificationState::RunningD3dFullScreen
        ) || (self.focus_assist_blocks() && skip_in_focus)
    }

    /// Mirrors `SkipSoundForCustom()` + `doMaybePlaySound()` in `notifications_manager_win.cpp`.
    pub fn should_skip_sound(&self, skip_in_focus: bool, screen_locked: bool) -> bool {
        matches!(
            self.user_state,
            UserNotificationState::NotPresent | UserNotificationState::PresentationMode
        ) || (self.focus_assist_blocks() && skip_in_focus)
            || self.quiet_hours_registry
            || screen_locked
    }

    /// Mirrors `SkipFlashBounceForCustom()` + `doMaybeFlashBounce()`.
    pub fn should_skip_flash_bounce(&self, skip_in_focus: bool) -> bool {
        self.should_skip_toast(skip_in_focus) || self.quiet_hours_registry
    }

    /// Mirrors `WaitForInputForCustom()`.
    pub fn should_wait_for_input(&self) -> bool {
        self.user_state != UserNotificationState::Busy
    }
}

/// Escape XML text and attribute values safely.
pub fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Format a local filesystem path as a `file:///` URI for WinRT toast XML.
pub fn file_uri_from_path(path: &Path) -> String {
    let native = path.to_string_lossy().replace('\\', "/");
    format!("file:///{}", native.trim_start_matches('/'))
}

/// Format the query-parameter identifier embedded in toast `launch` and `arguments` attributes
/// (matching `notifications_manager_win.cpp` lines 786-792).
pub fn format_toast_id_params(pid: u32, id: &NotificationId) -> String {
    format!(
        "pid={}&session={}&peer={}&topic={}&monoforumpeer={}&msg={}",
        pid,
        id.context.session_id,
        id.context.peer_id,
        id.context.topic_root_id,
        id.context.monoforum_peer_id,
        id.msg_id
    )
}

/// Build the complete WinRT ToastGeneric XML document matching `NotificationTemplate`
/// in `notifications_manager_win.cpp`.
pub fn build_toast_xml(pid: u32, content: &ToastContent) -> String {
    let id_params = format_toast_id_params(pid, &content.id);
    let escaped_id = xml_escape(&id_params);

    let image_tag = if !content.options.hide_name_and_photo {
        if let Some(ref img) = content.image_path {
            format!(
                "<image placement=\"appLogoOverride\" hint-crop=\"circle\" src=\"{}\"/>",
                xml_escape(&file_uri_from_path(img))
            )
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let subtitle_tag = match &content.subtitle {
        Some(sub) if !sub.is_empty() => format!("<text>{}</text>", xml_escape(sub)),
        _ => String::new(),
    };

    let mut actions = String::new();
    if !content.options.hide_reply_button {
        let send_img_attr = content
            .send_icon_path
            .as_ref()
            .map(|p| format!(" imageUri=\"{}\"", xml_escape(&file_uri_from_path(p))))
            .unwrap_or_default();
        actions.push_str(&format!(
            "<input id=\"fastReply\" type=\"text\" placeHolderContent=\"{}\"/>\
             <action content=\"{}\" arguments=\"action=reply&amp;{}\" activationType=\"background\"{send_img_attr} hint-inputId=\"fastReply\"/>",
            xml_escape(&content.reply_placeholder),
            xml_escape(&content.send_label),
            escaped_id,
        ));
    }
    if !content.options.hide_mark_as_read {
        actions.push_str(&format!(
            "<action content=\"{}\" arguments=\"action=mark&amp;{}\" activationType=\"background\"/>",
            xml_escape(&content.mark_as_read_label),
            escaped_id,
        ));
    }

    let actions_block = if actions.is_empty() {
        String::new()
    } else {
        format!("<actions>{actions}</actions>")
    };

    format!(
        "<toast launch=\"action=open&amp;{escaped_id}\">\
            <visual>\
                <binding template=\"ToastGeneric\">\
                    {image_tag}\
                    <text hint-maxLines=\"1\">{title}</text>\
                    {subtitle_tag}\
                    <text>{message}</text>\
                </binding>\
            </visual>\
            {actions_block}\
            <audio silent=\"true\"/>\
        </toast>",
        title = xml_escape(&content.title),
        message = xml_escape(&content.message),
    )
}

/// Normalize an activation string matching `ToastActivation::String` in `windows_toast_activator.cpp`:
/// caps length at 16384 characters and converts `\r` to `\n` when no `\n` is present.
pub fn normalize_activation_string(raw: &str) -> String {
    let mut s: String = raw.chars().take(MAX_ACTIVATION_STRING_CHARS).collect();
    if !s.contains('\n') {
        s = s.replace('\r', "\n");
    }
    s
}

/// Parse toast activation arguments and user-input pairs into a [`ToastActivationEvent`].
///
/// Mirrors `Manager::Private::handleActivation` in `notifications_manager_win.cpp`.
pub fn parse_toast_activation(
    args: &str,
    user_input: &[(String, String)],
) -> Option<ToastActivationEvent> {
    let norm_args = normalize_activation_string(args);
    let mut action_str = "open";
    let mut pid: u32 = 0;
    let mut session_id: u64 = 0;
    let mut peer_id: i64 = 0;
    let mut topic_root_id: i32 = 0;
    let mut monoforum_peer_id: i64 = 0;
    let mut msg_id: i32 = 0;

    for pair in norm_args.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        match k {
            "action" => action_str = v,
            "pid" => pid = v.parse().unwrap_or(0),
            "session" => session_id = v.parse().unwrap_or(0),
            "peer" => peer_id = v.parse().unwrap_or(0),
            "topic" => topic_root_id = v.parse().unwrap_or(0),
            "monoforumpeer" => monoforum_peer_id = v.parse().unwrap_or(0),
            "msg" => msg_id = v.parse().unwrap_or(0),
            _ => {}
        }
    }

    if session_id == 0 || peer_id == 0 || msg_id == 0 {
        return None;
    }

    let reply_text = user_input
        .iter()
        .find(|(k, _)| k == "fastReply")
        .map(|(_, v)| normalize_activation_string(v));

    let (action, draft) = match action_str {
        "reply" => (ToastAction::Reply(reply_text.unwrap_or_default()), None),
        "mark" => (ToastAction::MarkAsRead, None),
        _ => (ToastAction::Open, reply_text.filter(|s| !s.is_empty())),
    };

    Some(ToastActivationEvent {
        pid,
        id: NotificationId {
            context: ContextId {
                session_id,
                peer_id,
                topic_root_id,
                monoforum_peer_id,
            },
            msg_id,
        },
        action,
        draft,
        raw_args: norm_args,
    })
}

/// Rank Focus Assist profile restrictiveness (`notifications_manager_win.cpp` lines 311-317):
/// `2` for `.alarmsonly`, `1` for `.priorityonly`, `0` otherwise.
pub fn classify_focus_assist_profile_name(name: &str) -> u8 {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".alarmsonly") {
        2
    } else if lower.ends_with(".priorityonly") {
        1
    } else {
        0
    }
}

/// Query Windows `SHQueryUserNotificationState`.
pub fn query_user_notification_state() -> UserNotificationState {
    #[cfg(windows)]
    {
        win_notif::query_user_notification_state()
    }
    #[cfg(not(windows))]
    {
        UserNotificationState::AcceptsNotifications
    }
}

/// Query Windows 10/11 Focus Assist / Do Not Disturb state for `app_user_model_id`.
pub fn query_focus_assist(app_user_model_id: &str) -> FocusAssistProfile {
    #[cfg(windows)]
    {
        win_notif::query_focus_assist(app_user_model_id)
    }
    #[cfg(not(windows))]
    {
        let _ = app_user_model_id;
        FocusAssistProfile::Unavailable
    }
}

/// Query the pre-RS4 Windows 10 Quiet Hours registry value (`NOC_GLOBAL_SETTING_TOASTS_ENABLED == 0`)
/// under `root`.
///
/// When `root.prefix` is empty, automatically skips the query on Windows 10 build >= 17134
/// (matching `UseQuietHoursRegistryEntry` in `notifications_manager_win.cpp`).
pub fn query_quiet_hours_registry_in(root: &RegistryRoot) -> bool {
    #[cfg(windows)]
    {
        win_notif::query_quiet_hours_registry_in(root)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        false
    }
}

/// Query the full notification suppression state (`SHQueryUserNotificationState` +
/// Focus Assist + Quiet Hours).
pub fn query_notification_suppression(app_user_model_id: &str) -> NotificationSuppressionState {
    NotificationSuppressionState {
        user_state: query_user_notification_state(),
        focus_assist: query_focus_assist(app_user_model_id),
        quiet_hours_registry: query_quiet_hours_registry_in(&RegistryRoot::hkcu()),
    }
}

/// Register the COM `LocalServer32` entry for Rustgram's toast activator CLSID under `root`
/// (`Software\Classes\CLSID\<clsid>\LocalServer32`).
///
/// Takes `root: &RegistryRoot` so tests can use a throwaway key without modifying real HKCU.
pub fn register_toast_activator_com_in(
    root: &RegistryRoot,
    clsid: &str,
    executable: &Path,
) -> Result<()> {
    #[cfg(windows)]
    {
        win_notif::register_toast_activator_com_in(root, clsid, executable)
    }
    #[cfg(not(windows))]
    {
        let _ = (root, clsid, executable);
        Err(crate::error::PlatformError::Unsupported)
    }
}

/// Unregister the COM `LocalServer32` entry for `clsid` under `root`.
pub fn unregister_toast_activator_com_in(root: &RegistryRoot, clsid: &str) -> Result<()> {
    #[cfg(windows)]
    {
        win_notif::unregister_toast_activator_com_in(root, clsid)
    }
    #[cfg(not(windows))]
    {
        let _ = (root, clsid);
        Err(crate::error::PlatformError::Unsupported)
    }
}

/// Native toast notification manager backed by WinRT `ToastNotificationManager` and
/// COM `INotificationActivationCallback`.
pub struct ToastManager {
    #[cfg(windows)]
    inner: win_notif::WinToastManager,
    #[cfg(not(windows))]
    active: std::sync::Mutex<
        std::collections::BTreeMap<ContextId, std::collections::BTreeMap<i32, ()>>,
    >,
}

impl std::fmt::Debug for ToastManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToastManager")
            .field("active_count", &self.active_count())
            .finish()
    }
}

impl ToastManager {
    /// Create a toast notification manager bound to `app_user_model_id` (e.g. `"Rustgram"`).
    ///
    /// Note: Windows requires a valid Start-menu `.lnk` shortcut with `System.AppUserModel.ID`
    /// matching `app_user_model_id` (see [`crate::app_id::validate_or_create_shortcut_in`])
    /// and [`crate::app_id::set_current_process_app_id`] before WinRT will display toasts.
    pub fn new<F>(app_user_model_id: impl Into<String>, on_activate: F) -> Result<Self>
    where
        F: Fn(ToastActivationEvent) + Send + Sync + 'static,
    {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: win_notif::WinToastManager::new(
                    app_user_model_id.into(),
                    Arc::new(on_activate),
                )?,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = (app_user_model_id, on_activate);
            Ok(Self {
                active: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            })
        }
    }

    /// Show a native toast notification and track it under its `(ContextId, msg_id)` key.
    ///
    /// If a toast with the same `(ContextId, msg_id)` is already active, it is hidden first.
    pub fn show(&self, content: &ToastContent) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.show(content)
        }
        #[cfg(not(windows))]
        {
            if let Ok(mut map) = self.active.lock() {
                map.entry(content.id.context)
                    .or_default()
                    .insert(content.id.msg_id, ());
            }
            Ok(())
        }
    }

    /// Hide and untrack a single notification by `(context, msg_id)`.
    pub fn clear_item(&self, context: ContextId, msg_id: i32) {
        #[cfg(windows)]
        {
            self.inner.clear_item(context, msg_id);
        }
        #[cfg(not(windows))]
        {
            if let Ok(mut map) = self.active.lock()
                && let Some(inner) = map.get_mut(&context)
            {
                inner.remove(&msg_id);
                if inner.is_empty() {
                    map.remove(&context);
                }
            }
        }
    }

    /// Hide and untrack all notifications belonging to a specific forum topic.
    pub fn clear_topic(&self, session_id: u64, peer_id: i64, topic_root_id: i32) {
        #[cfg(windows)]
        {
            self.inner.clear_topic(session_id, peer_id, topic_root_id);
        }
        #[cfg(not(windows))]
        {
            if let Ok(mut map) = self.active.lock() {
                map.retain(|k, _| {
                    !(k.session_id == session_id
                        && k.peer_id == peer_id
                        && k.topic_root_id == topic_root_id)
                });
            }
        }
    }

    /// Hide and untrack all notifications for a chat (`session_id`, `peer_id`) across all topics.
    pub fn clear_chat(&self, session_id: u64, peer_id: i64) {
        #[cfg(windows)]
        {
            self.inner.clear_chat(session_id, peer_id);
        }
        #[cfg(not(windows))]
        {
            if let Ok(mut map) = self.active.lock() {
                map.retain(|k, _| !(k.session_id == session_id && k.peer_id == peer_id));
            }
        }
    }

    /// Hide and untrack all notifications belonging to an account/session.
    pub fn clear_session(&self, session_id: u64) {
        #[cfg(windows)]
        {
            self.inner.clear_session(session_id);
        }
        #[cfg(not(windows))]
        {
            if let Ok(mut map) = self.active.lock() {
                map.retain(|k, _| k.session_id != session_id);
            }
        }
    }

    /// Hide and untrack all active notifications.
    pub fn clear_all(&self) {
        #[cfg(windows)]
        {
            self.inner.clear_all();
        }
        #[cfg(not(windows))]
        {
            if let Ok(mut map) = self.active.lock() {
                map.clear();
            }
        }
    }

    /// Return the total number of currently tracked notifications.
    pub fn active_count(&self) -> usize {
        #[cfg(windows)]
        {
            self.inner.active_count()
        }
        #[cfg(not(windows))]
        {
            self.active
                .lock()
                .map(|m| m.values().map(std::collections::BTreeMap::len).sum())
                .unwrap_or(0)
        }
    }
}

#[cfg(windows)]
mod win_notif {
    use super::*;
    use crate::app_id::TOAST_ACTIVATOR_CLSID_STR;
    use crate::app_id::win_impl::{ComGuard, ensure_process_mta, guid_from_str};
    use crate::error::PlatformError;
    use crate::registration::win_reg::{open_reg_key, read_reg_dword, remove_reg_key, set_reg_sz};
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::{IReference, TypedEventHandler};
    use windows::UI::Notifications::{
        ToastActivatedEventArgs, ToastDismissalReason, ToastDismissedEventArgs,
        ToastFailedEventArgs, ToastNotification, ToastNotificationManager, ToastNotifier,
    };
    use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_NOINTERFACE};
    use windows::Win32::System::Com::{
        CLSCTX_LOCAL_SERVER, CoCreateInstance, CoRegisterClassObject, CoRevokeClassObject,
        CoTaskMemFree, IClassFactory, IClassFactory_Impl, REGCLS_MULTIPLEUSE,
    };
    use windows::Win32::UI::Notifications::{
        INotificationActivationCallback, INotificationActivationCallback_Impl,
        NOTIFICATION_USER_INPUT_DATA,
    };
    use windows::Win32::UI::Shell::{
        QUNS_ACCEPTS_NOTIFICATIONS, QUNS_APP, QUNS_BUSY, QUNS_NOT_PRESENT, QUNS_PRESENTATION_MODE,
        QUNS_QUIET_TIME, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
    };
    use windows::core::{
        BOOL, GUID, HRESULT, HSTRING, IInspectable, IUnknown, IUnknown_Vtbl, Interface, PCWSTR,
        PWSTR, implement, interface,
    };

    type ActivateCallback = Arc<dyn Fn(ToastActivationEvent) + Send + Sync>;

    // -------------------------------------------------------------------------
    // Focus Assist COM interfaces (port of windows_quiethours.idl)
    // -------------------------------------------------------------------------

    const CLSID_QUIET_HOURS_SETTINGS: GUID =
        GUID::from_u128(0xf53321fa_34f8_4b7f_b9a3_361877cb94cf);

    #[interface("e813fe81-62b6-417d-b951-9d2e08486ac1")]
    unsafe trait IQuietHoursProfile: IUnknown {
        unsafe fn get_display_name(&self, display_name: *mut PWSTR) -> HRESULT;
        unsafe fn get_profile_id(&self, profile_id: *mut PWSTR) -> HRESULT;
        unsafe fn get_setting(&self, setting: i32, value: *mut i32) -> HRESULT;
        unsafe fn put_setting(&self, setting: i32, value: i32) -> HRESULT;
        unsafe fn get_is_customizable(&self, result: *mut BOOL) -> HRESULT;
        unsafe fn get_allowed_contacts(&self, count: *mut u32, contacts: *mut PWSTR) -> HRESULT;
        unsafe fn add_allowed_contact(&self, contact: PCWSTR) -> HRESULT;
        unsafe fn remove_allowed_contact(&self, contact: PCWSTR) -> HRESULT;
        unsafe fn get_allowed_apps(&self, count: *mut u32, apps: *mut *mut PWSTR) -> HRESULT;
    }

    #[interface("b0217783-87b7-422c-b902-5c148c14f150")]
    unsafe trait IQuietMomentsManager: IUnknown {
        unsafe fn get_all_quiet_moment_modes(
            &self,
            count: *mut u32,
            modes: *mut *mut u32,
        ) -> HRESULT;
        unsafe fn get_quiet_moment(
            &self,
            moment_id: u32,
            moment: *mut *mut std::ffi::c_void,
        ) -> HRESULT;
        unsafe fn turn_off_currently_active_quiet_moment(&self) -> HRESULT;
        unsafe fn get_active_quiet_moment(&self, moment_id: *mut u32) -> HRESULT;
    }

    #[interface("6bff4732-81ec-4ffb-ae67-b6c1bc29631f")]
    unsafe trait IQuietHoursSettings: IUnknown {
        unsafe fn get_user_selected_profile(&self, profile_id: *mut PWSTR) -> HRESULT;
        unsafe fn put_user_selected_profile(&self, profile_id: PWSTR) -> HRESULT;
        unsafe fn get_profile(
            &self,
            profile_id: PCWSTR,
            profile: *mut Option<IQuietHoursProfile>,
        ) -> HRESULT;
        unsafe fn get_all_profile_data(
            &self,
            count: *mut u32,
            data: *mut std::ffi::c_void,
        ) -> HRESULT;
        unsafe fn get_display_name_for_profile(
            &self,
            profile_id: PCWSTR,
            display_name: *mut PWSTR,
        ) -> HRESULT;
        unsafe fn get_quiet_moments_manager(
            &self,
            manager: *mut Option<IQuietMomentsManager>,
        ) -> HRESULT;
        unsafe fn get_off_profile_id(&self, profile_id: *mut PWSTR) -> HRESULT;
        unsafe fn get_active_quiet_moment_profile(&self, profile_id: *mut PWSTR) -> HRESULT;
        unsafe fn put_active_quiet_moment_profile(&self, profile_id: PWSTR) -> HRESULT;
        unsafe fn get_active_profile(&self, profile_id: *mut PWSTR) -> HRESULT;
    }

    unsafe fn take_cotaskmem_string(ptr: PWSTR) -> Option<String> {
        if ptr.0.is_null() {
            return None;
        }
        // SAFETY: `ptr` is a valid null-terminated CoTaskMem string returned by COM.
        let s = unsafe { ptr.to_string() }.ok();
        // SAFETY: Freeing CoTaskMem string.
        unsafe { CoTaskMemFree(Some(ptr.0 as *const _)) };
        s.filter(|v| !v.is_empty())
    }

    pub(crate) fn query_focus_assist(app_user_model_id: &str) -> FocusAssistProfile {
        let _com = ComGuard::init_sta();
        // SAFETY: Instantiating the Windows QuietHoursSettings out-of-proc COM server.
        let Ok(settings) = (unsafe {
            CoCreateInstance::<_, IQuietHoursSettings>(
                &CLSID_QUIET_HOURS_SETTINGS,
                None,
                CLSCTX_LOCAL_SERVER,
            )
        }) else {
            return FocusAssistProfile::Unavailable;
        };

        let mut best_profile: Option<String> = None;
        let mut consider = |candidate: Option<String>| {
            if let Some(name) = candidate {
                let replace = match &best_profile {
                    None => true,
                    Some(cur) => {
                        classify_focus_assist_profile_name(&name)
                            > classify_focus_assist_profile_name(cur)
                    }
                };
                if replace {
                    best_profile = Some(name);
                }
            }
        };

        // Check ActiveProfile, ActiveQuietMomentProfile (when momentId != 0), and UserSelectedProfile
        // just like `notifications_manager_win.cpp` lines 331-347.
        // SAFETY: All out-params are initialized and freed via CoTaskMemFree.
        unsafe {
            let mut active = PWSTR::null();
            if settings.get_active_profile(&mut active).is_ok() {
                consider(take_cotaskmem_string(active));
            }

            let mut moments: Option<IQuietMomentsManager> = None;
            let mut moment_id: u32 = 0;
            if settings.get_quiet_moments_manager(&mut moments).is_ok()
                && let Some(m) = moments
                && m.get_active_quiet_moment(&mut moment_id).is_ok()
                && moment_id != 0
            {
                let mut moment_profile = PWSTR::null();
                if settings
                    .get_active_quiet_moment_profile(&mut moment_profile)
                    .is_ok()
                {
                    consider(take_cotaskmem_string(moment_profile));
                }
            }

            let mut selected = PWSTR::null();
            if settings.get_user_selected_profile(&mut selected).is_ok() {
                consider(take_cotaskmem_string(selected));
            }
        }

        let Some(profile_name) = best_profile else {
            return FocusAssistProfile::Unrestricted;
        };

        match classify_focus_assist_profile_name(&profile_name) {
            2 => FocusAssistProfile::AlarmsOnly,
            0 => FocusAssistProfile::Unrestricted,
            _ => {
                // Priority Only: check whether our AUMID is in GetAllowedApps.
                let profile_w = crate::app_id::win_impl::to_wide(&profile_name);
                let mut profile_obj: Option<IQuietHoursProfile> = None;
                // SAFETY: Calling GetProfile with null-terminated profile ID.
                let ok = unsafe {
                    settings
                        .get_profile(PCWSTR(profile_w.as_ptr()), &mut profile_obj)
                        .is_ok()
                };
                if !ok {
                    return FocusAssistProfile::PriorityOnly { app_allowed: false };
                }
                let Some(profile) = profile_obj else {
                    return FocusAssistProfile::PriorityOnly { app_allowed: false };
                };

                let mut count: u32 = 0;
                let mut apps_ptr: *mut PWSTR = std::ptr::null_mut();
                // SAFETY: Reading CoTaskMem-allocated array of CoTaskMem-allocated PWSTRs.
                if unsafe { profile.get_allowed_apps(&mut count, &mut apps_ptr) }.is_err()
                    || apps_ptr.is_null()
                {
                    return FocusAssistProfile::PriorityOnly { app_allowed: false };
                }

                let mut allowed = false;
                // SAFETY: `apps_ptr` points to `count` PWSTR entries allocated with CoTaskMemAlloc.
                unsafe {
                    let slice = std::slice::from_raw_parts(apps_ptr, count as usize);
                    for &entry in slice {
                        if let Some(app_id) = take_cotaskmem_string(entry)
                            && app_id.eq_ignore_ascii_case(app_user_model_id)
                        {
                            allowed = true;
                        }
                    }
                    CoTaskMemFree(Some(apps_ptr.cast()));
                }
                FocusAssistProfile::PriorityOnly {
                    app_allowed: allowed,
                }
            }
        }
    }

    pub(crate) fn query_user_notification_state() -> UserNotificationState {
        // SAFETY: Pure Shell query returning QUERY_USER_NOTIFICATION_STATE.
        match unsafe { SHQueryUserNotificationState() } {
            Ok(QUNS_NOT_PRESENT) => UserNotificationState::NotPresent,
            Ok(QUNS_BUSY) => UserNotificationState::Busy,
            Ok(QUNS_RUNNING_D3D_FULL_SCREEN) => UserNotificationState::RunningD3dFullScreen,
            Ok(QUNS_PRESENTATION_MODE) => UserNotificationState::PresentationMode,
            Ok(QUNS_ACCEPTS_NOTIFICATIONS) => UserNotificationState::AcceptsNotifications,
            Ok(QUNS_QUIET_TIME) => UserNotificationState::QuietTime,
            Ok(QUNS_APP) => UserNotificationState::App,
            Ok(other) => UserNotificationState::Unknown(other.0),
            Err(_) => UserNotificationState::AcceptsNotifications,
        }
    }

    pub(crate) fn query_quiet_hours_registry_in(root: &RegistryRoot) -> bool {
        if root.prefix.is_empty() {
            let ver = crate::system_info::windows_version();
            // Matching `UseQuietHoursRegistryEntry` in `notifications_manager_win.cpp`.
            if !(ver.major == 10 && ver.minor == 0 && ver.build < 17134) {
                return false;
            }
        }
        read_reg_dword(
            root,
            "Software\\Microsoft\\Windows\\CurrentVersion\\Notifications\\Settings",
            "NOC_GLOBAL_SETTING_TOASTS_ENABLED",
        ) == Some(0)
    }

    pub(crate) fn register_toast_activator_com_in(
        root: &RegistryRoot,
        clsid: &str,
        executable: &Path,
    ) -> Result<()> {
        let subkey = format!("Software\\Classes\\CLSID\\{clsid}\\LocalServer32");
        let key = open_reg_key(root, &subkey, true)?
            .ok_or_else(|| PlatformError::Registry(format!("failed to create '{subkey}'")))?;
        let exe = executable.to_string_lossy().replace('/', "\\");
        let cmd = format!("\"{exe}\"");
        set_reg_sz(key.0, None, &cmd)
    }

    pub(crate) fn unregister_toast_activator_com_in(
        root: &RegistryRoot,
        clsid: &str,
    ) -> Result<()> {
        remove_reg_key(
            root,
            &format!("Software\\Classes\\CLSID\\{clsid}\\LocalServer32"),
        );
        remove_reg_key(root, &format!("Software\\Classes\\CLSID\\{clsid}"));
        Ok(())
    }

    // -------------------------------------------------------------------------
    // COM INotificationActivationCallback (port of windows_toast_activator.cpp)
    // -------------------------------------------------------------------------

    #[implement(INotificationActivationCallback)]
    struct ToastActivatorCom {
        callback: ActivateCallback,
    }

    impl INotificationActivationCallback_Impl for ToastActivatorCom_Impl {
        fn Activate(
            &self,
            _appusermodelid: &PCWSTR,
            invokedargs: &PCWSTR,
            data: *const NOTIFICATION_USER_INPUT_DATA,
            count: u32,
        ) -> windows::core::Result<()> {
            // SAFETY: `invokedargs` is a COM-provided null-terminated UTF-16 string (may be null).
            let args = if invokedargs.is_null() {
                String::new()
            } else {
                unsafe { invokedargs.to_string() }.unwrap_or_default()
            };

            let mut inputs = Vec::with_capacity(count as usize);
            if !data.is_null() && count > 0 {
                // SAFETY: `data` points to `count` valid NOTIFICATION_USER_INPUT_DATA structs.
                let slice = unsafe { std::slice::from_raw_parts(data, count as usize) };
                for item in slice {
                    let k = if item.Key.is_null() {
                        String::new()
                    } else {
                        unsafe { item.Key.to_string() }.unwrap_or_default()
                    };
                    let v = if item.Value.is_null() {
                        String::new()
                    } else {
                        unsafe { item.Value.to_string() }.unwrap_or_default()
                    };
                    inputs.push((k, v));
                }
            }

            if let Some(event) = parse_toast_activation(&args, &inputs) {
                (self.callback)(event);
            }
            Ok(())
        }
    }

    #[implement(IClassFactory)]
    struct ToastActivatorFactory {
        callback: ActivateCallback,
    }

    impl IClassFactory_Impl for ToastActivatorFactory_Impl {
        fn CreateInstance(
            &self,
            punkouter: windows::core::Ref<'_, IUnknown>,
            riid: *const GUID,
            ppvobject: *mut *mut std::ffi::c_void,
        ) -> windows::core::Result<()> {
            if !punkouter.is_null() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            if riid.is_null() || ppvobject.is_null() {
                return Err(E_NOINTERFACE.into());
            }
            let obj: INotificationActivationCallback = ToastActivatorCom {
                callback: Arc::clone(&self.callback),
            }
            .into();
            // SAFETY: Delegating QueryInterface to our COM-implemented object.
            unsafe { obj.query(riid, ppvobject) }.ok()
        }

        fn LockServer(&self, _flock: BOOL) -> windows::core::Result<()> {
            Ok(())
        }
    }

    // -------------------------------------------------------------------------
    // WinRT ToastManager
    // -------------------------------------------------------------------------

    type TrackedMap = BTreeMap<ContextId, BTreeMap<i32, ToastNotification>>;

    pub(crate) struct WinToastManager {
        notifier: ToastNotifier,
        tracked: Arc<Mutex<TrackedMap>>,
        on_activate: ActivateCallback,
        com_cookie: Option<u32>,
    }

    impl WinToastManager {
        pub(crate) fn new(
            app_user_model_id: String,
            on_activate: ActivateCallback,
        ) -> Result<Self> {
            ensure_process_mta();
            let aumid_h = HSTRING::from(&app_user_model_id);
            let notifier = ToastNotificationManager::CreateToastNotifierWithId(&aumid_h)
                .map_err(|e| PlatformError::from_win("CreateToastNotifierWithId", e))?;

            // Best-effort registration of the in-process COM class factory for Action Center clicks.
            let com_cookie = guid_from_str(TOAST_ACTIVATOR_CLSID_STR)
                .ok()
                .and_then(|clsid| {
                    let factory: IClassFactory = ToastActivatorFactory {
                        callback: Arc::clone(&on_activate),
                    }
                    .into();
                    // SAFETY: Registering our IClassFactory for the toast activator CLSID.
                    unsafe {
                        CoRegisterClassObject(
                            &clsid,
                            &factory,
                            CLSCTX_LOCAL_SERVER,
                            REGCLS_MULTIPLEUSE,
                        )
                    }
                    .ok()
                });

            Ok(Self {
                notifier,
                tracked: Arc::new(Mutex::new(BTreeMap::new())),
                on_activate,
                com_cookie,
            })
        }

        pub(crate) fn show(&self, content: &ToastContent) -> Result<()> {
            let pid = std::process::id();
            let xml_str = build_toast_xml(pid, content);
            let doc =
                XmlDocument::new().map_err(|e| PlatformError::from_win("XmlDocument::new", e))?;
            doc.LoadXml(&HSTRING::from(&xml_str))
                .map_err(|e| PlatformError::from_win("XmlDocument::LoadXml", e))?;

            let toast = ToastNotification::CreateToastNotification(&doc).map_err(|e| {
                PlatformError::from_win("ToastNotification::CreateToastNotification", e)
            })?;

            // Attach in-process Activated handler matching notifications_manager_win.cpp lines 857-891.
            let cb = Arc::clone(&self.on_activate);
            let fallback_args = format!("action=open&{}", format_toast_id_params(pid, &content.id));
            let tracked_act = Arc::clone(&self.tracked);
            let notif_id = content.id;

            let _ = toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
                move |_sender, inspectable| {
                    let mut raw_args = fallback_args.clone();
                    let mut inputs = Vec::new();
                    if let Some(obj) = inspectable.as_ref()
                        && let Ok(args) = obj.cast::<ToastActivatedEventArgs>()
                    {
                        if let Ok(arg_str) = args.Arguments() {
                            let s = arg_str.to_string_lossy();
                            if !s.is_empty() {
                                raw_args = s;
                            }
                        }
                        if let Ok(user_input) = args.UserInput() {
                            let key = HSTRING::from("fastReply");
                            if let Ok(val_obj) = user_input.Lookup(&key)
                                && let Ok(val_ref) = val_obj.cast::<IReference<HSTRING>>()
                                && let Ok(text) = val_ref.Value()
                            {
                                inputs.push(("fastReply".to_string(), text.to_string_lossy()));
                            }
                        }
                    }

                    remove_tracked(&tracked_act, notif_id);
                    if let Some(event) = parse_toast_activation(&raw_args, &inputs) {
                        cb(event);
                    }
                    Ok(())
                },
            ));

            let tracked_dis = Arc::clone(&self.tracked);
            let _ = toast.Dismissed(&TypedEventHandler::<
                ToastNotification,
                ToastDismissedEventArgs,
            >::new(move |_sender, args| {
                if let Some(a) = args.as_ref()
                    && let Ok(reason) = a.Reason()
                    && reason == ToastDismissalReason::UserCanceled
                {
                    remove_tracked(&tracked_dis, notif_id);
                }
                Ok(())
            }));

            let tracked_fail = Arc::clone(&self.tracked);
            let _ = toast.Failed(
                &TypedEventHandler::<ToastNotification, ToastFailedEventArgs>::new(
                    move |_sender, _args| {
                        remove_tracked(&tracked_fail, notif_id);
                        Ok(())
                    },
                ),
            );

            // Replace any existing toast for the same message.
            self.clear_item(content.id.context, content.id.msg_id);

            self.notifier
                .Show(&toast)
                .map_err(|e| PlatformError::from_win("ToastNotifier::Show", e))?;

            if let Ok(mut map) = self.tracked.lock() {
                map.entry(content.id.context)
                    .or_default()
                    .insert(content.id.msg_id, toast);
            }
            Ok(())
        }

        fn try_hide(&self, toast: &ToastNotification) {
            let _ = self.notifier.Hide(toast);
        }

        pub(crate) fn clear_item(&self, context: ContextId, msg_id: i32) {
            let removed = self.tracked.lock().ok().and_then(|mut map| {
                let item = map.get_mut(&context).and_then(|m| m.remove(&msg_id));
                if map.get(&context).is_some_and(BTreeMap::is_empty) {
                    map.remove(&context);
                }
                item
            });
            if let Some(toast) = removed {
                self.try_hide(&toast);
            }
        }

        pub(crate) fn clear_topic(&self, session_id: u64, peer_id: i64, topic_root_id: i32) {
            let to_hide = self.take_matching(|k| {
                k.session_id == session_id
                    && k.peer_id == peer_id
                    && k.topic_root_id == topic_root_id
            });
            for toast in to_hide {
                self.try_hide(&toast);
            }
        }

        pub(crate) fn clear_chat(&self, session_id: u64, peer_id: i64) {
            let to_hide =
                self.take_matching(|k| k.session_id == session_id && k.peer_id == peer_id);
            for toast in to_hide {
                self.try_hide(&toast);
            }
        }

        pub(crate) fn clear_session(&self, session_id: u64) {
            let to_hide = self.take_matching(|k| k.session_id == session_id);
            for toast in to_hide {
                self.try_hide(&toast);
            }
        }

        pub(crate) fn clear_all(&self) {
            let to_hide = self.take_matching(|_| true);
            for toast in to_hide {
                self.try_hide(&toast);
            }
        }

        pub(crate) fn active_count(&self) -> usize {
            self.tracked
                .lock()
                .map(|m| m.values().map(BTreeMap::len).sum())
                .unwrap_or(0)
        }

        fn take_matching(&self, pred: impl Fn(&ContextId) -> bool) -> Vec<ToastNotification> {
            let mut out = Vec::new();
            if let Ok(mut map) = self.tracked.lock() {
                let keys: Vec<ContextId> = map.keys().copied().filter(&pred).collect();
                for k in keys {
                    if let Some(inner) = map.remove(&k) {
                        out.extend(inner.into_values());
                    }
                }
            }
            out
        }
    }

    impl Drop for WinToastManager {
        fn drop(&mut self) {
            self.clear_all();
            if let Some(cookie) = self.com_cookie.take() {
                // SAFETY: Revoking our registered COM class object cookie once.
                let _ = unsafe { CoRevokeClassObject(cookie) };
            }
        }
    }

    fn remove_tracked(tracked: &Mutex<TrackedMap>, id: NotificationId) {
        if let Ok(mut map) = tracked.lock()
            && let Some(inner) = map.get_mut(&id.context)
        {
            inner.remove(&id.msg_id);
            if inner.is_empty() {
                map.remove(&id.context);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_id() -> NotificationId {
        NotificationId {
            context: ContextId {
                session_id: 1,
                peer_id: 123456,
                topic_root_id: 42,
                monoforum_peer_id: 0,
            },
            msg_id: 99,
        }
    }

    #[test]
    fn toast_xml_escapes_and_includes_actions() {
        let mut content = ToastContent::simple(sample_id(), "Alice & Bob", "Hello <world>!");
        content.subtitle = Some("General".into());
        content.image_path = Some(PathBuf::from("C:\\temp\\avatar.png"));
        let xml = build_toast_xml(777, &content);

        assert!(xml.contains("launch=\"action=open&amp;pid=777&amp;session=1&amp;peer=123456&amp;topic=42&amp;monoforumpeer=0&amp;msg=99\""));
        assert!(xml.contains("file:///C:/temp/avatar.png"));
        assert!(xml.contains("Alice &amp; Bob"));
        assert!(xml.contains("Hello &lt;world&gt;!"));
        assert!(xml.contains("<text>General</text>"));
        assert!(xml.contains("id=\"fastReply\""));
        assert!(xml.contains("action=reply&amp;"));
        assert!(xml.contains("action=mark&amp;"));
        assert!(xml.contains("<audio silent=\"true\"/>"));
    }

    #[test]
    fn toast_xml_respects_hide_options() {
        let mut content = ToastContent::simple(sample_id(), "Title", "Body");
        content.image_path = Some(PathBuf::from("C:\\a.png"));
        content.options = DisplayOptions {
            hide_reply_button: true,
            hide_mark_as_read: true,
            hide_name_and_photo: true,
        };
        let xml = build_toast_xml(10, &content);
        assert!(!xml.contains("<actions>"));
        assert!(!xml.contains("appLogoOverride"));
    }

    #[test]
    fn activation_parsing_handles_open_reply_and_mark() {
        let id_str = format_toast_id_params(555, &sample_id());

        let open = parse_toast_activation(
            &format!("action=open&{id_str}"),
            &[("fastReply".into(), "draft\rline2".into())],
        )
        .unwrap();
        assert_eq!(open.pid, 555);
        assert_eq!(open.id, sample_id());
        assert_eq!(open.action, ToastAction::Open);
        assert_eq!(open.draft.as_deref(), Some("draft\nline2"));

        let reply = parse_toast_activation(
            &format!("action=reply&{id_str}"),
            &[("fastReply".into(), "On my way!".into())],
        )
        .unwrap();
        assert_eq!(reply.action, ToastAction::Reply("On my way!".into()));

        let mark = parse_toast_activation(&format!("action=mark&{id_str}"), &[]).unwrap();
        assert_eq!(mark.action, ToastAction::MarkAsRead);

        // Missing required IDs returns None.
        assert!(parse_toast_activation("action=open&pid=1", &[]).is_none());
    }

    #[test]
    fn focus_assist_and_suppression_policy() {
        assert_eq!(
            classify_focus_assist_profile_name("Microsoft.QuietHoursProfile.AlarmsOnly"),
            2
        );
        assert_eq!(
            classify_focus_assist_profile_name("Microsoft.QuietHoursProfile.PriorityOnly"),
            1
        );
        assert_eq!(
            classify_focus_assist_profile_name("Microsoft.QuietHoursProfile.Unrestricted"),
            0
        );

        let alarms = NotificationSuppressionState {
            user_state: UserNotificationState::AcceptsNotifications,
            focus_assist: FocusAssistProfile::AlarmsOnly,
            quiet_hours_registry: false,
        };
        assert!(alarms.focus_assist_blocks());
        assert!(alarms.should_skip_toast(true));
        assert!(!alarms.should_skip_toast(false));

        let allowed = NotificationSuppressionState {
            user_state: UserNotificationState::AcceptsNotifications,
            focus_assist: FocusAssistProfile::PriorityOnly { app_allowed: true },
            quiet_hours_registry: false,
        };
        assert!(!allowed.focus_assist_blocks());
        assert!(!allowed.should_skip_toast(true));
    }

    #[cfg(windows)]
    #[test]
    fn quiet_hours_and_com_registration_in_throwaway_key() {
        use crate::registration::win_reg::write_reg_dword;

        let root = RegistryRoot::hkcu_subkey(format!(
            "Software\\RustgramTest\\notif_{}",
            std::process::id()
        ));
        let _ = root.delete_tree();

        assert!(!query_quiet_hours_registry_in(&root));
        write_reg_dword(
            &root,
            "Software\\Microsoft\\Windows\\CurrentVersion\\Notifications\\Settings",
            "NOC_GLOBAL_SETTING_TOASTS_ENABLED",
            0,
        )
        .unwrap();
        assert!(query_quiet_hours_registry_in(&root));

        let clsid = crate::app_id::TOAST_ACTIVATOR_CLSID_STR;
        let exe = PathBuf::from("C:\\Rustgram\\rustgram.exe");
        register_toast_activator_com_in(&root, clsid, &exe).unwrap();
        unregister_toast_activator_com_in(&root, clsid).unwrap();

        // Read-only system queries must not crash.
        let _ = query_notification_suppression("Rustgram");

        root.delete_tree().unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "touches real Start Menu shortcut and displays a live Windows toast notification"]
    fn show_and_clear_live_windows_toast() {
        let aumid = crate::app_id::DEFAULT_APP_USER_MODEL_ID;
        crate::app_id::set_current_process_app_id(aumid).unwrap();
        let exe = crate::app_id::current_executable_path().unwrap();
        let programs = crate::app_id::start_menu_programs_dir().unwrap();
        let work_dir = exe.parent().unwrap().to_path_buf();
        let cfg = crate::app_id::ShortcutConfig::for_executable(exe, work_dir);
        crate::app_id::validate_or_create_shortcut_in(&programs, &cfg).unwrap();

        let mgr = ToastManager::new(aumid, |_| {}).unwrap();
        let content =
            ToastContent::simple(sample_id(), "Rustgram Test", "Live toast integration check");
        mgr.show(&content).unwrap();
        assert_eq!(mgr.active_count(), 1);
        mgr.clear_chat(sample_id().context.session_id, sample_id().context.peer_id);
        assert_eq!(mgr.active_count(), 0);
    }
}
