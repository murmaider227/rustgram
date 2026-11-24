//! Windows desktop window-shell integration gluing `tg-platform::frameless`,
//! `FramelessTitleBarState`, and `tg-platform::taskbar_buttons` (`ITaskbarList3`) into
//! `app.rs` / `main.rs` with zero direct Win32/COM calls in unit tests.
//!
//! Port origin:
//! - `Telegram/SourceFiles/platform/win/main_window_win.{h,cpp}`
//! - `Telegram/SourceFiles/platform/win/windows_taskbar_buttons.{h,cpp}`

use egui::{CornerRadius, Frame, Panel, Ui, ViewportBuilder, ViewportCommand};
use tg_platform::frameless::{Win32WindowFrameBackend, WindowFrameBackend};
use tg_platform::taskbar_buttons::{
    TaskbarButtonId, TaskbarButtonsBackend, TaskbarButtonsController, TaskbarMediaState,
    Win32TaskbarButtonsBackend,
};

use crate::window::chat_preview::FramelessTitleBarState;
use tg_app_core::stand_in::theme::Palette;
use tg_app_media::media::player_bar::AudioPlayerBar;

/// Computes whether the custom frameless title bar should be active for the current window mode.
///
/// Crash recovery always uses native window decorations so emergency diagnostics remain movable
/// even if custom framing fails.
#[must_use]
pub const fn should_use_frameless(native_window_frame: bool, has_crash_recovery: bool) -> bool {
    !native_window_frame && !has_crash_recovery
}

/// Applies the initial `native_window_frame` decoration setting to an `eframe` [`ViewportBuilder`].
#[must_use]
pub fn apply_initial_viewport_frame(
    viewport: ViewportBuilder,
    native_window_frame: bool,
    has_crash_recovery: bool,
) -> ViewportBuilder {
    let use_frameless = should_use_frameless(native_window_frame, has_crash_recovery);
    viewport.with_decorations(!use_frameless)
}

/// Extracts the Win32 `HWND` (`isize`) from an `eframe::Frame` using `raw-window-handle`.
///
/// Returns `0` on non-Windows platforms or when no native handle is attached (headless tests).
#[must_use]
pub fn extract_eframe_hwnd(frame: &eframe::Frame) -> isize {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match frame.window_handle() {
            Ok(handle) => match handle.as_raw() {
                RawWindowHandle::Win32(win) => win.hwnd.get(),
                _ => 0,
            },
            Err(_) => 0,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = frame;
        0
    }
}

/// Builds a [`TaskbarMediaState`] snapshot from the active [`AudioPlayerBar`].
#[must_use]
pub fn extract_taskbar_media_state(bar: &AudioPlayerBar) -> TaskbarMediaState {
    let slot = bar.active_type();
    let current = bar.player().current(slot);
    let state = bar.current_state();
    let active = bar.is_visible() && current.is_valid() && !state.state.is_stopped_or_stopping();
    if !active {
        return TaskbarMediaState::inactive();
    }
    TaskbarMediaState {
        active: true,
        playing: !state.state.is_paused_or_pausing() && !state.state.is_stopped_or_stopping(),
        previous_available: bar.player().previous_available(slot),
        next_available: bar.player().next_available(slot),
    }
}

/// Dispatches a clicked Windows taskbar thumbnail button to [`AudioPlayerBar`].
pub fn dispatch_taskbar_button_click(id: TaskbarButtonId, bar: &mut AudioPlayerBar) -> bool {
    bar.apply_smtc_command(id.to_smtc_command())
}

/// Composite Win32 window-frame + taskbar-thumbbar shell state owned by `TgforkApp`.
///
/// Generic over [`WindowFrameBackend`] and [`TaskbarButtonsBackend`] so tests can inject
/// `FakeWindowFrameBackend` and `FakeTaskbarButtonsBackend` without creating real windows or COM
/// objects.
pub struct PlatformWindowShell<
    F: WindowFrameBackend = Win32WindowFrameBackend,
    T: TaskbarButtonsBackend = Win32TaskbarButtonsBackend,
> {
    frame_backend: F,
    title_bar: FramelessTitleBarState,
    taskbar_buttons: Option<TaskbarButtonsController<T>>,
    last_dark_mode: Option<bool>,
    last_frameless: Option<bool>,
    last_passcode_locked: Option<bool>,
}

impl std::fmt::Debug for PlatformWindowShell<Win32WindowFrameBackend, Win32TaskbarButtonsBackend> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformWindowShell")
            .field("has_taskbar_buttons", &self.taskbar_buttons.is_some())
            .field("last_dark_mode", &self.last_dark_mode)
            .field("last_frameless", &self.last_frameless)
            .field("last_passcode_locked", &self.last_passcode_locked)
            .finish()
    }
}

impl PlatformWindowShell<Win32WindowFrameBackend, Win32TaskbarButtonsBackend> {
    /// Creates a production shell wired to Win32 DWM and `ITaskbarList3`.
    ///
    /// Gracefully disables `ITaskbarList3` when Explorer is absent (e.g. headless CI).
    #[must_use]
    pub fn new_win32() -> Self {
        let taskbar_buttons = Win32TaskbarButtonsBackend::new()
            .ok()
            .map(TaskbarButtonsController::new);
        Self {
            frame_backend: Win32WindowFrameBackend,
            title_bar: FramelessTitleBarState::default(),
            taskbar_buttons,
            last_dark_mode: None,
            last_frameless: None,
            last_passcode_locked: None,
        }
    }
}

