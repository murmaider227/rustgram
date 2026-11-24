//! Windows taskbar thumbnail toolbar media buttons (`ITaskbarList3::ThumbBarAddButtons` /
//! `ThumbBarUpdateButtons`) behind a testable [`TaskbarButtonsBackend`] trait with a deterministic
//! [`FakeTaskbarButtonsBackend`].
//!
//! Port of:
//! - `Telegram/SourceFiles/platform/win/windows_taskbar_buttons.h`
//! - `Telegram/SourceFiles/platform/win/windows_taskbar_buttons.cpp`

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::error::Result;
use crate::smtc::SmtcCommand;
use crate::system_info::RgbaColor;
use crate::tray::RgbaImage;

/// Win32 notification code placed in `HIWORD(wParam)` of `WM_COMMAND` when the user clicks a
/// taskbar thumbnail toolbar button (`THBN_CLICKED` from `ShObjIdl.h`).
pub const THBN_CLICKED_CODE: u16 = 0x1800;

/// Button IDs matching `Platform::ButtonId` in `windows_taskbar_buttons.cpp:24-28`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TaskbarButtonId {
    /// Previous track button (`id = 1`).
    Previous = 1,
    /// Play / Pause toggle button (`id = 2`).
    PlayPause = 2,
    /// Next track button (`id = 3`).
    Next = 3,
}

impl TaskbarButtonId {
    /// Converts a raw Win32 `THUMBBUTTON.iId` command ID into a [`TaskbarButtonId`].
    #[must_use]
    pub const fn from_command_id(id: u32) -> Option<Self> {
        match id {
            1 => Some(Self::Previous),
            2 => Some(Self::PlayPause),
            3 => Some(Self::Next),
            _ => None,
        }
    }

    /// Returns the raw Win32 `THUMBBUTTON.iId` value.
    #[must_use]
    pub const fn to_command_id(self) -> u32 {
        self as u32
    }

    /// Decodes a Win32 `WM_COMMAND` `wParam` emitted when a thumbnail toolbar button is clicked
    /// (`HIWORD(wParam) == THBN_CLICKED`, `LOWORD(wParam) == button_id`).
    #[must_use]
    pub const fn from_wm_command(wparam: usize) -> Option<Self> {
        let notify_code = ((wparam >> 16) & 0xFFFF) as u16;
        if notify_code != THBN_CLICKED_CODE {
            return None;
        }
        let cmd_id = (wparam & 0xFFFF) as u32;
        Self::from_command_id(cmd_id)
    }

    /// Maps this taskbar button click to the equivalent [`SmtcCommand`] for `AudioPlayerBar`.
    #[must_use]
    pub const fn to_smtc_command(self) -> SmtcCommand {
        match self {
            Self::Previous => SmtcCommand::Previous,
            Self::PlayPause => SmtcCommand::PlayPause,
            Self::Next => SmtcCommand::Next,
        }
    }
}

/// Media player state mirrored into the 3 Windows taskbar thumbnail toolbar buttons
/// (`TaskbarButtons::State` in `windows_taskbar_buttons.h:29-36`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TaskbarMediaState {
    /// Whether an audio/voice track is currently active in the player.
    pub active: bool,
    /// Whether playback is currently playing (`true` shows Pause icon; `false` shows Play icon).
    pub playing: bool,
    /// Whether a previous track is available in the playlist.
    pub previous_available: bool,
    /// Whether a next track is available in the playlist.
    pub next_available: bool,
}

impl TaskbarMediaState {
    /// Returns an inactive (hidden buttons) state.
    #[must_use]
    pub const fn inactive() -> Self {
        Self {
            active: false,
            playing: false,
            previous_available: false,
            next_available: false,
        }
    }
}

/// Logical icon glyph displayed on a thumbnail toolbar button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskbarButtonIcon {
    /// Previous track (`|<`).
    Previous,
    /// Resume / play (`>`).
    Play,
    /// Pause (`||`).
    Pause,
    /// Next track (`>|`).
    Next,
}

/// Platform-independent specification for a single `THUMBBUTTON`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskbarButtonSpec {
    /// Stable button ID (`Previous`, `PlayPause`, `Next`).
    pub id: TaskbarButtonId,
    /// Glyph icon to show.
    pub icon: TaskbarButtonIcon,
    /// Tooltip text shown on hover (truncated to 259 UTF-16 code units by Win32).
    pub tooltip: String,
    /// Whether the button is visible (`!visible` sets `THBF_HIDDEN`).
    pub visible: bool,
    /// Whether the button is clickable (`THBF_ENABLED` vs `THBF_DISABLED`).
    pub enabled: bool,
}

