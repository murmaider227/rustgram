//! Windows `SystemMediaTransportControls` (SMTC) integration and window integration helpers.
//!
//! Port of:
//! - `Telegram/lib_base/base/platform/base_platform_system_media_controls.h`
//! - `Telegram/lib_base/base/platform/win/base_system_media_controls_win.cpp`
//! - `Telegram/SourceFiles/media/system_media_controls_manager.h`
//! - `Telegram/SourceFiles/media/system_media_controls_manager.cpp`
//! - `Telegram/SourceFiles/platform/win/integration_win.cpp` (power-broadcast / activation hooks)
//!
//! On Windows 10/11, SMTC runs on a dedicated hidden top-level `WS_EX_TOOLWINDOW | WS_POPUP`
//! window (never shown with `WS_VISIBLE`) inside an STA worker thread so UI calls never block
//! on WinRT COM marshaling or thumbnail stream writes. On older Windows (< 10) or non-Windows
//! hosts, [`SystemMediaControls::new`] degrades gracefully with [`PlatformError::Unsupported`].

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::error::{PlatformError, Result};

/// Default skip interval for `SkipForward` / `SkipBackward` commands (`kSkipMs` = 15s).
pub const SMTC_SKIP_MS: i64 = 15_000;

/// User command emitted by the Windows SMTC overlay or hardware media keys.
///
/// Mirrors `base::Platform::SystemMediaControls::Command`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SmtcCommand {
    /// Toggle play/pause.
    PlayPause,
    /// Start or resume playback.
    Play,
    /// Pause playback.
    Pause,
    /// Skip to next track in playlist.
    Next,
    /// Return to previous track in playlist.
    Previous,
    /// Stop playback.
    Stop,
    /// Close player and stop playback.
    Quit,
    /// User clicked the SMTC flyout to raise/activate the main window (`WM_NCACTIVATE`).
    Raise,
    /// Disable repeat.
    LoopNone,
    /// Repeat current track.
    LoopTrack,
    /// Repeat playlist.
    LoopPlaylist,
    /// Toggle shuffle mode.
    Shuffle,
    /// Seek backward by [`SMTC_SKIP_MS`].
    SkipBackward,
    /// Seek forward by [`SMTC_SKIP_MS`].
    SkipForward,
    /// Unmapped or unsupported SMTC button press.
    None,
}

/// Playback state reported to Windows SMTC (`Windows.Media.MediaPlaybackStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SmtcPlaybackStatus {
    /// Media session closed / inactive.
    Closed,
    /// Media is transitioning tracks.
    Changing,
    /// Media is stopped.
    #[default]
    Stopped,
    /// Media is actively playing.
    Playing,
    /// Media is paused or buffering.
    Paused,
}

/// Playlist loop status (`base::Platform::SystemMediaControls::LoopStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SmtcLoopStatus {
    /// No repeat.
    #[default]
    None,
    /// Repeat single track.
    Track,
    /// Repeat entire playlist.
    Playlist,
}

/// Media classification reported via `DisplayUpdater.Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SmtcMediaType {
    /// Music / voice audio track (`MediaPlaybackType::Music`).
    #[default]
    Music,
    /// Video clip (`MediaPlaybackType::Video`).
    Video,
    /// Still image (`MediaPlaybackType::Image`).
    Image,
    /// Unspecified media type (`MediaPlaybackType::Unknown`).
    Unknown,
}

/// Metadata & optional cover thumbnail displayed in the Windows media flyout.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SmtcMetadata {
    /// Track title or voice sender name.
    pub title: String,
    /// Artist / performer name or voice timestamp.
    pub artist: String,
    /// Optional album title.
    pub album_title: String,
    /// Media kind (`Music` by default).
    pub media_type: SmtcMediaType,
    /// Optional encoded image bytes (PNG or JPEG) for the cover art thumbnail.
    pub thumbnail_bytes: Option<Vec<u8>>,
}

/// Control enablement flags and playback status synchronized with SMTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SmtcControlsState {
    /// Whether the SMTC media session is active (`IsEnabled`).
    pub enabled: bool,
    /// Whether the Play button is enabled (`IsPlayEnabled`).
    pub play_enabled: bool,
    /// Whether the Pause button is enabled (`IsPauseEnabled`).
    pub pause_enabled: bool,
    /// Whether the Next button is enabled (`IsNextEnabled`).
    pub next_enabled: bool,
    /// Whether the Previous button is enabled (`IsPreviousEnabled`).
    pub previous_enabled: bool,
    /// Whether the Stop button is enabled (`IsStopEnabled`).
    pub stop_enabled: bool,
    /// Current playback status.
    pub status: SmtcPlaybackStatus,
}

