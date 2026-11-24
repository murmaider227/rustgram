//! Pre-launch crash recovery screen shown when the previous session exited uncleanly.
//!
//! Port origin: `Telegram/SourceFiles/core/crash_report_window.h` and
//! `Telegram/SourceFiles/core/crash_report_window.cpp` (`LastCrashedWindow`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use egui::{Align, Color32, Frame, Layout, Margin, RichText, ScrollArea, Stroke, Ui, Vec2};
use tg_core::CrashUploadConfig;

use crate::stand_in::theme::Palette;

/// Recommended initial dialog width for the crash recovery screen.
pub const CRASH_WINDOW_WIDTH: f32 = 580.0;

/// Recommended initial dialog height for the crash recovery screen.
pub const CRASH_WINDOW_HEIGHT: f32 = 470.0;

/// User decision emitted by [`CrashRecoveryState::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashRecoveryAction {
    /// Acknowledge the previous crash, reset `tdata/working`, and restart/continue launching Rustgram.
    Continue,
    /// Exit Rustgram without starting the main workspace.
    Quit,
}

/// Structured highlights extracted from a `tg_core::CrashReportManager` text crash dump.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrashSummary {
    /// App version line (`Version: ...`), if present.
    pub version: Option<String>,
    /// Operating system line (`OS: ...`), if present.
    pub os: Option<String>,
    /// Faulting thread line (`Thread: ...`), if present.
    pub thread: Option<String>,
    /// Timestamp line (`CrashedAt: ...`), if present.
    pub crashed_at: Option<String>,
    /// Panic or native exception message (`Panic: ...` / `Exception: ...`), if present.
    pub panic_reason: Option<String>,
    /// Source location (`Location: ...`), if present.
    pub location: Option<String>,
}

impl CrashSummary {
    /// Extracts key fields from a UTF-8 crash report written by `tg_core::CrashReportManager`.
    #[must_use]
    pub fn parse(report_text: &str) -> Self {
        let mut summary = Self::default();
        for line in report_text.lines() {
            if let Some(v) = line.strip_prefix("Version:") {
                summary.version = Some(v.trim().to_owned());
            } else if let Some(v) = line.strip_prefix("OS:") {
                summary.os = Some(v.trim().to_owned());
            } else if let Some(v) = line.strip_prefix("Thread:") {
                summary.thread = Some(v.trim().to_owned());
            } else if let Some(v) = line.strip_prefix("CrashedAt:") {
                summary.crashed_at = Some(v.trim().to_owned());
            } else if let Some(v) = line
                .strip_prefix("Panic:")
                .or_else(|| line.strip_prefix("Exception:"))
            {
                summary.panic_reason = Some(v.trim().to_owned());
            } else if let Some(v) = line.strip_prefix("Location:") {
                summary.location = Some(v.trim().to_owned());
            }
        }
        summary
    }

    /// Returns `true` if no structured metadata fields were found in the dump.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.version.is_none()
            && self.os.is_none()
            && self.thread.is_none()
            && self.crashed_at.is_none()
            && self.panic_reason.is_none()
            && self.location.is_none()
    }
}

/// Interactive state for the crash recovery view (`LastCrashedWindow`).
///
/// Offers local-only crash inspection and recovery options:
/// - Open the crash report folder (`<workdir>/tdata/dumps/`)
/// - Copy the crash report text to the clipboard
/// - Restart / continue launching Rustgram (or Quit)
///
/// Crash upload is configured via [`Self::upload_config`] and defaults to disabled.
#[derive(Debug, Clone)]
pub struct CrashRecoveryState {
    /// Full UTF-8 text decoded from `tdata/working` (empty if killed before writing a dump).
    pub report_text: String,
    /// Extracted key crash annotations for concise display.
    pub summary: CrashSummary,
    /// Directory where timestamped crash dumps are archived (`<working_dir>/tdata/dumps`).
    pub dumps_dir: PathBuf,
    /// Configurable crash upload endpoint (defaults to off; no automatic upload).
    pub upload_config: CrashUploadConfig,
    /// Whether the full crash report text box is expanded.
    pub report_expanded: bool,
    /// Transient confirmation notice after clicking "Copy Report".
    pub copied_notice: bool,
    /// Transient status message (e.g. after opening the crash dumps folder).
    pub status_notice: Option<String>,
}