/// Builds the 3 taskbar thumbnail button specifications for `state`
/// (`TaskbarButtons::apply` in `windows_taskbar_buttons.cpp:258-282`).
#[must_use]
pub fn build_thumb_button_specs(state: TaskbarMediaState) -> [TaskbarButtonSpec; 3] {
    [
        TaskbarButtonSpec {
            id: TaskbarButtonId::Previous,
            icon: TaskbarButtonIcon::Previous,
            tooltip: "Previous".to_owned(),
            visible: state.active,
            enabled: state.active && state.previous_available,
        },
        TaskbarButtonSpec {
            id: TaskbarButtonId::PlayPause,
            icon: if state.playing {
                TaskbarButtonIcon::Pause
            } else {
                TaskbarButtonIcon::Play
            },
            tooltip: if state.playing {
                "Pause".to_owned()
            } else {
                "Play".to_owned()
            },
            visible: state.active,
            enabled: state.active,
        },
        TaskbarButtonSpec {
            id: TaskbarButtonId::Next,
            icon: TaskbarButtonIcon::Next,
            tooltip: "Next".to_owned(),
            visible: state.active,
            enabled: state.active && state.next_available,
        },
    ]
}

/// Renders a crisp monochrome media control glyph (`size x size` RGBA) for Windows taskbar
/// thumbnail buttons (white on dark taskbar, black on light taskbar, matching
/// `windows_taskbar_buttons.cpp:220-229`).
#[must_use]
pub fn render_thumb_icon(kind: TaskbarButtonIcon, size: u32, dark_taskbar: bool) -> RgbaImage {
    let size = size.max(16);
    let mut img = RgbaImage::transparent(size, size);
    let fg = if dark_taskbar {
        RgbaColor::rgb(255, 255, 255)
    } else {
        RgbaColor::rgb(0, 0, 0)
    };
    let s = size as f32;
    let margin = (s * 0.20).round() as i32;
    let top = margin;
    let bottom = (size as i32 - margin).max(top + 2);
    let height = (bottom - top) as f32;
    let bar_w = (s * 0.14).round().max(2.0) as i32;

    let fill_rect = |img: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32| {
        for y in y0..y1 {
            for x in x0..x1 {
                img.blend_pixel(x, y, fg);
            }
        }
    };

    let fill_triangle_right = |img: &mut RgbaImage, left: i32, right: i32, y0: i32, y1: i32| {
        let w = (right - left).max(1) as f32;
        let cy = (y0 + y1) as f32 * 0.5;
        let half_h = ((y1 - y0) as f32 * 0.5).max(1.0);
        for y in y0..y1 {
            let dy = ((y as f32 + 0.5) - cy).abs();
            let frac = (1.0 - dy / half_h).clamp(0.0, 1.0);
            let x_end = left + (w * frac).round() as i32;
            for x in left..x_end {
                img.blend_pixel(x, y, fg);
            }
        }
    };

    let fill_triangle_left = |img: &mut RgbaImage, left: i32, right: i32, y0: i32, y1: i32| {
        let w = (right - left).max(1) as f32;
        let cy = (y0 + y1) as f32 * 0.5;
        let half_h = ((y1 - y0) as f32 * 0.5).max(1.0);
        for y in y0..y1 {
            let dy = ((y as f32 + 0.5) - cy).abs();
            let frac = (1.0 - dy / half_h).clamp(0.0, 1.0);
            let x_start = right - (w * frac).round() as i32;
            for x in x_start..right {
                img.blend_pixel(x, y, fg);
            }
        }
    };

    match kind {
        TaskbarButtonIcon::Play => {
            let left = margin + (s * 0.08).round() as i32;
            let right = size as i32 - margin;
            fill_triangle_right(&mut img, left, right, top, bottom);
        }
        TaskbarButtonIcon::Pause => {
            let gap = (s * 0.16).round().max(2.0) as i32;
            let pw = (s * 0.18).round().max(2.0) as i32;
            let total = pw * 2 + gap;
            let x0 = (size as i32 - total) / 2;
            fill_rect(&mut img, x0, top, x0 + pw, bottom);
            fill_rect(&mut img, x0 + pw + gap, top, x0 + pw * 2 + gap, bottom);
        }
        TaskbarButtonIcon::Previous => {
            let x_bar = margin;
            let tri_left = x_bar + bar_w + 1;
            let tri_right = size as i32 - margin;
            let tri_top = top + (height * 0.05).round() as i32;
            let tri_bottom = bottom - (height * 0.05).round() as i32;
            fill_rect(&mut img, x_bar, top, x_bar + bar_w, bottom);
            fill_triangle_left(&mut img, tri_left, tri_right, tri_top, tri_bottom);
        }
        TaskbarButtonIcon::Next => {
            let x_bar = size as i32 - margin - bar_w;
            let tri_left = margin;
            let tri_right = x_bar - 1;
            let tri_top = top + (height * 0.05).round() as i32;
            let tri_bottom = bottom - (height * 0.05).round() as i32;
            fill_triangle_right(&mut img, tri_left, tri_right, tri_top, tri_bottom);
            fill_rect(&mut img, x_bar, top, x_bar + bar_w, bottom);
        }
    }

    img
}

