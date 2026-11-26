//! Core notification decision engine, rate limiting, grouping, privacy, and platform switch.
//!
//! Port of `Telegram/SourceFiles/window/notifications_manager.h`,
//! `Telegram/SourceFiles/window/notifications_manager.cpp`, and Windows toast integration from
//! `Telegram/SourceFiles/platform/win/notifications_manager_win.cpp` wired to `tg-platform`
//! and `tg-audio`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tg_platform::app_id::DEFAULT_APP_USER_MODEL_ID;
use tg_platform::notifications::{
    ContextId, DisplayOptions as PlatformDisplayOptions, FocusAssistProfile, NotificationId,
    NotificationSuppressionState, ToastAction, ToastActivationEvent, ToastContent, ToastManager,
    UserNotificationState, query_notification_suppression,
};
use tg_platform::tray::{flash_window, stop_flash_window};

use crate::notifications::backend::{
    IncomingMessageNotification, IncomingNotifyEvent, ItemNotificationType, NotificationMediaKind,
    NotifyBackend, TextSpoilerRange,
};
use crate::notifications::custom_popup::{CustomPopupContent, CustomPopupManager, PopupAction};
use crate::notifications::settings::{NotificationPreferences, NotifySettingsStore, NotifyView};
use crate::notifications::sound::NotificationSoundPlayer;
use crate::notifications::strings;
use crate::notifications::utilities::{CachedUserpics, UserpicKey, initials_from_title};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};

/// Minimal delay before dispatching a standard notification (`kMinimalDelay = 100ms`).
pub const MINIMAL_DELAY_MS: i64 = 100;

/// Minimal delay for forwarded messages so batches can coalesce (`kMinimalForwardDelay = 500ms`).
pub const MINIMAL_FORWARD_DELAY_MS: i64 = 500;

/// Per-peer rate limit for notification sounds and taskbar flashes (`kMinimalAlertDelay = 500ms`).
pub const MINIMAL_ALERT_DELAY_MS: i64 = 500;

/// Wait window for forwarded / album grouped messages (`kWaitingForAllGroupedDelay = 1000ms`).
pub const WAITING_FOR_ALL_GROUPED_DELAY_MS: i64 = 1_000;

/// Deduplication window for reaction and poll-vote notifications (`kReactionNotificationEach = 1h`).
pub const REACTION_NOTIFICATION_EACH_MS: i64 = 60 * 60 * 1_000;

/// Interval between Windows Focus Assist / QUNS polls (`kQuerySettingsEachMs = 1000ms`).
pub const QUERY_SUPPRESSION_EACH_MS: i64 = 1_000;

/// Default delay when another session is online and user is inactive (`notifyCloudDelay = 30s`).
pub const DEFAULT_NOTIFY_CLOUD_DELAY_MS: i64 = 30_000;

/// Default delay when another session was online recently (`notifyDefaultDelay = 1.5s`).
pub const DEFAULT_NOTIFY_DEFAULT_DELAY_MS: i64 = 1_500;

/// Cloud online freshness threshold (`onlineCloudTimeout = 300s`).
pub const DEFAULT_ONLINE_CLOUD_TIMEOUT_MS: i64 = 300_000;

/// Maximum notification preview text length (`kNotificationTextLimit = 255`).
pub const NOTIFICATION_TEXT_LIMIT: usize = 255;

/// Maximum pinned snippet length (`kPinnedMessageTextLimit = 16`).
pub const PINNED_MESSAGE_TEXT_LIMIT: usize = 16;

/// Active notification backend mode (`Window::Notifications::ManagerType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerType {
    /// No-op manager when notifications cannot be shown.
    Dummy,
    /// Custom in-app stacked notification popups (`notifications_manager_default.cpp`).
    Default,
    /// Native OS notifications (`tg_platform::notifications::ToastManager`).
    Native,
}

/// Settings mutation events (`Window::Notifications::ChangeType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeType {
    /// Sound on/off changed.
    SoundEnabled,
    /// Taskbar flash on/off changed.
    FlashBounceEnabled,
    /// Include muted chats in unread badge changed.
    IncludeMuted,
    /// Count unread messages vs chats changed.
    CountMessages,
    /// Desktop notifications enabled/disabled changed.
    DesktopEnabled,
    /// Privacy view (`ShowPreview` / `ShowName` / `ShowNothing`) changed.
    ViewParams,
    /// Maximum custom notification count changed.
    MaxCount,
    /// Custom notification screen corner changed.
    Corner,
    /// Demo notification preview shown.
    DemoIsShown,
    /// Demo notification preview hidden.
    DemoIsHidden,
    /// Native vs custom notification mode changed.
    NativeEnabled,
}

/// Live window focus, lock, and multi-device online context used by notification decisions.
#[derive(Debug, Clone, Default)]
pub struct WindowFocusState {
    /// `true` when the main Rustgram window currently has OS input focus.
    pub window_focused: bool,
    /// `true` when the window is minimized or hidden to tray.
    pub window_minimized: bool,
    /// `true` when local passcode lock screen is active (`Core::App().passcodeLocked()`).
    pub passcode_locked: bool,
    /// `true` when the OS desktop session is locked (`Core::App().screenIsLocked()`).
    pub screen_locked: bool,
    /// `true` when the application is shutting down (`Core::Quitting()`).
    pub quitting: bool,
    /// Active account session ID in the foreground window.
    pub active_session_id: u64,
    /// Currently open chat peer in the foreground window.
    pub active_peer_id: Option<PeerId>,
    /// Currently open forum topic root ID (`MsgId(0)` for regular chat view).
    pub active_topic_root_id: MsgId,
    /// Native Win32 `HWND` for `FlashWindowEx` (if available).
    pub hwnd: Option<isize>,
    /// Timestamp (Unix seconds) until which another Telegram client is reported online (`cOtherOnline`).
    pub other_online_unix: i64,
    /// Whether this client was most recently online (`updates.lastWasOnline()`).
    pub last_was_online: bool,
    /// Monotonic ms when this client last set online (`updates.lastSetOnline()`).
    pub last_set_online_ms: i64,
    /// Monotonic ms of the last OS user input event (`lastNonIdleTime`).
    pub last_user_input_ms: Option<i64>,
    /// Server-configured cloud delay in ms.
    pub notify_cloud_delay_ms: i64,
    /// Server-configured default other-online delay in ms.
    pub notify_default_delay_ms: i64,
    /// Server-configured online cloud timeout in ms.
    pub online_cloud_timeout_ms: i64,
}

impl WindowFocusState {
    /// Creates a default active foreground window state with standard server delay timings.
    #[must_use]
    pub fn with_defaults(active_session_id: u64) -> Self {
        Self {
            active_session_id,
            notify_cloud_delay_ms: DEFAULT_NOTIFY_CLOUD_DELAY_MS,
            notify_default_delay_ms: DEFAULT_NOTIFY_DEFAULT_DELAY_MS,
            online_cloud_timeout_ms: DEFAULT_ONLINE_CLOUD_TIMEOUT_MS,
            ..Self::default()
        }
    }

    /// Returns `true` if the user is actively viewing `(session_id, peer_id, topic_root_id)`
    /// in a focused, unlocked, non-minimized window.
    #[must_use]
    pub fn is_viewing_chat(&self, session_id: u64, peer_id: PeerId, topic_root_id: MsgId) -> bool {
        self.window_focused
            && !self.window_minimized
            && !self.passcode_locked
            && !self.screen_locked
            && self.active_session_id == session_id
            && self.active_peer_id == Some(peer_id)
            && (self.active_topic_root_id.0 == 0 || self.active_topic_root_id == topic_root_id)
    }
}

/// Reason a notification candidate was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Application is quitting.
    Quitting,
    /// Target chat is open in a focused window.
    ActiveChatFocused,
    /// Account is not active and `notify_from_all` is disabled.
    InactiveAccount,
    /// Global temporary mute (e.g. "Mute for 1 hour") is active.
    GloballyMuted,
    /// Message is outgoing and not a scheduled reminder.
    OutgoingNonScheduled,
    /// User is not a member of the channel.
    NotInChannel,
    /// Silent contact signup or imported forward (`HistoryItem::skipNotification()`).
    ItemFlagSkipped,
    /// Chat/topic is muted and no unmuted mention/reply/reminder applies.
    Muted,
    /// Mentioning/reacting sender is blocked or muted.
    SenderBlockedOrMuted,
    /// Duplicate reaction or poll-vote notification within 1 hour.
    RateLimitedDuplicate,
    /// Reaction/poll-vote suppressed because privacy hides sender name and photo.
    HiddenByPrivacy,
}

/// Decision produced by [`NotificationSystem::evaluate_skip`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipDecision {
    /// Do not notify for this event.
    Skip(SkipReason),
    /// Peer/scope notification settings are still unknown; hold in `_setting_waiters`.
    WaitForSettings,
    /// Proceed with notification (possibly silent, i.e. toast only without sound/flash).
    Notify {
        /// `true` when sound and taskbar flash should be suppressed.
        silent: bool,
    },
}

/// Per-notification visibility and action flags (`Manager::DisplayOptions`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NotificationDisplayOptions {
    /// Hide sender name and avatar photo.
    pub hide_name_and_photo: bool,
    /// Hide message preview body text.
    pub hide_message_text: bool,
    /// Hide the "Mark as read" action button.
    pub hide_mark_as_read: bool,
    /// Hide the inline quick-reply box / button.
    pub hide_reply_button: bool,
    /// Obfuscate 5+ digit login codes (`spoilerLoginCode`).
    pub spoiler_login_code: bool,
}

