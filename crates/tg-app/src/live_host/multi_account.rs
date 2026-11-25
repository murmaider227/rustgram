//! Multi-account live workspace coordinator (`MultiAccountWorkspaceHost`) and
//! cross-account notification aggregator (`MultiAccountNotifyBackend`).
//!
//! Port origin: `Telegram/SourceFiles/main/main_domain.cpp`,
//! `Telegram/SourceFiles/window/window_controller.cpp`,
//! `Telegram/SourceFiles/window/notifications_manager.cpp`, and
//! `Telegram/SourceFiles/tray_accounts_menu.cpp`.
//!
//! Keeps every signed-in account's [`SessionRuntime`] running in the background while one
//! account is displayed in the foreground [`LiveActiveWorkspace`]:
//! - Background accounts continue receiving updates, maintaining unread counters, and emitting
//!   desktop/custom notifications tagged with their `session_id` and account display name.
//! - Clicking a notification from an inactive account switches to that account and opens the
//!   target chat/message.
//! - The Main Menu accounts list and system tray accounts menu stay synchronized across all
//!   accounts, and the tray/taskbar badge sums unread counts across accounts respecting each
//!   account's `include_muted_counter` / `count_unread_messages` settings.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tg_data::UserId;
use tg_session::{MAX_ACCOUNTS, PREMIUM_MAX_ACCOUNTS, SessionEvent, SessionRuntime};

use crate::core_types::erased_rpc::ErasedRpcClient;
use crate::live::host::LiveActiveWorkspace;
use crate::live::session::LiveSession;
use crate::notifications::AppNotifyTrayHost;
use crate::notifications::backend::{IncomingNotifyEvent, NotifyBackend, NotifyPeerInfo};
use crate::notifications::manager::NotificationSystem;
use crate::notifications::settings::{
    AccountUnreadEntry, NotifySettingsStore, UnreadBadgeResult, UnreadBadgeState,
    aggregate_account_badges_per_account,
};
use crate::stand_in::backend::Backend;
use crate::stand_in::settings::{ShellSettings, WindowPosition};
use crate::stand_in::types::{MsgId, PeerId};
use crate::startup::updater::UpdateSnapshot;
use crate::tray::{TrayAccountEntry, TrayPlatform};
use crate::window::controller::RootScreen;
use crate::window::main_menu::MainMenuAccountItem;

/// Cross-account chat navigation request produced when clicking a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAccountChatOpen {
    /// Target account slot index in `AccountManager` / `Domain`.
    pub slot_index: i32,
    /// Target account unique session ID (`session->uniqueId()`).
    pub session_id: u64,
    /// Chat peer to open inside that account.
    pub peer_id: PeerId,
    /// Forum topic root message ID (`MsgId(0)` for standard chat).
    pub topic_root_id: MsgId,
    /// Target message ID to scroll/highlight.
    pub msg_id: MsgId,
    /// Optional draft text to prefill into the composer.
    pub draft: Option<String>,
}

/// High-level action emitted by [`MultiAccountWorkspaceHost::show`] to its outer owner
/// (such as [`crate::live::LiveAppHost`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiAccountFrameAction {
    /// No outer lifecycle transition needed this frame.
    None,
    /// Active account changed to `slot_index` (from Main Menu, Tray menu, or notification click).
    SwitchedAccount(i32),
    /// User clicked `"Add Account"` in Main Menu while below the dynamic account cap.
    AddAccount,
    /// User confirmed logout (or server sent `ForceLogout`) for `slot_index`.
    LogoutAccount(i32),
}

struct MultiAccountNotifySlot {
    slot_index: i32,
    session_id: u64,
    session: LiveSession,
    display_name: String,
    include_muted_counter: bool,
    count_unread_messages: bool,
}

/// Shared synchronization table read by [`MultiAccountNotifyBackend`] and updated by
/// [`MultiAccountWorkspaceHost`].
#[derive(Default)]
pub struct MultiAccountNotifyState {
    slots: BTreeMap<i32, MultiAccountNotifySlot>,
    active_slot: i32,
    pending_opens: VecDeque<PendingAccountChatOpen>,
}

impl MultiAccountNotifyState {
    fn register_slot(
        &mut self,
        slot_index: i32,
        session_id: u64,
        session: LiveSession,
        display_name: String,
        include_muted_counter: bool,
        count_unread_messages: bool,
    ) {
        self.slots.insert(
            slot_index,
            MultiAccountNotifySlot {
                slot_index,
                session_id,
                session,
                display_name,
                include_muted_counter,
                count_unread_messages,
            },
        );
    }

    fn remove_slot(&mut self, slot_index: i32) {
        self.slots.remove(&slot_index);
    }

    fn drain_pending_opens(&mut self) -> Vec<PendingAccountChatOpen> {
        self.pending_opens.drain(..).collect()
    }
}

/// [`NotifyBackend`] implementation aggregating all running [`LiveSession`] instances across
/// active and background accounts.
pub struct MultiAccountNotifyBackend {
    state: Arc<Mutex<MultiAccountNotifyState>>,
}

impl Clone for MultiAccountNotifyBackend {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

impl MultiAccountNotifyBackend {
    /// Creates a new [`MultiAccountNotifyBackend`] bound to shared `state`.
    #[must_use]
    pub fn new(state: Arc<Mutex<MultiAccountNotifyState>>) -> Self {
        Self { state }
    }
}

impl NotifyBackend for MultiAccountNotifyBackend {
    fn poll_events(&mut self) -> Vec<IncomingNotifyEvent> {
        let Ok(mut guard) = self.state.lock() else {
            return Vec::new();
        };
        let mut all = Vec::new();
        for entry in guard.slots.values_mut() {
            all.extend(entry.session.poll_events());
        }
        all
    }

    fn mark_read(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        max_msg_id: MsgId,
    ) {
        let target = self.state.lock().ok().and_then(|g| {
            g.slots
                .values()
                .find(|s| s.session_id == session_id)
                .map(|s| s.session.clone())
        });
        if let Some(mut session) = target {
            NotifyBackend::mark_read(&mut session, session_id, peer_id, topic_root_id, max_msg_id);
        }
    }

    fn send_quick_reply(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        reply_to_msg_id: MsgId,
        text: String,
    ) {
        let target = self.state.lock().ok().and_then(|g| {
            g.slots
                .values()
                .find(|s| s.session_id == session_id)
                .map(|s| s.session.clone())
        });
        if let Some(mut session) = target {
            session.send_quick_reply(session_id, peer_id, topic_root_id, reply_to_msg_id, text);
        }
    }

    fn open_chat(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        msg_id: MsgId,
        draft: Option<String>,
    ) {
        if let Ok(mut guard) = self.state.lock() {
            let slot_index = guard
                .slots
                .values()
                .find(|s| s.session_id == session_id)
                .map_or(guard.active_slot, |s| s.slot_index);
            guard.pending_opens.push_back(PendingAccountChatOpen {
                slot_index,
                session_id,
                peer_id,
                topic_root_id,
                msg_id,
                draft,
            });
            if let Some(slot) = guard.slots.get(&slot_index) {
                slot.session.bridge_state().notify_ui();
            }
        }
    }