impl SmtcControlsState {
    /// Construct an inactive/disabled SMTC state.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            play_enabled: false,
            pause_enabled: false,
            next_enabled: false,
            previous_enabled: false,
            stop_enabled: false,
            status: SmtcPlaybackStatus::Stopped,
        }
    }

    /// Construct an active SMTC state for a playing/paused track.
    #[must_use]
    pub const fn active(
        status: SmtcPlaybackStatus,
        previous_enabled: bool,
        next_enabled: bool,
    ) -> Self {
        Self {
            enabled: true,
            play_enabled: true,
            pause_enabled: true,
            next_enabled,
            previous_enabled,
            stop_enabled: true,
            status,
        }
    }
}

/// Maps raw WinRT `SystemMediaTransportControlsButton` integer values to [`SmtcCommand`].
///
/// Matches `SMTCButtonToCommand` in `base_system_media_controls_win.cpp:161-185`:
/// - `0` = Play
/// - `1` = Pause
/// - `2` = Stop
/// - `3` = Record -> `None`
/// - `4` = FastForward -> `None`
/// - `5` = Rewind -> `None`
/// - `6` = Next
/// - `7` = Previous
/// - `8` = ChannelUp -> `None`
/// - `9` = ChannelDown -> `None`
#[must_use]
pub const fn map_button_id_to_command(raw_button: i32) -> SmtcCommand {
    match raw_button {
        0 => SmtcCommand::Play,
        1 => SmtcCommand::Pause,
        2 => SmtcCommand::Stop,
        6 => SmtcCommand::Next,
        7 => SmtcCommand::Previous,
        _ => SmtcCommand::None,
    }
}

/// Maps player playback flags into [`SmtcPlaybackStatus`], matching
/// `SystemMediaControlsManager` in `media/system_media_controls_manager.cpp:279-292`.
#[must_use]
pub const fn map_playback_state(
    is_stopped_or_stopping: bool,
    is_paused_or_pausing: bool,
    is_buffering: bool,
) -> SmtcPlaybackStatus {
    if is_stopped_or_stopping {
        SmtcPlaybackStatus::Stopped
    } else if is_paused_or_pausing || is_buffering {
        SmtcPlaybackStatus::Paused
    } else {
        SmtcPlaybackStatus::Playing
    }
}

/// Windows suspend/resume power state tracker ported from `platform/win/integration_win.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowsPowerState {
    /// Normal awake desktop state.
    #[default]
    Awake,
    /// `PBT_APMSUSPEND` received; debounce timer pending.
    SuspendNotified,
    /// Suspend debounce timer elapsed.
    SuspendSettled,
    /// `PBT_APMRESUMEAUTOMATIC` received; debounce timer pending.
    AutomaticResumeNotified,
    /// Automatic resume debounce timer elapsed.
    AutomaticResumeSettled,
    /// `PBT_APMRESUMESUSPEND` received; debounce timer pending.
    UserResumeNotified,
}

/// Win32 `WM_POWERBROADCAST` `wParam` constants mirrored from `integration_win.cpp:216-238`.
pub const PBT_APMSUSPEND_CODE: usize = 0x0004;
/// Win32 `PBT_APMRESUMESUSPEND` constant.
pub const PBT_APMRESUMESUSPEND_CODE: usize = 0x0007;
/// Win32 `PBT_APMRESUMEAUTOMATIC` constant.
pub const PBT_APMRESUMEAUTOMATIC_CODE: usize = 0x0012;

impl WindowsPowerState {
    /// Process a `WM_POWERBROADCAST` `wParam` event; returns `true` when the application should
    /// be notified of a sleep/wake cycle (port of `WindowsIntegration::processEvent`).
    pub fn on_power_broadcast(&mut self, wparam: usize) -> bool {
        match wparam {
            PBT_APMSUSPEND_CODE => {
                let notify = *self != Self::SuspendNotified;
                *self = Self::SuspendNotified;
                notify
            }
            PBT_APMRESUMEAUTOMATIC_CODE => {
                let notify = matches!(
                    *self,
                    Self::Awake | Self::AutomaticResumeSettled | Self::UserResumeNotified
                );
                *self = Self::AutomaticResumeNotified;
                notify
            }
            PBT_APMRESUMESUSPEND_CODE => {
                let notify = *self == Self::Awake;
                *self = Self::UserResumeNotified;
                notify
            }
            _ => false,
        }
    }

