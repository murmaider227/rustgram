//! Session window-restore offer & restore placeholder shell (`RestoreShell`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/window/window_restore_shell.h`
//! - `Telegram/SourceFiles/window/window_restore_shell.cpp`
//! - `Telegram/SourceFiles/window/window_saved_windows.cpp`

use std::path::{Path, PathBuf};

use egui::{Align2, CornerRadius, FontId, Frame, Margin, Stroke, Ui};
use serde::{Deserialize, Serialize};
use tg_core::settings::WindowPosition;
use tg_lang::tr;

/// Minimum width of a restore shell window (`st::windowShellMinWidth`).
pub const WINDOW_SHELL_MIN_WIDTH: i32 = 380;
/// Minimum height of a restore shell window (`st::windowShellMinHeight`).
pub const WINDOW_SHELL_MIN_HEIGHT: i32 = 480;
/// Default fallback width when no valid saved width exists (`st::windowDefaultWidth`).
pub const WINDOW_DEFAULT_WIDTH: i32 = 800;
/// Default fallback height when no valid saved height exists (`st::windowDefaultHeight`).
pub const WINDOW_DEFAULT_HEIGHT: i32 = 600;

/// Relative filename under `<working_dir>/tdata/` storing saved window geometry & policy.
pub const RESTORE_WINDOWS_FILE: &str = "restore_windows.json";

/// User preference for restoring previous-session windows on launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RestoreWindowsPolicy {
    /// Prompt the user with `"Restore windows?"` when saved windows are available.
    #[default]
    Ask,
    /// Automatically restore saved windows on launch without prompting.
    Always,
    /// Never restore previous windows and suppress the startup offer.
    Never,
}

/// Serializable window geometry snapshot matching `Core::WindowPosition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SavedWindowGeometry {
    /// Screen checksum (`moncrc`).
    pub moncrc: i32,
    /// `1` if maximized, `0` otherwise.
    pub maximized: i32,
    /// Scale percentage when saved.
    pub scale: i32,
    /// X offset relative to target screen origin.
    pub x: i32,
    /// Y offset relative to target screen origin.
    pub y: i32,
    /// Width in logical pixels.
    pub w: i32,
    /// Height in logical pixels.
    pub h: i32,
}

impl From<WindowPosition> for SavedWindowGeometry {
    fn from(p: WindowPosition) -> Self {
        Self {
            moncrc: p.moncrc,
            maximized: p.maximized,
            scale: p.scale,
            x: p.x,
            y: p.y,
            w: p.w,
            h: p.h,
        }
    }
}

impl From<SavedWindowGeometry> for WindowPosition {
    fn from(g: SavedWindowGeometry) -> Self {
        Self {
            moncrc: g.moncrc,
            maximized: g.maximized,
            scale: g.scale,
            x: g.x,
            y: g.y,
            w: g.w,
            h: g.h,
        }
    }
}

/// A single saved separate/secondary chat window from a previous session (`Window::SavedWindow`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWindowEntry {
    /// Stable restore step ID.
    pub id: u64,
    /// Window title shown in the shell title bar while loading.
    pub title: String,
    /// Account index that owns this window.
    pub account_index: usize,
    /// Optional peer ID of the separate chat window.
    pub peer_id: Option<i64>,
    /// Saved window geometry.
    pub position: SavedWindowGeometry,
}

/// On-disk snapshot persisted in `<working_dir>/tdata/restore_windows.json`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SavedWindowsSnapshot {
    /// Restore policy (`Ask` / `Always` / `Never`).
    pub policy: RestoreWindowsPolicy,
    /// Saved windows from the previous session.
    pub windows: Vec<SavedWindowEntry>,
}

impl SavedWindowsSnapshot {
    /// Returns `<working_dir>/tdata/restore_windows.json`.
    #[must_use]
    pub fn file_path(working_dir: &Path) -> PathBuf {
        working_dir.join("tdata").join(RESTORE_WINDOWS_FILE)
    }

    /// Loads the snapshot from `<working_dir>/tdata/restore_windows.json`, returning defaults if
    /// the file does not exist or cannot be parsed.
    #[must_use]
    pub fn load_from(working_dir: &Path) -> Self {
        let path = Self::file_path(working_dir);
        std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Persists the snapshot to `<working_dir>/tdata/restore_windows.json`.
    pub fn save_to(&self, working_dir: &Path) -> std::io::Result<()> {
        let path = Self::file_path(working_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, bytes)
    }
}

/// Screen bounds `(x, y, width, height)` used by [`count_initial_shell_geometry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenBounds {
    /// Left coordinate of the target screen's available geometry.
    pub x: i32,
    /// Top coordinate of the target screen's available geometry.
    pub y: i32,
    /// Available width in logical pixels.
    pub w: i32,
    /// Available height in logical pixels.
    pub h: i32,
}