impl NotificationDisplayOptions {
    /// Converts to `tg_platform::notifications::DisplayOptions`.
    #[must_use]
    pub const fn to_platform(self) -> PlatformDisplayOptions {
        PlatformDisplayOptions {
            hide_reply_button: self.hide_reply_button,
            hide_mark_as_read: self.hide_mark_as_read,
            hide_name_and_photo: self.hide_name_and_photo,
        }
    }
}

/// Fully formatted notification ready for dispatch to native toast or custom popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedNotification {
    /// Target notification ID `(context, msg_id)`.
    pub id: NotificationId,
    /// Primary title line (chat name, topic + chat, or `"Rustgram"`).
    pub title: String,
    /// Optional subtitle line (sender name in group or reaction sender).
    pub subtitle: Option<String>,
    /// Formatted message body (with spoilers masked, grouped summary, reaction text, etc.).
    pub message: String,
    /// Display privacy & action flags.
    pub options: NotificationDisplayOptions,
    /// Optional temporary PNG avatar path for native Windows toasts.
    pub image_path: Option<PathBuf>,
    /// Fallback initials for custom in-app popups.
    pub initials: String,
    /// Fallback avatar RGB tint for custom in-app popups.
    pub avatar_rgb: [u8; 3],
    /// Number of collapsed messages (`1` for single message, `>1` for burst or forwarded batch).
    pub collapsed_count: usize,
}

/// Abstraction over Focus Assist / Quiet Hours / `SHQueryUserNotificationState` checks.
pub trait SuppressionProvider: Send {
    /// Queries the current OS notification suppression snapshot.
    fn query(&mut self, now_ms: i64) -> NotificationSuppressionState;
}

/// Real Windows suppression provider backed by `tg_platform::notifications::query_notification_suppression`
/// with 1-second caching (`ShouldQuerySettings` in `notifications_manager_win.cpp:60-67`).
#[derive(Debug)]
pub struct PlatformSuppressionProvider {
    aumid: String,
    last_query_ms: Option<i64>,
    cached: NotificationSuppressionState,
}

impl PlatformSuppressionProvider {
    /// Creates a provider querying Focus Assist for `aumid`.
    #[must_use]
    pub fn new(aumid: impl Into<String>) -> Self {
        Self {
            aumid: aumid.into(),
            last_query_ms: None,
            cached: NotificationSuppressionState {
                user_state: UserNotificationState::AcceptsNotifications,
                focus_assist: FocusAssistProfile::Unrestricted,
                quiet_hours_registry: false,
            },
        }
    }
}

impl Default for PlatformSuppressionProvider {
    fn default() -> Self {
        Self::new(DEFAULT_APP_USER_MODEL_ID)
    }
}

impl SuppressionProvider for PlatformSuppressionProvider {
    fn query(&mut self, now_ms: i64) -> NotificationSuppressionState {
        let should_query = match self.last_query_ms {
            None => true,
            Some(prev) => now_ms > prev.saturating_add(QUERY_SUPPRESSION_EACH_MS),
        };
        if should_query {
            self.cached = query_notification_suppression(&self.aumid);
            self.last_query_ms = Some(now_ms);
        }
        self.cached
    }
}

/// Deterministic mock [`SuppressionProvider`] for unit tests.
#[derive(Debug, Clone)]
pub struct MockSuppressionProvider {
    /// State returned on every `query()` call.
    pub state: NotificationSuppressionState,
}

impl Default for MockSuppressionProvider {
    fn default() -> Self {
        Self {
            state: NotificationSuppressionState {
                user_state: UserNotificationState::AcceptsNotifications,
                focus_assist: FocusAssistProfile::Unrestricted,
                quiet_hours_registry: false,
            },
        }
    }
}

impl SuppressionProvider for MockSuppressionProvider {
    fn query(&mut self, _now_ms: i64) -> NotificationSuppressionState {
        self.state
    }
}

/// Abstraction over taskbar window flashing (`FlashWindowEx`).
pub trait WindowFlasher: Send {
    /// Flashes the application window taskbar button.
    fn flash(&mut self, hwnd: Option<isize>);
    /// Stops any active window flashing.
    fn stop_flash(&mut self, hwnd: Option<isize>);
}

/// Real Windows flasher calling `tg_platform::tray::{flash_window, stop_flash_window}`.
#[derive(Debug, Default)]
pub struct PlatformWindowFlasher;

impl WindowFlasher for PlatformWindowFlasher {
    fn flash(&mut self, hwnd: Option<isize>) {
        if let Some(h) = hwnd
            && h != 0
            && let Err(err) = flash_window(h, 0, 0)
        {
            tracing::debug!(%err, "FlashWindowEx failed");
        }
    }

    fn stop_flash(&mut self, hwnd: Option<isize>) {
        if let Some(h) = hwnd
            && h != 0
        {
            let _ = stop_flash_window(h);
        }
    }
}

/// Recording mock [`WindowFlasher`] for unit tests.
#[derive(Debug, Default)]
pub struct MockWindowFlasher {
    /// Number of `flash()` calls recorded.
    pub flash_count: usize,
    /// Number of `stop_flash()` calls recorded.
    pub stop_count: usize,
}

impl WindowFlasher for MockWindowFlasher {
    fn flash(&mut self, _hwnd: Option<isize>) {
        self.flash_count += 1;
    }

    fn stop_flash(&mut self, _hwnd: Option<isize>) {
        self.stop_count += 1;
    }
}

/// Native Windows toast backend wrapper with a thread-safe activation event queue.
pub struct NativeToastBackend {
    manager: ToastManager,
    events: Arc<Mutex<VecDeque<ToastActivationEvent>>>,
}

impl std::fmt::Debug for NativeToastBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeToastBackend")
            .field("active_count", &self.manager.active_count())
            .finish()
    }
}

impl NativeToastBackend {
    /// Initializes a WinRT [`ToastManager`] bound to `aumid` and captures activation callbacks.
    pub fn new(aumid: impl Into<String>) -> tg_platform::Result<Self> {
        let events: Arc<Mutex<VecDeque<ToastActivationEvent>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let sink = Arc::clone(&events);
        let manager = ToastManager::new(aumid, move |ev| {
            if let Ok(mut queue) = sink.lock() {
                queue.push_back(ev);
            }
        })?;
        Ok(Self { manager, events })
    }

    /// Shows a native Windows toast notification.
    pub fn show(&self, rendered: &RenderedNotification) -> tg_platform::Result<()> {
        let mut toast = ToastContent::simple(rendered.id, &rendered.title, &rendered.message);
        toast.subtitle = rendered.subtitle.clone();
        toast.image_path = rendered.image_path.clone();
        toast.options = rendered.options.to_platform();
        self.manager.show(&toast)
    }

    /// Drains all pending Windows toast activation callbacks (click, inline reply, mark as read).
    pub fn drain_activations(&self) -> Vec<ToastActivationEvent> {
        self.events
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }

    /// Clears a single toast.
    pub fn clear_item(&self, context: ContextId, msg_id: i32) {
        self.manager.clear_item(context, msg_id);
    }

    /// Clears all toasts for a forum topic.
    pub fn clear_topic(&self, session_id: u64, peer_id: i64, topic_root_id: i32) {
        self.manager.clear_topic(session_id, peer_id, topic_root_id);
    }

    /// Clears all toasts for a chat.
    pub fn clear_chat(&self, session_id: u64, peer_id: i64) {
        self.manager.clear_chat(session_id, peer_id);
    }

    /// Clears all toasts for a session.
    pub fn clear_session(&self, session_id: u64) {
        self.manager.clear_session(session_id);
    }

    /// Clears all active toasts.
    pub fn clear_all(&self) {
        self.manager.clear_all();
    }
}

/// Unique thread key used for per-chat/topic queues and rate limiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ThreadKey {
    session_id: u64,
    peer_id: i64,
    topic_root_id: i32,
    monoforum_peer_id: i64,
}

impl ThreadKey {
    fn from_notification(n: &IncomingMessageNotification) -> Self {
        Self {
            session_id: n.session_id,
            peer_id: n.peer_id.0,
            topic_root_id: n.topic_root_id.0,
            monoforum_peer_id: n.monoforum_peer_id.0,
        }
    }