    fn unread_state(&self) -> UnreadBadgeState {
        let Ok(guard) = self.state.lock() else {
            return UnreadBadgeState::default();
        };
        let mut combined = UnreadBadgeState::default();
        for entry in guard.slots.values() {
            let s = entry.session.unread_state();
            combined.messages = combined.messages.saturating_add(s.messages);
            combined.messages_muted = combined.messages_muted.saturating_add(s.messages_muted);
            combined.chats = combined.chats.saturating_add(s.chats);
            combined.chats_muted = combined.chats_muted.saturating_add(s.chats_muted);
            combined.marks = combined.marks.saturating_add(s.marks);
            combined.marks_muted = combined.marks_muted.saturating_add(s.marks_muted);
            combined.mentions = combined.mentions.saturating_add(s.mentions);
        }
        combined
    }

    fn peer_info(&self, peer_id: PeerId) -> Option<NotifyPeerInfo> {
        let guard = self.state.lock().ok()?;
        if let Some(active) = guard.slots.get(&guard.active_slot)
            && let Some(info) = active.session.peer_info(peer_id)
        {
            return Some(info);
        }
        for (&idx, entry) in &guard.slots {
            if idx == guard.active_slot {
                continue;
            }
            if let Some(info) = entry.session.peer_info(peer_id) {
                return Some(info);
            }
        }
        None
    }

    fn account_name(&self, session_id: u64) -> Option<String> {
        let guard = self.state.lock().ok()?;
        guard
            .slots
            .values()
            .find(|s| s.session_id == session_id)
            .map(|s| s.display_name.clone())
    }

    fn has_multiple_accounts(&self) -> bool {
        self.state.lock().is_ok_and(|g| g.slots.len() > 1)
    }
}

/// Runtime entry for one authenticated account managed by [`MultiAccountWorkspaceHost`].
pub struct ManagedAccountEntry {
    /// Slot index in `AccountManager` / `Domain`.
    pub slot_index: i32,
    /// Stable unique session ID used in notifications (`session->uniqueId()`).
    pub session_id: u64,
    /// Authenticated Telegram user ID.
    pub user_id: UserId,
    /// Whether this account is on Telegram test DCs.
    pub is_test_mode: bool,
    /// Cached Premium flag (synced live from `tg_data::Session`).
    pub is_premium: bool,
    /// Per-account preference: include muted chats in unread counter.
    pub include_muted_counter: bool,
    /// Per-account preference: count unread messages rather than unread chats.
    pub count_unread_messages: bool,
    /// Background `SessionRuntime` kept running even when another account is active.
    pub runtime: Arc<SessionRuntime<ErasedRpcClient>>,
    /// Complete desktop workspace (`WindowController` + Calls + Stories + WebView) for this slot.
    pub workspace: LiveActiveWorkspace,
    last_bridge_gen: u64,
    last_settings_badge_prefs: Option<(bool, bool)>,
    last_session_premium: Option<bool>,
}

/// Snapshot describing one account slot inside [`MultiAccountWorkspaceHost`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedAccountSummary {
    /// Account slot index.
    pub slot_index: i32,
    /// Session ID used in notifications.
    pub session_id: u64,
    /// Authenticated user ID.
    pub user_id: UserId,
    /// Current display name.
    pub display_name: String,
    /// Whether this account is currently active in the foreground.
    pub is_active: bool,
    /// Whether this account has Telegram Premium.
    pub is_premium: bool,
    /// Individual account unread badge.
    pub unread_badge: UnreadBadgeResult,
}

/// Hosts multiple concurrent live accounts ([`ManagedAccountEntry`]) with one shared
/// [`AppNotifyTrayHost`], seamless account switching, background unread tracking, cross-account
/// notification click routing, and dynamic `3..=6` account limit enforcement.
pub struct MultiAccountWorkspaceHost {
    accounts: BTreeMap<i32, ManagedAccountEntry>,
    active_slot: i32,
    notify_state: Arc<Mutex<MultiAccountNotifyState>>,
    notify_tray: Option<AppNotifyTrayHost>,
    headless: bool,
    settings: ShellSettings,
    settings_path: PathBuf,
    tdata_dir: Option<PathBuf>,
    data_name: String,
    downloads_dir: PathBuf,
    native_hwnd: isize,
    update_snapshot: UpdateSnapshot,
    widget_helper_path: Option<PathBuf>,
    window_hwnd: usize,
}

/// Resolves the OS Downloads directory for Rustgram (`<USERPROFILE>/Downloads/Rustgram` on
/// Windows, `<HOME>/Downloads/Rustgram` on Unix) so the media viewer saves files to the real
/// user Downloads folder instead of `%TEMP%\rustgram_downloads`.
#[must_use]
pub fn resolve_default_downloads_dir() -> PathBuf {
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return PathBuf::from(profile).join("Downloads").join("Rustgram");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home).join("Downloads").join("Rustgram");
    }
    std::env::temp_dir().join("RustgramDownloads")
}

/// Computes the per-account persistent media cache directory inside `tdata`, keyed by the
/// account's tdesktop data-name hash (`ToFilePart(compute_data_name_key(compose_data_string(..)))`)
/// rather than the raw slot index.
#[must_use]
pub fn account_media_cache_dir_for_data_name(
    tdata_dir: &std::path::Path,
    data_name: &str,
    slot_index: i32,
) -> PathBuf {
    let key_part = tg_session::account::account_data_key_file_part(data_name, slot_index);
    tdata_dir.join("media_cache").join(key_part)
}

/// Computes the per-account persistent media cache directory inside `tdata` for the default
/// `"data"` profile, keyed by the account's data key rather than the slot index.
#[must_use]
pub fn account_media_cache_dir(tdata_dir: &std::path::Path, slot_index: i32) -> PathBuf {
    account_media_cache_dir_for_data_name(tdata_dir, "data", slot_index)
}

/// Computes the per-account temporary directory inside `tdata` (`tdata/temp_<acc_data_name>`),
/// matching tdesktop's `Storage::Account::_tempPath`.
#[must_use]
pub fn account_temp_dir(tdata_dir: &std::path::Path, data_name: &str, slot_index: i32) -> PathBuf {
    tdata_dir.join(tg_session::account::account_temp_dir_name(
        data_name, slot_index,
    ))
}

fn cleanup_unlocked_dir_files(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            cleanup_unlocked_dir_files(&p);
            let _ = std::fs::remove_dir(&p);
        } else {
            let _ = std::fs::remove_file(&p);
        }
    }
    let _ = std::fs::remove_dir(dir);
}