impl Default for ScreenBounds {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        }
    }
}

/// Computes the clamped initial geometry `(x, y, w, h)` for a [`RestoreShell`]
/// (`RestoreShell::applyPosition` in `window_restore_shell.cpp:154-180`).
#[must_use]
pub fn count_initial_shell_geometry(
    saved: WindowPosition,
    screen: ScreenBounds,
) -> (i32, i32, i32, i32) {
    let default_w = WINDOW_DEFAULT_WIDTH.min(screen.w.max(WINDOW_SHELL_MIN_WIDTH));
    let default_h = WINDOW_DEFAULT_HEIGHT.min(screen.h.max(WINDOW_SHELL_MIN_HEIGHT));

    let valid = saved.w >= WINDOW_SHELL_MIN_WIDTH && saved.h >= WINDOW_SHELL_MIN_HEIGHT;
    let w = if valid {
        saved
            .w
            .clamp(WINDOW_SHELL_MIN_WIDTH, screen.w.max(WINDOW_SHELL_MIN_WIDTH))
    } else {
        default_w
    };
    let h = if valid {
        saved.h.clamp(
            WINDOW_SHELL_MIN_HEIGHT,
            screen.h.max(WINDOW_SHELL_MIN_HEIGHT),
        )
    } else {
        default_h
    };
    let (x, y) = if valid {
        let max_x = screen.x + (screen.w - w).max(0);
        let max_y = screen.y + (screen.h - h).max(0);
        (
            (screen.x + saved.x).clamp(screen.x, max_x),
            (screen.y + saved.y).clamp(screen.y, max_y),
        )
    } else {
        (
            screen.x + (screen.w - w).max(0) / 2,
            screen.y + (screen.h - h).max(0) / 2,
        )
    };
    (x, y, w, h)
}

/// Lifecycle status of a [`RestoreShell`] placeholder window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreShellStatus {
    /// Showing centered loading spinner while the restored chat/peer hydrates.
    Loading,
    /// Target chat became available and the real window took over.
    Ready,
    /// Target chat is no longer accessible (`showUnavailable` -> `lng_restore_windows_unavailable`).
    Unavailable,
    /// Shell window was closed by the user or cancelled.
    Closed,
}

/// Placeholder window shown while restoring a saved chat window (`Window::RestoreShell`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreShell {
    /// Restore step identifier.
    pub id: u64,
    /// Display title (defaults to `"Rustgram"` when empty).
    pub title: String,
    /// Optional peer ID associated with this shell.
    pub peer_id: Option<i64>,
    /// Current lifecycle state.
    pub status: RestoreShellStatus,
    /// Clamped normal (non-maximized) body geometry `(x, y, w, h)`.
    pub normal_body: (i32, i32, i32, i32),
    /// Whether the shell was restored maximized.
    pub maximized: bool,
    /// Whether this shell is currently the active window.
    pub active: bool,
    /// Whether the user requested closing the shell (`closeRequests()`).
    pub close_requested: bool,
}

impl RestoreShell {
    /// Creates a new [`RestoreShell`] in the [`RestoreShellStatus::Loading`] state
    /// (`RestoreShell::RestoreShell` in `window_restore_shell.cpp:28-48`).
    #[must_use]
    pub fn new(
        id: u64,
        title: impl Into<String>,
        peer_id: Option<i64>,
        position: WindowPosition,
        screen: ScreenBounds,
    ) -> Self {
        let raw_title = title.into();
        let title = if raw_title.trim().is_empty() {
            "Rustgram".to_owned()
        } else {
            raw_title
        };
        let normal_body = count_initial_shell_geometry(position, screen);
        Self {
            id,
            title,
            peer_id,
            status: RestoreShellStatus::Loading,
            normal_body,
            maximized: position.maximized != 0,
            active: false,
            close_requested: false,
        }
    }

    /// Transitions the shell to the `"This chat is no longer available."` state
    /// (`RestoreShell::showUnavailable` in `window_restore_shell.cpp:76-99`).
    pub fn show_unavailable(&mut self) {
        if self.status != RestoreShellStatus::Unavailable {
            self.status = RestoreShellStatus::Unavailable;
        }
    }