    fn to_context_id(self) -> ContextId {
        ContextId {
            session_id: self.session_id,
            peer_id: self.peer_id,
            topic_root_id: self.topic_root_id,
            monoforum_peer_id: self.monoforum_peer_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct SentDedupKey {
    session_id: u64,
    peer_id: i64,
    msg_id: i32,
}

#[derive(Debug, Clone)]
struct PendingNotification {
    when_ms: i64,
    item: IncomingMessageNotification,
}

#[derive(Debug, Clone)]
struct PendingAlert {
    when_ms: i64,
    sound_id: u64,
    volume_pct: u8,
}

/// One account's notification settings, as last synced from its backend.
#[derive(Debug, Clone)]
struct AccountNotifyStore {
    version: u64,
    store: NotifySettingsStore,
}

#[derive(Debug, Clone)]
struct PendingGrouped {
    thread: ThreadKey,
    when_ms: i64,
    last_item: IncomingMessageNotification,
    forwarded_count: usize,
}

/// Returns a peer's loaded photo userpic by full peer id (`tg_ui::userpic_images::userpic_image`
/// on the app's context); `None` while there is none or it is loading.
pub type UserpicPhotoLookup = std::sync::Arc<
    dyn Fn(i64) -> Option<std::sync::Arc<tg_ui::userpic_images::UserpicImage>> + Send + Sync,
>;

/// High-level notification system porting `Window::Notifications::System`.
///
/// Orchestrates:
/// - Decision logic (focus, mute, per-scope defaults, mentions/replies, pinned, reactions, poll votes)
/// - Delay & rate limiting (sound/flash throttle, cloud delay, 1h reaction dedup)
/// - Grouping / collapsing (albums, forwarded batches, multi-message burst `"N new messages"`)
/// - Clear-on-read across native toasts, custom popups, and pending queues
/// - Platform switching (`Native` WinRT toasts vs `Default` custom popups vs `Dummy`)
/// - Audio playback via `tg-audio` and window flashing via `tg-platform`
pub struct NotificationSystem {
    prefs: NotificationPreferences,
    settings_store: NotifySettingsStore,
    /// Per-account stores synced from [`NotifyBackend::notify_settings_versions`]; accounts
    /// without one use `settings_store` (tdesktop: `thread->owner().notifySettings()`).
    account_stores: BTreeMap<u64, AccountNotifyStore>,
    manager_type: ManagerType,
    native_supported: bool,
    native_backend: Option<NativeToastBackend>,
    custom_popups: CustomPopupManager,
    userpics: CachedUserpics,
    /// Loaded photo userpics by full peer id, for native toasts.
    userpic_photos: Option<UserpicPhotoLookup>,
    sound_player: NotificationSoundPlayer,
    suppression: Box<dyn SuppressionProvider>,
    flasher: Box<dyn WindowFlasher>,

    pending_by_thread: BTreeMap<ThreadKey, Vec<PendingNotification>>,
    setting_waiters: Vec<IncomingMessageNotification>,
    pending_alerts: BTreeMap<ThreadKey, Vec<PendingAlert>>,
    last_alert_ms_by_thread: BTreeMap<ThreadKey, i64>,
    pending_grouped: Option<PendingGrouped>,
    shown_incoming: BTreeMap<ThreadKey, BTreeSet<i32>>,
    sent_reactions: BTreeMap<SentDedupKey, i64>,
    sent_poll_votes: BTreeMap<SentDedupKey, i64>,
    dispatched_log: Vec<RenderedNotification>,
    change_log: Vec<ChangeType>,
}

impl std::fmt::Debug for NotificationSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotificationSystem")
            .field("manager_type", &self.manager_type)
            .field("native_supported", &self.native_supported)
            .field("pending_threads", &self.pending_by_thread.len())
            .field("setting_waiters", &self.setting_waiters.len())
            .field("dispatched_count", &self.dispatched_log.len())
            .finish()
    }
}

impl NotificationSystem {
    /// Creates a production [`NotificationSystem`] wired to `tg-platform` (WinRT toasts,
    /// Focus Assist, `FlashWindowEx`) and `tg-audio`.
    #[must_use]
    pub fn new(prefs: NotificationPreferences, settings_store: NotifySettingsStore) -> Self {
        let native_backend = NativeToastBackend::new(DEFAULT_APP_USER_MODEL_ID).ok();
        let native_supported = cfg!(windows) && native_backend.is_some();
        let mut system = Self::with_components(
            prefs,
            settings_store,
            native_supported,
            native_backend,
            CachedUserpics::default(),
            NotificationSoundPlayer::default(),
            Box::new(PlatformSuppressionProvider::default()),
            Box::new(PlatformWindowFlasher),
        );
        system.select_manager(0);
        system
    }

    /// Creates a deterministic headless [`NotificationSystem`] for unit tests (never touches
    /// WinRT toasts, audio hardware, Focus Assist COM, or real user directories).
    #[must_use]
    pub fn headless(
        prefs: NotificationPreferences,
        settings_store: NotifySettingsStore,
        temp_userpic_dir: PathBuf,
    ) -> Self {
        let mut system = Self::with_components(
            prefs,
            settings_store,
            false,
            None,
            CachedUserpics::new(temp_userpic_dir),
            NotificationSoundPlayer::headless(),
            Box::new(MockSuppressionProvider::default()),
            Box::new(MockWindowFlasher::default()),
        );
        system.select_manager(0);
        system
    }

    /// Sets where native toasts take loaded photo userpics from (the app's userpic source);
    /// without one they show the empty userpic.
    pub fn set_userpic_photo_lookup(&mut self, lookup: Option<UserpicPhotoLookup>) {
        self.userpic_photos = lookup;
    }

    /// Whether [`Self::set_userpic_photo_lookup`] installed a lookup.
    #[must_use]
    pub fn has_userpic_photo_lookup(&self) -> bool {
        self.userpic_photos.is_some()
    }

    /// Constructs a [`NotificationSystem`] with explicit injected platform components.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn with_components(
        prefs: NotificationPreferences,
        settings_store: NotifySettingsStore,
        native_supported: bool,
        native_backend: Option<NativeToastBackend>,
        userpics: CachedUserpics,
        sound_player: NotificationSoundPlayer,
        suppression: Box<dyn SuppressionProvider>,
        flasher: Box<dyn WindowFlasher>,
    ) -> Self {
        let custom_popups = CustomPopupManager::new(
            prefs.clamped_notifications_count(),
            prefs.notifications_corner,
        );
        Self {
            prefs,
            settings_store,
            account_stores: BTreeMap::new(),
            manager_type: ManagerType::Dummy,
            native_supported,
            native_backend,
            custom_popups,
            userpics,
            userpic_photos: None,
            sound_player,
            suppression,
            flasher,
            pending_by_thread: BTreeMap::new(),
            setting_waiters: Vec::new(),
            pending_alerts: BTreeMap::new(),
            last_alert_ms_by_thread: BTreeMap::new(),
            pending_grouped: None,
            shown_incoming: BTreeMap::new(),
            sent_reactions: BTreeMap::new(),
            sent_poll_votes: BTreeMap::new(),
            dispatched_log: Vec::new(),
            change_log: Vec::new(),
        }
    }

    /// Returns current global notification preferences.
    #[must_use]
    pub const fn preferences(&self) -> &NotificationPreferences {
        &self.prefs
    }

    /// Returns mutable global notification preferences.
    pub fn preferences_mut(&mut self) -> &mut NotificationPreferences {
        &mut self.prefs
    }

    /// Returns the per-peer / per-scope notification settings store.
    #[must_use]
    pub const fn settings_store(&self) -> &NotifySettingsStore {
        &self.settings_store
    }

    /// Returns mutable access to the per-peer / per-scope notification settings store.
    pub fn settings_store_mut(&mut self) -> &mut NotifySettingsStore {
        &mut self.settings_store
    }

    /// Returns the store synced for account `session_id`, if its backend provides one.
    #[must_use]
    pub fn account_settings_store(&self, session_id: u64) -> Option<&NotifySettingsStore> {
        self.account_stores.get(&session_id).map(|a| &a.store)
    }

    /// The settings that decide notifications of account `session_id`.
    fn store_for(&self, session_id: u64) -> &NotifySettingsStore {
        self.account_settings_store(session_id)
            .unwrap_or(&self.settings_store)
    }

    /// Re-syncs every account store whose settings version moved. A new account starts with
    /// unknown defaults, so its notifications wait until the defaults are loaded. Returns `true`
    /// if any store changed.
    pub fn sync_account_stores(&mut self, backend: &dyn NotifyBackend, now_unix: i64) -> bool {
        let versions = backend.notify_settings_versions();
        self.account_stores
            .retain(|session_id, _| versions.iter().any(|(id, _)| id == session_id));
        let mut synced = false;
        for (session_id, version) in versions {
            let entry = self.account_stores.entry(session_id);
            let account = match entry {
                std::collections::btree_map::Entry::Occupied(o) => {
                    let account = o.into_mut();
                    if account.version == version {
                        continue;
                    }
                    account
                }
                std::collections::btree_map::Entry::Vacant(v) => v.insert(AccountNotifyStore {
                    version,
                    store: NotifySettingsStore::empty_unknown(),
                }),
            };
            account.version = version;
            backend.sync_notify_settings(session_id, &mut account.store, now_unix);
            synced = true;
        }
        synced
    }

    /// Returns the currently active manager type (`Native`, `Default`, or `Dummy`).
    #[must_use]
    pub const fn manager_type(&self) -> ManagerType {
        self.manager_type
    }

    /// Returns immutable access to the custom in-app popup manager.
    #[must_use]
    pub const fn custom_popups(&self) -> &CustomPopupManager {
        &self.custom_popups
    }

    /// Returns mutable access to the custom in-app popup manager.
    pub fn custom_popups_mut(&mut self) -> &mut CustomPopupManager {
        &mut self.custom_popups
    }

    /// Returns immutable access to the `tg-audio` notification sound player.
    #[must_use]
    pub const fn sound_player(&self) -> &NotificationSoundPlayer {
        &self.sound_player
    }

    /// Returns mutable access to the `tg-audio` notification sound player.
    pub fn sound_player_mut(&mut self) -> &mut NotificationSoundPlayer {
        &mut self.sound_player
    }

    /// Replaces the Focus Assist / suppression provider (useful in tests).
    pub fn set_suppression_provider(&mut self, provider: Box<dyn SuppressionProvider>) {
        self.suppression = provider;
    }

    /// Replaces the taskbar window flasher (useful in tests).
    pub fn set_window_flasher(&mut self, flasher: Box<dyn WindowFlasher>) {
        self.flasher = flasher;
    }

    /// Returns all rendered notifications dispatched since creation or last [`Self::clear_dispatched_log`].
    #[must_use]
    pub fn dispatched_notifications(&self) -> &[RenderedNotification] {
        &self.dispatched_log
    }

    /// Clears the diagnostic dispatched notification log.
    pub fn clear_dispatched_log(&mut self) {
        self.dispatched_log.clear();
    }

    /// Drains emitted [`ChangeType`] events.
    pub fn take_changes(&mut self) -> Vec<ChangeType> {
        std::mem::take(&mut self.change_log)
    }