fn configure_account_cache_and_temp(
    tdata_dir: &std::path::Path,
    data_name: &str,
    slot_index: i32,
    live: &LiveSession,
) {
    let cache_dir = account_media_cache_dir_for_data_name(tdata_dir, data_name, slot_index);
    // Migrate/remove any legacy slot-indexed plaintext cache folder.
    let legacy_slot_dir = tdata_dir
        .join("media_cache")
        .join(format!("account_{slot_index}"));
    if legacy_slot_dir != cache_dir && legacy_slot_dir.exists() {
        let _ = std::fs::remove_dir_all(&legacy_slot_dir);
    }

    if let Some(enc_key) =
        tg_session::account::resolve_account_cache_key_for_tdata(tdata_dir, data_name, slot_index)
    {
        let _ = live.set_media_cache_dir_with_key(cache_dir, enc_key);
    } else {
        let _ = live.set_media_cache_dir(cache_dir);
    }

    let temp_dir = account_temp_dir(tdata_dir, data_name, slot_index);
    cleanup_unlocked_dir_files(&temp_dir);
    live.set_account_temp_dir(temp_dir);
}

fn sync_process_temp_dirs_for_account(temp_dir: Option<&std::path::Path>) {
    match temp_dir {
        Some(base) => {
            crate::history::compose_extras::set_active_round_video_temp_dir(Some(
                base.join("round_videos"),
            ));
            crate::history::video_editor::set_active_video_editor_temp_dir(Some(
                base.join("video_edit"),
            ));
            crate::notifications::utilities::set_default_userpics_dir(Some(
                base.join("notify_userpics"),
            ));
        }
        None => {
            crate::history::compose_extras::set_active_round_video_temp_dir(None);
            crate::history::video_editor::set_active_video_editor_temp_dir(None);
            crate::notifications::utilities::set_default_userpics_dir(None);
        }
    }
}

/// Starts the requests the main window needs from the network: the first chat-list page
/// (`LiveSession::start_chats_list_request`), `messages.getDialogFilters` for the folder tabs and
/// `account.getAutoDownloadSettings`. Nothing waits for them: like tdesktop (`firstShow` right
/// after `startDomain`, `core/application.cpp:448-451`) the window shows at once, with the chat
/// list's "Loading..." state, and each reply repaints it when it arrives.
fn start_main_window_requests(live_session: &LiveSession, egui_ctx: Option<&egui::Context>) {
    use tg_app_core::dialogs::backend::DialogsBackend;
    use tg_app_core::settings::backend::SettingsBackend;
    let repaint = move |ctx: Option<egui::Context>| {
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    };
    let ctx = egui_ctx.cloned();
    live_session.start_chats_list_request(move || repaint(ctx));
    let session = live_session.clone();
    let ctx = egui_ctx.cloned();
    let _ = std::thread::Builder::new()
        .name("first-frame-warm-up".to_owned())
        .spawn(move || {
            let filters = {
                let session = session.clone();
                std::thread::spawn(move || {
                    let _ = DialogsBackend::chat_folders(&session);
                })
            };
            let _ = SettingsBackend::auto_download(&session);
            let _ = filters.join();
            repaint(ctx);
        });
}

impl MultiAccountWorkspaceHost {
    /// Creates a production multi-account host with a shared OS tray + notifications host.
    #[must_use]
    pub fn new(settings: ShellSettings, settings_path: PathBuf) -> Self {
        let notify_state = Arc::new(Mutex::new(MultiAccountNotifyState::default()));
        let notify_backend = MultiAccountNotifyBackend::new(Arc::clone(&notify_state));
        let notify_tray = Some(AppNotifyTrayHost::with_backend(
            Box::new(notify_backend),
            NotifySettingsStore::default(),
            Box::new(|_| {}),
        ));
        Self {
            accounts: BTreeMap::new(),
            active_slot: 0,
            notify_state,
            notify_tray,
            headless: false,
            settings,
            settings_path,
            tdata_dir: None,
            data_name: "data".to_owned(),
            downloads_dir: resolve_default_downloads_dir(),
            native_hwnd: 0,
            update_snapshot: UpdateSnapshot::default(),
            widget_helper_path: None,
            window_hwnd: 0,
        }
    }

    /// Creates a deterministic headless multi-account host for integration/UI tests with an
    /// injectable [`NotificationSystem`] and [`TrayPlatform`] (e.g., `MockTrayPlatform`).
    #[must_use]
    pub fn new_headless_with_tray(
        settings: ShellSettings,
        settings_path: PathBuf,
        system: NotificationSystem,
        tray_platform: Box<dyn TrayPlatform>,
    ) -> Self {
        let notify_state = Arc::new(Mutex::new(MultiAccountNotifyState::default()));
        let notify_backend = MultiAccountNotifyBackend::new(Arc::clone(&notify_state));
        let notify_tray = Some(AppNotifyTrayHost::with_backend_and_platform(
            Box::new(notify_backend),
            system,
            tray_platform,
            Box::new(|_| {}),
        ));
        Self {
            accounts: BTreeMap::new(),
            active_slot: 0,
            notify_state,
            notify_tray,
            headless: true,
            settings,
            settings_path,
            tdata_dir: None,
            data_name: "data".to_owned(),
            downloads_dir: resolve_default_downloads_dir(),
            native_hwnd: 0,
            update_snapshot: UpdateSnapshot::default(),
            widget_helper_path: None,
            window_hwnd: 0,
        }
    }

    /// Configures the `tdata` root directory for this host and binds per-account runtime storage
    /// (`attach_proxy_tdata_path`, per-account encrypted `set_media_cache_dir`, account temp dirs,
    /// `langpack().attach_tdata_dir`, and `attach_shortcuts_tdata_dir`) on all running accounts
    /// and every account added subsequently.
    pub fn set_tdata_dir(&mut self, tdata_dir: PathBuf) {
        if let Some((unlocked_name, _)) =
            tg_session::account::unlocked_domain_key_for_tdata(&tdata_dir)
        {
            self.data_name = unlocked_name;
        }
        self.tdata_dir = Some(tdata_dir.clone());
        let data_name = self.data_name.clone();
        for (&slot_index, entry) in &mut self.accounts {
            let live = entry.workspace.live_session();
            let _ = live.attach_proxy_tdata_path(tdata_dir.clone());
            configure_account_cache_and_temp(&tdata_dir, &data_name, slot_index, live);
            let _ = live.langpack().attach_tdata_dir(tdata_dir.clone());
            entry
                .workspace
                .controller_mut()
                .attach_shortcuts_tdata_dir(&tdata_dir);
        }
        let active_temp = account_temp_dir(&tdata_dir, &self.data_name, self.active_slot);
        sync_process_temp_dirs_for_account(Some(&active_temp));
    }

    /// Configures the Downloads directory wired into every account's [`crate::media::MediaOverlayWidget`]
    /// (`set_save_directory`), replacing the default `%TEMP%\rustgram_downloads` fallback.
    pub fn set_downloads_dir(&mut self, dir: PathBuf) {
        self.downloads_dir = dir.clone();
        for entry in self.accounts.values_mut() {
            entry
                .workspace
                .controller_mut()
                .media_overlay_mut()
                .set_save_directory(dir.clone());
        }
    }

    /// Returns the active Downloads directory configured for media viewer saves.
    #[must_use]
    pub fn downloads_dir(&self) -> &std::path::Path {
        &self.downloads_dir
    }