impl CrashRecoveryState {
    /// Builds a [`CrashRecoveryState`] from raw bytes returned by
    /// [`tg_core::CrashStartResult::PreviousCrash`].
    #[must_use]
    pub fn from_dump(raw_bytes: &[u8], dumps_dir: PathBuf) -> Self {
        Self::from_dump_with_upload_config(raw_bytes, dumps_dir, CrashUploadConfig::disabled())
    }

    /// Builds a [`CrashRecoveryState`] with an explicit [`CrashUploadConfig`].
    #[must_use]
    pub fn from_dump_with_upload_config(
        raw_bytes: &[u8],
        dumps_dir: PathBuf,
        upload_config: CrashUploadConfig,
    ) -> Self {
        let report_text = String::from_utf8_lossy(raw_bytes).trim().to_owned();
        let summary = CrashSummary::parse(&report_text);
        Self {
            report_text,
            summary,
            dumps_dir,
            upload_config,
            report_expanded: false,
            copied_notice: false,
            status_notice: None,
        }
    }

    /// Returns `true` when `tdata/working` contained crash dump text (as opposed to an empty flag
    /// left by a hard kill / power loss).
    #[must_use]
    pub fn has_dump_text(&self) -> bool {
        !self.report_text.is_empty()
    }

    /// Headline matching `LastCrashedWindow` in `core/crash_report_window.cpp:393-397`.
    #[must_use]
    pub fn headline(&self) -> &'static str {
        if self.has_dump_text() {
            "Last time Rustgram crashed"
        } else {
            "Last time Rustgram was not closed properly."
        }
    }

    /// Opens [`Self::dumps_dir`] in the system file manager, creating it first if needed.
    pub fn open_report_folder(&mut self) -> Result<(), String> {
        self.open_report_folder_with(open_folder_in_file_manager)
    }

    /// Opens [`Self::dumps_dir`] using an injectable `opener` callback (for offline unit tests).
    pub fn open_report_folder_with<F>(&mut self, opener: F) -> Result<(), String>
    where
        F: FnOnce(&Path) -> Result<(), String>,
    {
        if let Err(err) = fs::create_dir_all(&self.dumps_dir) {
            let msg = format!("Could not access dumps folder: {err}");
            self.status_notice = Some(msg.clone());
            return Err(msg);
        }
        match opener(&self.dumps_dir) {
            Ok(()) => {
                self.status_notice = Some(format!("Opened {}", self.dumps_dir.display()));
                Ok(())
            }
            Err(err) => {
                self.status_notice = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Renders the crash recovery dialog inside `ui`, returning an action when the user clicks
    /// `"RESTART RUSTGRAM"` or `"QUIT"`.
    pub fn show(&mut self, ui: &mut Ui, palette: &Palette) -> Option<CrashRecoveryAction> {
        let mut action = None;

        Frame::NONE
            .fill(palette.window_bg)
            .inner_margin(Margin::same(24))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());

                ui.label(
                    RichText::new(self.headline())
                        .size(20.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(8.0);

                let subtitle = if self.has_dump_text() {
                    "A crash report was saved locally. No data is uploaded automatically. \
                     You can open the report folder, copy the report, or restart Rustgram."
                } else {
                    "The previous session did not exit cleanly (no crash dump was recorded). \
                     You can open the report folder or restart Rustgram."
                };
                ui.label(
                    RichText::new(subtitle)
                        .size(13.0)
                        .color(palette.window_sub_text_fg),
                );
                ui.add_space(12.0);

                if !self.summary.is_empty() {
                    Frame::NONE
                        .fill(palette.input_bg)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(6.0)
                        .inner_margin(Margin::same(12))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            if let Some(ref panic_reason) = self.summary.panic_reason {
                                ui.label(
                                    RichText::new(format!("Crash: {panic_reason}"))
                                        .size(12.5)
                                        .strong()
                                        .color(palette.attention_fg),
                                );
                            }
                            if let Some(ref loc) = self.summary.location {
                                ui.label(
                                    RichText::new(format!("Location: {loc}"))
                                        .size(12.0)
                                        .monospace()
                                        .color(palette.window_bold_fg),
                                );
                            }
                            if let Some(ref ver) = self.summary.version {
                                ui.label(
                                    RichText::new(format!("Version: {ver}"))
                                        .size(12.0)
                                        .color(palette.window_sub_text_fg),
                                );
                            }
                            if let Some(ref os) = self.summary.os {
                                ui.label(
                                    RichText::new(format!("OS: {os}"))
                                        .size(12.0)
                                        .color(palette.window_sub_text_fg),
                                );
                            }
                            if let Some(ref thread) = self.summary.thread {
                                ui.label(
                                    RichText::new(format!("Thread: {thread}"))
                                        .size(12.0)
                                        .color(palette.window_sub_text_fg),
                                );
                            }
                            if let Some(ref ts) = self.summary.crashed_at {
                                ui.label(
                                    RichText::new(format!("Time: {ts}"))
                                        .size(12.0)
                                        .color(palette.window_sub_text_fg),
                                );
                            }
                        });
                    ui.add_space(10.0);
                }

                ui.label(
                    RichText::new(format!("Crash dumps folder: {}", self.dumps_dir.display()))
                        .size(11.5)
                        .color(palette.window_sub_text_fg),
                );
                let upload_status = if self.upload_config.is_active() {
                    format!(
                        "Crash upload endpoint: {} (manual only)",
                        self.upload_config.endpoint.as_deref().unwrap_or("")
                    )
                } else {
                    "Crash report upload: Off (local only)".to_owned()
                };
                ui.label(
                    RichText::new(upload_status)
                        .size(11.5)
                        .color(palette.window_sub_text_fg),
                );
                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    if ui.button("OPEN REPORT FOLDER").clicked() {
                        let _ = self.open_report_folder();
                    }
                    if self.has_dump_text() {
                        if ui.button("COPY REPORT").clicked() {
                            ui.ctx().copy_text(self.report_text.clone());
                            self.copied_notice = true;
                        }
                        let toggle_label = if self.report_expanded {
                            "HIDE CRASH REPORT"
                        } else {
                            "VIEW CRASH REPORT"
                        };
                        if ui.button(toggle_label).clicked() {
                            self.report_expanded = !self.report_expanded;
                        }
                    }
                });

                if self.copied_notice {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Crash report copied to clipboard")
                            .size(12.0)
                            .color(palette.window_active_text_fg),
                    );
                }
                if let Some(ref notice) = self.status_notice {
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(notice)
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                    );
                }

                if self.has_dump_text() && self.report_expanded {
                    ui.add_space(8.0);
                    let scroll_h = (ui.available_height() - 64.0).clamp(110.0, 260.0);
                    Frame::NONE
                        .fill(palette.input_bg)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(6.0)
                        .inner_margin(Margin::same(8))
                        .show(ui, |ui| {
                            ScrollArea::both().max_height(scroll_h).show(ui, |ui| {
                                let mut readonly_text = self.report_text.as_str();
                                ui.add(
                                    egui::TextEdit::multiline(&mut readonly_text)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(f32::INFINITY)
                                        .interactive(true),
                                );
                            });
                        });
                }

                // Bottom action row (`QUIT` on left, `RESTART RUSTGRAM` primary button on right).
                ui.with_layout(Layout::bottom_up(Align::RIGHT), |ui| {
                    ui.horizontal(|ui| {
                        let restart_btn = egui::Button::new(
                            RichText::new("RESTART RUSTGRAM")
                                .size(13.0)
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(palette.window_active_text_fg)
                        .min_size(Vec2::new(160.0, 36.0));
                        if ui.add(restart_btn).clicked() {
                            action = Some(CrashRecoveryAction::Continue);
                        }

                        ui.add_space(8.0);
                        let quit_btn = egui::Button::new(
                            RichText::new("QUIT")
                                .size(13.0)
                                .color(palette.window_sub_text_fg),
                        )
                        .min_size(Vec2::new(88.0, 36.0));
                        if ui.add(quit_btn).clicked() {
                            action = Some(CrashRecoveryAction::Quit);
                        }
                    });
                });
            });

        action
    }
}

/// Launches the OS file manager on `dir` (`explorer.exe` on Windows, `open` on macOS, `xdg-open` on Linux).
pub fn open_folder_in_file_manager(dir: &Path) -> Result<(), String> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("explorer.exe");
        c.arg(dir);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(dir);
        c
    };
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(dir);
        c
    };

    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not open folder '{}': {e}", dir.display()))
}