    /// Applies the platform switch (`System::setManager` in `notifications_manager.cpp:244-274`).
    ///
    /// - If `native_notifications` is on (and `force_custom_notifications` is off) and the OS
    ///   supports native toasts, selects [`ManagerType::Native`].
    /// - Otherwise selects [`ManagerType::Default`] (custom in-app notification popups).
    pub fn select_manager(&mut self, now_ms: i64) -> ManagerType {
        let desired = if self.prefs.native_notifications
            && !self.prefs.force_custom_notifications
            && self.native_supported
        {
            ManagerType::Native
        } else {
            ManagerType::Default
        };

        if self.manager_type != desired {
            self.clear_displayed_only(now_ms);
            self.manager_type = desired;
        }
        self.custom_popups.apply_preferences(&self.prefs, now_ms);
        self.manager_type
    }

    /// Explicitly overrides manager type (e.g. `Dummy` for headless disable or testing platform switch).
    pub fn set_manager_type(&mut self, manager_type: ManagerType, now_ms: i64) {
        if self.manager_type != manager_type {
            self.clear_displayed_only(now_ms);
            self.manager_type = manager_type;
        }
    }

    /// Applies a [`ChangeType`] notification settings update (`System::notifySettingsChanged`).
    pub fn notify_settings_changed(&mut self, change: ChangeType, now_ms: i64) {
        self.change_log.push(change);
        match change {
            ChangeType::DesktopEnabled => {
                if !self.prefs.desktop_notify {
                    self.clear_all(now_ms);
                }
            }
            ChangeType::NativeEnabled => {
                self.select_manager(now_ms);
            }
            ChangeType::Corner => {
                self.custom_popups
                    .set_corner(self.prefs.notifications_corner);
            }
            ChangeType::MaxCount => {
                self.custom_popups
                    .set_max_count(self.prefs.clamped_notifications_count(), now_ms);
            }
            ChangeType::DemoIsShown => {
                self.custom_popups.set_demo_shown(true);
            }
            ChangeType::DemoIsHidden => {
                self.custom_popups.set_demo_shown(false);
            }
            ChangeType::ViewParams => {
                if self.manager_type == ManagerType::Native
                    && let Some(ref native) = self.native_backend
                {
                    native.clear_all();
                }
            }
            ChangeType::SoundEnabled
            | ChangeType::FlashBounceEnabled
            | ChangeType::IncludeMuted
            | ChangeType::CountMessages => {}
        }
    }

    /// Evaluates whether `notif` should notify, wait for unknown settings, or be skipped
    /// (`System::skipNotification` + `System::computeSkipState` in `notifications_manager.cpp:319-397`).
    #[must_use]
    pub fn evaluate_skip(
        &self,
        notif: &IncomingMessageNotification,
        focus: &WindowFocusState,
        now_ms: i64,
        now_unix: i64,
    ) -> SkipDecision {
        if focus.quitting {
            return SkipDecision::Skip(SkipReason::Quitting);
        }

        if self.prefs.is_globally_muted(now_unix) {
            return SkipDecision::Skip(SkipReason::GloballyMuted);
        }

        if !self.prefs.notify_from_all && notif.session_id != focus.active_session_id {
            return SkipDecision::Skip(SkipReason::InactiveAccount);
        }

        if !notif.am_in_chat {
            return SkipDecision::Skip(SkipReason::NotInChannel);
        }

        let is_message_like = matches!(
            notif.notification_type,
            ItemNotificationType::Message | ItemNotificationType::PinnedMessage
        );

        // Skip if chat is currently open and window is focused & unlocked.
        if focus.is_viewing_chat(notif.session_id, notif.peer_id, notif.topic_root_id) {
            return SkipDecision::Skip(SkipReason::ActiveChatFocused);
        }

        // Outgoing messages only notify when they are scheduled reminders (`HistoryItem::showNotification`).
        if is_message_like && (notif.outgoing || notif.is_self_chat) && !notif.is_from_scheduled {
            return SkipDecision::Skip(SkipReason::OutgoingNonScheduled);
        }

        // Item-level flags (`HistoryItem::skipNotification`).
        if is_message_like
            && ((notif.silent && notif.is_contact_signup) || notif.is_imported_forward)
        {
            return SkipDecision::Skip(SkipReason::ItemFlagSkipped);
        }

        // Privacy rule: reactions & poll votes are skipped when sender name/photo is hidden.
        let hide_everything = focus.passcode_locked || focus.screen_locked;
        if !is_message_like && (hide_everything || self.prefs.notify_view > NotifyView::ShowName) {
            return SkipDecision::Skip(SkipReason::HiddenByPrivacy);
        }

        // 1-hour deduplication for reactions and poll votes (`skipSentNotification`).
        let dedup_key = SentDedupKey {
            session_id: notif.session_id,
            peer_id: notif.peer_id.0,
            msg_id: notif.msg_id.0,
        };
        match notif.notification_type {
            ItemNotificationType::Reaction => {
                if self.is_sent_recently(&self.sent_reactions, dedup_key, now_ms) {
                    return SkipDecision::Skip(SkipReason::RateLimitedDuplicate);
                }
            }
            ItemNotificationType::PollVote => {
                if self.is_sent_recently(&self.sent_poll_votes, dedup_key, now_ms) {
                    return SkipDecision::Skip(SkipReason::RateLimitedDuplicate);
                }
            }
            ItemNotificationType::Message | ItemNotificationType::PinnedMessage => {}
        }

        // Scheduled reminders notify even in muted chats (silently).
        let show_for_muted = is_message_like && notif.outgoing && notif.is_from_scheduled;

        // Special notification peer: the author of a message that mentions me in a non-private
        // chat, or the reacting/voting sender (`HistoryItem::specialNotificationPeer`,
        // `history/history_item.cpp:2270-2274`). Replies to my messages count through the
        // server's `mentioned` flag, as in tdesktop; a local reply guess does not.
        let notify_by: Option<PeerId> = if is_message_like {
            if notif.mentions_me && notif.peer_kind != PeerKind::User {
                Some(notif.sender_id)
            } else {
                None
            }
        } else {
            Some(notif.sender_id)
        };

        let store = self.store_for(notif.session_id);
        let sound_none = store
            .sound_for_topic(notif.peer_id, notif.topic_root_id, notif.is_self_chat)
            .none;

        let make_notify = |force_silent: bool| SkipDecision::Notify {
            silent: force_silent || !is_message_like || notif.silent || sound_none,
        };

        // 1. Check thread/chat mute status for regular/pinned messages.
        if is_message_like {
            if store.topic_mute_unknown(notif.peer_id, notif.topic_root_id) {
                return SkipDecision::WaitForSettings;
            }
            let thread_muted = store.is_topic_muted(notif.peer_id, notif.topic_root_id, now_unix);
            if !thread_muted {
                // Silent pinned messages still show a silent toast unless chat is muted.
                return make_notify(false);
            }
        }

        // 2. Chat is muted (or this is a reaction/poll vote): check `notify_by`.
        let Some(sender_peer) = notify_by else {
            return if show_for_muted {
                make_notify(true)
            } else {
                SkipDecision::Skip(SkipReason::Muted)
            };
        };

        if store.mute_unknown(sender_peer) {
            return SkipDecision::WaitForSettings;
        }

        let sender_muted = store.is_peer_muted(sender_peer, now_unix);
        let sender_allowed = !sender_muted && (is_message_like || !notif.sender_blocked);
        if sender_allowed {
            make_notify(false)
        } else if show_for_muted {
            make_notify(true)
        } else {
            SkipDecision::Skip(SkipReason::SenderBlockedOrMuted)
        }
    }

    /// Computes dispatch delay (`System::countTiming` in `notifications_manager.cpp:399-419`).
    #[must_use]
    pub fn compute_timing(
        &self,
        notif: &IncomingMessageNotification,
        focus: &WindowFocusState,
        now_ms: i64,
        now_unix: i64,
    ) -> (i64, i64) {
        let minimal_delay = match notif.notification_type {
            ItemNotificationType::Reaction | ItemNotificationType::PollVote => MINIMAL_DELAY_MS,
            ItemNotificationType::Message | ItemNotificationType::PinnedMessage => {
                if notif.is_forwarded {
                    MINIMAL_FORWARD_DELAY_MS
                } else {
                    MINIMAL_DELAY_MS
                }
            }
        };

        let mut delay = minimal_delay;
        let now_sec_ms = now_unix.saturating_mul(1000);
        let other_ms = focus.other_online_unix.saturating_mul(1000);
        let cloud_timeout = if focus.online_cloud_timeout_ms > 0 {
            focus.online_cloud_timeout_ms
        } else {
            DEFAULT_ONLINE_CLOUD_TIMEOUT_MS
        };
        let other_not_old = other_ms.saturating_add(cloud_timeout) > now_sec_ms;
        let other_later_than_me =
            other_ms.saturating_add(now_ms.saturating_sub(focus.last_set_online_ms)) > now_sec_ms;

        if !focus.last_was_online && other_not_old && other_later_than_me {
            delay = if focus.notify_cloud_delay_ms > 0 {
                focus.notify_cloud_delay_ms
            } else {
                DEFAULT_NOTIFY_CLOUD_DELAY_MS
            };
        } else if focus.other_online_unix >= now_unix {
            delay = if focus.notify_default_delay_ms > 0 {
                focus.notify_default_delay_ms
            } else {
                DEFAULT_NOTIFY_DEFAULT_DELAY_MS
            };
        }

        (delay, now_ms.saturating_add(delay))
    }