/// Asynchronous Win32 window event observed for taskbar thumbnail toolbar buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskbarButtonEvent {
    /// User clicked one of the 3 thumbnail toolbar buttons (`WM_COMMAND` with `THBN_CLICKED`).
    Clicked(TaskbarButtonId),
    /// Explorer broadcast `TaskbarButtonCreated` (initial creation or Explorer restart).
    TaskbarButtonCreated,
}

/// Platform backend trait abstracting `ITaskbarList3` thumbnail toolbar calls so tests never
/// initialize Explorer COM objects or touch real windows.
pub trait TaskbarButtonsBackend: Send {
    /// Adds the 3 thumbnail toolbar buttons to `hwnd` (`ITaskbarList3::ThumbBarAddButtons`).
    fn add_buttons(
        &mut self,
        hwnd: isize,
        specs: &[TaskbarButtonSpec; 3],
        dark_taskbar: bool,
    ) -> Result<()>;

    /// Updates existing thumbnail toolbar buttons on `hwnd` (`ITaskbarList3::ThumbBarUpdateButtons`).
    fn update_buttons(
        &mut self,
        hwnd: isize,
        specs: &[TaskbarButtonSpec; 3],
        dark_taskbar: bool,
    ) -> Result<()>;

    /// Drains any pending taskbar button click or `TaskbarButtonCreated` events since the last poll.
    fn poll_events(&mut self) -> Vec<TaskbarButtonEvent> {
        Vec::new()
    }
}

/// Stateful controller coordinating `ThumbBarAddButtons` / `ThumbBarUpdateButtons` transitions
/// without redundant COM updates (`Platform::TaskbarButtons` in `windows_taskbar_buttons.cpp`).
#[derive(Debug)]
pub struct TaskbarButtonsController<B: TaskbarButtonsBackend> {
    backend: B,
    taskbar_ready: bool,
    added: bool,
    applied_hwnd: isize,
    applied_state: TaskbarMediaState,
    applied_dark: Option<bool>,
}

impl<B: TaskbarButtonsBackend> TaskbarButtonsController<B> {
    /// Creates a new controller wrapping `backend`.
    ///
    /// `taskbar_ready` defaults to `true` because modern top-level `eframe` windows are already
    /// registered with the taskbar by the first rendered frame; call
    /// [`Self::on_taskbar_button_created`] whenever Explorer restarts and broadcasts
    /// `TaskbarButtonCreated`.
    #[must_use]
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            taskbar_ready: true,
            added: false,
            applied_hwnd: 0,
            applied_state: TaskbarMediaState::inactive(),
            applied_dark: None,
        }
    }

    /// Returns a shared reference to the underlying backend.
    #[must_use]
    pub const fn backend(&self) -> &B {
        &self.backend
    }

    /// Returns a mutable reference to the underlying backend.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Returns the last media state successfully applied to the taskbar.
    #[must_use]
    pub const fn applied_state(&self) -> TaskbarMediaState {
        self.applied_state
    }

    /// Returns `true` if `ThumbBarAddButtons` has succeeded for the current taskbar button.
    #[must_use]
    pub const fn is_added(&self) -> bool {
        self.added
    }

    /// Resets button registration state after Explorer broadcasts `TaskbarButtonCreated`
    /// (`TaskbarButtons::buttonsCreated` in `windows_taskbar_buttons.cpp:148-155`).
    pub fn on_taskbar_button_created(&mut self) {
        self.taskbar_ready = true;
        self.added = false;
        self.applied_state = TaskbarMediaState::inactive();
        self.applied_dark = None;
    }

    /// Synchronizes `state` and `dark_taskbar` onto `hwnd`.
    ///
    /// Returns `Ok(true)` when a backend `add_buttons` or `update_buttons` call was issued, or
    /// `Ok(false)` when no update was required (matching `windows_taskbar_buttons.cpp:181-191`).
    pub fn sync(
        &mut self,
        hwnd: isize,
        state: TaskbarMediaState,
        dark_taskbar: bool,
    ) -> Result<bool> {
        if hwnd == 0 || !self.taskbar_ready {
            return Ok(false);
        }
        if self.applied_hwnd != hwnd {
            self.applied_hwnd = hwnd;
            self.added = false;
            self.applied_state = TaskbarMediaState::inactive();
            self.applied_dark = None;
        }

        let was_active = self.added && self.applied_state.active;
        if !state.active && !was_active {
            return Ok(false);
        }

        let specs = build_thumb_button_specs(state);
        if !self.added {
            self.backend.add_buttons(hwnd, &specs, dark_taskbar)?;
            self.added = true;
            self.applied_state = state;
            self.applied_dark = Some(dark_taskbar);
            return Ok(true);
        }

        if self.applied_state != state || self.applied_dark != Some(dark_taskbar) {
            self.backend.update_buttons(hwnd, &specs, dark_taskbar)?;
            self.applied_state = state;
            self.applied_dark = Some(dark_taskbar);
            return Ok(true);
        }

        Ok(false)
    }

    /// Drains pending backend events, applying any `TaskbarButtonCreated` resets and returning
    /// clicked [`TaskbarButtonId`]s in FIFO order.
    pub fn poll_clicked_buttons(&mut self) -> Vec<TaskbarButtonId> {
        let mut clicks = Vec::new();
        for event in self.backend.poll_events() {
            match event {
                TaskbarButtonEvent::Clicked(id) => clicks.push(id),
                TaskbarButtonEvent::TaskbarButtonCreated => self.on_taskbar_button_created(),
            }
        }
        clicks
    }
}