    /// Returns the configured `tdata` root directory, if any.
    #[must_use]
    pub fn tdata_dir(&self) -> Option<&std::path::Path> {
        self.tdata_dir.as_deref()
    }

    /// Updates the native window handle (`HWND` on Windows, `0` otherwise) and synchronizes
    /// screenshot protection for the active account workspace.
    pub fn set_native_hwnd(&mut self, hwnd: isize) {
        self.native_hwnd = hwnd;
        let active_slot = self.active_slot;
        for (&slot_index, entry) in &mut self.accounts {
            if slot_index == active_slot {
                entry.workspace.controller_mut().set_native_hwnd(hwnd);
            } else {
                entry.workspace.controller_mut().set_native_hwnd(0);
            }
        }
    }

    /// Registers an authenticated account's running [`SessionRuntime`] into this host.
    ///
    /// If this is the first account registered (or `make_active` is `true`), it becomes active;
    /// otherwise it stays running in the background receiving updates and notifications.
    #[allow(clippy::too_many_arguments)]
    pub fn add_account(
        &mut self,
        slot_index: i32,
        user_id: UserId,
        is_test_mode: bool,
        is_premium: bool,
        runtime: Arc<SessionRuntime<ErasedRpcClient>>,
        egui_ctx: Option<&egui::Context>,
        make_active: bool,
        bootstrap_stories: bool,
    ) {
        let session_id = compute_session_id(user_id, is_test_mode);
        let live_session = LiveSession::new(Arc::clone(&runtime), session_id);
        live_session
            .passkeys_handle()
            .set_window_hwnd(self.window_hwnd);
        if let Some(ref tdata_dir) = self.tdata_dir {
            let _ = live_session.attach_proxy_tdata_path(tdata_dir.clone());
            configure_account_cache_and_temp(tdata_dir, &self.data_name, slot_index, &live_session);
            let _ = live_session.langpack().attach_tdata_dir(tdata_dir.clone());
        }
        if !self.headless {
            start_main_window_requests(&live_session, egui_ctx);
        }
        let notify_session_handle = live_session.clone();
        let display_name = live_session
            .current_user()
            .map(|u| u.name.clone())
            .unwrap_or_else(|| format!("Account {}", slot_index + 1));

        let mut workspace = if self.headless {
            LiveActiveWorkspace::new_headless(
                live_session,
                self.settings.clone(),
                self.settings_path.clone(),
                bootstrap_stories,
            )
        } else {
            LiveActiveWorkspace::new_without_tray(
                live_session,
                self.settings.clone(),
                self.settings_path.clone(),
                egui_ctx,
            )
        };
        workspace
            .controller_mut()
            .set_update_snapshot(self.update_snapshot.clone());
        if let Some(ref helper_path) = self.widget_helper_path {
            workspace
                .controller_mut()
                .set_widget_helper_path(helper_path.clone());
        }
        if let Some(ref tdata_dir) = self.tdata_dir {
            workspace
                .controller_mut()
                .attach_shortcuts_tdata_dir(tdata_dir);
        }
        workspace
            .controller_mut()
            .media_overlay_mut()
            .set_save_directory(self.downloads_dir.clone());
        workspace
            .controller_mut()
            .set_window_hwnd(self.window_hwnd as isize);
        let will_be_active = self.accounts.is_empty() || make_active;
        let was_empty = self.accounts.is_empty();
        if !was_empty
            && make_active
            && self.active_slot != slot_index
            && let Some(prev) = self.accounts.get_mut(&self.active_slot)
        {
            prev.workspace.controller_mut().set_native_hwnd(0);
        }
        workspace
            .controller_mut()
            .set_native_hwnd(if will_be_active { self.native_hwnd } else { 0 });

        let initial_gen = workspace.live_session().bridge_state().generation();
        self.accounts.insert(
            slot_index,
            ManagedAccountEntry {
                slot_index,
                session_id,
                user_id,
                is_test_mode,
                is_premium,
                include_muted_counter: true,
                count_unread_messages: true,
                runtime,
                workspace,
                last_bridge_gen: initial_gen,
                last_settings_badge_prefs: None,
                last_session_premium: None,
            },
        );

        if let Ok(mut guard) = self.notify_state.lock() {
            guard.register_slot(
                slot_index,
                session_id,
                notify_session_handle,
                display_name,
                true,
                true,
            );
            if was_empty || make_active {
                guard.active_slot = slot_index;
            }
        }
        if was_empty || make_active {
            self.active_slot = slot_index;
            if let Some(ref tdata_dir) = self.tdata_dir {
                let active_temp = account_temp_dir(tdata_dir, &self.data_name, slot_index);
                sync_process_temp_dirs_for_account(Some(&active_temp));
            }
        }
        self.sync_tray_and_menus();
    }

    /// Removes an account slot, shutting down its auxiliary hosts and `SessionRuntime`.
    ///
    /// If the removed slot was active, automatically switches `active_slot` to another remaining
    /// account (returning `Some(new_active_slot)`), or `None` if no accounts remain.
    pub fn remove_account(&mut self, slot_index: i32) -> Option<i32> {
        if let Ok(mut guard) = self.notify_state.lock() {
            guard.remove_slot(slot_index);
        }
        if let Some(mut removed) = self.accounts.remove(&slot_index) {
            removed.workspace.controller_mut().set_native_hwnd(0);
            // Release SQLite/blob file handles before AccountManager wipes the cache directory,
            // and purge unlocked account temp files.
            removed
                .workspace
                .live_session()
                .media_cache()
                .close_to_memory();
            removed
                .workspace
                .live_session()
                .cleanup_account_temp_files();
            removed.workspace.on_exit();
            let runtime = removed.runtime;
            drop(removed.workspace);
            if let Ok(rt) = Arc::try_unwrap(runtime) {
                rt.shutdown();
            }
        }
        if let Some(ref tdata_dir) = self.tdata_dir {
            let removed_temp = account_temp_dir(tdata_dir, &self.data_name, slot_index);
            cleanup_unlocked_dir_files(&removed_temp);
        }
        if self.active_slot == slot_index
            && let Some(&next_slot) = self.accounts.keys().next()
        {
            self.active_slot = next_slot;
            if let Some(next) = self.accounts.get_mut(&next_slot) {
                next.workspace
                    .controller_mut()
                    .set_native_hwnd(self.native_hwnd);
            }
            if let Ok(mut guard) = self.notify_state.lock() {
                guard.active_slot = next_slot;
            }
        }
        if self.accounts.is_empty() {
            sync_process_temp_dirs_for_account(None);
        } else if let Some(ref tdata_dir) = self.tdata_dir {
            let active_temp = account_temp_dir(tdata_dir, &self.data_name, self.active_slot);
            sync_process_temp_dirs_for_account(Some(&active_temp));
        }
        self.sync_tray_and_menus();
        (!self.accounts.is_empty()).then_some(self.active_slot)
    }