    /// Schedules an incoming notification candidate (`System::schedule`).
    pub fn schedule(
        &mut self,
        notif: IncomingMessageNotification,
        focus: &WindowFocusState,
        now_ms: i64,
        now_unix: i64,
    ) -> SkipDecision {
        let decision = self.evaluate_skip(&notif, focus, now_ms, now_unix);
        match decision {
            SkipDecision::Skip(_) => return decision,
            SkipDecision::WaitForSettings => {
                self.setting_waiters.push(notif);
                return decision;
            }
            SkipDecision::Notify { silent } => {
                // Record reaction/poll-vote dedup timestamp once accepted.
                let dedup_key = SentDedupKey {
                    session_id: notif.session_id,
                    peer_id: notif.peer_id.0,
                    msg_id: notif.msg_id.0,
                };
                match notif.notification_type {
                    ItemNotificationType::Reaction => {
                        Self::prune_sent_map(&mut self.sent_reactions, now_ms);
                        self.sent_reactions.insert(dedup_key, now_ms);
                    }
                    ItemNotificationType::PollVote => {
                        Self::prune_sent_map(&mut self.sent_poll_votes, now_ms);
                        self.sent_poll_votes.insert(dedup_key, now_ms);
                    }
                    ItemNotificationType::Message | ItemNotificationType::PinnedMessage => {}
                }

                let (_delay, when_ms) = self.compute_timing(&notif, focus, now_ms, now_unix);
                let thread = ThreadKey::from_notification(&notif);

                if !silent {
                    let store = self.store_for(notif.session_id);
                    let sound = store.sound_for_topic(
                        notif.peer_id,
                        notif.topic_root_id,
                        notif.is_self_chat,
                    );
                    if !sound.none {
                        let vol = store
                            .ringtone_volume_override(notif.peer_id)
                            .or_else(|| self.settings_store.ringtone_volume_override(notif.peer_id))
                            .unwrap_or(self.prefs.notifications_volume_pct);
                        self.pending_alerts
                            .entry(thread)
                            .or_default()
                            .push(PendingAlert {
                                when_ms,
                                sound_id: sound.id,
                                volume_pct: vol,
                            });
                    }
                }

                if self.prefs.desktop_notify {
                    self.pending_by_thread
                        .entry(thread)
                        .or_default()
                        .push(PendingNotification {
                            when_ms,
                            item: notif,
                        });
                }
            }
        }
        decision
    }

    /// Re-evaluates notifications held in `setting_waiters` after settings arrive (`System::checkDelayed`).
    pub fn check_delayed(&mut self, focus: &WindowFocusState, now_ms: i64, now_unix: i64) {
        let waiters = std::mem::take(&mut self.setting_waiters);
        for item in waiters {
            let _ = self.schedule(item, focus, now_ms, now_unix);
        }
    }

    /// Drains events from `backend`, expires temporary mutes, dispatches ready notifications,
    /// plays rate-limited sounds / taskbar flashes, and routes user activations back to `backend`.
    pub fn tick(
        &mut self,
        backend: &mut dyn NotifyBackend,
        focus: &WindowFocusState,
        now_ms: i64,
        now_unix: i64,
    ) {
        // 1. Expire temporary global mute & per-peer timed mutes.
        let _ = self.prefs.expire_global_mute(now_unix);
        let _ = self.settings_store.unmute_by_finished(now_unix);
        for account in self.account_stores.values_mut() {
            let _ = account.store.unmute_by_finished(now_unix);
        }

        // 2. Drain incoming backend events, then bring each account's mute view up to date
        // before judging them: a mute change that arrived before a message is always seen.
        let events = backend.poll_events();
        let settings_synced = self.sync_account_stores(&*backend, now_unix);
        if settings_synced && !self.setting_waiters.is_empty() {
            self.check_delayed(focus, now_ms, now_unix);
        }
        for event in events {
            match event {
                IncomingNotifyEvent::Notification(n) => {
                    let _ = self.schedule(*n, focus, now_ms, now_unix);
                }
                IncomingNotifyEvent::HistoryRead {
                    session_id,
                    peer_id,
                    read_till_id,
                } => {
                    self.clear_read_from_history(session_id, peer_id, read_till_id, now_ms);
                }
                IncomingNotifyEvent::TopicRead {
                    session_id,
                    peer_id,
                    topic_root_id,
                    read_till_id,
                } => {
                    self.clear_read_from_topic(
                        session_id,
                        peer_id,
                        topic_root_id,
                        read_till_id,
                        now_ms,
                    );
                }
                IncomingNotifyEvent::MessageDeleted {
                    session_id,
                    peer_id,
                    msg_id,
                } => {
                    self.clear_from_item(session_id, peer_id, MsgId(0), msg_id, now_ms);
                }
            }
        }

        // 3. Route any native Windows toast activations back to `backend`.
        let native_activations = self
            .native_backend
            .as_ref()
            .map(NativeToastBackend::drain_activations)
            .unwrap_or_default();
        for ev in native_activations {
            self.handle_toast_activation(ev, backend, focus, now_ms);
        }

        // 4. Query Windows Focus Assist / QUNS state (cached every 1s).
        let suppression = self.suppression.query(now_ms);

        // 5. Process due audio / flash alerts with per-peer 500ms rate limiting (`MINIMAL_ALERT_DELAY_MS`).
        self.process_due_alerts(focus, suppression, now_ms);

        // 6. Flush due grouped/forwarded batch if its 1s wait window elapsed.
        if let Some(ref grouped) = self.pending_grouped
            && grouped.when_ms <= now_ms
        {
            self.flush_pending_grouped(backend, focus, suppression, now_ms);
        }

        // 7. Dispatch due desktop notifications (skipping if desktop_notify off or Focus Assist blocks).
        let skip_toast = !self.prefs.desktop_notify
            || self.manager_type == ManagerType::Dummy
            || suppression.should_skip_toast(self.prefs.skip_in_focus_assist);
        if !skip_toast {
            self.process_due_notifications(backend, focus, suppression, now_ms);
        }

        // 8. Advance custom popup animations & clean expired temporary avatar PNGs.
        self.custom_popups.tick(
            now_ms,
            focus.last_user_input_ms,
            suppression.should_wait_for_input(),
        );
        let _ = self.userpics.clear_expired(now_ms);
        self.sound_player.detach_if_idle();
    }

    /// Renders custom in-app popups (when `manager_type == ManagerType::Default`) and applies
    /// any user actions (click, quick-reply, mark as read, hide all) to `backend`.
    pub fn show_custom_popups_egui(
        &mut self,
        ctx: &egui::Context,
        palette: &tg_app_core::stand_in::theme::Palette,
        backend: &mut dyn NotifyBackend,
        focus: &WindowFocusState,
        now_ms: i64,
    ) -> Vec<PopupAction> {
        if self.manager_type != ManagerType::Default {
            return Vec::new();
        }
        let actions = self.custom_popups.show_egui(ctx, palette, now_ms);
        for action in &actions {
            if focus.passcode_locked {
                if matches!(action, PopupAction::HideAll) {
                    self.clear_all(now_ms);
                }
                continue;
            }
            match action {
                PopupAction::Activate { id, draft } => {
                    backend.open_chat(
                        id.context.session_id,
                        PeerId(id.context.peer_id),
                        MsgId(id.context.topic_root_id),
                        MsgId(id.msg_id),
                        draft.clone(),
                    );
                    self.clear_from_history(
                        id.context.session_id,
                        PeerId(id.context.peer_id),
                        now_ms,
                    );
                }
                PopupAction::Reply { id, text } => {
                    backend.send_quick_reply(
                        id.context.session_id,
                        PeerId(id.context.peer_id),
                        MsgId(id.context.topic_root_id),
                        MsgId(id.msg_id),
                        text.clone(),
                    );
                    self.clear_from_item(
                        id.context.session_id,
                        PeerId(id.context.peer_id),
                        MsgId(id.context.topic_root_id),
                        MsgId(id.msg_id),
                        now_ms,
                    );
                }
                PopupAction::MarkAsRead(id) => {
                    backend.mark_read(
                        id.context.session_id,
                        PeerId(id.context.peer_id),
                        MsgId(id.context.topic_root_id),
                        MsgId(id.msg_id),
                    );
                    self.clear_read_from_history(
                        id.context.session_id,
                        PeerId(id.context.peer_id),
                        MsgId(id.msg_id),
                        now_ms,
                    );
                }
                PopupAction::Dismiss(id) => {
                    self.clear_from_item(
                        id.context.session_id,
                        PeerId(id.context.peer_id),
                        MsgId(id.context.topic_root_id),
                        MsgId(id.msg_id),
                        now_ms,
                    );
                }
                PopupAction::HideAll => {
                    self.clear_all(now_ms);
                }
            }
        }
        actions
    }

    /// Computes [`NotificationDisplayOptions`] for `notif` (`Manager::getNotificationOptions`).
    #[must_use]
    pub fn display_options(
        &self,
        notif: &IncomingMessageNotification,
        focus: &WindowFocusState,
    ) -> NotificationDisplayOptions {
        let hide_everything = focus.passcode_locked || focus.screen_locked;
        let peer_previews = self
            .store_for(notif.session_id)
            .show_previews_for_peer(notif.peer_id);
        let hide_name_and_photo = hide_everything || self.prefs.notify_view > NotifyView::ShowName;
        let hide_message_text =
            hide_everything || !peer_previews || self.prefs.notify_view > NotifyView::ShowPreview;
        let is_message_like = matches!(
            notif.notification_type,
            ItemNotificationType::Message | ItemNotificationType::PinnedMessage
        );
        let hide_mark_as_read = hide_message_text
            || !is_message_like
            || ((notif.outgoing || notif.is_self_chat) && notif.is_from_scheduled);
        let hide_reply_button = hide_mark_as_read
            || !notif.can_send_texts
            || notif.is_broadcast
            || notif.slowmode_seconds_left > 0
            || notif.stars_per_message > 0
            || self.prefs.hide_reply_button;
        let spoiler_login_code = !notif.outgoing && notif.is_notifications_service;

        NotificationDisplayOptions {
            hide_name_and_photo,
            hide_message_text,
            hide_mark_as_read,
            hide_reply_button,
            spoiler_login_code,
        }
    }

