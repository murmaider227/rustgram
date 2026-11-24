//! Export lifecycle manager, panel controller, settings persistence, 24-hour takeout wait
//! handling, and real async [`tg_export::ExportController`] execution.
//!
//! Port of `Telegram/SourceFiles/export/export_manager.h`,
//! `Telegram/SourceFiles/export/export_manager.cpp`,
//! `Telegram/SourceFiles/export/view/export_view_panel_controller.h`, and
//! `Telegram/SourceFiles/export/view/export_view_panel_controller.cpp`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tg_export::stand_in::{InputPeer, TimeId};
use tg_export::{
    ApiError, Environment, ExportControlHandle, ExportController, ExportRpc, ExportSource,
    ExportState, FinishedState, InMemoryExportSource, ProcessingState, Settings, TlExportSource,
};

use crate::export::content::{content_from_processing, content_from_state};
use crate::export::demo_source::{PacedExportSource, SkipFileSignal, build_demo_export_source};
use crate::export::progress_view::{ProgressView, ProgressViewAction};
use crate::export::settings_view::{
    DateLimitKind, DatePickerState, SettingsView, SettingsViewAction, TimePickerState,
    apply_from_date, apply_from_time, apply_till_date, apply_till_time, format_date_time_full,
    is_default_export_path,
};
use crate::export::top_bar::ExportTopBar;

/// Debounce timeout for persisting export settings (`kSaveSettingsTimeout = 1000ms`).
pub const SAVE_SETTINGS_TIMEOUT: Duration = Duration::from_millis(1000);

/// Default export panel dimensions (`st::exportPanelSize = size(364px, 480px)`).
pub const EXPORT_PANEL_WIDTH: f32 = 364.0;
/// Default export panel height (`st::exportPanelSize = size(364px, 480px)`).
pub const EXPORT_PANEL_HEIGHT: f32 = 480.0;

// ============================================================================
// Platform & Persistence Abstractions (injectable for deterministic tests)
// ============================================================================

/// Abstraction over the native OS folder chooser dialog (`FileDialog::GetFolder`).
pub trait FolderChooser: Send + Sync {
    /// Opens a folder chooser dialog starting at `initial_dir` and returns the selected directory.
    fn pick_folder(&self, title: &str, initial_dir: &str) -> Option<PathBuf>;
}

/// Native folder chooser backed by the `rfd` crate.
#[derive(Debug, Default, Clone, Copy)]
pub struct RfdFolderChooser;

impl FolderChooser for RfdFolderChooser {
    fn pick_folder(&self, title: &str, initial_dir: &str) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new().set_title(title);
        if !initial_dir.is_empty() {
            let p = Path::new(initial_dir);
            if p.exists() {
                dialog = dialog.set_directory(p);
            }
        }
        dialog.pick_folder()
    }
}

/// Abstraction over revealing an exported file/folder in the OS file manager (`File::ShowInFolder`).
pub trait FolderOpener: Send + Sync {
    /// Reveals `path` in Explorer / file manager.
    fn show_in_folder(&self, path: &Path) -> Result<(), String>;
}

/// Native folder opener backed by [`tg_platform::system_info::show_in_folder`].
#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformFolderOpener;

impl FolderOpener for PlatformFolderOpener {
    fn show_in_folder(&self, path: &Path) -> Result<(), String> {
        tg_platform::system_info::show_in_folder(path).map_err(|e| e.to_string())
    }
}

/// Persistence store for [`tg_export::Settings`] using tdesktop's `QDataStream` binary format
/// (`Settings::serialize_qt` / `Settings::deserialize_qt`).
pub trait SettingsStore: Send + Sync {
    /// Loads persisted export settings, or returns `Settings::default()` if absent/corrupt.
    fn load(&self) -> Settings;
    /// Persists export settings bytes.
    fn save(&self, settings: &Settings) -> Result<(), String>;
}

/// File-backed settings store serializing via [`Settings::serialize_qt`] / [`Settings::deserialize_qt`].
#[derive(Debug, Clone)]
pub struct QtFileSettingsStore {
    path: PathBuf,
}

impl QtFileSettingsStore {
    /// Creates a store reading and writing `path`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Returns the default settings binary file path under the user's local app data / temp dir.
    #[must_use]
    pub fn default_path() -> PathBuf {
        if let Some(dir) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
            return PathBuf::from(dir)
                .join("Rustgram")
                .join("export_settings.bin");
        }
        std::env::temp_dir().join("rustgram_export_settings.bin")
    }

    /// Returns the backing file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl SettingsStore for QtFileSettingsStore {
    fn load(&self) -> Settings {
        match std::fs::read(&self.path) {
            Ok(bytes) => Settings::deserialize_qt(&bytes).unwrap_or_default(),
            Err(_) => Settings::default(),
        }
    }

    fn save(&self, settings: &Settings) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = settings.serialize_qt();
        std::fs::write(&self.path, bytes).map_err(|e| e.to_string())
    }
}