    /// Marks the shell as hydrated and ready to be replaced by a live chat window.
    pub fn mark_ready(&mut self) {
        self.status = RestoreShellStatus::Ready;
    }

    /// Raises and focuses the shell (`RestoreShell::activate`).
    pub fn activate(&mut self) {
        self.active = true;
    }

    /// Closes the shell (`RestoreShell::close`).
    pub fn close(&mut self) {
        self.active = false;
        self.close_requested = true;
        self.status = RestoreShellStatus::Closed;
    }

    /// Computes a [`WindowPosition`] suitable for persisting back to `tdata`
    /// (`RestoreShell::countPositionForSave` in `window_restore_shell.cpp:114-144`).
    #[must_use]
    pub const fn count_position_for_save(
        &self,
        scale: i32,
        screen_origin: (i32, i32),
        moncrc: i32,
    ) -> WindowPosition {
        WindowPosition {
            moncrc,
            maximized: if self.maximized { 1 } else { 0 },
            scale,
            x: self.normal_body.0 - screen_origin.0,
            y: self.normal_body.1 - screen_origin.1,
            w: self.normal_body.2,
            h: self.normal_body.3,
        }
    }

    /// Renders the shell body inside `ui` (centered spinner while loading, or
    /// `tr::lng_restore_windows_unavailable()` when unavailable).
    pub fn render_body(&self, ui: &mut Ui) {
        let rect = ui.max_rect();
        match self.status {
            RestoreShellStatus::Loading => {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.spinner();
                        ui.add_space(8.0);
                        ui.label(&self.title);
                    });
                });
            }
            RestoreShellStatus::Unavailable => {
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    tr::lng_restore_windows_unavailable(),
                    FontId::proportional(14.0),
                    ui.visuals().weak_text_color(),
                );
            }
            RestoreShellStatus::Ready | RestoreShellStatus::Closed => {}
        }
    }
}

/// User decision on the startup `"Restore windows?"` offer (`window_saved_windows.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreWindowsAction {
    /// Restore saved windows for this session only (`lng_restore_windows_restore`).
    RestoreOnce,
    /// Restore now and always restore on future launches (`lng_restore_windows_always`).
    Always,
    /// Dismiss now and never ask on future launches (`lng_restore_windows_never`).
    Never,
    /// Dismiss for this launch without changing the stored policy.
    Dismiss,
}

/// Active startup offer prompting the user to restore saved windows from the previous session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreWindowsOffer {
    /// Saved window entries available to restore.
    pub windows: Vec<SavedWindowEntry>,
}

impl RestoreWindowsOffer {
    /// Localized prompt question (`"Restore windows?"`).
    #[must_use]
    pub fn question(&self) -> String {
        tr::lng_restore_windows_question()
    }
}

/// Orchestrates saved-window snapshots, the startup restore offer, and active [`RestoreShell`]s
/// (`Window::SavedWindows` + `Window::RestoreShell`).
#[derive(Debug, Clone, Default)]
pub struct RestoreWindowsManager {
    /// Active restore policy.
    pub policy: RestoreWindowsPolicy,
    /// Pending startup offer shown to the user when `policy == Ask` and saved windows exist.
    pub pending_offer: Option<RestoreWindowsOffer>,
    /// Currently loading or active placeholder shells.
    pub shells: Vec<RestoreShell>,
    /// Shells moved to unavailable state until the user closes them (`_deadShells`).
    pub dead_shells: Vec<RestoreShell>,
}

impl RestoreWindowsManager {
    /// Initializes the manager from a persisted [`SavedWindowsSnapshot`] on startup.
    ///
    /// - `Always`: immediately spawns [`RestoreShell`]s for every saved window.
    /// - `Ask`: populates `pending_offer` when `snapshot.windows` is non-empty.
    /// - `Never`: ignores saved windows.
    #[must_use]
    pub fn from_snapshot(snapshot: SavedWindowsSnapshot, screen: ScreenBounds) -> Self {
        let mut mgr = Self {
            policy: snapshot.policy,
            pending_offer: None,
            shells: Vec::new(),
            dead_shells: Vec::new(),
        };
        if snapshot.windows.is_empty() {
            return mgr;
        }
        match snapshot.policy {
            RestoreWindowsPolicy::Always => {
                mgr.spawn_shells(&snapshot.windows, screen);
            }
            RestoreWindowsPolicy::Ask => {
                mgr.pending_offer = Some(RestoreWindowsOffer {
                    windows: snapshot.windows,
                });
            }
            RestoreWindowsPolicy::Never => {}
        }
        mgr
    }