    /// Clears a single notification from pending queues and displayed popups/toasts.
    pub fn clear_from_item(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        msg_id: MsgId,
        now_ms: i64,
    ) {
        for queue in self.pending_by_thread.values_mut() {
            queue.retain(|p| {
                !(p.item.session_id == session_id
                    && p.item.peer_id == peer_id
                    && p.item.msg_id == msg_id)
            });
        }
        self.pending_by_thread.retain(|_, q| !q.is_empty());

        let ctx = ContextId {
            session_id,
            peer_id: peer_id.0,
            topic_root_id: topic_root_id.0,
            monoforum_peer_id: 0,
        };
        if let Some(ref native) = self.native_backend {
            native.clear_item(ctx, msg_id.0);
        }
        self.custom_popups.clear_item(
            NotificationId {
                context: ctx,
                msg_id: msg_id.0,
            },
            now_ms,
        );
    }

    /// Clears all pending and shown incoming notifications in `(session_id, peer_id)` with
    /// `msg_id <= read_till_id` (`clearReadIncoming`).
    pub fn clear_read_from_history(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        read_till_id: MsgId,
        now_ms: i64,
    ) {
        if read_till_id == MsgId::SHOW_AT_END {
            self.clear_from_history(session_id, peer_id, now_ms);
            return;
        }
        for (thread, queue) in &mut self.pending_by_thread {
            if thread.session_id == session_id && thread.peer_id == peer_id.0 {
                queue.retain(|p| p.item.outgoing || p.item.msg_id > read_till_id);
            }
        }
        self.pending_by_thread.retain(|_, q| !q.is_empty());

        if let Some(ref grouped) = self.pending_grouped
            && grouped.thread.session_id == session_id
            && grouped.thread.peer_id == peer_id.0
            && grouped.last_item.msg_id <= read_till_id
        {
            self.pending_grouped = None;
        }

        let mut to_hide = Vec::new();
        for (thread, shown) in &mut self.shown_incoming {
            if thread.session_id == session_id && thread.peer_id == peer_id.0 {
                let expired: Vec<i32> = shown
                    .iter()
                    .copied()
                    .filter(|&id| id <= read_till_id.0)
                    .collect();
                for id in expired {
                    shown.remove(&id);
                    to_hide.push((*thread, id));
                }
            }
        }
        self.shown_incoming.retain(|_, s| !s.is_empty());

        // Clear newest first (`notifications_manager.cpp:710-713`).
        for (thread, msg_id) in to_hide.into_iter().rev() {
            let ctx = thread.to_context_id();
            if let Some(ref native) = self.native_backend {
                native.clear_item(ctx, msg_id);
            }
            self.custom_popups.clear_item(
                NotificationId {
                    context: ctx,
                    msg_id,
                },
                now_ms,
            );
        }
    }

    /// Clears all pending and shown incoming notifications in a forum topic with `msg_id <= read_till_id`.
    pub fn clear_read_from_topic(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        read_till_id: MsgId,
        now_ms: i64,
    ) {
        if read_till_id == MsgId::SHOW_AT_END {
            self.clear_from_topic(session_id, peer_id, topic_root_id, now_ms);
            return;
        }
        for (thread, queue) in &mut self.pending_by_thread {
            if thread.session_id == session_id
                && thread.peer_id == peer_id.0
                && thread.topic_root_id == topic_root_id.0
            {
                queue.retain(|p| p.item.outgoing || p.item.msg_id > read_till_id);
            }
        }
        self.pending_by_thread.retain(|_, q| !q.is_empty());

        let mut to_hide = Vec::new();
        for (thread, shown) in &mut self.shown_incoming {
            if thread.session_id == session_id
                && thread.peer_id == peer_id.0
                && thread.topic_root_id == topic_root_id.0
            {
                let expired: Vec<i32> = shown
                    .iter()
                    .copied()
                    .filter(|&id| id <= read_till_id.0)
                    .collect();
                for id in expired {
                    shown.remove(&id);
                    to_hide.push((*thread, id));
                }
            }
        }
        self.shown_incoming.retain(|_, s| !s.is_empty());

        for (thread, msg_id) in to_hide.into_iter().rev() {
            let ctx = thread.to_context_id();
            if let Some(ref native) = self.native_backend {
                native.clear_item(ctx, msg_id);
            }
            self.custom_popups.clear_item(
                NotificationId {
                    context: ctx,
                    msg_id,
                },
                now_ms,
            );
        }
    }

    /// Clears all notifications for a specific forum topic (`clearFromTopic`).
    pub fn clear_from_topic(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        now_ms: i64,
    ) {
        let matches = |t: &ThreadKey| {
            t.session_id == session_id
                && t.peer_id == peer_id.0
                && t.topic_root_id == topic_root_id.0
        };
        self.pending_by_thread.retain(|t, _| !matches(t));
        self.pending_alerts.retain(|t, _| !matches(t));
        self.shown_incoming.retain(|t, _| !matches(t));
        if self
            .pending_grouped
            .as_ref()
            .is_some_and(|g| matches(&g.thread))
        {
            self.pending_grouped = None;
        }
        if let Some(ref native) = self.native_backend {
            native.clear_topic(session_id, peer_id.0, topic_root_id.0);
        }
        self.custom_popups
            .clear_topic(session_id, peer_id.0, topic_root_id.0, now_ms);
    }

    /// Clears all notifications for a chat across all topics (`clearFromHistory`).
    pub fn clear_from_history(&mut self, session_id: u64, peer_id: PeerId, now_ms: i64) {
        let matches = |t: &ThreadKey| t.session_id == session_id && t.peer_id == peer_id.0;
        self.pending_by_thread.retain(|t, _| !matches(t));
        self.pending_alerts.retain(|t, _| !matches(t));
        self.shown_incoming.retain(|t, _| !matches(t));
        self.setting_waiters
            .retain(|n| !(n.session_id == session_id && n.peer_id == peer_id));
        if self
            .pending_grouped
            .as_ref()
            .is_some_and(|g| matches(&g.thread))
        {
            self.pending_grouped = None;
        }
        if let Some(ref native) = self.native_backend {
            native.clear_chat(session_id, peer_id.0);
        }
        self.custom_popups.clear_chat(session_id, peer_id.0, now_ms);
    }

    /// Clears all notifications for an account session (`clearFromSession`).
    pub fn clear_from_session(&mut self, session_id: u64, now_ms: i64) {
        let matches = |t: &ThreadKey| t.session_id == session_id;
        self.pending_by_thread.retain(|t, _| !matches(t));
        self.pending_alerts.retain(|t, _| !matches(t));
        self.shown_incoming.retain(|t, _| !matches(t));
        self.setting_waiters.retain(|n| n.session_id != session_id);
        if self
            .pending_grouped
            .as_ref()
            .is_some_and(|g| matches(&g.thread))
        {
            self.pending_grouped = None;
        }
        if let Some(ref native) = self.native_backend {
            native.clear_session(session_id);
        }
        self.custom_popups.clear_session(session_id, now_ms);
    }

    /// Smoothly clears all pending and displayed notifications (`clearAll`).
    pub fn clear_all(&mut self, now_ms: i64) {
        self.pending_by_thread.clear();
        self.pending_alerts.clear();
        self.setting_waiters.clear();
        self.shown_incoming.clear();
        self.pending_grouped = None;
        self.clear_displayed_only(now_ms);
    }

    /// Immediately clears everything without waiting for fade-out (`clearAllFast`).
    pub fn clear_all_fast(&mut self) {
        self.pending_by_thread.clear();
        self.pending_alerts.clear();
        self.setting_waiters.clear();
        self.shown_incoming.clear();
        self.pending_grouped = None;
        if let Some(ref native) = self.native_backend {
            native.clear_all();
        }
        self.custom_popups.clear_all_fast();
    }

    /// Stops any active taskbar flash when the user focuses the window.
    pub fn stop_window_flash(&mut self, hwnd: Option<isize>) {
        self.flasher.stop_flash(hwnd);
    }

    fn clear_displayed_only(&mut self, now_ms: i64) {
        if let Some(ref native) = self.native_backend {
            native.clear_all();
        }
        self.custom_popups.clear_all(now_ms);
    }

    fn handle_toast_activation(
        &mut self,
        event: ToastActivationEvent,
        backend: &mut dyn NotifyBackend,
        focus: &WindowFocusState,
        now_ms: i64,
    ) {
        if focus.passcode_locked {
            self.clear_all(now_ms);
            return;
        }
        let session_id = event.id.context.session_id;
        let peer_id = PeerId(event.id.context.peer_id);
        let topic_root_id = MsgId(event.id.context.topic_root_id);
        let msg_id = MsgId(event.id.msg_id);

        match event.action {
            ToastAction::Open => {
                backend.open_chat(session_id, peer_id, topic_root_id, msg_id, event.draft);
                self.clear_from_history(session_id, peer_id, now_ms);
            }
            ToastAction::Reply(reply_text) => {
                backend.send_quick_reply(session_id, peer_id, topic_root_id, msg_id, reply_text);
                self.clear_from_item(session_id, peer_id, topic_root_id, msg_id, now_ms);
            }
            ToastAction::MarkAsRead => {
                backend.mark_read(session_id, peer_id, topic_root_id, msg_id);
                self.clear_read_from_history(session_id, peer_id, msg_id, now_ms);
            }
        }
    }