/// Computes the default download/export root directory (`File::DefaultDownloadPath`).
#[must_use]
pub fn default_export_download_path() -> String {
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(profile)
            .join("Downloads")
            .join("Telegram Desktop");
        return p.to_string_lossy().into_owned();
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(home)
            .join("Downloads")
            .join("Telegram Desktop");
        return p.to_string_lossy().into_owned();
    }
    std::env::temp_dir()
        .join("RustgramExport")
        .to_string_lossy()
        .into_owned()
}

/// Resolves empty paths and single-peer date limits (port of `ResolveSettings` in `export_view_panel_controller.cpp:129-139`).
pub fn resolve_export_settings(settings: &mut Settings, default_download_path: &str) {
    if settings.path.is_empty() {
        settings.path = default_download_path.to_owned();
        settings.force_sub_path = true;
    } else {
        settings.force_sub_path = is_default_export_path(&settings.path, default_download_path);
    }
    if !settings.only_single_peer() {
        settings.single_peer_from = 0;
        settings.single_peer_till = 0;
    }
}

// ============================================================================
// 24-Hour Takeout Security Delay Parsing (`TAKEOUT_INIT_DELAY_<seconds>`)
// ============================================================================

/// Parsed `TAKEOUT_INIT_DELAY_<seconds>` wait state (port of `PanelController::showError`
/// in `export_view_panel_controller.cpp:234-258`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeoutDelayInfo {
    /// Required delay in seconds (`>= 1`).
    pub delay_seconds: i32,
    /// Whole hours remaining (`delay_seconds / 3600`).
    pub hours: i32,
    /// Unix timestamp when export becomes available (`now + delay_seconds`).
    pub available_at: TimeId,
    /// Human-readable hours fragment (`"less than an hour"`, `"1 hour"`, `"24 hours"`).
    pub hours_text: String,
    /// Formatted availability date/time string.
    pub available_date_text: String,
    /// Complete localized Telegram security notice (`lng_export_delay`).
    pub message: String,
}

/// Parses a `TAKEOUT_INIT_DELAY_<seconds>` RPC error kind at `now_unix` seconds,
/// delegating delay extraction to [`ApiError::parse_takeout_init_delay`] and formatting
/// localized copy via `tg_lang::tr::lng_export_delay`.
#[must_use]
pub fn parse_takeout_init_delay(error_kind: &str, now_unix: TimeId) -> Option<TakeoutDelayInfo> {
    let api_err = ApiError::new(420, error_kind, "");
    takeout_delay_info_from_error(&api_err, now_unix)
}

/// Builds a [`TakeoutDelayInfo`] from an [`ApiError`] using [`ApiError::parse_takeout_init_delay`].
#[must_use]
pub fn takeout_delay_info_from_error(
    error: &ApiError,
    now_unix: TimeId,
) -> Option<TakeoutDelayInfo> {
    let parsed = error.parse_takeout_init_delay()?.max(1);
    let hours = parsed / 3600;
    let hours_text = if hours <= 0 {
        tg_lang::tr::lng_export_delay_less_than_hour()
    } else if hours == 1 {
        "1 hour".to_owned()
    } else {
        format!("{hours} hours")
    };
    let available_at = now_unix.saturating_add(parsed);
    let available_date_text = format_date_time_full(available_at);
    let message = tg_lang::tr::lng_export_delay(&available_date_text, &hours_text);
    Some(TakeoutDelayInfo {
        delay_seconds: parsed,
        hours,
        available_at,
        hours_text,
        available_date_text,
        message,
    })
}

/// Current Unix timestamp as [`TimeId`].
#[must_use]
pub fn current_unix_time() -> TimeId {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i32::MAX as u64) as TimeId)
        .unwrap_or(1_735_689_600)
}

// ============================================================================
// Export Target, Screen, and Modal States
// ============================================================================

/// Entry target that opened the export panel.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportTarget {
    /// Full account export ("Export Telegram data").
    FullAccount,
    /// Single-chat export ("Export chat history" from a chat's menu).
    SingleChat {
        /// TL peer handle.
        peer: InputPeer,
        /// Display title of the chat.
        title: String,
    },
    /// Single forum topic export ("Export topic history").
    SingleTopic {
        /// Forum supergroup TL peer handle.
        peer: InputPeer,
        /// Forum topic root message ID.
        topic_root_id: i32,
        /// Forum supergroup peer ID.
        peer_id: u64,
        /// Display title of the topic.
        topic_title: String,
    },
}

/// Active screen inside the Export panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportScreen {
    /// Settings configuration screen (`SettingsWidget`).
    Settings,
    /// Running or completed progress screen (`ProgressWidget`).
    Progress,
    /// Fatal API or disk I/O error message (`PanelController::showCriticalError`).
    CriticalError(String),
}