    /// Switches the foreground workspace to `slot_index`, keeping the previous account's
    /// `SessionRuntime` running in the background.
    pub fn switch_to(&mut self, slot_index: i32) -> bool {
        if !self.accounts.contains_key(&slot_index) {
            return false;
        }
        if self.active_slot != slot_index {
            // Carry theme/window settings forward so switching accounts preserves shell visuals.
            if let Some(prev) = self.accounts.get_mut(&self.active_slot) {
                self.settings = prev.workspace.controller().settings().clone();
                prev.workspace.controller_mut().set_native_hwnd(0);
            }
            self.active_slot = slot_index;
            if let Some(next) = self.accounts.get_mut(&slot_index) {
                next.workspace
                    .controller_mut()
                    .set_native_hwnd(self.native_hwnd);
            }
            if let Ok(mut guard) = self.notify_state.lock() {
                guard.active_slot = slot_index;
            }
            if let Some(ref tdata_dir) = self.tdata_dir {
                let active_temp = account_temp_dir(tdata_dir, &self.data_name, slot_index);
                sync_process_temp_dirs_for_account(Some(&active_temp));
            }
            self.sync_tray_and_menus();
        }
        true
    }

    /// Returns the currently active account slot index.
    #[must_use]
    pub const fn active_slot(&self) -> i32 {
        self.active_slot
    }

    /// Returns the number of running accounts.
    #[must_use]
    pub fn account_count(&self) -> usize {
        self.accounts.len()
    }

    /// Returns `true` when no accounts are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Computes the dynamic maximum accounts cap (`Main::Domain::maxAccounts`):
    /// `min(premium_or_test_count + 3, 6)`.
    #[must_use]
    pub fn max_accounts(&self) -> usize {
        let bonus = self
            .accounts
            .values()
            .filter(|a| a.is_premium || a.is_test_mode)
            .count();
        (bonus + MAX_ACCOUNTS).min(PREMIUM_MAX_ACCOUNTS)
    }

    /// Returns `true` when another account can be added below [`Self::max_accounts`].
    #[must_use]
    pub fn can_add_account(&self) -> bool {
        self.accounts.len() < self.max_accounts()
    }

    /// Updates per-account unread badge counting preferences (`includeMutedCounter` and
    /// `countUnreadMessages`).
    pub fn set_account_unread_prefs(
        &mut self,
        slot_index: i32,
        include_muted_counter: bool,
        count_unread_messages: bool,
    ) {
        if let Some(entry) = self.accounts.get_mut(&slot_index) {
            entry.include_muted_counter = include_muted_counter;
            entry.count_unread_messages = count_unread_messages;
        }
        if let Ok(mut guard) = self.notify_state.lock()
            && let Some(slot) = guard.slots.get_mut(&slot_index)
        {
            slot.include_muted_counter = include_muted_counter;
            slot.count_unread_messages = count_unread_messages;
        }
        self.sync_tray_and_menus();
    }

    /// Updates unread badge counting preferences across all running accounts (matching tdesktop's
    /// domain-wide `Core::Settings::includeMutedCounter` / `countUnreadMessages`).
    pub fn set_all_accounts_unread_prefs(
        &mut self,
        include_muted_counter: bool,
        count_unread_messages: bool,
    ) {
        for entry in self.accounts.values_mut() {
            entry.include_muted_counter = include_muted_counter;
            entry.count_unread_messages = count_unread_messages;
        }
        if let Ok(mut guard) = self.notify_state.lock() {
            for slot in guard.slots.values_mut() {
                slot.include_muted_counter = include_muted_counter;
                slot.count_unread_messages = count_unread_messages;
            }
        }
        self.sync_tray_and_menus();
    }

    /// Returns `(slot_index, is_premium)` pairs for every running account so `AccountManager`
    /// can persist live Premium status changes into `tdata` and keep `max_accounts()` in sync.
    #[must_use]
    pub fn slot_premium_states(&self) -> Vec<(i32, bool)> {
        self.accounts
            .values()
            .map(|e| (e.slot_index, e.is_premium))
            .collect()
    }

    /// Updates the cached Premium flag for `slot_index` (in addition to live syncing from
    /// `tg_data::Session`).
    pub fn set_account_premium(&mut self, slot_index: i32, is_premium: bool) {
        if let Some(entry) = self.accounts.get_mut(&slot_index) {
            entry.is_premium = is_premium;
        }
        self.sync_tray_and_menus();
    }

    /// Borrows the currently active [`LiveActiveWorkspace`].
    #[must_use]
    pub fn active_workspace(&self) -> Option<&LiveActiveWorkspace> {
        self.accounts.get(&self.active_slot).map(|e| &e.workspace)
    }

    /// Mutably borrows the currently active [`LiveActiveWorkspace`].
    pub fn active_workspace_mut(&mut self) -> Option<&mut LiveActiveWorkspace> {
        self.accounts
            .get_mut(&self.active_slot)
            .map(|e| &mut e.workspace)
    }

    /// Returns all registered account slot indices in ascending order.
    #[must_use]
    pub fn slot_indices(&self) -> Vec<i32> {
        self.accounts.keys().copied().collect()
    }

    /// Borrows a [`ManagedAccountEntry`] by slot index.
    #[must_use]
    pub fn account(&self, slot_index: i32) -> Option<&ManagedAccountEntry> {
        self.accounts.get(&slot_index)
    }

    /// Mutably borrows a [`ManagedAccountEntry`] by slot index.
    pub fn account_mut(&mut self, slot_index: i32) -> Option<&mut ManagedAccountEntry> {
        self.accounts.get_mut(&slot_index)
    }

    /// Returns a cloned [`MultiAccountNotifyBackend`] sharing this host's notification queue
    /// (useful for tests simulating notification clicks from inactive accounts).
    #[must_use]
    pub fn notify_backend_handle(&self) -> MultiAccountNotifyBackend {
        MultiAccountNotifyBackend::new(Arc::clone(&self.notify_state))
    }

    /// Borrows the shared [`AppNotifyTrayHost`], if present.
    #[must_use]
    pub fn notify_tray(&self) -> Option<&AppNotifyTrayHost> {
        self.notify_tray.as_ref()
    }

    /// Mutably borrows the shared [`AppNotifyTrayHost`], if present.
    pub fn notify_tray_mut(&mut self) -> Option<&mut AppNotifyTrayHost> {
        self.notify_tray.as_mut()
    }

    /// Computes the summed tray/taskbar unread badge across all running accounts, respecting
    /// each account's `include_muted_counter` and `count_unread_messages` settings.
    #[must_use]
    pub fn total_unread_badge(&self) -> UnreadBadgeResult {
        aggregate_account_badges_per_account(self.collect_unread_entries())
    }

    /// Returns per-account summaries in slot order.
    #[must_use]
    pub fn account_summaries(&self) -> Vec<ManagedAccountSummary> {
        self.accounts
            .values()
            .map(|entry| {
                let session = entry.workspace.live_session();
                let unread_entry = AccountUnreadEntry {
                    state: session.unread_state(),
                    include_muted_counter: entry.include_muted_counter,
                    count_unread_messages: entry.count_unread_messages,
                };
                let display_name = session
                    .current_user()
                    .map(|u| u.name.clone())
                    .unwrap_or_else(|| format!("Account {}", entry.slot_index + 1));
                ManagedAccountSummary {
                    slot_index: entry.slot_index,
                    session_id: entry.session_id,
                    user_id: entry.user_id,
                    display_name,
                    is_active: entry.slot_index == self.active_slot,
                    is_premium: entry.is_premium,
                    unread_badge: unread_entry.compute_badge(),
                }
            })
            .collect()
    }