    fn process_due_alerts(
        &mut self,
        focus: &WindowFocusState,
        suppression: NotificationSuppressionState,
        now_ms: i64,
    ) {
        let mut chosen_alert: Option<PendingAlert> = None;

        for (&thread, alerts) in &mut self.pending_alerts {
            alerts.sort_by_key(|a| a.when_ms);
            let due_count = alerts.iter().take_while(|a| a.when_ms <= now_ms).count();
            if due_count == 0 {
                continue;
            }
            let last_fired = self.last_alert_ms_by_thread.get(&thread).copied();
            let rate_limited =
                last_fired.is_some_and(|prev| now_ms.saturating_sub(prev) < MINIMAL_ALERT_DELAY_MS);

            // Drain all alerts within `now_ms + MINIMAL_ALERT_DELAY_MS` (matches `notifications_manager.cpp:838-841`).
            let first = alerts[0].clone();
            alerts.retain(|a| a.when_ms > now_ms.saturating_add(MINIMAL_ALERT_DELAY_MS));

            if !rate_limited {
                self.last_alert_ms_by_thread.insert(thread, now_ms);
                chosen_alert = Some(first);
            }
        }
        self.pending_alerts.retain(|_, a| !a.is_empty());

        if let Some(alert) = chosen_alert {
            if self.prefs.flash_bounce_notify
                && !suppression.should_skip_flash_bounce(self.prefs.skip_in_focus_assist)
            {
                self.flasher.flash(focus.hwnd);
            }
            if self.prefs.sound_notify
                && !suppression
                    .should_skip_sound(self.prefs.skip_in_focus_assist, focus.screen_locked)
            {
                let _ = self.sound_player.play(alert.sound_id, alert.volume_pct);
            }
        }
    }

    fn process_due_notifications(
        &mut self,
        backend: &dyn NotifyBackend,
        focus: &WindowFocusState,
        suppression: NotificationSuppressionState,
        now_ms: i64,
    ) {
        let threads: Vec<ThreadKey> = self.pending_by_thread.keys().copied().collect();
        for thread in threads {
            let due_items: Vec<PendingNotification> = {
                let Some(queue) = self.pending_by_thread.get_mut(&thread) else {
                    continue;
                };
                queue.sort_by_key(|p| p.when_ms);
                let split = queue.iter().take_while(|p| p.when_ms <= now_ms).count();
                if split == 0 {
                    continue;
                }
                queue.drain(..split).collect()
            };

            // Collapse burst of many plain messages in the same thread into `"N new messages"`.
            let threshold = self.prefs.collapse_burst_threshold.max(2);
            let all_plain_messages = due_items.len() >= threshold
                && due_items.iter().all(|p| {
                    p.item.notification_type == ItemNotificationType::Message
                        && !p.item.is_forwarded
                        && p.item.group_id == 0
                });
            if all_plain_messages {
                self.flush_pending_grouped(backend, focus, suppression, now_ms);
                let count = due_items.len();
                if let Some(last) = due_items.last() {
                    for p in &due_items {
                        self.remember_shown_incoming(thread, &p.item);
                    }
                    let rendered =
                        self.render_notification(&last.item, 0, count, backend, focus, now_ms);
                    self.dispatch_rendered(rendered, suppression, now_ms);
                }
                continue;
            }

            let mut idx = 0usize;
            while idx < due_items.len() {
                let current = &due_items[idx].item;
                let is_message = current.notification_type == ItemNotificationType::Message;
                let is_forwarded = is_message && current.is_forwarded;
                let is_album = is_message && current.group_id != 0;

                if is_forwarded || is_album {
                    let mut last_grouped = current.clone();
                    let mut fwd_count = usize::from(is_forwarded);
                    idx += 1;

                    // Coalesce consecutive items in the same forward batch or album within <2s
                    // (`notifications_manager.cpp:995-1017`).
                    while idx < due_items.len() {
                        let next = &due_items[idx].item;
                        let same_date = (next.date_unix - last_grouped.date_unix).abs() < 2;
                        let same_fwd = is_forwarded
                            && next.notification_type == ItemNotificationType::Message
                            && next.is_forwarded
                            && next.sender_id == last_grouped.sender_id;
                        let same_album = is_album
                            && next.notification_type == ItemNotificationType::Message
                            && next.group_id != 0
                            && next.group_id == last_grouped.group_id;
                        if same_date && (same_fwd || same_album) {
                            if same_fwd {
                                fwd_count += 1;
                            }
                            last_grouped = next.clone();
                            idx += 1;
                        } else {
                            break;
                        }
                    }

                    let same_as_pending = self.pending_grouped.as_ref().is_some_and(|g| {
                        g.thread == thread
                            && ((is_album && g.last_item.group_id == last_grouped.group_id)
                                || (is_forwarded
                                    && g.last_item.sender_id == last_grouped.sender_id))
                    });
                    if self.pending_grouped.is_some() && !same_as_pending {
                        self.flush_pending_grouped(backend, focus, suppression, now_ms);
                    }
                    let total_fwd = self
                        .pending_grouped
                        .as_ref()
                        .map_or(0, |g| g.forwarded_count)
                        + fwd_count;
                    self.pending_grouped = Some(PendingGrouped {
                        thread,
                        when_ms: now_ms.saturating_add(WAITING_FOR_ALL_GROUPED_DELAY_MS),
                        last_item: last_grouped,
                        forwarded_count: total_fwd,
                    });
                } else {
                    self.flush_pending_grouped(backend, focus, suppression, now_ms);
                    self.remember_shown_incoming(thread, current);
                    let rendered = self.render_notification(current, 0, 1, backend, focus, now_ms);
                    self.dispatch_rendered(rendered, suppression, now_ms);
                    idx += 1;
                }
            }
        }
        self.pending_by_thread.retain(|_, q| !q.is_empty());
    }

    fn flush_pending_grouped(
        &mut self,
        backend: &dyn NotifyBackend,
        focus: &WindowFocusState,
        suppression: NotificationSuppressionState,
        now_ms: i64,
    ) {
        let Some(grouped) = self.pending_grouped.take() else {
            return;
        };
        if grouped.forwarded_count < 2 {
            self.remember_shown_incoming(grouped.thread, &grouped.last_item);
        }
        let collapsed = if grouped.forwarded_count > 1 {
            grouped.forwarded_count
        } else {
            1
        };
        let rendered = self.render_notification(
            &grouped.last_item,
            grouped.forwarded_count,
            collapsed,
            backend,
            focus,
            now_ms,
        );
        self.dispatch_rendered(rendered, suppression, now_ms);
    }

    fn render_notification(
        &mut self,
        item: &IncomingMessageNotification,
        forwarded_count: usize,
        collapsed_count: usize,
        backend: &dyn NotifyBackend,
        focus: &WindowFocusState,
        now_ms: i64,
    ) -> RenderedNotification {
        let options = self.display_options(item, focus);
        let peer_info = backend.peer_info(item.peer_id);

        let is_reaction = item.notification_type == ItemNotificationType::Reaction;
        let is_poll_vote = item.notification_type == ItemNotificationType::PollVote;
        let is_pinned =
            item.notification_type == ItemNotificationType::PinnedMessage || item.is_pinned;
        let scheduled = !options.hide_name_and_photo
            && !is_reaction
            && !is_poll_vote
            && (item.outgoing || item.is_self_chat)
            && item.is_from_scheduled;

        let base_title = if options.hide_name_and_photo {
            strings::APP_NAME.to_owned()
        } else if scheduled && item.is_self_chat {
            tg_lang::tr::lng_notification_reminder()
        } else if let Some(ref topic_title) = item.topic_title {
            format!("{topic_title} ({})", item.chat_title)
        } else {
            item.chat_title.clone()
        };

        let title_with_account = if backend.has_multiple_accounts() {
            if let Some(acct) = backend.account_name(item.session_id) {
                format!("{base_title} \u{279C} {acct}")
            } else {
                base_title
            }
        } else {
            base_title
        };

        let title = if scheduled {
            format!("\u{1F4C5} {title_with_account}")
        } else {
            title_with_account
        };

        let subtitle = if options.hide_name_and_photo {
            None
        } else if is_reaction || is_poll_vote {
            if self.prefs.reaction_show_previews && item.sender_id != item.peer_id {
                Some(item.sender_name.clone())
            } else {
                None
            }
        } else if item.peer_kind == PeerKind::Group && !item.sender_name.is_empty() {
            Some(item.sender_name.clone())
        } else {
            None
        };

        let message = if is_poll_vote {
            compose_poll_vote_text(item, options.hide_message_text)
        } else if is_reaction {
            compose_reaction_text(item, options.hide_message_text)
        } else if options.hide_message_text {
            tg_lang::tr::lng_notification_preview()
        } else if collapsed_count > 1 && forwarded_count == 0 {
            strings::collapsed_new_messages(collapsed_count)
        } else if forwarded_count > 1 {
            tg_lang::tr::lng_forward_messages(forwarded_count as i64)
        } else if item.group_id != 0 {
            tg_lang::tr::lng_in_dlg_album()
        } else if is_pinned {
            compose_pinned_text(item)
        } else {
            let mut body = apply_permanent_spoilers(&item.text, &item.spoiler_ranges);
            if options.spoiler_login_code {
                body = obfuscate_login_code(&body);
            }
            if body.is_empty() {
                body = fallback_media_label(&item.media_kind);
            }
            body = truncate_chars(&body, NOTIFICATION_TEXT_LIMIT);
            if forwarded_count == 1 {
                format!("\u{27A1}\u{FE0F}{body}")
            } else {
                body
            }
        };

        let (initials, avatar_rgb) = match peer_info.as_ref() {
            Some(p) => (p.initials.clone(), p.avatar_rgb),
            None => (initials_from_title(&item.chat_title), [0x40, 0xA7, 0xE3]),
        };

        let image_path = if self.manager_type == ManagerType::Native {
            let key = if options.hide_name_and_photo {
                UserpicKey::HIDDEN_PLACEHOLDER
            } else {
                let color_rev = (u64::from(avatar_rgb[0]) << 16)
                    | (u64::from(avatar_rgb[1]) << 8)
                    | u64::from(avatar_rgb[2]);
                UserpicKey::for_peer(item.peer_id.0, color_rev)
            };
            // `CachedUserpics::get(key, peer, view)`: the peer's photo once it is loaded, its
            // empty userpic until then; a hidden sender gets the app placeholder.
            let photo = (!options.hide_name_and_photo)
                .then(|| self.userpic_photos.as_ref().and_then(|f| f(item.peer_id.0)))
                .flatten();
            let key = photo.as_ref().map_or(key, |p| {
                UserpicKey::for_peer_photo(item.peer_id.0, p.photo_id)
            });
            self.userpics
                .get_with_photo(key, peer_info.as_ref(), photo.as_deref(), now_ms)
                .ok()
        } else {
            None
        };

        let id = NotificationId {
            context: ContextId {
                session_id: item.session_id,
                peer_id: item.peer_id.0,
                topic_root_id: item.topic_root_id.0,
                monoforum_peer_id: item.monoforum_peer_id.0,
            },
            msg_id: item.msg_id.0,
        };

        RenderedNotification {
            id,
            title,
            subtitle,
            message,
            options,
            image_path,
            initials,
            avatar_rgb,
            collapsed_count,
        }
    }