/// Modal dialog displayed over the Export panel or workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportModal {
    /// Confirm stopping an in-progress export (`lng_export_sure_stop`).
    ConfirmStop,
    /// Interactive calendar date picker for single-chat `From` / `To` date bounds.
    DatePicker {
        /// Which boundary (`From` or `Till`) is being edited.
        kind: DateLimitKind,
        /// Calendar picker state.
        state: DatePickerState,
    },
    /// Interactive time-of-day picker for single-chat `From` / `To` bounds.
    TimePicker {
        /// Which boundary (`From` or `Till`) is being edited.
        kind: DateLimitKind,
        /// Time picker state.
        state: TimePickerState,
    },
    /// Inform modal (`TAKEOUT_INVALID`, `TAKEOUT_INIT_DELAY_*`, or folder-open error).
    Inform {
        /// Dialog title.
        title: String,
        /// Body text.
        message: String,
        /// If `true`, closing the inform modal closes the export panel (`export_view_panel_controller.cpp:297-301`).
        close_panel_on_ok: bool,
    },
    /// Prompt shown once a previous 24-hour takeout delay has elapsed (`SuggestBox`).
    SuggestStart {
        /// Timestamp when the export became available.
        available_at: TimeId,
    },
}

/// Live handle to a background [`ExportController`] run.
struct ActiveRun {
    control: ExportControlHandle,
    skip_signal: SkipFileSignal,
    last_state: ExportState,
}

/// Factory creating an [`InMemoryExportSource`] for demo runs (or simulating `TAKEOUT_INIT_DELAY_*`).
pub type DemoSourceBuilder =
    Arc<dyn Fn(&ExportTarget) -> InMemoryExportSource + Send + Sync + 'static>;

/// Type-erased live export starter wired by `crate::live::export` so [`ExportManager`]
/// runs [`TlExportSource`] over a live [`crate::live::export::LiveExportRpc`] without
/// making [`ExportManager`] generic over `RpcClient`.
pub type LiveExportStarter = Arc<
    dyn Fn(
            &ExportTarget,
            &Settings,
            SkipFileSignal,
        ) -> (
            ExportControlHandle,
            Pin<Box<dyn Future<Output = ExportState> + Send + 'static>>,
        ) + Send
        + Sync
        + 'static,
>;

/// Single-instance export manager and panel controller (`Export::Manager` + `Export::View::PanelController`).
///
/// Guarantees at most one export panel/run exists at a time: attempting to start another export
/// while one is open activates and focuses the existing panel.
pub struct ExportManager {
    /// Owned fallback runtime used when no external Tokio handle is injected.
    _owned_runtime: Option<Arc<tokio::runtime::Runtime>>,
    tokio_handle: tokio::runtime::Handle,
    folder_chooser: Box<dyn FolderChooser>,
    folder_opener: Box<dyn FolderOpener>,
    settings_store: Box<dyn SettingsStore>,
    default_download_path: String,
    demo_step_delay: Duration,
    demo_source_builder: DemoSourceBuilder,
    /// Optional live `TlExportSource` starter (replaces `demo_source_builder` in live mode).
    live_starter: Option<LiveExportStarter>,
    /// Skip `DataExport_YYYY-MM-DD` subdirectory suffix when writing output (mainly for tests).
    skip_path_normalization: bool,

    /// Persisted / active export settings.
    settings: Settings,
    settings_dirty_since: Option<Instant>,

    /// Whether an export session (panel or running export) is active.
    active: bool,
    /// Whether the floating export panel is currently visible (can be hidden while running; top bar stays visible).
    panel_visible: bool,
    /// Current export target (full account, single chat, or single topic).
    target: ExportTarget,
    /// Last single-chat title remembered across a 24h takeout delay resume.
    remembered_single_chat_title: String,
    /// Active screen inside the panel.
    screen: ExportScreen,
    /// Active child modal dialog, if any.
    modal: Option<ExportModal>,
    /// Settings widget UI state.
    settings_view: SettingsView,
    /// Progress widget UI state.
    progress_view: ProgressView,
    /// Persistent top bar state while running.
    top_bar: ExportTopBar,
    /// Background `ExportController` task handle when running or just finished.
    active_run: Option<ActiveRun>,
    /// Latest takeout delay info (if `TAKEOUT_INIT_DELAY_*` occurred).
    takeout_delay: Option<TakeoutDelayInfo>,
    /// Latest finished state (for `"Show my data"`).
    finished_state: Option<FinishedState>,
}