    /// Advance debounce timer (`kPowerBroadcastDebounce` = 1000ms in `integration_win.cpp:41-49`).
    pub fn on_debounce_timeout(&mut self) {
        *self = match *self {
            Self::SuspendNotified => Self::SuspendSettled,
            Self::AutomaticResumeNotified => Self::AutomaticResumeSettled,
            Self::UserResumeNotified => Self::Awake,
            other => other,
        };
    }
}

/// Diff emitted by [`SmtcStateTracker::diff`] so 60 FPS UI loops only call WinRT when state changes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SmtcSyncDiff {
    /// Updated controls/playback state, if changed.
    pub controls: Option<SmtcControlsState>,
    /// Updated text metadata (title/artist/album/type), if changed.
    pub metadata_text_changed: bool,
    /// Updated thumbnail bytes (`Some(None)` clears cover, `Some(Some(bytes))` sets cover).
    pub thumbnail_update: Option<Option<Vec<u8>>>,
    /// `true` when metadata should be cleared completely (session transitioned to disabled).
    pub clear_all: bool,
}

impl SmtcSyncDiff {
    /// Returns `true` when there are no updates to send to the OS.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.controls.is_none()
            && !self.metadata_text_changed
            && self.thumbnail_update.is_none()
            && !self.clear_all
    }
}

/// Tracks the last-sent SMTC state and computes minimal incremental updates.
#[derive(Debug, Default)]
pub struct SmtcStateTracker {
    last_controls: Option<SmtcControlsState>,
    last_title: String,
    last_artist: String,
    last_album: String,
    last_media_type: SmtcMediaType,
    last_thumb_hash: Option<u64>,
}

impl SmtcStateTracker {
    /// Creates an empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Computes what changed between the previously synchronized state and `(controls, metadata)`.
    pub fn compute_diff(
        &mut self,
        controls: SmtcControlsState,
        metadata: Option<&SmtcMetadata>,
    ) -> SmtcSyncDiff {
        let mut diff = SmtcSyncDiff::default();

        if !controls.enabled {
            let was_enabled = self.last_controls.is_some_and(|c| c.enabled);
            if was_enabled || self.last_controls != Some(controls) {
                diff.controls = Some(controls);
                diff.clear_all = was_enabled;
            }
            self.last_controls = Some(controls);
            self.last_title.clear();
            self.last_artist.clear();
            self.last_album.clear();
            self.last_thumb_hash = None;
            return diff;
        }

        if self.last_controls != Some(controls) {
            diff.controls = Some(controls);
            self.last_controls = Some(controls);
        }

        if let Some(meta) = metadata {
            if self.last_title != meta.title
                || self.last_artist != meta.artist
                || self.last_album != meta.album_title
                || self.last_media_type != meta.media_type
            {
                self.last_title = meta.title.clone();
                self.last_artist = meta.artist.clone();
                self.last_album = meta.album_title.clone();
                self.last_media_type = meta.media_type;
                diff.metadata_text_changed = true;
            }

            let next_hash = meta.thumbnail_bytes.as_deref().map(hash_bytes);
            if self.last_thumb_hash != next_hash {
                self.last_thumb_hash = next_hash;
                diff.thumbnail_update = Some(meta.thumbnail_bytes.clone());
            }
        }

        diff
    }