/// Recorded call emitted into [`FakeTaskbarButtonsBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedThumbBarCall {
    /// Target window handle.
    pub hwnd: isize,
    /// Whether `ThumbBarAddButtons` (`true`) or `ThumbBarUpdateButtons` (`false`) was called.
    pub is_add: bool,
    /// Snapshot of the 3 button specs passed to the backend.
    pub specs: [TaskbarButtonSpec; 3],
    /// Whether dark taskbar icons were requested.
    pub dark_taskbar: bool,
}

/// Deterministic in-memory [`TaskbarButtonsBackend`] for headless unit & integration tests.
#[derive(Debug, Clone, Default)]
pub struct FakeTaskbarButtonsBackend {
    inner: Arc<Mutex<FakeTaskbarState>>,
}

#[derive(Debug, Default)]
struct FakeTaskbarState {
    calls: Vec<RecordedThumbBarCall>,
    events: VecDeque<TaskbarButtonEvent>,
    error_message: Option<String>,
}

impl FakeTaskbarButtonsBackend {
    /// Creates an empty fake taskbar buttons backend.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueues a synthetic [`TaskbarButtonEvent`] to be returned by [`TaskbarButtonsBackend::poll_events`].
    pub fn push_event(&self, event: TaskbarButtonEvent) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.events.push_back(event);
        }
    }

    /// Injects an error to be returned by subsequent calls.
    pub fn set_error(&self, message: Option<String>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.error_message = message;
        }
    }

    /// Returns all recorded calls in chronological order.
    #[must_use]
    pub fn calls(&self) -> Vec<RecordedThumbBarCall> {
        self.inner
            .lock()
            .map(|g| g.calls.clone())
            .unwrap_or_default()
    }

    /// Returns the most recently recorded call, if any.
    #[must_use]
    pub fn last_call(&self) -> Option<RecordedThumbBarCall> {
        self.inner.lock().ok().and_then(|g| g.calls.last().cloned())
    }
}

impl TaskbarButtonsBackend for FakeTaskbarButtonsBackend {
    fn add_buttons(
        &mut self,
        hwnd: isize,
        specs: &[TaskbarButtonSpec; 3],
        dark_taskbar: bool,
    ) -> Result<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| crate::error::PlatformError::TaskbarButtons("lock poisoned".to_owned()))?;
        if let Some(ref msg) = guard.error_message {
            return Err(crate::error::PlatformError::TaskbarButtons(msg.clone()));
        }
        guard.calls.push(RecordedThumbBarCall {
            hwnd,
            is_add: true,
            specs: specs.clone(),
            dark_taskbar,
        });
        Ok(())
    }

    fn update_buttons(
        &mut self,
        hwnd: isize,
        specs: &[TaskbarButtonSpec; 3],
        dark_taskbar: bool,
    ) -> Result<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| crate::error::PlatformError::TaskbarButtons("lock poisoned".to_owned()))?;
        if let Some(ref msg) = guard.error_message {
            return Err(crate::error::PlatformError::TaskbarButtons(msg.clone()));
        }
        guard.calls.push(RecordedThumbBarCall {
            hwnd,
            is_add: false,
            specs: specs.clone(),
            dark_taskbar,
        });
        Ok(())
    }

    fn poll_events(&mut self) -> Vec<TaskbarButtonEvent> {
        self.inner
            .lock()
            .map(|mut g| g.events.drain(..).collect())
            .unwrap_or_default()
    }
}