impl ExportManager {
    /// Creates a new `ExportManager` wired to `rfd` folder chooser, `tg-platform` Explorer opener,
    /// Qt-binary settings persistence, and the built-in demo `InMemoryExportSource`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_dependencies(
            Box::new(RfdFolderChooser),
            Box::new(PlatformFolderOpener),
            Box::new(QtFileSettingsStore::new(QtFileSettingsStore::default_path())),
            default_export_download_path(),
            Duration::from_millis(140),
        )
    }

    /// Creates an `ExportManager` with injected platform/persistence dependencies (ideal for tests).
    #[must_use]
    pub fn with_dependencies(
        folder_chooser: Box<dyn FolderChooser>,
        folder_opener: Box<dyn FolderOpener>,
        settings_store: Box<dyn SettingsStore>,
        default_download_path: String,
        demo_step_delay: Duration,
    ) -> Self {
        Self::with_dependencies_and_runtime(
            folder_chooser,
            folder_opener,
            settings_store,
            default_download_path,
            demo_step_delay,
            None,
        )
    }

    /// Creates an `ExportManager` with injected dependencies and an optional shared Tokio runtime handle.
    #[must_use]
    pub fn with_dependencies_and_runtime(
        folder_chooser: Box<dyn FolderChooser>,
        folder_opener: Box<dyn FolderOpener>,
        settings_store: Box<dyn SettingsStore>,
        default_download_path: String,
        demo_step_delay: Duration,
        external_handle: Option<tokio::runtime::Handle>,
    ) -> Self {
        let (owned_runtime, tokio_handle) = if let Some(handle) = external_handle {
            (None, handle)
        } else {
            let runtime = Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .thread_name("rustgram-export")
                    .build()
                    .unwrap_or_else(|_| {
                        tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("build fallback tokio runtime for export")
                    }),
            );
            let handle = runtime.handle().clone();
            (Some(runtime), handle)
        };
        let mut settings = settings_store.load();
        resolve_export_settings(&mut settings, &default_download_path);
        let initial_content = content_from_processing(&settings, &ProcessingState::default());

        Self {
            _owned_runtime: owned_runtime,
            tokio_handle,
            folder_chooser,
            folder_opener,
            settings_store,
            default_download_path,
            demo_step_delay,
            demo_source_builder: Arc::new(|target| match target {
                ExportTarget::FullAccount => build_demo_export_source(None),
                ExportTarget::SingleChat { peer, title } => {
                    build_demo_export_source(Some((peer, title)))
                }
                ExportTarget::SingleTopic {
                    peer, topic_title, ..
                } => build_demo_export_source(Some((peer, topic_title))),
            }),
            live_starter: None,
            skip_path_normalization: false,
            settings,
            settings_dirty_since: None,
            active: false,
            panel_visible: false,
            target: ExportTarget::FullAccount,
            remembered_single_chat_title: String::new(),
            screen: ExportScreen::Settings,
            modal: None,
            settings_view: SettingsView::new(),
            progress_view: ProgressView::new(initial_content.clone()),
            top_bar: ExportTopBar::new(&initial_content),
            active_run: None,
            takeout_delay: None,
            finished_state: None,
        }
    }

    /// Installs a live [`LiveExportStarter`] backed by [`TlExportSource`], replacing
    /// [`DemoSourceBuilder`] whenever the user clicks Export.
    pub fn set_live_export_starter(&mut self, starter: LiveExportStarter) {
        self.live_starter = Some(starter);
    }

    /// Removes any installed [`LiveExportStarter`], returning to demo mode.
    pub fn clear_live_export_starter(&mut self) {
        self.live_starter = None;
    }

    /// Returns `true` when wired to a live [`TlExportSource`] starter instead of the demo builder.
    #[must_use]
    pub fn is_live_mode(&self) -> bool {
        self.live_starter.is_some()
    }

    /// Overrides the Tokio runtime handle used to spawn background export controllers.
    pub fn set_tokio_handle(&mut self, handle: tokio::runtime::Handle) {
        self.tokio_handle = handle;
    }

    /// Controls whether [`ExportController::set_skip_path_normalization`] is enabled for direct runs.
    pub fn set_skip_path_normalization(&mut self, skip: bool) {
        self.skip_path_normalization = skip;
    }

    /// Overrides the demo [`InMemoryExportSource`] builder (e.g. to inject a `TAKEOUT_INIT_DELAY_86400`
    /// error or custom dataset in tests).
    pub fn set_demo_source_builder(&mut self, builder: DemoSourceBuilder) {
        self.demo_source_builder = builder;
    }

    /// Returns `true` if any export session (settings or running export) is active (`Manager::inProgress`).
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Returns `true` if the background export controller is actively processing (`v::is<ProcessingState>`).
    #[must_use]
    pub fn is_export_running(&self) -> bool {
        self.active_run
            .as_ref()
            .is_some_and(|r| matches!(r.last_state, ExportState::Processing(_)))
    }

    /// Returns `true` if the export panel window is currently visible.
    #[must_use]
    pub const fn is_panel_visible(&self) -> bool {
        self.active && self.panel_visible
    }

    /// Returns `true` if the persistent top bar should be shown (`Manager::inProgress` during processing).
    #[must_use]
    pub fn should_show_top_bar(&self) -> bool {
        self.active && self.is_export_running()
    }

    /// Returns the active [`ExportScreen`].
    #[must_use]
    pub const fn screen(&self) -> &ExportScreen {
        &self.screen
    }

    /// Returns the active [`ExportModal`], if any.
    #[must_use]
    pub const fn modal(&self) -> Option<&ExportModal> {
        self.modal.as_ref()
    }

    /// Returns the current [`Settings`].
    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Returns mutable access to [`Settings`] (for tests/previews).
    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// Returns the default download path used for path normalization.
    #[must_use]
    pub fn default_download_path(&self) -> &str {
        &self.default_download_path
    }

    /// Returns the current [`ExportTarget`].
    #[must_use]
    pub const fn target(&self) -> &ExportTarget {
        &self.target
    }

    /// Returns the panel window title matching `PanelController::createPanel` / `showProgress` / `updateState`.
    #[must_use]
    pub fn panel_title(&self) -> &'static str {
        if self.screen == ExportScreen::Progress && !self.progress_view.is_done() {
            return "Exporting your data";
        }
        if self.settings.only_single_topic() {
            "Topic export settings"
        } else if self.settings.only_single_peer() {
            "Chat export settings"
        } else {
            "Export Your Data"
        }
    }

    /// Returns the latest [`TakeoutDelayInfo`], if a `TAKEOUT_INIT_DELAY_*` error occurred.
    #[must_use]
    pub const fn takeout_delay(&self) -> Option<&TakeoutDelayInfo> {
        self.takeout_delay.as_ref()
    }

    /// Returns the latest [`FinishedState`], if the export completed successfully.
    #[must_use]
    pub const fn finished_state(&self) -> Option<&FinishedState> {
        self.finished_state.as_ref()
    }

    /// Returns a reference to [`SettingsView`].
    #[must_use]
    pub const fn settings_view(&self) -> &SettingsView {
        &self.settings_view
    }

    /// Returns a mutable reference to [`SettingsView`].
    pub fn settings_view_mut(&mut self) -> &mut SettingsView {
        &mut self.settings_view
    }

    /// Returns a reference to [`ProgressView`].
    #[must_use]
    pub const fn progress_view(&self) -> &ProgressView {
        &self.progress_view
    }

    /// Returns a mutable reference to [`ProgressView`].
    pub fn progress_view_mut(&mut self) -> &mut ProgressView {
        &mut self.progress_view
    }

    /// Returns a reference to [`ExportTopBar`].
    #[must_use]
    pub const fn top_bar(&self) -> &ExportTopBar {
        &self.top_bar
    }

    /// Opens the full account export settings panel (`Manager::start(session)`).
    ///
    /// If an export session is already active, activates its existing panel instead of starting
    /// a second concurrent export.
    pub fn start_full_export(&mut self) {
        if self.active {
            self.activate_panel();
            return;
        }
        self.open_with_target(ExportTarget::FullAccount);
    }

    /// Opens the reduced single-chat export settings panel ("Export chat history" from a chat's menu).
    ///
    /// If an export session is already active, activates its existing panel instead.
    pub fn start_single_chat_export(&mut self, peer: InputPeer, title: impl Into<String>) {
        if self.active {
            self.activate_panel();
            return;
        }
        self.open_with_target(ExportTarget::SingleChat {
            peer,
            title: title.into(),
        });
    }

    /// Opens the single-topic export settings panel (`Manager::startTopic`).
    pub fn start_single_topic_export(
        &mut self,
        peer: InputPeer,
        topic_root_id: i32,
        peer_id: u64,
        topic_title: impl Into<String>,
    ) {
        if self.active {
            self.activate_panel();
            return;
        }
        self.open_with_target(ExportTarget::SingleTopic {
            peer,
            topic_root_id,
            peer_id,
            topic_title: topic_title.into(),
        });
    }

    fn open_with_target(&mut self, target: ExportTarget) {
        let mut loaded = self.settings_store.load();
        match &target {
            ExportTarget::FullAccount => {
                loaded.single_peer = InputPeer::Empty;
                loaded.single_topic_root_id = 0;
                loaded.single_topic_peer_id = 0;
                loaded.single_topic_title.clear();
            }
            ExportTarget::SingleChat { peer, title } => {
                loaded.single_peer = peer.clone();
                loaded.single_topic_root_id = 0;
                loaded.single_topic_peer_id = 0;
                loaded.single_topic_title.clear();
                self.remembered_single_chat_title = title.clone();
            }
            ExportTarget::SingleTopic {
                peer,
                topic_root_id,
                peer_id,
                topic_title,
            } => {
                loaded.single_peer = peer.clone();
                loaded.single_topic_root_id = *topic_root_id;
                loaded.single_topic_peer_id = *peer_id;
                loaded.single_topic_title = topic_title.clone();
            }
        }
        resolve_export_settings(&mut loaded, &self.default_download_path);

        self.settings = loaded;
        self.target = target;
        self.active = true;
        self.panel_visible = true;
        self.screen = ExportScreen::Settings;
        self.modal = None;
        self.active_run = None;
        self.finished_state = None;
        self.takeout_delay = None;
        self.settings_view = SettingsView::new();
    }

    /// Restores and focuses the export panel window (`PanelController::activatePanel`).
    pub fn activate_panel(&mut self) {
        if self.active {
            self.panel_visible = true;
        }
    }

    /// Handles clicking the panel close button (`X`) or pressing Escape.
    ///
    /// While an export is running, hiding the panel keeps the export running in the background
    /// with the [`ExportTopBar`] visible (`export_view_panel_controller.cpp:183-187`).
    /// When not running (settings / done / error), this stops and closes the session completely.
    pub fn close_or_hide_panel(&mut self) {
        if !self.active {
            return;
        }
        if self.is_export_running() {
            self.panel_visible = false;
        } else {
            self.stop();
        }
    }

    /// Starts executing the export using the installed [`LiveExportStarter`] when in live mode,
    /// or falls back to [`Self::start_demo_export`] when running offline/demo.
    pub fn start_configured_export(&mut self) {
        if let Some(starter) = self.live_starter.clone() {
            self.clear_suggest_start();
            self.flush_settings();

            let skip_signal = SkipFileSignal::new();
            let (handle, fut) = starter(&self.target, &self.settings, skip_signal.clone());
            self.begin_progress_run(handle, skip_signal);
            self.tokio_handle.spawn(async move {
                let _ = fut.await;
            });
        } else {
            self.start_demo_export();
        }
    }

    /// Starts executing the real [`ExportController`] with the built-in demo [`InMemoryExportSource`].
    pub fn start_demo_export(&mut self) {
        let source = (self.demo_source_builder)(&self.target);
        let skip_signal = SkipFileSignal::new();
        let paced = PacedExportSource::new(source, self.demo_step_delay, skip_signal.clone());
        self.start_export_with_source(paced, skip_signal);
    }

    /// Starts executing the real [`ExportController`] with a live [`TlExportSource`] backed by `rpc`.
    pub fn start_tl_export<C: ExportRpc + 'static>(&mut self, rpc: C, utc_offset_secs: i32) {
        let skip_signal = SkipFileSignal::new();
        let paced = PacedExportSource::from_tl_rpc(rpc, utc_offset_secs, skip_signal.clone());
        self.start_export_with_source(paced, skip_signal);
    }

    /// Starts executing the real [`ExportController`] with any [`ExportSource`] implementation.
    pub fn start_export_with_source<S: ExportSource + 'static>(
        &mut self,
        source: S,
        skip_signal: SkipFileSignal,
    ) {
        // Clear any pending 24h wait suggestion (`PanelController::showProgress`).
        self.clear_suggest_start();
        self.flush_settings();

        let (mut controller, handle) = match &self.target {
            ExportTarget::SingleTopic {
                peer,
                topic_root_id,
                peer_id,
                topic_title,
            } => ExportController::with_topic(
                source,
                peer.clone(),
                *topic_root_id,
                *peer_id,
                topic_title.clone(),
            ),
            _ => ExportController::new(source, self.settings.single_peer.clone()),
        };
        if self.skip_path_normalization {
            controller.set_skip_path_normalization(true);
        }

        let settings = self.settings.clone();
        let environment = Environment::default_english();

        self.begin_progress_run(handle, skip_signal);

        self.tokio_handle.spawn(async move {
            let _ = controller.run(&settings, &environment).await;
        });
    }

    fn begin_progress_run(&mut self, handle: ExportControlHandle, skip_signal: SkipFileSignal) {
        let initial_processing = ProcessingState::default();
        let initial_content = content_from_processing(&self.settings, &initial_processing);
        self.progress_view = ProgressView::new(initial_content.clone());
        self.top_bar = ExportTopBar::new(&initial_content);
        self.screen = ExportScreen::Progress;
        self.modal = None;
        self.finished_state = None;
        self.takeout_delay = None;
        self.active = true;
        self.panel_visible = true;

        self.active_run = Some(ActiveRun {
            control: handle,
            skip_signal,
            last_state: ExportState::Processing(initial_processing),
        });
    }

    /// Returns to the settings screen after a critical error so the user can adjust options and retry.
    pub fn retry_from_error(&mut self) {
        if !self.active {
            return;
        }
        self.active_run = None;
        self.modal = None;
        self.screen = ExportScreen::Settings;
        self.panel_visible = true;
    }

    /// Checks whether a previously persisted 24-hour takeout wait (`available_at`) has elapsed
    /// and opens the `"Data export ready"` suggestion prompt (`Export::View::SuggestStart`).
    pub fn check_takeout_suggestion(&mut self, now_unix: TimeId) -> bool {
        if self.active || self.modal.is_some() {
            return false;
        }
        let persisted = self.settings_store.load();
        if persisted.available_at > 0 && now_unix >= persisted.available_at {
            self.modal = Some(ExportModal::SuggestStart {
                available_at: persisted.available_at,
            });
            self.clear_suggest_start();
            return true;
        }
        false
    }

    /// Clears any persisted `available_at` timestamp (`Export::View::ClearSuggestStart`).
    pub fn clear_suggest_start(&mut self) {
        if self.settings.available_at != 0 {
            self.settings.available_at = 0;
            let _ = self.save_settings_now();
        } else {
            let mut stored = self.settings_store.load();
            if stored.available_at != 0 {
                stored.available_at = 0;
                let _ = self.settings_store.save(&stored);
            }
        }
    }

    /// Requests stopping the running export with confirmation (`PanelController::stopWithConfirmation`).
    pub fn stop_with_confirmation(&mut self) {
        if !self.is_export_running() {
            self.stop();
            return;
        }
        self.panel_visible = true;
        self.modal = Some(ExportModal::ConfirmStop);
    }

    /// Confirms stopping the running export, cancelling the controller immediately.
    pub fn confirm_stop(&mut self) {
        if let Some(ref run) = self.active_run {
            run.control.cancel_fast();
        }
        self.modal = None;
        self.stop();
    }

    /// Stops and destroys the active export panel and background handle (`Manager::stop`).
    pub fn stop(&mut self) {
        if let Some(ref run) = self.active_run {
            run.control.cancel_fast();
        }
        self.flush_settings();
        self.active_run = None;
        self.active = false;
        self.panel_visible = false;
        self.modal = None;
        self.screen = ExportScreen::Settings;
    }

    /// Polls the live [`ExportController`] state and debounced settings timer.
    ///
    /// Should be called every UI frame (and in tests to observe state transitions).
    pub fn poll(&mut self, now_secs: f64) {
        if let Some(since) = self.settings_dirty_since
            && Instant::now().saturating_duration_since(since) >= SAVE_SETTINGS_TIMEOUT
        {
            self.flush_settings();
        }

        let next_state = self.active_run.as_ref().map(|r| r.control.current_state());

        if let Some(state) = next_state
            && !matches!(state, ExportState::None | ExportState::PasswordCheck(_))
        {
            let changed = self
                .active_run
                .as_ref()
                .is_none_or(|r| r.last_state != state);
            if changed {
                if let Some(ref mut run) = self.active_run {
                    run.last_state = state.clone();
                }
                self.handle_controller_state(state, now_secs);
            }
        }
    }

    /// Feeds a controller [`ExportState`] transition into the panel controller
    /// (port of `PanelController::updateState` in `export_view_panel_controller.cpp:402-421`).
    pub fn handle_controller_state(&mut self, state: ExportState, now_secs: f64) {
        if let Some(content) = content_from_state(&self.settings, &state) {
            self.progress_view.update_content(content.clone(), now_secs);
            self.top_bar.update_data(&content);
        }

        match state {
            ExportState::None | ExportState::PasswordCheck(_) => {}
            ExportState::Processing(_) => {
                self.screen = ExportScreen::Progress;
            }
            ExportState::Finished(finished) => {
                self.finished_state = Some(finished);
                self.screen = ExportScreen::Progress;
                self.panel_visible = true;
            }
            ExportState::Cancelled => {
                self.stop();
            }
            ExportState::ApiError(err) => {
                self.handle_api_error(&err, current_unix_time());
            }
            ExportState::OutputError { path } => {
                self.screen = ExportScreen::CriticalError(format!(
                    "Disk Error happened :(\nCould not write path:\n{path}"
                ));
                self.panel_visible = true;
            }
        }
    }

    /// Handles an [`ApiError`] including `TAKEOUT_INVALID` and `TAKEOUT_INIT_DELAY_<seconds>`
    /// (exact port of `PanelController::showError(const ApiErrorState&)`).
    pub fn handle_api_error(&mut self, error: &ApiError, now_unix: TimeId) {
        self.panel_visible = true;
        if error.is_takeout_invalid() {
            self.modal = Some(ExportModal::Inform {
                title: "Export Your Data".to_owned(),
                message:
                    "Sorry, you started a new data export, so this data export has been canceled."
                        .to_owned(),
                close_panel_on_ok: true,
            });
        } else if let Some(delay) = takeout_delay_info_from_error(error, now_unix) {
            self.settings.available_at = delay.available_at;
            self.mark_settings_dirty();
            let _ = self.save_settings_now();
            self.modal = Some(ExportModal::Inform {
                title: "Export Your Data".to_owned(),
                message: delay.message.clone(),
                close_panel_on_ok: true,
            });
            self.takeout_delay = Some(delay);
        } else {
            self.screen = ExportScreen::CriticalError(format!(
                "API Error happened :(\n{}: {}\n{}",
                error.code, error.kind, error.description
            ));
        }
    }

    /// Processes actions emitted by [`SettingsView`].
    pub fn handle_settings_actions(&mut self, actions: Vec<SettingsViewAction>) {
        for act in actions {
            match act {
                SettingsViewAction::Changed => {
                    self.mark_settings_dirty();
                }
                SettingsViewAction::Start => {
                    self.start_configured_export();
                }
                SettingsViewAction::Cancel => {
                    self.stop();
                }
                SettingsViewAction::ChooseFolder => {
                    if let Some(chosen) = self
                        .folder_chooser
                        .pick_folder("Choose export folder", &self.settings.path)
                    {
                        let chosen_str = chosen.to_string_lossy().into_owned();
                        self.settings.path = chosen_str;
                        self.settings.force_sub_path = is_default_export_path(
                            &self.settings.path,
                            &self.default_download_path,
                        );
                        self.mark_settings_dirty();
                    }
                }
                SettingsViewAction::EditFromDate => {
                    self.modal = Some(ExportModal::DatePicker {
                        kind: DateLimitKind::From,
                        state: DatePickerState::for_limit(
                            self.settings.single_peer_from,
                            0,
                            self.settings.single_peer_till,
                        ),
                    });
                }
                SettingsViewAction::EditFromTime => {
                    self.modal = Some(ExportModal::TimePicker {
                        kind: DateLimitKind::From,
                        state: TimePickerState::from_timestamp(self.settings.single_peer_from),
                    });
                }
                SettingsViewAction::EditTillDate => {
                    self.modal = Some(ExportModal::DatePicker {
                        kind: DateLimitKind::Till,
                        state: DatePickerState::for_limit(
                            self.settings.single_peer_till,
                            self.settings.single_peer_from,
                            0,
                        ),
                    });
                }
                SettingsViewAction::EditTillTime => {
                    self.modal = Some(ExportModal::TimePicker {
                        kind: DateLimitKind::Till,
                        state: TimePickerState::from_timestamp(self.settings.single_peer_till),
                    });
                }
            }
        }
    }

    /// Processes an action emitted by [`ProgressView`].
    pub fn handle_progress_action(&mut self, action: ProgressViewAction) {
        match action {
            ProgressViewAction::SkipFile(random_id) => {
                if let Some(ref run) = self.active_run {
                    run.skip_signal.request_skip(random_id);
                }
            }
            ProgressViewAction::RequestCancel => {
                self.stop_with_confirmation();
            }
            ProgressViewAction::ShowMyData => {
                self.open_exported_folder();
            }
        }
    }

    /// Opens the finished export output in Explorer via [`FolderOpener`] (`tg-platform`) and closes the panel.
    pub fn open_exported_folder(&mut self) {
        if let Some(finished) = self.finished_state.clone() {
            let target_path = PathBuf::from(&finished.path);
            let _ = self.folder_opener.show_in_folder(&target_path);
            self.stop();
        }
    }

    /// Applies the result of a date picker modal (`Some(ts)` sets date, `None` cancels).
    pub fn finish_date_picker(&mut self, kind: DateLimitKind, chosen: Option<TimeId>) {
        if let Some(limit) = chosen {
            match kind {
                DateLimitKind::From => apply_from_date(&mut self.settings, limit),
                DateLimitKind::Till => apply_till_date(&mut self.settings, limit),
            }
            self.mark_settings_dirty();
        }
        self.modal = None;
    }

    /// Applies the result of a time picker modal (`Some(secs)` sets time, `None` cancels).
    pub fn finish_time_picker(&mut self, kind: DateLimitKind, chosen: Option<TimeId>) {
        if let Some(secs) = chosen {
            match kind {
                DateLimitKind::From => apply_from_time(&mut self.settings, secs),
                DateLimitKind::Till => apply_till_time(&mut self.settings, secs),
            }
            self.mark_settings_dirty();
        }
        self.modal = None;
    }

    /// Dismisses the active inform modal, closing the panel if `close_panel_on_ok` was set.
    pub fn dismiss_inform_modal(&mut self) {
        let should_close = matches!(
            self.modal,
            Some(ExportModal::Inform {
                close_panel_on_ok: true,
                ..
            })
        );
        self.modal = None;
        if should_close {
            self.stop();
        }
    }

    /// Accepts the `"Data export ready"` suggestion (`SuggestBox` OK button), resuming
    /// the previously deferred full-account, single-chat, or single-topic export.
    pub fn accept_suggest_start(&mut self) {
        self.modal = None;
        let stored = self.settings_store.load();
        if stored.only_single_topic() {
            self.start_single_topic_export(
                stored.single_peer,
                stored.single_topic_root_id,
                stored.single_topic_peer_id,
                stored.single_topic_title,
            );
        } else if stored.only_single_peer() {
            let title = if self.remembered_single_chat_title.is_empty() {
                "Exported Chat".to_owned()
            } else {
                self.remembered_single_chat_title.clone()
            };
            self.start_single_chat_export(stored.single_peer, title);
        } else {
            self.start_full_export();
        }
    }

    /// Dismisses the `"Data export ready"` suggestion (`"Not now"`).
    pub fn dismiss_suggest_start(&mut self) {
        self.modal = None;
    }

    /// Replaces the active modal dialog.
    pub(crate) fn replace_modal_inner(&mut self, modal: Option<ExportModal>) {
        self.modal = modal;
    }

    /// Forces immediate persistence of settings via `tg_export::Settings::serialize_qt`.
    pub fn flush_settings(&mut self) {
        if self.settings_dirty_since.take().is_some() {
            let _ = self.save_settings_now();
        }
    }

    /// Saves settings immediately, clearing `path` to empty when it matches the default download path
    /// (matching `PanelController::saveSettings` in `export_view_panel_controller.cpp:423-435`).
    pub fn save_settings_now(&self) -> Result<(), String> {
        let mut to_save = self.settings.clone();
        if is_default_export_path(&to_save.path, &self.default_download_path) {
            to_save.path.clear();
        }
        self.settings_store.save(&to_save)
    }

    fn mark_settings_dirty(&mut self) {
        if self.settings_dirty_since.is_none() {
            self.settings_dirty_since = Some(Instant::now());
        }
    }
}

impl Default for ExportManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Ensures [`TlExportSource`] stays type-checked and directly usable by callers plugging in `tg-api`.
#[allow(dead_code)]
pub fn create_tl_export_source<C: ExportRpc>(rpc: C, utc_offset_secs: i32) -> TlExportSource<C> {
    TlExportSource::with_utc_offset(rpc, utc_offset_secs)
}
