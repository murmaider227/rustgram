//! Screen-sharing source enumeration (Windows monitors and visible windows) and picker UI.
//!
//! Port origin: `Telegram/SourceFiles/calls/group/calls_group_choose_screen.cpp` and
//! `tg_calls` / `tg-voip` desktop capturer source selection.

use egui::{CornerRadius, RichText, Ui, Vec2};

use crate::calls::theme::CallPalette;

pub use tg_app_core::core_types::screen_picker::*;

/// Modal dialog for selecting a monitor or window to share during a 1:1 or group call.
#[derive(Debug, Default)]
pub struct ScreenSharePickerDialog {
    /// Whether the picker window is currently visible.
    pub open: bool,
    /// Cached list of enumerated sources.
    pub sources: Vec<ScreenShareSource>,
    /// Currently highlighted source ID.
    pub selected_id: Option<String>,
}

impl ScreenSharePickerDialog {
    /// Opens the picker dialog populated with `sources`.
    pub fn open_with_sources(&mut self, sources: Vec<ScreenShareSource>) {
        self.selected_id = sources.first().map(|s| s.id.clone());
        self.sources = sources;
        self.open = true;
    }

    /// Closes the picker dialog without starting capture.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Renders the screen-sharing source picker dialog and returns `Some(source)` when the user
    /// confirms sharing.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        id_salt: &str,
        palette: &CallPalette,
        refresh_sources: impl FnOnce() -> Vec<ScreenShareSource>,
    ) -> Option<ScreenShareSource> {
        if !self.open {
            return None;
        }
        let mut chosen: Option<ScreenShareSource> = None;
        let mut keep_open = self.open;
        let mut close_now = false;
        let mut do_refresh = false;

        egui::Window::new("Share Screen")
            .id(egui::Id::unique(id_salt))
            .collapsible(false)
            .resizable(false)
            .fixed_size(Vec2::new(420.0, 440.0))
            .open(&mut keep_open)
            .frame(
                egui::Frame::new()
                    .fill(palette.group_members_bg)
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Choose a screen or window to share")
                            .size(13.0)
                            .color(palette.group_members_fg),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Refresh").clicked() {
                            do_refresh = true;
                        }
                    });
                });
                ui.add_space(8.0);

                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        let displays: Vec<&ScreenShareSource> = self
                            .sources
                            .iter()
                            .filter(|s| s.kind == ScreenSourceKind::Display)
                            .collect();
                        if !displays.is_empty() {
                            ui.label(
                                RichText::new("Screens")
                                    .strong()
                                    .size(12.5)
                                    .color(palette.group_active_fg),
                            );
                            ui.add_space(4.0);
                            for src in displays {
                                render_source_row(ui, src, &mut self.selected_id, palette);
                            }
                            ui.add_space(10.0);
                        }

                        let windows: Vec<&ScreenShareSource> = self
                            .sources
                            .iter()
                            .filter(|s| s.kind == ScreenSourceKind::Window)
                            .collect();
                        if !windows.is_empty() {
                            ui.label(
                                RichText::new("Windows")
                                    .strong()
                                    .size(12.5)
                                    .color(palette.group_active_fg),
                            );
                            ui.add_space(4.0);
                            for src in windows {
                                render_source_row(ui, src, &mut self.selected_id, palette);
                            }
                        }
                    });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let selected = self
                            .selected_id
                            .as_deref()
                            .and_then(|id| self.sources.iter().find(|s| s.id == id))
                            .cloned();
                        if ui
                            .add_enabled(selected.is_some(), egui::Button::new("Share"))
                            .clicked()
                            && let Some(src) = selected
                        {
                            chosen = Some(src);
                            close_now = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close_now = true;
                        }
                    });
                });
            });

        if do_refresh {
            self.sources = refresh_sources();
            if self
                .selected_id
                .as_deref()
                .is_none_or(|id| !self.sources.iter().any(|s| s.id == id))
            {
                self.selected_id = self.sources.first().map(|s| s.id.clone());
            }
        }

        self.open = keep_open && !close_now;
        chosen
    }
}

fn render_source_row(
    ui: &mut Ui,
    source: &ScreenShareSource,
    selected_id: &mut Option<String>,
    palette: &CallPalette,
) {
    let is_selected = selected_id.as_deref() == Some(source.id.as_str());
    let bg = if is_selected {
        palette.group_members_bg_over
    } else {
        palette.group_bg
    };
    let resp = egui::Frame::new()
        .fill(bg)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .stroke(if is_selected {
            egui::Stroke::new(1.5, palette.group_active_fg)
        } else {
            egui::Stroke::NONE
        })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                let badge = match source.kind {
                    ScreenSourceKind::Display => "[Monitor]",
                    ScreenSourceKind::Window => "[Window]",
                };
                ui.label(
                    RichText::new(badge)
                        .size(11.0)
                        .color(palette.group_active_fg),
                );
                ui.label(
                    RichText::new(&source.name)
                        .size(12.5)
                        .color(palette.group_members_fg),
                );
                let res = source.resolution_label();
                if !res.is_empty() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(res)
                                .size(11.0)
                                .color(palette.group_member_sub_fg),
                        );
                    });
                }
            });
        })
        .response
        .interact(egui::Sense::click());
    if resp.clicked() {
        *selected_id = Some(source.id.clone());
    }
    ui.add_space(4.0);
}