/// Production Windows `ITaskbarList3` thumbnail toolbar backend.
pub struct Win32TaskbarButtonsBackend {
    #[cfg(windows)]
    inner: win_impl::WinTaskbarButtons,
}

impl std::fmt::Debug for Win32TaskbarButtonsBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Win32TaskbarButtonsBackend")
            .finish_non_exhaustive()
    }
}

impl Win32TaskbarButtonsBackend {
    /// Initializes a COM `ITaskbarList3` instance on the current thread.
    pub fn new() -> Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: win_impl::WinTaskbarButtons::new()?,
            })
        }
        #[cfg(not(windows))]
        {
            Err(crate::error::PlatformError::Unsupported)
        }
    }
}

impl TaskbarButtonsBackend for Win32TaskbarButtonsBackend {
    fn add_buttons(
        &mut self,
        hwnd: isize,
        specs: &[TaskbarButtonSpec; 3],
        dark_taskbar: bool,
    ) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.apply(hwnd, specs, dark_taskbar, true)
        }
        #[cfg(not(windows))]
        {
            let _ = (hwnd, specs, dark_taskbar);
            Ok(())
        }
    }

    fn update_buttons(
        &mut self,
        hwnd: isize,
        specs: &[TaskbarButtonSpec; 3],
        dark_taskbar: bool,
    ) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.apply(hwnd, specs, dark_taskbar, false)
        }
        #[cfg(not(windows))]
        {
            let _ = (hwnd, specs, dark_taskbar);
            Ok(())
        }
    }

    fn poll_events(&mut self) -> Vec<TaskbarButtonEvent> {
        #[cfg(windows)]
        {
            self.inner.poll_events()
        }
        #[cfg(not(windows))]
        {
            Vec::new()
        }
    }
}