    /// Returns the display name of the currently active account (used for "Back to <name>" bar
    /// when adding a new account).
    #[must_use]
    pub fn active_display_name(&self) -> String {
        self.accounts
            .get(&self.active_slot)
            .and_then(|e| e.workspace.live_session().current_user())
            .map(|u| u.name.clone())
            .unwrap_or_else(|| "Previous Account".to_owned())
    }

    /// Updates the updater snapshot on every account's workspace.
    pub fn set_update_snapshot(&mut self, snapshot: UpdateSnapshot) {
        self.update_snapshot = snapshot.clone();
        for entry in self.accounts.values_mut() {
            entry
                .workspace
                .controller_mut()
                .set_update_snapshot(snapshot.clone());
        }
    }

    /// Configures the startup-resolved `rustgram-widget-helper.exe` path on every account workspace.
    pub fn set_widget_helper_path(&mut self, path: PathBuf) {
        self.widget_helper_path = Some(path.clone());
        for entry in self.accounts.values_mut() {
            entry
                .workspace
                .controller_mut()
                .set_widget_helper_path(path.clone());
        }
    }

    /// Sets the parent main-window Win32 `HWND` on every account's passkey and window controllers.
    pub fn set_window_hwnd(&mut self, hwnd: usize) {
        self.window_hwnd = hwnd;
        for entry in self.accounts.values_mut() {
            entry
                .workspace
                .live_session()
                .passkeys_handle()
                .set_window_hwnd(hwnd);
            entry
                .workspace
                .controller_mut()
                .set_window_hwnd(hwnd as isize);
        }
    }

    /// Records updated window geometry across all account workspaces.
    pub fn note_window_position(&mut self, pos: WindowPosition) {
        self.settings.window_position = pos.clamped();
        for entry in self.accounts.values_mut() {
            entry.workspace.controller_mut().note_window_position(pos);
        }
    }

    /// Returns the current shell settings snapshot from the active workspace.
    #[must_use]
    pub fn settings(&self) -> &ShellSettings {
        self.active_workspace()
            .map(|w| w.controller().settings())
            .unwrap_or(&self.settings)
    }

    /// Renders one complete frame for the active account while polling background accounts,
    /// cross-account notifications, tray menu actions, and unread badge aggregation.
    pub fn show(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) -> MultiAccountFrameAction {
        // 1. Poll background accounts' SessionEvents so cached views stay fresh and
        //    background ForceLogout is detected promptly.
        let mut background_force_logout: Option<i32> = None;
        for (&slot, entry) in &mut self.accounts {
            entry.workspace.live_session().set_egui_ctx(ctx.clone());
            // Keep Premium status synchronized live from tg_data::Session when it changes
            // (non-blocking), without clobbering explicit overrides from `set_account_premium`.
            if let Ok(guard) = entry.runtime.session().try_read()
                && let Some(user) = guard.peer(guard.self_peer_id()).and_then(|p| p.as_user())
            {
                let live_premium = user.is_premium();
                if entry.last_session_premium != Some(live_premium) {
                    if entry.last_session_premium.is_some() || live_premium {
                        entry.is_premium = live_premium;
                    }
                    entry.last_session_premium = Some(live_premium);
                }
            }
            if slot == self.active_slot {
                continue;
            }
            let events = entry
                .workspace
                .live_session()
                .bridge_state()
                .drain_session_events();
            for ev in events {
                match ev {
                    SessionEvent::Ready => {
                        entry.workspace.live_session_mut().refresh_cached_views();
                    }
                    SessionEvent::ForceLogout { .. } => {
                        background_force_logout = Some(slot);
                    }
                    _ => {}
                }
            }
        }

        // 2. Sync MainMenu accounts list, tray accounts menu, and summed unread counters.
        self.sync_tray_and_menus();

        // 3. Render active account's workspace.
        let mut action = MultiAccountFrameAction::None;
        let mut active_palette = crate::stand_in::theme::Palette::day();
        let mut active_passcode_locked = false;
        let mut active_focused_peer = None;
        let mut active_session_id = 1u64;

        let mut updated_badge_prefs: Option<(bool, bool)> = None;
        if let Some(entry) = self.accounts.get_mut(&self.active_slot) {
            let logout_requested = entry.workspace.show(ctx, ui);
            active_palette = entry.workspace.controller().palette();
            active_passcode_locked =
                entry.workspace.controller().root_screen() == RootScreen::PasscodeLock;
            active_focused_peer = entry.workspace.controller().active_focused_peer();
            active_session_id = entry.session_id;

            if let Some(prefs) = entry.workspace.controller_mut().notification_badge_prefs() {
                if let Some(prev) = entry.last_settings_badge_prefs
                    && prev != prefs
                {
                    updated_badge_prefs = Some(prefs);
                }
                entry.last_settings_badge_prefs = Some(prefs);
            }

            if logout_requested {
                action = MultiAccountFrameAction::LogoutAccount(self.active_slot);
            } else if let Some(target_slot) = entry
                .workspace
                .controller_mut()
                .take_switch_account_request()
            {
                if self.switch_to(target_slot) {
                    action = MultiAccountFrameAction::SwitchedAccount(target_slot);
                }
            } else if entry.workspace.controller_mut().take_add_account_request() {
                action = MultiAccountFrameAction::AddAccount;
            }
        }
        if let Some((include_muted, count_messages)) = updated_badge_prefs {
            self.set_all_accounts_unread_prefs(include_muted, count_messages);
        }

        // 4. Drive the ONE shared AppNotifyTrayHost (all accounts' notifications + tray menu + summed badge).
        let mut tray_switch_slot: Option<i32> = None;
        if let Some(notify_tray) = self.notify_tray.as_mut() {
            notify_tray.set_active_session_id(active_session_id);
            notify_tray.set_active_peer(active_focused_peer);
            notify_tray.update_and_render(ctx, &active_palette, active_passcode_locked);
            if let Some(slot) = notify_tray
                .take_account_switch_requests()
                .into_iter()
                .last()
            {
                tray_switch_slot = Some(slot);
            }
        }

        if let Some(target_slot) = tray_switch_slot
            && matches!(action, MultiAccountFrameAction::None)
            && self.switch_to(target_slot)
        {
            action = MultiAccountFrameAction::SwitchedAccount(target_slot);
        }

        // 5. Process cross-account notification clicks: switch to target account & open target chat!
        let pending_opens = self
            .notify_state
            .lock()
            .map(|mut g| g.drain_pending_opens())
            .unwrap_or_default();
        for open in pending_opens {
            if self.accounts.contains_key(&open.slot_index) {
                let switched = self.active_slot != open.slot_index;
                if switched {
                    self.switch_to(open.slot_index);
                    if matches!(action, MultiAccountFrameAction::None) {
                        action = MultiAccountFrameAction::SwitchedAccount(open.slot_index);
                    }
                }
                if let Some(entry) = self.accounts.get_mut(&open.slot_index) {
                    entry
                        .workspace
                        .controller_mut()
                        .open_chat_from_notification(
                            open.peer_id,
                            open.msg_id,
                            open.draft,
                            Some(ctx),
                        );
                }
            }
        }

        if matches!(action, MultiAccountFrameAction::None)
            && let Some(slot) = background_force_logout
        {
            action = MultiAccountFrameAction::LogoutAccount(slot);
        }

        action
    }