impl<F: WindowFrameBackend, T: TaskbarButtonsBackend> PlatformWindowShell<F, T> {
    /// Creates a shell backed by explicit trait implementations (used by unit tests with fakes).
    #[must_use]
    pub fn with_backends(frame_backend: F, taskbar_backend: Option<T>) -> Self {
        Self {
            frame_backend,
            title_bar: FramelessTitleBarState::default(),
            taskbar_buttons: taskbar_backend.map(TaskbarButtonsController::new),
            last_dark_mode: None,
            last_frameless: None,
            last_passcode_locked: None,
        }
    }

    /// Returns a shared reference to the underlying window-frame backend.
    #[must_use]
    pub const fn frame_backend(&self) -> &F {
        &self.frame_backend
    }

    /// Returns a shared reference to the taskbar buttons controller, if initialized.
    #[must_use]
    pub const fn taskbar_buttons(&self) -> Option<&TaskbarButtonsController<T>> {
        self.taskbar_buttons.as_ref()
    }

    /// Returns a mutable reference to the taskbar buttons controller, if initialized.
    pub fn taskbar_buttons_mut(&mut self) -> Option<&mut TaskbarButtonsController<T>> {
        self.taskbar_buttons.as_mut()
    }

    /// Synchronizes Win32 DWM dark mode, frameless Aero-snap styles, viewport decorations, and
    /// passcode-locked iconic preview state whenever any input changes.
    pub fn sync_window_frame(
        &mut self,
        ctx: Option<&egui::Context>,
        hwnd: isize,
        dark_mode: bool,
        native_window_frame: bool,
        has_crash_recovery: bool,
        passcode_locked: bool,
    ) {
        let use_frameless = should_use_frameless(native_window_frame, has_crash_recovery);

        if self.last_frameless != Some(use_frameless) {
            if let Some(egui_ctx) = ctx {
                egui_ctx.send_viewport_cmd(ViewportCommand::Decorations(!use_frameless));
            }
            if hwnd != 0 {
                let _ = self
                    .frame_backend
                    .set_frameless_aero_snap(hwnd, use_frameless);
            }
            self.last_frameless = Some(use_frameless);
        }

        if hwnd != 0 && self.last_dark_mode != Some(dark_mode) {
            let _ = self.frame_backend.set_dark_mode(hwnd, dark_mode);
            self.last_dark_mode = Some(dark_mode);
        }

        if hwnd != 0 && self.last_passcode_locked != Some(passcode_locked) {
            let _ = self
                .frame_backend
                .set_passcode_iconic_preview(hwnd, passcode_locked);
            self.last_passcode_locked = Some(passcode_locked);
        }
    }

    /// Renders the custom frameless title bar (`FramelessTitleBarState`) in a top panel when
    /// `native_window_frame` is `false` and crash recovery is inactive.
    ///
    /// Returns `true` when the custom title bar was rendered.
    pub fn show_frameless_title_bar(
        &mut self,
        ui: &mut Ui,
        title: &str,
        palette: &Palette,
        native_window_frame: bool,
        has_crash_recovery: bool,
    ) -> bool {
        if !should_use_frameless(native_window_frame, has_crash_recovery) {
            return false;
        }
        tg_ui::fonts::ensure_tdesktop_fonts(ui.ctx());
        Panel::top("tg_frameless_title_bar")
            .frame(
                Frame::NONE
                    .fill(palette.window_bg)
                    .corner_radius(CornerRadius::ZERO),
            )
            .show(ui, |ui| {
                self.title_bar.show_title_bar(ui, title, palette);
            });
        true
    }

    /// Synchronizes the Windows taskbar media thumbnail buttons from `state`.
    pub fn sync_taskbar_media_state(
        &mut self,
        hwnd: isize,
        state: TaskbarMediaState,
        dark_taskbar: bool,
    ) -> bool {
        let Some(ctrl) = self.taskbar_buttons.as_mut() else {
            return false;
        };
        ctrl.sync(hwnd, state, dark_taskbar).unwrap_or(false)
    }

    /// Synchronizes the Windows taskbar media thumbnail buttons directly from [`AudioPlayerBar`].
    pub fn sync_taskbar_from_audio_bar(
        &mut self,
        hwnd: isize,
        bar: &AudioPlayerBar,
        dark_taskbar: bool,
    ) -> bool {
        let state = extract_taskbar_media_state(bar);
        self.sync_taskbar_media_state(hwnd, state, dark_taskbar)
    }

    /// Drains any clicked Windows taskbar thumbnail buttons observed since the last frame
    /// (also processing any `TaskbarButtonCreated` Explorer restart events).
    pub fn poll_taskbar_button_clicks(&mut self) -> Vec<TaskbarButtonId> {
        let Some(ctrl) = self.taskbar_buttons.as_mut() else {
            return Vec::new();
        };
        ctrl.poll_clicked_buttons()
    }

    /// Polls pending taskbar thumbnail button clicks and dispatches each one to [`AudioPlayerBar`].
    ///
    /// Returns the number of commands applied.
    pub fn poll_and_dispatch_taskbar_clicks(&mut self, bar: &mut AudioPlayerBar) -> usize {
        let mut applied = 0;
        for id in self.poll_taskbar_button_clicks() {
            if dispatch_taskbar_button_click(id, bar) {
                applied += 1;
            }
        }
        applied
    }
}