    /// Spawns [`RestoreShell`] placeholders for `entries`.
    pub fn spawn_shells(&mut self, entries: &[SavedWindowEntry], screen: ScreenBounds) {
        for entry in entries {
            let shell = RestoreShell::new(
                entry.id,
                entry.title.clone(),
                entry.peer_id,
                entry.position.into(),
                screen,
            );
            self.shells.push(shell);
        }
    }

    /// Applies the user's choice on the startup restore offer, returning an optional toast
    /// message (`lng_restore_windows_enabled_toast` / `lng_restore_windows_disabled_toast`).
    pub fn apply_offer_action(
        &mut self,
        action: RestoreWindowsAction,
        screen: ScreenBounds,
    ) -> Option<String> {
        let offer = self.pending_offer.take()?;
        match action {
            RestoreWindowsAction::RestoreOnce => {
                self.spawn_shells(&offer.windows, screen);
                None
            }
            RestoreWindowsAction::Always => {
                self.policy = RestoreWindowsPolicy::Always;
                self.spawn_shells(&offer.windows, screen);
                Some(tr::lng_restore_windows_enabled_toast())
            }
            RestoreWindowsAction::Never => {
                self.policy = RestoreWindowsPolicy::Never;
                Some(tr::lng_restore_windows_disabled_toast())
            }
            RestoreWindowsAction::Dismiss => None,
        }
    }

    /// Marks shell `id` unavailable and moves it to `dead_shells` (matching `showUnavailable`
    /// in `window_saved_windows.cpp:1208-1227`).
    pub fn mark_shell_unavailable(&mut self, id: u64) -> bool {
        if let Some(pos) = self.shells.iter().position(|s| s.id == id) {
            let mut shell = self.shells.remove(pos);
            shell.show_unavailable();
            self.dead_shells.push(shell);
            true
        } else {
            false
        }
    }

    /// Closes a shell (live or dead) and removes it from tracking.
    pub fn close_shell(&mut self, id: u64) -> bool {
        if let Some(pos) = self.shells.iter().position(|s| s.id == id) {
            self.shells[pos].close();
            self.shells.remove(pos);
            return true;
        }
        if let Some(pos) = self.dead_shells.iter().position(|s| s.id == id) {
            self.dead_shells[pos].close();
            self.dead_shells.remove(pos);
            return true;
        }
        false
    }

    /// Builds a [`SavedWindowsSnapshot`] capturing the current policy and open shells.
    #[must_use]
    pub fn to_snapshot(
        &self,
        scale: i32,
        screen_origin: (i32, i32),
        moncrc: i32,
    ) -> SavedWindowsSnapshot {
        let windows = self
            .shells
            .iter()
            .filter(|s| s.status != RestoreShellStatus::Closed)
            .map(|s| SavedWindowEntry {
                id: s.id,
                title: s.title.clone(),
                account_index: 0,
                peer_id: s.peer_id,
                position: s
                    .count_position_for_save(scale, screen_origin, moncrc)
                    .into(),
            })
            .collect();
        SavedWindowsSnapshot {
            policy: self.policy,
            windows,
        }
    }

    /// Renders the startup `"Restore windows?"` offer banner if present, returning any
    /// [`RestoreWindowsAction`] triggered by clicking Restore / Always / Never / Close.
    pub fn render_offer_banner(&self, ui: &mut Ui) -> Option<RestoreWindowsAction> {
        let offer = self.pending_offer.as_ref()?;
        let mut chosen = None;
        Frame::NONE
            .fill(ui.visuals().faint_bg_color)
            .stroke(Stroke::new(
                1.0,
                ui.visuals().widgets.noninteractive.bg_stroke.color,
            ))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong(offer.question());
                    ui.label(format!("({} saved)", offer.windows.len()));
                    if ui.button(tr::lng_restore_windows_restore()).clicked() {
                        chosen = Some(RestoreWindowsAction::RestoreOnce);
                    }
                    if ui.button(tr::lng_restore_windows_always()).clicked() {
                        chosen = Some(RestoreWindowsAction::Always);
                    }
                    if ui.button(tr::lng_restore_windows_never()).clicked() {
                        chosen = Some(RestoreWindowsAction::Never);
                    }
                });
            });
        chosen
    }
}