    /// Polls all running accounts, tray updates, and cross-account notifications while the UI is
    /// temporarily showing the `"Add Account"` login intro screen.
    ///
    /// Returns `Some(target_slot)` if the user clicked an existing account in the tray menu or
    /// clicked a notification from an existing account so the host can cancel the pending add and
    /// jump directly to that account (and chat).
    pub fn poll_background_only(&mut self, ctx: &egui::Context) -> Option<i32> {
        for entry in self.accounts.values_mut() {
            entry.workspace.live_session().set_egui_ctx(ctx.clone());
            if let Ok(guard) = entry.runtime.session().try_read()
                && let Some(user) = guard.peer(guard.self_peer_id()).and_then(|p| p.as_user())
            {
                let live_premium = user.is_premium();
                if entry.last_session_premium != Some(live_premium) {
                    if entry.last_session_premium.is_some() || live_premium {
                        entry.is_premium = live_premium;
                    }
                    entry.last_session_premium = Some(live_premium);
                }
            }
            let events = entry
                .workspace
                .live_session()
                .bridge_state()
                .drain_session_events();
            for ev in events {
                if matches!(ev, SessionEvent::Ready) {
                    entry.workspace.live_session_mut().refresh_cached_views();
                }
            }
        }

        self.sync_tray_and_menus();

        let mut switch_target: Option<i32> = None;
        let active_palette = self
            .accounts
            .get(&self.active_slot)
            .map(|e| e.workspace.controller().palette())
            .unwrap_or_else(crate::stand_in::theme::Palette::day);
        if let Some(notify_tray) = self.notify_tray.as_mut() {
            notify_tray.set_active_peer(None);
            notify_tray.update_and_render(ctx, &active_palette, false);
            if let Some(slot) = notify_tray
                .take_account_switch_requests()
                .into_iter()
                .last()
                && self.accounts.contains_key(&slot)
            {
                switch_target = Some(slot);
            }
        }

        let pending_opens = self
            .notify_state
            .lock()
            .map(|mut g| g.drain_pending_opens())
            .unwrap_or_default();
        for open in pending_opens {
            if self.accounts.contains_key(&open.slot_index) {
                switch_target = Some(open.slot_index);
                if let Some(entry) = self.accounts.get_mut(&open.slot_index) {
                    entry
                        .workspace
                        .controller_mut()
                        .open_chat_from_notification(
                            open.peer_id,
                            open.msg_id,
                            open.draft,
                            Some(ctx),
                        );
                }
            }
        }

        switch_target
    }

    /// Flushes settings and cleanly shuts down every account's workspace, runtime, and the shared
    /// tray/notifications host on exit.
    pub fn on_exit(&mut self) {
        if let Some(active) = self.accounts.get(&self.active_slot) {
            self.settings = active.workspace.controller().settings().clone();
        }
        if let Some(notify_tray) = self.notify_tray.as_mut() {
            notify_tray.on_exit();
        }
        if let Ok(mut guard) = self.notify_state.lock() {
            guard.slots.clear();
        }
        let entries = std::mem::take(&mut self.accounts);
        for (slot_index, mut entry) in entries {
            entry.workspace.live_session().cleanup_account_temp_files();
            entry.workspace.on_exit();
            let runtime = entry.runtime;
            drop(entry.workspace);
            if let Ok(rt) = Arc::try_unwrap(runtime) {
                rt.shutdown();
            }
            if let Some(ref tdata_dir) = self.tdata_dir {
                let acc_temp = account_temp_dir(tdata_dir, &self.data_name, slot_index);
                cleanup_unlocked_dir_files(&acc_temp);
            }
        }
        sync_process_temp_dirs_for_account(None);
    }

    /// Rebuilds the synchronized Main Menu account list, Tray accounts menu, and summed unread
    /// entries immediately (also callable by tests after pushing background updates).
    pub fn sync_tray_and_menus(&mut self) {
        let max_accounts = self.max_accounts();
        let mut menu_accounts = Vec::with_capacity(self.accounts.len());
        let mut tray_accounts = Vec::with_capacity(self.accounts.len());
        let mut unread_entries = Vec::with_capacity(self.accounts.len());

        for entry in self.accounts.values_mut() {
            let cur_gen = entry.workspace.live_session().bridge_state().generation();
            if cur_gen != entry.last_bridge_gen
                || entry.workspace.live_session().current_user().is_none()
            {
                entry.workspace.live_session_mut().refresh_cached_views();
                entry.last_bridge_gen = cur_gen;
            }
            let session = entry.workspace.live_session();
            let user = session.current_user();
            let unread_entry = AccountUnreadEntry {
                state: session.unread_state(),
                include_muted_counter: entry.include_muted_counter,
                count_unread_messages: entry.count_unread_messages,
            };
            let badge = unread_entry.compute_badge();
            unread_entries.push(unread_entry);

            let (name, initials, avatar_rgb, phone) = if let Some(u) = user {
                (
                    u.name.clone(),
                    u.initials.clone(),
                    u.avatar_rgb,
                    u.phone.clone(),
                )
            } else {
                (
                    format!("Account {}", entry.slot_index + 1),
                    format!("A{}", entry.slot_index + 1),
                    [0x40, 0xA7, 0xE3],
                    String::new(),
                )
            };

            let is_active = entry.slot_index == self.active_slot;
            menu_accounts.push(MainMenuAccountItem {
                slot_index: entry.slot_index,
                name: name.clone(),
                initials,
                avatar_rgb,
                phone,
                is_active,
                is_premium: entry.is_premium,
                unread_count: badge.count,
                unread_muted: badge.muted,
            });
            tray_accounts.push(TrayAccountEntry {
                slot_index: entry.slot_index,
                display_name: name.clone(),
                is_active,
                unread_count: badge.count,
                unread_muted: badge.muted,
            });

            if let Ok(mut guard) = self.notify_state.lock()
                && let Some(slot) = guard.slots.get_mut(&entry.slot_index)
            {
                slot.display_name = name;
                slot.include_muted_counter = entry.include_muted_counter;
                slot.count_unread_messages = entry.count_unread_messages;
            }
        }

        if let Some(active) = self.accounts.get_mut(&self.active_slot) {
            active
                .workspace
                .controller_mut()
                .set_menu_accounts(menu_accounts, max_accounts);
        }
        if let Some(notify_tray) = self.notify_tray.as_mut() {
            notify_tray.set_accounts(tray_accounts);
            notify_tray.set_account_unread_entries(unread_entries);
        }
    }