#[cfg(windows)]
mod win_impl {
    use std::collections::VecDeque;
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex};

    use windows::Win32::Foundation::{E_NOTIMPL, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    };
    use windows::Win32::UI::Shell::{
        DefSubclassProc, ITaskbarList3, RemoveWindowSubclass, SetWindowSubclass, THB_FLAGS,
        THB_ICON, THB_TOOLTIP, THBF_DISABLED, THBF_ENABLED, THBF_HIDDEN, THUMBBUTTON, TaskbarList,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DestroyIcon, GetSystemMetrics, HICON, RegisterWindowMessageW, SM_CXSMICON, WM_COMMAND,
        WM_NCDESTROY,
    };
    use windows_core::w;

    use super::{
        TaskbarButtonEvent, TaskbarButtonIcon, TaskbarButtonId, TaskbarButtonSpec,
        render_thumb_icon,
    };
    use crate::error::{PlatformError, Result};
    use crate::tray::rgba_to_hicon_for_platform;

    /// Arbitrary unique subclass ID for `SetWindowSubclass` on the top-level window.
    const THUMBBAR_SUBCLASS_ID: usize = 0x5447_5442; // "TGTB"

    struct SubclassState {
        taskbar_created_msg: u32,
        events: Mutex<VecDeque<TaskbarButtonEvent>>,
    }

    unsafe extern "system" fn thumb_bar_subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        uid_subclass: usize,
        dw_ref_data: usize,
    ) -> LRESULT {
        if dw_ref_data != 0 {
            // SAFETY: `dw_ref_data` is an `Arc<SubclassState>` raw pointer installed by
            // `ensure_subclass` and kept alive until `RemoveWindowSubclass` succeeds here or in
            // `WinTaskbarButtons::unhook_subclass`.
            let state = unsafe { &*(dw_ref_data as *const SubclassState) };
            if msg == WM_COMMAND
                && let Some(btn) = TaskbarButtonId::from_wm_command(wparam.0)
            {
                if let Ok(mut queue) = state.events.lock() {
                    queue.push_back(TaskbarButtonEvent::Clicked(btn));
                }
                return LRESULT(0);
            }
            if state.taskbar_created_msg != 0
                && msg == state.taskbar_created_msg
                && let Ok(mut queue) = state.events.lock()
            {
                queue.push_back(TaskbarButtonEvent::TaskbarButtonCreated);
            }
            if msg == WM_NCDESTROY {
                // SAFETY: Removing our own subclass before the window finishes tearing down;
                // only reclaim the `Arc` reference if `RemoveWindowSubclass` reports it was still
                // registered so `Drop` cannot double-free.
                let removed = unsafe {
                    RemoveWindowSubclass(hwnd, Some(thumb_bar_subclass_proc), uid_subclass)
                }
                .as_bool();
                if removed {
                    // SAFETY: Reclaims the single `Arc::into_raw` reference owned by the subclass.
                    let _ = unsafe { Arc::from_raw(dw_ref_data as *const SubclassState) };
                }
            }
        }
        // SAFETY: Always forwards unhandled messages down the comctl32 subclass chain.
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    struct ComGuard(bool);

    impl ComGuard {
        fn init_sta() -> Self {
            // SAFETY: Balanced with `CoUninitialize` in `Drop` when `CoInitializeEx` succeeds.
            let ok = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
            Self(ok)
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: Matches successful `CoInitializeEx` on this thread.
                unsafe { CoUninitialize() };
            }
        }
    }

    pub(super) struct WinTaskbarButtons {
        taskbar: ITaskbarList3,
        prev_icon: HICON,
        play_icon: HICON,
        pause_icon: HICON,
        next_icon: HICON,
        icons_dark: Option<bool>,
        icons_size: u32,
        hooked_hwnd: isize,
        subclass_state: Arc<SubclassState>,
        subclass_raw_ptr: *const SubclassState,
        // IMPORTANT: Must remain the last field so `taskbar` is released BEFORE `CoUninitialize`.
        _com: ComGuard,
    }

    // SAFETY: `WinTaskbarButtons` is owned by the UI shell thread (`TgforkApp`), never shared
    // concurrently across threads; `Send` allows moving into `TgforkApp` at startup.
    unsafe impl Send for WinTaskbarButtons {}

    impl WinTaskbarButtons {
        pub(super) fn new() -> Result<Self> {
            let com = ComGuard::init_sta();
            // SAFETY: Creates the standard Shell `TaskbarList` COM object and initializes it.
            let taskbar: ITaskbarList3 = unsafe {
                CoCreateInstance(&TaskbarList, None, CLSCTX_ALL)
                    .map_err(|e| PlatformError::from_win("CoCreateInstance(TaskbarList)", e))?
            };
            // SAFETY: Required by `ITaskbarList3` before calling `ThumbBar*` methods.
            unsafe { taskbar.HrInit() }.map_err(|e| {
                if e.code() == E_NOTIMPL {
                    PlatformError::Unsupported
                } else {
                    PlatformError::from_win("ITaskbarList3::HrInit", e)
                }
            })?;
            // SAFETY: Registers the standard Explorer broadcast message ID for taskbar rebuilds.
            let taskbar_created_msg = unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) };
            Ok(Self {
                taskbar,
                prev_icon: HICON::default(),
                play_icon: HICON::default(),
                pause_icon: HICON::default(),
                next_icon: HICON::default(),
                icons_dark: None,
                icons_size: 0,
                hooked_hwnd: 0,
                subclass_state: Arc::new(SubclassState {
                    taskbar_created_msg,
                    events: Mutex::new(VecDeque::new()),
                }),
                subclass_raw_ptr: std::ptr::null(),
                _com: com,
            })
        }

        fn ensure_subclass(&mut self, hwnd: isize) {
            if hwnd == 0 || self.hooked_hwnd == hwnd {
                return;
            }
            self.unhook_subclass();
            let raw = Arc::into_raw(Arc::clone(&self.subclass_state));
            // SAFETY: Attaches a comctl32 subclass callback to the caller-owned top-level `hwnd`;
            // if registration fails we immediately reclaim the `Arc` reference.
            let installed = unsafe {
                SetWindowSubclass(
                    HWND(hwnd as *mut c_void),
                    Some(thumb_bar_subclass_proc),
                    THUMBBAR_SUBCLASS_ID,
                    raw as usize,
                )
            }
            .as_bool();
            if installed {
                self.hooked_hwnd = hwnd;
                self.subclass_raw_ptr = raw;
            } else {
                // SAFETY: `SetWindowSubclass` failed so no callback holds `raw`.
                let _ = unsafe { Arc::from_raw(raw) };
            }
        }

        fn unhook_subclass(&mut self) {
            if self.hooked_hwnd == 0 {
                return;
            }
            let hwnd = HWND(self.hooked_hwnd as *mut c_void);
            // SAFETY: Removes our previously installed subclass on the UI thread. Only if
            // `RemoveWindowSubclass` returns `TRUE` (meaning `WM_NCDESTROY` did not already remove
            // it) do we reclaim `subclass_raw_ptr`.
            let removed = unsafe {
                RemoveWindowSubclass(hwnd, Some(thumb_bar_subclass_proc), THUMBBAR_SUBCLASS_ID)
            }
            .as_bool();
            if removed && !self.subclass_raw_ptr.is_null() {
                // SAFETY: Matches the `Arc::into_raw` in `ensure_subclass`.
                let _ = unsafe { Arc::from_raw(self.subclass_raw_ptr) };
            }
            self.hooked_hwnd = 0;
            self.subclass_raw_ptr = std::ptr::null();
        }

        pub(super) fn poll_events(&mut self) -> Vec<TaskbarButtonEvent> {
            self.subclass_state
                .events
                .lock()
                .map(|mut q| q.drain(..).collect())
                .unwrap_or_default()
        }

        fn destroy_icons(&mut self) {
            for icon in [
                &mut self.prev_icon,
                &mut self.play_icon,
                &mut self.pause_icon,
                &mut self.next_icon,
            ] {
                if !icon.is_invalid() {
                    // SAFETY: Destroying owned HICON created by `CreateIconIndirect`.
                    let _ = unsafe { DestroyIcon(*icon) };
                    *icon = HICON::default();
                }
            }
        }

        fn ensure_icons(&mut self, dark_taskbar: bool) -> Result<()> {
            // SAFETY: Querying standard small icon size metric.
            let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
            if self.icons_dark == Some(dark_taskbar)
                && self.icons_size == size
                && !self.prev_icon.is_invalid()
            {
                return Ok(());
            }
            self.destroy_icons();
            self.prev_icon = rgba_to_hicon_for_platform(&render_thumb_icon(
                TaskbarButtonIcon::Previous,
                size,
                dark_taskbar,
            ))?;
            self.play_icon = rgba_to_hicon_for_platform(&render_thumb_icon(
                TaskbarButtonIcon::Play,
                size,
                dark_taskbar,
            ))?;
            self.pause_icon = rgba_to_hicon_for_platform(&render_thumb_icon(
                TaskbarButtonIcon::Pause,
                size,
                dark_taskbar,
            ))?;
            self.next_icon = rgba_to_hicon_for_platform(&render_thumb_icon(
                TaskbarButtonIcon::Next,
                size,
                dark_taskbar,
            ))?;
            self.icons_dark = Some(dark_taskbar);
            self.icons_size = size;
            Ok(())
        }

        fn icon_for(&self, icon: TaskbarButtonIcon) -> HICON {
            match icon {
                TaskbarButtonIcon::Previous => self.prev_icon,
                TaskbarButtonIcon::Play => self.play_icon,
                TaskbarButtonIcon::Pause => self.pause_icon,
                TaskbarButtonIcon::Next => self.next_icon,
            }
        }

        pub(super) fn apply(
            &mut self,
            hwnd: isize,
            specs: &[TaskbarButtonSpec; 3],
            dark_taskbar: bool,
            create: bool,
        ) -> Result<()> {
            if hwnd == 0 {
                return Ok(());
            }
            self.ensure_subclass(hwnd);
            self.ensure_icons(dark_taskbar)?;
            let mut buttons = [THUMBBUTTON::default(); 3];
            for (slot, spec) in buttons.iter_mut().zip(specs.iter()) {
                slot.dwMask = THB_ICON | THB_TOOLTIP | THB_FLAGS;
                slot.iId = spec.id.to_command_id();
                slot.hIcon = self.icon_for(spec.icon);
                slot.dwFlags = if !spec.visible {
                    THBF_HIDDEN
                } else if spec.enabled {
                    THBF_ENABLED
                } else {
                    THBF_DISABLED
                };
                slot.szTip.fill(0);
                let max = slot.szTip.len().saturating_sub(1);
                for (dst, unit) in slot
                    .szTip
                    .iter_mut()
                    .take(max)
                    .zip(spec.tooltip.encode_utf16())
                {
                    *dst = unit;
                }
            }

            let target = HWND(hwnd as *mut c_void);
            // SAFETY: `buttons` is a valid 3-element `THUMBBUTTON` slice with valid `HICON`s owned
            // by `self` and NUL-terminated UTF-16 tooltips.
            let res = if create {
                unsafe { self.taskbar.ThumbBarAddButtons(target, &buttons) }
            } else {
                unsafe { self.taskbar.ThumbBarUpdateButtons(target, &buttons) }
            };
            res.map_err(|e| {
                PlatformError::from_win(
                    if create {
                        "ITaskbarList3::ThumbBarAddButtons"
                    } else {
                        "ITaskbarList3::ThumbBarUpdateButtons"
                    },
                    e,
                )
            })
        }
    }

    impl Drop for WinTaskbarButtons {
        fn drop(&mut self) {
            self.unhook_subclass();
            self.destroy_icons();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wm_command_decodes_thbn_clicked_button_ids() {
        let mk = |code: u16, id: u16| ((code as usize) << 16) | (id as usize);
        assert_eq!(
            TaskbarButtonId::from_wm_command(mk(THBN_CLICKED_CODE, 1)),
            Some(TaskbarButtonId::Previous)
        );
        assert_eq!(
            TaskbarButtonId::from_wm_command(mk(THBN_CLICKED_CODE, 2)),
            Some(TaskbarButtonId::PlayPause)
        );
        assert_eq!(
            TaskbarButtonId::from_wm_command(mk(THBN_CLICKED_CODE, 3)),
            Some(TaskbarButtonId::Next)
        );
        assert_eq!(TaskbarButtonId::from_wm_command(mk(0x0000, 1)), None);
        assert_eq!(
            TaskbarButtonId::from_wm_command(mk(THBN_CLICKED_CODE, 99)),
            None
        );
        assert_eq!(
            TaskbarButtonId::PlayPause.to_smtc_command(),
            SmtcCommand::PlayPause
        );
    }

    #[test]
    fn controller_lifecycle_adds_once_updates_on_change_and_hides_on_stop() {
        let fake = FakeTaskbarButtonsBackend::new();
        let mut ctrl = TaskbarButtonsController::new(fake.clone());

        // Inactive before any media plays -> no COM call.
        assert!(
            !ctrl
                .sync(0x100, TaskbarMediaState::inactive(), true)
                .unwrap()
        );
        assert!(fake.calls().is_empty());

        // Start playing track with next available -> ThumbBarAddButtons.
        let playing = TaskbarMediaState {
            active: true,
            playing: true,
            previous_available: false,
            next_available: true,
        };
        assert!(ctrl.sync(0x100, playing, true).unwrap());
        assert_eq!(fake.calls().len(), 1);
        let first = fake.last_call().unwrap();
        assert!(first.is_add);
        assert!(!first.specs[0].enabled);
        assert_eq!(first.specs[1].icon, TaskbarButtonIcon::Pause);
        assert!(first.specs[2].enabled);

        // Identical state -> deduplicated no-op.
        assert!(!ctrl.sync(0x100, playing, true).unwrap());
        assert_eq!(fake.calls().len(), 1);

        // Pause -> ThumbBarUpdateButtons with Play icon.
        let paused = TaskbarMediaState {
            playing: false,
            ..playing
        };
        assert!(ctrl.sync(0x100, paused, true).unwrap());
        assert_eq!(fake.calls().len(), 2);
        let second = fake.last_call().unwrap();
        assert!(!second.is_add);
        assert_eq!(second.specs[1].icon, TaskbarButtonIcon::Play);

        // Stop -> ThumbBarUpdateButtons with hidden buttons.
        assert!(
            ctrl.sync(0x100, TaskbarMediaState::inactive(), true)
                .unwrap()
        );
        assert_eq!(fake.calls().len(), 3);
        let third = fake.last_call().unwrap();
        assert!(third.specs.iter().all(|b| !b.visible));

        // Explorer restart -> TaskbarButtonCreated resets `added` so next active state re-adds.
        ctrl.on_taskbar_button_created();
        assert!(!ctrl.is_added());
        assert!(ctrl.sync(0x100, playing, false).unwrap());
        let fourth = fake.last_call().unwrap();
        assert!(fourth.is_add);
        assert!(!fourth.dark_taskbar);

        // Polling backend events dispatches clicks and resets on TaskbarButtonCreated.
        fake.push_event(TaskbarButtonEvent::Clicked(TaskbarButtonId::PlayPause));
        fake.push_event(TaskbarButtonEvent::TaskbarButtonCreated);
        fake.push_event(TaskbarButtonEvent::Clicked(TaskbarButtonId::Next));
        let clicks = ctrl.poll_clicked_buttons();
        assert_eq!(
            clicks,
            vec![TaskbarButtonId::PlayPause, TaskbarButtonId::Next]
        );
        assert!(!ctrl.is_added());
    }

    #[test]
    fn thumb_icons_render_non_empty_pixels() {
        for kind in [
            TaskbarButtonIcon::Previous,
            TaskbarButtonIcon::Play,
            TaskbarButtonIcon::Pause,
            TaskbarButtonIcon::Next,
        ] {
            let img = render_thumb_icon(kind, 16, true);
            assert_eq!((img.width, img.height), (16, 16));
            assert!(
                img.pixels.as_chunks::<4>().0.iter().any(|px| px[3] > 0),
                "icon {kind:?} should draw opaque foreground pixels"
            );
        }
    }
}