    /// Resets tracked state so the next sync pushes a full refresh.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.len().hash(&mut hasher);
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Returns `true` when Windows SMTC is supported on this OS (`Windows 10+`, matching
/// `SystemMediaControls::Supported()` in `base_system_media_controls_win.cpp:442-444`).
#[must_use]
pub fn is_supported() -> bool {
    #[cfg(windows)]
    {
        crate::system_info::windows_version().is_windows_10_or_greater()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Handle to the Windows `SystemMediaTransportControls` media session.
///
/// Thread-safe (`Send`): all WinRT calls execute on a dedicated background STA worker thread
/// that owns the hidden SMTC activation window (`WS_EX_TOOLWINDOW | WS_POPUP`).
pub struct SystemMediaControls {
    #[cfg(windows)]
    inner: win_smtc::SmtcWorker,
    #[cfg(not(windows))]
    _private: (),
}

impl std::fmt::Debug for SystemMediaControls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemMediaControls")
            .field("supported", &is_supported())
            .finish()
    }
}

impl SystemMediaControls {
    /// Initializes Windows SMTC with a dedicated hidden activation window.
    ///
    /// Returns [`PlatformError::Unsupported`] on non-Windows hosts or Windows versions earlier
    /// than Windows 10, and degrades cleanly if the WinRT activation factory is unavailable.
    pub fn new() -> Result<Self> {
        if !is_supported() {
            return Err(PlatformError::Unsupported);
        }
        #[cfg(windows)]
        {
            Ok(Self {
                inner: win_smtc::SmtcWorker::start()?,
            })
        }
        #[cfg(not(windows))]
        {
            Err(PlatformError::Unsupported)
        }
    }

    /// Best-effort constructor that logs a warning and returns `None` if SMTC cannot initialize.
    #[must_use]
    pub fn try_new() -> Option<Self> {
        match Self::new() {
            Ok(ctrl) => Some(ctrl),
            Err(err) => {
                tracing::debug!(%err, "Windows SMTC unavailable; continuing without OS media controls");
                None
            }
        }
    }

    /// Applies updated playback status and button enablement flags.
    pub fn set_controls_state(&self, state: SmtcControlsState) {
        #[cfg(windows)]
        {
            self.inner.send(win_smtc::WorkerMsg::SetControls(state));
        }
        #[cfg(not(windows))]
        {
            let _ = state;
        }
    }

    /// Updates track title, artist, album, media type, and optional cover thumbnail.
    pub fn set_metadata(&self, metadata: SmtcMetadata) {
        #[cfg(windows)]
        {
            self.inner.send(win_smtc::WorkerMsg::SetMetadata(metadata));
        }
        #[cfg(not(windows))]
        {
            let _ = metadata;
        }
    }

    /// Updates only the cover art thumbnail (`None` clears the thumbnail).
    pub fn set_thumbnail(&self, thumbnail_bytes: Option<Vec<u8>>) {
        #[cfg(windows)]
        {
            self.inner
                .send(win_smtc::WorkerMsg::SetThumbnail(thumbnail_bytes));
        }
        #[cfg(not(windows))]
        {
            let _ = thumbnail_bytes;
        }
    }

    /// Clears all displayed metadata and disables SMTC.
    pub fn clear_metadata(&self) {
        #[cfg(windows)]
        {
            self.inner.send(win_smtc::WorkerMsg::ClearMetadata);
        }
    }

    /// Applies an incremental [`SmtcSyncDiff`] computed by [`SmtcStateTracker`].
    pub fn apply_diff(&self, diff: SmtcSyncDiff, metadata: Option<&SmtcMetadata>) {
        if diff.is_empty() {
            return;
        }
        if diff.clear_all {
            self.clear_metadata();
            if let Some(ctrl) = diff.controls {
                self.set_controls_state(ctrl);
            }
            return;
        }
        if let Some(ctrl) = diff.controls {
            self.set_controls_state(ctrl);
        }
        if diff.metadata_text_changed
            && let Some(meta) = metadata
        {
            let mut text_only = meta.clone();
            text_only.thumbnail_bytes = None;
            self.set_metadata(text_only);
        }
        if let Some(thumb) = diff.thumbnail_update {
            self.set_thumbnail(thumb);
        }
    }

    /// Drains all pending [`SmtcCommand`] events triggered by media keys or the Windows media flyout.
    #[must_use]
    pub fn drain_commands(&self) -> Vec<SmtcCommand> {
        #[cfg(windows)]
        {
            self.inner.drain_commands()
        }
        #[cfg(not(windows))]
        {
            Vec::new()
        }
    }
}

#[cfg(windows)]
mod win_smtc {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread::{self, JoinHandle};

    use windows::Foundation::TypedEventHandler;
    use windows::Media::{
        MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls,
        SystemMediaTransportControlsButtonPressedEventArgs,
        SystemMediaTransportControlsDisplayUpdater,
    };
    use windows::Storage::Streams::{
        DataWriter, InMemoryRandomAccessStream, RandomAccessStreamReference,
    };
    use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
    use windows::Win32::UI::WindowsAndMessaging::{
        CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, MSG, PostMessageW, PostQuitMessage,
        RegisterClassExW, SetWindowLongPtrW, TranslateMessage, WM_APP, WM_CLOSE, WM_DESTROY,
        WM_NCACTIVATE, WM_NCCREATE, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
    };
    use windows::core::{HSTRING, PCWSTR};

    use super::{
        SmtcCommand, SmtcControlsState, SmtcMediaType, SmtcMetadata, SmtcPlaybackStatus,
        map_button_id_to_command,
    };
    use crate::app_id::win_impl::{ComGuard, ensure_process_mta, to_wide};
    use crate::error::{PlatformError, Result};

    const WM_SMTC_WAKE: u32 = WM_APP + 1;

    pub(super) enum WorkerMsg {
        SetControls(SmtcControlsState),
        SetMetadata(SmtcMetadata),
        SetThumbnail(Option<Vec<u8>>),
        ClearMetadata,
    }

    pub(super) struct SmtcWorker {
        hwnd_raw: isize,
        tx: mpsc::Sender<WorkerMsg>,
        commands: Arc<Mutex<VecDeque<SmtcCommand>>>,
        thread: Option<JoinHandle<()>>,
    }

    impl SmtcWorker {
        pub(super) fn start() -> Result<Self> {
            let (init_tx, init_rx) = mpsc::sync_channel::<Result<isize>>(1);
            let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();
            let commands = Arc::new(Mutex::new(VecDeque::<SmtcCommand>::new()));
            let commands_for_thread = Arc::clone(&commands);

            let thread = thread::Builder::new()
                .name("tg-smtc-worker".into())
                .spawn(move || {
                    run_smtc_thread(init_tx, msg_rx, commands_for_thread);
                })?;

            let hwnd_raw = init_rx.recv().map_err(|_| {
                PlatformError::Smtc("SMTC worker thread exited before initialization".into())
            })??;

            Ok(Self {
                hwnd_raw,
                tx: msg_tx,
                commands,
                thread: Some(thread),
            })
        }

        pub(super) fn send(&self, msg: WorkerMsg) {
            if self.tx.send(msg).is_ok() && self.hwnd_raw != 0 {
                // SAFETY: Posting an application-defined wake message (`WM_SMTC_WAKE`) to our
                // owned hidden SMTC window so its message loop processes queued updates promptly.
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(self.hwnd_raw as *mut _)),
                        WM_SMTC_WAKE,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            }
        }

        pub(super) fn drain_commands(&self) -> Vec<SmtcCommand> {
            self.commands
                .lock()
                .map(|mut q| q.drain(..).collect())
                .unwrap_or_default()
        }
    }

    impl Drop for SmtcWorker {
        fn drop(&mut self) {
            if self.hwnd_raw != 0 {
                // SAFETY: Posting `WM_CLOSE` to our owned hidden SMTC window to tear down the
                // Win32 message loop cleanly before joining the worker thread.
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(self.hwnd_raw as *mut _)),
                        WM_CLOSE,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            }
            if let Some(handle) = self.thread.take() {
                let _ = handle.join();
            }
        }
    }