    fn collect_unread_entries(&self) -> Vec<AccountUnreadEntry> {
        self.accounts
            .values()
            .map(|entry| AccountUnreadEntry {
                state: entry.workspace.live_session().unread_state(),
                include_muted_counter: entry.include_muted_counter,
                count_unread_messages: entry.count_unread_messages,
            })
            .collect()
    }
}

fn compute_session_id(user_id: UserId, is_test_mode: bool) -> u64 {
    let base = user_id.0.max(1);
    if is_test_mode {
        base | 0x0100_0000_0000_0000
    } else {
        base
    }
}

#[cfg(test)]
mod sec2b_host_tests {
    use super::*;
    use crate::media::backend::MediaItemId;
    use crate::notifications::manager::NotificationSystem;
    use crate::notifications::settings::{NotificationPreferences, NotifySettingsStore};
    use crate::tray::MockTrayPlatform;
    use tg_api::{DcId, InvocationError, RpcClient};
    use tg_session::SessionRuntimeConfig;
    use tg_tl::RemoteCall;

    #[derive(Clone, Default)]
    struct NoopRpcClient;

    impl RpcClient for NoopRpcClient {
        type Error = InvocationError;

        async fn invoke<R: RemoteCall + Send + Sync>(
            &self,
            _req: &R,
        ) -> Result<R::Return, Self::Error> {
            Err(InvocationError::Dropped)
        }

        async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
            &self,
            _dc_id: DcId,
            _req: &R,
        ) -> Result<R::Return, Self::Error> {
            Err(InvocationError::Dropped)
        }

        fn main_dc_id(&self) -> DcId {
            2
        }

        fn set_main_dc_id(&self, _dc_id: DcId) {}
    }

    #[test]
    fn d2_account_media_cache_dir_keyed_by_data_key_not_slot() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let tdata = dir.path().join("tdata");
        let slot0 = account_media_cache_dir(&tdata, 0);
        let slot1 = account_media_cache_dir(&tdata, 1);
        let alt_profile_slot0 = account_media_cache_dir_for_data_name(&tdata, "work", 0);

        let name0 = slot0.file_name().and_then(|s| s.to_str()).unwrap_or("");
        assert_ne!(
            name0, "account_0",
            "media cache folder must be keyed by account data key, not slot index"
        );
        assert_eq!(
            name0,
            tg_session::account::account_data_key_file_part("data", 0)
        );
        assert_ne!(slot0, slot1);
        assert_ne!(slot0, alt_profile_slot0);
    }

    #[test]
    fn d4_host_wires_real_downloads_folder_to_media_overlay() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let system = NotificationSystem::headless(
            NotificationPreferences::default(),
            NotifySettingsStore::default(),
            dir.path().join("userpics"),
        );
        let mut host = MultiAccountWorkspaceHost::new_headless_with_tray(
            ShellSettings::default(),
            dir.path().join("shell.json"),
            system,
            Box::new(MockTrayPlatform::default()),
        );

        let default_dl = host.downloads_dir().to_path_buf();
        assert_ne!(
            default_dl,
            std::env::temp_dir().join("rustgram_downloads"),
            "host must resolve real Downloads folder, never %TEMP%\\rustgram_downloads"
        );

        // Override downloads dir to an isolated TempDir and verify overlay receives it and saves there.
        let custom_dl = dir.path().join("real_downloads");
        std::fs::create_dir_all(&custom_dl).expect("mkdir downloads");
        host.set_downloads_dir(custom_dl.clone());

        let rt = Arc::new(
            SessionRuntime::start(
                UserId(42),
                ErasedRpcClient::new(NoopRpcClient),
                SessionRuntimeConfig {
                    init_updates_state: false,
                    run_get_difference_on_start: false,
                    load_initial_dialogs: false,
                    fetch_self_user_on_start: false,
                    ..SessionRuntimeConfig::default()
                },
            )
            .expect("start runtime"),
        );
        host.add_account(0, UserId(42), false, false, rt, None, true, false);

        // Verify the media overlay saves into `custom_dl` (NOT %TEMP%\rustgram_downloads) with Zone.Identifier.
        let entry = host.account_mut(0).expect("slot 0");
        let live = entry.workspace.live_session().clone();
        let photo_id = 99_001u64;
        let photo = tg_data::PhotoData {
            id: tg_data::PhotoId(photo_id),
            access_hash: 1,
            file_reference: vec![1, 2],
            date: 1_700_000_000,
            dc_id: 2,
            sizes: vec![tg_data::PhotoSize {
                kind: "y".to_owned(),
                width: 32,
                height: 32,
                size: 14,
            }],
            ..tg_data::PhotoData::default()
        };
        let ckey = crate::live::media_cache::photo_cache_key(2, photo_id, "y");
        live.media_cache()
            .put_blob(
                &ckey,
                crate::live::media_cache::MediaCacheTag::Photo,
                b"DOWNLOADED_IMG",
            )
            .expect("seed blob");
        let peer = tg_data::PeerId::from_user(UserId(99));
        let mut msg = tg_data::Message::new_text(
            peer,
            tg_data::MsgId(50),
            Some(peer),
            1_700_000_000,
            tg_data::TextWithEntities::plain(String::new()),
        );
        msg.flags = tg_data::MessageFlags::empty();
        msg.content = tg_data::MessageContent::Regular {
            text: tg_data::TextWithEntities::plain(String::new()),
            media: tg_data::MessageMedia::Photo(Box::new(photo)),
        };
        let sess = Arc::clone(live.runtime().session());
        live.runtime().tokio_handle().block_on(async move {
            sess.write().await.insert_message(msg);
        });
        let item_id = MediaItemId(photo_id);

        let mut events = Vec::new();
        let ctrl = entry.workspace.controller_mut();
        ctrl.media_overlay_mut()
            .open_item(&live, peer.0, item_id, 1_000)
            .expect("open photo in media overlay");
        ctrl.media_overlay_mut()
            .save_current_item(&live, 1_000, &mut events);

        let saved_path = events
            .iter()
            .find_map(|ev| match ev {
                crate::media::OverlayEvent::Saved(path) => Some(path.clone()),
                _ => None,
            })
            .expect("overlay save_current_item must emit OverlayEvent::Saved");
        let canon_dl = std::fs::canonicalize(&custom_dl).expect("canon dl");
        let canon_saved = std::fs::canonicalize(&saved_path).expect("canon saved");
        assert!(
            canon_saved.starts_with(&canon_dl),
            "viewer must save into host-wired Downloads dir {}, got {}",
            canon_dl.display(),
            canon_saved.display()
        );
        #[cfg(windows)]
        {
            let mut ads = saved_path.as_os_str().to_os_string();
            ads.push(":Zone.Identifier");
            let zone = std::fs::read_to_string(PathBuf::from(ads)).expect("Zone.Identifier ADS");
            assert!(zone.contains("ZoneId=3"));
        }

        host.on_exit();
    }
}