    fn dispatch_rendered(
        &mut self,
        rendered: RenderedNotification,
        suppression: NotificationSuppressionState,
        now_ms: i64,
    ) {
        tracing::debug!(
            msg_id = rendered.id.msg_id,
            collapsed = rendered.collapsed_count,
            "notification dispatched"
        );
        self.dispatched_log.push(rendered.clone());
        match self.manager_type {
            ManagerType::Native => {
                if let Some(ref native) = self.native_backend
                    && let Err(err) = native.show(&rendered)
                {
                    tracing::warn!(%err, "Native Windows toast failed; falling back to custom popup");
                    self.push_custom_popup(&rendered, suppression, now_ms);
                }
            }
            ManagerType::Default => {
                self.push_custom_popup(&rendered, suppression, now_ms);
            }
            ManagerType::Dummy => {}
        }
    }

    fn push_custom_popup(
        &mut self,
        rendered: &RenderedNotification,
        suppression: NotificationSuppressionState,
        now_ms: i64,
    ) {
        self.custom_popups.push(
            CustomPopupContent {
                id: rendered.id,
                title: rendered.title.clone(),
                subtitle: rendered.subtitle.clone(),
                message: rendered.message.clone(),
                initials: rendered.initials.clone(),
                avatar_rgb: rendered.avatar_rgb,
                hide_name_and_photo: rendered.options.hide_name_and_photo,
                hide_reply_button: rendered.options.hide_reply_button,
                hide_mark_as_read: rendered.options.hide_mark_as_read,
            },
            now_ms,
            suppression.should_wait_for_input(),
        );
    }

    fn remember_shown_incoming(&mut self, thread: ThreadKey, item: &IncomingMessageNotification) {
        if !item.outgoing {
            self.shown_incoming
                .entry(thread)
                .or_default()
                .insert(item.msg_id.0);
        }
    }

    fn is_sent_recently(
        &self,
        map: &BTreeMap<SentDedupKey, i64>,
        key: SentDedupKey,
        now_ms: i64,
    ) -> bool {
        let cutoff = now_ms.saturating_sub(REACTION_NOTIFICATION_EACH_MS);
        map.get(&key).is_some_and(|&ts| ts > cutoff)
    }

    fn prune_sent_map(map: &mut BTreeMap<SentDedupKey, i64>, now_ms: i64) {
        let cutoff = now_ms.saturating_sub(REACTION_NOTIFICATION_EACH_MS);
        map.retain(|_, &mut ts| ts > cutoff);
    }
}

/// Replaces spoiler character ranges with `▓` (`U+259A`, `TextWithPermanentSpoiler`).
#[must_use]
pub fn apply_permanent_spoilers(text: &str, ranges: &[TextSpoilerRange]) -> String {
    if ranges.is_empty() {
        return text.to_owned();
    }
    let mut chars: Vec<char> = text.chars().collect();
    for range in ranges {
        let start = range.char_offset.min(chars.len());
        let end = start.saturating_add(range.char_len).min(chars.len());
        for ch in &mut chars[start..end] {
            *ch = '\u{259A}';
        }
    }
    chars.into_iter().collect()
}

/// Masks 5+ digit login verification codes in Telegram service messages (`spoilerLoginCode`).
#[must_use]
pub fn obfuscate_login_code(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = chars.clone();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i - start >= 5 {
                for ch in &mut out[start..i] {
                    *ch = '\u{259A}';
                }
            }
        } else {
            i += 1;
        }
    }
    out.into_iter().collect()
}

/// Formats reaction notification text (`Manager::ComposeReactionNotification`).
#[must_use]
pub fn compose_reaction_text(item: &IncomingMessageNotification, hide_content: bool) -> String {
    let emoji = item
        .reaction_emoji
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("\u{1F4AD}");
    if hide_content {
        return tg_lang::tr::lng_reaction_notext(emoji);
    }
    match &item.media_kind {
        NotificationMediaKind::Text => {
            let body = apply_permanent_spoilers(&item.text, &item.spoiler_ranges);
            if body.is_empty() {
                tg_lang::tr::lng_reaction_notext(emoji)
            } else {
                tg_lang::tr::lng_reaction_text(
                    emoji,
                    &truncate_chars(&body, NOTIFICATION_TEXT_LIMIT),
                )
            }
        }
        NotificationMediaKind::Photo => tg_lang::tr::lng_reaction_photo(emoji),
        NotificationMediaKind::VoiceMessage => tg_lang::tr::lng_reaction_voice_message(emoji),
        NotificationMediaKind::VideoMessage => tg_lang::tr::lng_reaction_video_message(emoji),
        NotificationMediaKind::Gif => tg_lang::tr::lng_reaction_gif(emoji),
        NotificationMediaKind::Video => tg_lang::tr::lng_reaction_video(emoji),
        NotificationMediaKind::Sticker(alt) => {
            if alt.is_empty() {
                tg_lang::tr::lng_reaction_notext(emoji)
            } else {
                tg_lang::tr::lng_reaction_sticker(emoji, alt)
            }
        }
        NotificationMediaKind::Document => tg_lang::tr::lng_reaction_document(emoji),
        NotificationMediaKind::Contact(name) => tg_lang::tr::lng_reaction_contact(emoji, name),
        NotificationMediaKind::Location => tg_lang::tr::lng_reaction_location(emoji),
        NotificationMediaKind::Poll { question, is_quiz } => {
            if *is_quiz {
                tg_lang::tr::lng_reaction_quiz(emoji, question)
            } else {
                tg_lang::tr::lng_reaction_poll(emoji, question)
            }
        }
        NotificationMediaKind::Game => tg_lang::tr::lng_reaction_game(emoji),
        NotificationMediaKind::Invoice => tg_lang::tr::lng_reaction_invoice(emoji),
    }
}

/// Formats poll vote notification text (`Manager::ComposePollVoteNotification`).
#[must_use]
pub fn compose_poll_vote_text(item: &IncomingMessageNotification, hide_content: bool) -> String {
    if hide_content {
        return tg_lang::tr::lng_poll_vote_notext();
    }
    if let Some(ref option) = item.poll_vote_option
        && !option.is_empty()
    {
        return tg_lang::tr::lng_poll_vote_option(option);
    }
    if let NotificationMediaKind::Poll { ref question, .. } = item.media_kind
        && !question.is_empty()
    {
        return tg_lang::tr::lng_poll_vote(question);
    }
    tg_lang::tr::lng_poll_vote_notext()
}

/// Formats a pinned-message notification snippet (`preparePinnedText`).
#[must_use]
pub fn compose_pinned_text(item: &IncomingMessageNotification) -> String {
    let snippet = apply_permanent_spoilers(&item.text, &item.spoiler_ranges);
    let actor = if item.sender_name.is_empty() {
        &item.chat_title
    } else {
        &item.sender_name
    };
    if snippet.is_empty() {
        strings::pinned_generic_message(actor)
    } else {
        tg_lang::tr::lng_action_pinned_message(
            actor,
            &truncate_chars(&snippet, PINNED_MESSAGE_TEXT_LIMIT),
        )
    }
}

fn fallback_media_label(media: &NotificationMediaKind) -> String {
    match media {
        NotificationMediaKind::Text => String::new(),
        NotificationMediaKind::Photo => tg_lang::tr::lng_in_dlg_photo(),
        NotificationMediaKind::VoiceMessage => tg_lang::tr::lng_in_dlg_audio(),
        NotificationMediaKind::VideoMessage => tg_lang::tr::lng_in_dlg_video_message(),
        NotificationMediaKind::Gif => "GIF".to_owned(),
        NotificationMediaKind::Video => tg_lang::tr::lng_in_dlg_video(),
        NotificationMediaKind::Sticker(alt) => {
            if alt.is_empty() {
                tg_lang::tr::lng_in_dlg_sticker()
            } else {
                tg_lang::tr::lng_in_dlg_sticker_emoji(alt)
            }
        }
        NotificationMediaKind::Document => tg_lang::tr::lng_in_dlg_file(),
        NotificationMediaKind::Contact(name) => {
            format!("{}: {name}", tg_lang::tr::lng_in_dlg_contact())
        }
        NotificationMediaKind::Location => tg_lang::tr::lng_in_dlg_file(),
        NotificationMediaKind::Poll { question, .. } => {
            format!("{}: {question}", tg_lang::tr::lng_in_dlg_poll())
        }
        NotificationMediaKind::Game => tg_lang::tr::lng_in_dlg_widget(),
        NotificationMediaKind::Invoice => tg_lang::tr::lng_in_dlg_file(),
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let collected: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{collected}…")
    } else {
        collected
    }
}