    struct WindowUserData {
        commands: Arc<Mutex<VecDeque<SmtcCommand>>>,
    }

    unsafe extern "system" fn smtc_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_NCCREATE => {
                // SAFETY: During `WM_NCCREATE`, `lparam` points to a valid `CREATESTRUCTW` whose
                // `lpCreateParams` holds the `Box<WindowUserData>` pointer passed to `CreateWindowExW`.
                let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
                // SAFETY: Storing the non-owning `WindowUserData` pointer in `GWLP_USERDATA` for
                // the lifetime of the hidden window.
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize) };
                // SAFETY: Standard Win32 default window procedure forwarding.
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_NCACTIVATE if wparam.0 != 0 => {
                // Clicking the Windows media flyout activates the SMTC host window (`WM_NCACTIVATE`
                // with `wParam != 0`), matching `ControlsWndProc` in `base_system_media_controls_win.cpp:64-70`.
                // SAFETY: Reading the `WindowUserData` pointer stored during `WM_NCCREATE`.
                let ptr =
                    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowUserData;
                if !ptr.is_null() {
                    // SAFETY: `WindowUserData` outlives the window (reclaimed only after `GetMessageW` exits).
                    let data = unsafe { &*ptr };
                    if let Ok(mut q) = data.commands.lock() {
                        q.push_back(SmtcCommand::Raise);
                    }
                }
                // SAFETY: Standard Win32 default window procedure forwarding.
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_CLOSE => {
                // SAFETY: Destroying our owned hidden SMTC window.
                let _ = unsafe { DestroyWindow(hwnd) };
                LRESULT(0)
            }
            WM_DESTROY => {
                // SAFETY: Terminating this worker thread's Win32 message loop.
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            // SAFETY: Standard Win32 default window procedure forwarding.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn run_smtc_thread(
        init_tx: mpsc::SyncSender<Result<isize>>,
        msg_rx: mpsc::Receiver<WorkerMsg>,
        commands: Arc<Mutex<VecDeque<SmtcCommand>>>,
    ) {
        // Pin the process-wide MTA before activating WinRT factories so `windows-core`'s
        // `FactoryCache` stays valid even after this STA worker thread calls `CoUninitialize`.
        ensure_process_mta();
        // Initialize STA COM/WinRT apartment for this dedicated window & SMTC thread.
        let _com = ComGuard::init_sta();

        let class_name = to_wide("RustgramSystemMediaControls");
        // SAFETY: Querying the module handle for the current executable.
        let hinst: HMODULE = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(smtc_wnd_proc),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        // SAFETY: Registering the hidden SMTC window class (re-registration is harmless).
        unsafe { RegisterClassExW(&wc) };

        let user_data = Box::into_raw(Box::new(WindowUserData {
            commands: Arc::clone(&commands),
        }));

        // Hidden top-level window (never shown with WS_VISIBLE, matching `CreateControlsWindow`
        // in `base_system_media_controls_win.cpp:96-113`).
        // SAFETY: Valid registered window class, module handle, and heap-allocated `user_data`.
        let hwnd = match unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                PCWSTR(class_name.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinst.into()),
                Some(user_data.cast()),
            )
        } {
            Ok(h) if !h.0.is_null() => h,
            Err(e) => {
                // SAFETY: Reclaiming `user_data` after window creation failure.
                let _ = unsafe { Box::from_raw(user_data) };
                let _ = init_tx.send(Err(PlatformError::from_win("CreateWindowExW(SMTC)", e)));
                return;
            }
            Ok(_) => {
                // SAFETY: Reclaiming `user_data` after null HWND.
                let _ = unsafe { Box::from_raw(user_data) };
                let _ = init_tx.send(Err(PlatformError::last_win32("CreateWindowExW(SMTC)")));
                return;
            }
        };

        let controls = match create_smtc_for_window(hwnd) {
            Ok(c) => c,
            Err(err) => {
                // SAFETY: Destroying the unused hidden window before reclaiming `user_data`.
                let _ = unsafe { DestroyWindow(hwnd) };
                // SAFETY: Reclaiming `user_data` after window destruction.
                let _ = unsafe { Box::from_raw(user_data) };
                let _ = init_tx.send(Err(err));
                return;
            }
        };

        let cmd_queue = Arc::clone(&commands);
        let event_token = controls
            .ButtonPressed(&TypedEventHandler::<
                SystemMediaTransportControls,
                SystemMediaTransportControlsButtonPressedEventArgs,
            >::new(move |_sender, args| {
                if let Some(a) = args.as_ref()
                    && let Ok(btn) = a.Button()
                {
                    let cmd = map_button_id_to_command(btn.0);
                    if cmd != SmtcCommand::None
                        && let Ok(mut q) = cmd_queue.lock()
                    {
                        q.push_back(cmd);
                    }
                }
                Ok(())
            }))
            .ok();

        let _ = controls.SetIsEnabled(false);
        let display_updater = controls.DisplayUpdater().ok();

        let _ = init_tx.send(Ok(hwnd.0 as isize));

        let mut msg = MSG::default();
        // SAFETY: Standard Win32 message loop for this thread's hidden window.
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
            if msg.message == WM_SMTC_WAKE {
                while let Ok(work) = msg_rx.try_recv() {
                    handle_worker_msg(&controls, display_updater.as_ref(), work);
                }
            }
            // SAFETY: Standard Win32 message translation and dispatch.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        if let Some(token) = event_token {
            let _ = controls.RemoveButtonPressed(token);
        }
        if let Some(ref updater) = display_updater {
            let _ = updater.ClearAll();
            let _ = updater.Update();
        }
        let _ = controls.SetIsEnabled(false);

        // SAFETY: The hidden window is destroyed and its message loop has exited; reclaim `user_data`.
        let _ = unsafe { Box::from_raw(user_data) };
    }

    fn create_smtc_for_window(hwnd: HWND) -> Result<SystemMediaTransportControls> {
        let interop = windows::core::factory::<
            SystemMediaTransportControls,
            ISystemMediaTransportControlsInterop,
        >()
        .map_err(|e| PlatformError::from_win("ISystemMediaTransportControlsInterop", e))?;

        // SAFETY: `hwnd` is a valid top-level window owned by this thread; `GetForWindow`
        // returns an owned WinRT `SystemMediaTransportControls` interface pointer.
        unsafe { interop.GetForWindow::<SystemMediaTransportControls>(hwnd) }.map_err(|e| {
            PlatformError::from_win("ISystemMediaTransportControlsInterop::GetForWindow", e)
        })
    }

    fn handle_worker_msg(
        controls: &SystemMediaTransportControls,
        updater: Option<&SystemMediaTransportControlsDisplayUpdater>,
        msg: WorkerMsg,
    ) {
        match msg {
            WorkerMsg::SetControls(state) => {
                let _ = controls.SetIsEnabled(state.enabled);
                let _ = controls.SetIsPlayEnabled(state.play_enabled);
                let _ = controls.SetIsPauseEnabled(state.pause_enabled);
                let _ = controls.SetIsNextEnabled(state.next_enabled);
                let _ = controls.SetIsPreviousEnabled(state.previous_enabled);
                let _ = controls.SetIsStopEnabled(state.stop_enabled);
                let _ = controls.SetPlaybackStatus(to_winrt_status(state.status));
            }
            WorkerMsg::SetMetadata(meta) => {
                if let Some(up) = updater {
                    let _ = up.SetType(to_winrt_media_type(meta.media_type));
                    if meta.media_type == SmtcMediaType::Video {
                        if let Ok(props) = up.VideoProperties() {
                            let _ = props.SetTitle(&HSTRING::from(&meta.title));
                            let _ = props.SetSubtitle(&HSTRING::from(&meta.artist));
                        }
                    } else if let Ok(props) = up.MusicProperties() {
                        let _ = props.SetTitle(&HSTRING::from(&meta.title));
                        let _ = props.SetArtist(&HSTRING::from(&meta.artist));
                        let _ = props.SetAlbumTitle(&HSTRING::from(&meta.album_title));
                    }
                    if let Some(bytes) = meta.thumbnail_bytes {
                        apply_thumbnail(up, Some(&bytes));
                    }
                    let _ = up.Update();
                }
            }
            WorkerMsg::SetThumbnail(thumb) => {
                if let Some(up) = updater {
                    apply_thumbnail(up, thumb.as_deref());
                    let _ = up.Update();
                }
            }
            WorkerMsg::ClearMetadata => {
                if let Some(up) = updater {
                    let _ = up.ClearAll();
                    let _ = up.Update();
                }
                let _ = controls.SetIsEnabled(false);
            }
        }
    }

    fn apply_thumbnail(updater: &SystemMediaTransportControlsDisplayUpdater, bytes: Option<&[u8]>) {
        let Some(data) = bytes.filter(|b| !b.is_empty()) else {
            let _ = updater.SetThumbnail(None);
            return;
        };
        let Ok(stream) = InMemoryRandomAccessStream::new() else {
            return;
        };
        let Ok(writer) = DataWriter::CreateDataWriter(&stream) else {
            return;
        };
        if writer.WriteBytes(data).is_err() {
            return;
        }
        if writer.StoreAsync().and_then(|op| op.join()).is_err() {
            return;
        }
        let _ = writer.FlushAsync().and_then(|op| op.join());
        let _ = writer.DetachStream();
        if stream.Seek(0).is_err() {
            return;
        }
        if let Ok(reference) = RandomAccessStreamReference::CreateFromStream(&stream) {
            let _ = updater.SetThumbnail(&reference);
        }
    }

    fn to_winrt_status(status: SmtcPlaybackStatus) -> MediaPlaybackStatus {
        match status {
            SmtcPlaybackStatus::Closed => MediaPlaybackStatus::Closed,
            SmtcPlaybackStatus::Changing => MediaPlaybackStatus::Changing,
            SmtcPlaybackStatus::Stopped => MediaPlaybackStatus::Stopped,
            SmtcPlaybackStatus::Playing => MediaPlaybackStatus::Playing,
            SmtcPlaybackStatus::Paused => MediaPlaybackStatus::Paused,
        }
    }

    fn to_winrt_media_type(kind: SmtcMediaType) -> MediaPlaybackType {
        match kind {
            SmtcMediaType::Music => MediaPlaybackType::Music,
            SmtcMediaType::Video => MediaPlaybackType::Video,
            SmtcMediaType::Image => MediaPlaybackType::Image,
            SmtcMediaType::Unknown => MediaPlaybackType::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smtc_button_mapping_matches_tdesktop() {
        assert_eq!(map_button_id_to_command(0), SmtcCommand::Play);
        assert_eq!(map_button_id_to_command(1), SmtcCommand::Pause);
        assert_eq!(map_button_id_to_command(2), SmtcCommand::Stop);
        assert_eq!(map_button_id_to_command(3), SmtcCommand::None); // Record
        assert_eq!(map_button_id_to_command(4), SmtcCommand::None); // FastForward
        assert_eq!(map_button_id_to_command(5), SmtcCommand::None); // Rewind
        assert_eq!(map_button_id_to_command(6), SmtcCommand::Next);
        assert_eq!(map_button_id_to_command(7), SmtcCommand::Previous);
        assert_eq!(map_button_id_to_command(8), SmtcCommand::None); // ChannelUp
        assert_eq!(map_button_id_to_command(9), SmtcCommand::None); // ChannelDown
        assert_eq!(map_button_id_to_command(999), SmtcCommand::None);
    }

    #[test]
    fn smtc_playback_status_mapping() {
        assert_eq!(
            map_playback_state(true, false, false),
            SmtcPlaybackStatus::Stopped
        );
        assert_eq!(
            map_playback_state(true, true, true),
            SmtcPlaybackStatus::Stopped
        );
        assert_eq!(
            map_playback_state(false, true, false),
            SmtcPlaybackStatus::Paused
        );
        assert_eq!(
            map_playback_state(false, false, true),
            SmtcPlaybackStatus::Paused
        );
        assert_eq!(
            map_playback_state(false, false, false),
            SmtcPlaybackStatus::Playing
        );
    }

    #[test]
    fn smtc_state_tracker_deduplicates_frames_and_detects_cover_changes() {
        let mut tracker = SmtcStateTracker::new();
        let meta = SmtcMetadata {
            title: "Alpine Suite".into(),
            artist: "Studio Ensemble".into(),
            album_title: "Synthetic".into(),
            media_type: SmtcMediaType::Music,
            thumbnail_bytes: Some(vec![0x89, b'P', b'N', b'G']),
        };
        let active = SmtcControlsState::active(SmtcPlaybackStatus::Playing, true, true);

        // First frame pushes controls, text, and cover art.
        let d1 = tracker.compute_diff(active, Some(&meta));
        assert_eq!(d1.controls, Some(active));
        assert!(d1.metadata_text_changed);
        assert_eq!(
            d1.thumbnail_update,
            Some(Some(vec![0x89, b'P', b'N', b'G']))
        );
        assert!(!d1.clear_all);

        // Identical next frame produces an empty diff.
        let d2 = tracker.compute_diff(active, Some(&meta));
        assert!(d2.is_empty());

        // Pausing updates only controls state.
        let paused = SmtcControlsState::active(SmtcPlaybackStatus::Paused, true, true);
        let d3 = tracker.compute_diff(paused, Some(&meta));
        assert_eq!(d3.controls, Some(paused));
        assert!(!d3.metadata_text_changed);
        assert!(d3.thumbnail_update.is_none());

        // Clearing cover art updates only thumbnail.
        let mut no_cover = meta.clone();
        no_cover.thumbnail_bytes = None;
        let d4 = tracker.compute_diff(paused, Some(&no_cover));
        assert!(d4.controls.is_none());
        assert!(!d4.metadata_text_changed);
        assert_eq!(d4.thumbnail_update, Some(None));

        // Disabling emits `clear_all`.
        let d5 = tracker.compute_diff(SmtcControlsState::disabled(), None);
        assert!(d5.clear_all);
        assert_eq!(d5.controls, Some(SmtcControlsState::disabled()));

        // Staying disabled produces an empty diff.
        let d6 = tracker.compute_diff(SmtcControlsState::disabled(), None);
        assert!(d6.is_empty());
    }

    #[test]
    fn power_broadcast_state_machine_matches_integration_win() {
        let mut state = WindowsPowerState::Awake;
        assert!(state.on_power_broadcast(PBT_APMSUSPEND_CODE));
        assert_eq!(state, WindowsPowerState::SuspendNotified);
        // Duplicate suspend within debounce window does not re-notify.
        assert!(!state.on_power_broadcast(PBT_APMSUSPEND_CODE));
        state.on_debounce_timeout();
        assert_eq!(state, WindowsPowerState::SuspendSettled);

        // Automatic resume after settled suspend does not re-notify sleep.
        assert!(!state.on_power_broadcast(PBT_APMRESUMEAUTOMATIC_CODE));
        assert_eq!(state, WindowsPowerState::AutomaticResumeNotified);
        state.on_debounce_timeout();
        assert_eq!(state, WindowsPowerState::AutomaticResumeSettled);

        // Another automatic resume after settled notifies once.
        assert!(state.on_power_broadcast(PBT_APMRESUMEAUTOMATIC_CODE));
        assert!(!state.on_power_broadcast(PBT_APMRESUMESUSPEND_CODE));
        assert_eq!(state, WindowsPowerState::UserResumeNotified);
        state.on_debounce_timeout();
        assert_eq!(state, WindowsPowerState::Awake);
    }
}
