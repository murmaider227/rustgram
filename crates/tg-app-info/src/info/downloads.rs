//! Downloads Manager section (`Info::Downloads`).
//!
//! Port origin: `Telegram/SourceFiles/info/downloads/info_downloads_widget.cpp`,
//! `Telegram/SourceFiles/info/downloads/info_downloads_inner_widget.cpp`,
//! and `Telegram/SourceFiles/info/downloads/info_downloads_provider.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Role, Sense, Stroke, Ui, Vec2,
    WidgetInfo,
};
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::stand_in::{Colors, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::input_field::InputField;

use crate::info::backend::{DownloadEntry, DownloadStatus};
use crate::info::values::format_file_size;
use tg_app_core::stand_in::types::PeerId;

/// Events emitted by [`DownloadsSection::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadsAction {
    /// Pause or resume the given download entry.
    TogglePause(u64),
    /// Cancel an active download or remove an entry from the list.
    Delete {
        /// Download identifier.
        download_id: u64,
        /// Whether to delete the local file if completed.
        delete_file: bool,
    },
    /// Jump to the source chat message for a downloaded file.
    OpenSourceChat(PeerId),
    /// Clear all completed downloads from the list.
    ClearCompleted,
    /// Delete all downloads from the list.
    DeleteAll,
}

/// Renders the Downloads Manager Info section (`Info::Downloads`).
pub struct DownloadsSection;

impl DownloadsSection {
    /// Renders search box, bulk controls, and date-grouped download rows.
    pub fn show(
        ui: &mut Ui,
        entries: &[DownloadEntry],
        search_query: &mut String,
        colors: &dyn Colors,
    ) -> Option<DownloadsAction> {
        let mut action = None;

        // Search input + summary toolbar
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            let input_w = (ui.available_width() - 14.0).max(120.0);
            ui.allocate_ui(Vec2::new(input_w, 34.0), |ui| {
                InputField::single_line(
                    "info_downloads_search",
                    search_query,
                    "Search downloads",
                    InputFieldStyle::default_input(colors),
                )
                .show(ui);
            });
        });

        let active_count = entries
            .iter()
            .filter(|e| {
                matches!(
                    e.status,
                    DownloadStatus::Downloading { .. } | DownloadStatus::Paused { .. }
                )
            })
            .count();
        let completed_count = entries
            .iter()
            .filter(|e| matches!(e.status, DownloadStatus::Completed))
            .count();

        if !entries.is_empty() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                let summary = format!("{} file(s) · {} active", entries.len(), active_count);
                ui.label(
                    egui::RichText::new(summary)
                        .font(FontId::new(12.0, family_regular()))
                        .color(colors.color("windowSubTextFg")),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(14.0);
                    if small_pill_button(
                        ui,
                        "dl_delete_all",
                        "Delete All",
                        colors.color("attentionButtonFg"),
                        colors,
                    ) {
                        action = Some(DownloadsAction::DeleteAll);
                    }
                    if completed_count > 0 {
                        ui.add_space(6.0);
                        if small_pill_button(
                            ui,
                            "dl_clear_done",
                            "Clear Completed",
                            colors.color("windowActiveTextFg"),
                            colors,
                        ) {
                            action = Some(DownloadsAction::ClearCompleted);
                        }
                    }
                });
            });
        }

        if entries.is_empty() {
            ui.add_space(32.0);
            ui.vertical_centered(|ui| {
                let empty_msg = if search_query.trim().is_empty() {
                    "No downloaded files yet."
                } else {
                    "No downloads match your search."
                };
                ui.label(
                    egui::RichText::new(empty_msg)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                );
            });
            ui.add_space(32.0);
            return action;
        }

        let mut last_group = "";
        for entry in entries {
            if entry.date_group != last_group {
                last_group = entry.date_group.as_str();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new(last_group)
                            .font(FontId::new(12.0, family_semibold()))
                            .color(colors.color("windowSubTextFg")),
                    );
                });
                ui.add_space(2.0);
            }
            if let Some(row_act) = download_row(ui, entry, colors) {
                action = Some(row_act);
            }
        }

        ui.add_space(14.0);
        action
    }
}

fn download_row(
    ui: &mut Ui,
    entry: &DownloadEntry,
    colors: &dyn Colors,
) -> Option<DownloadsAction> {
    let mut action = None;
    let width = ui.available_width().max(220.0);
    let row_h = 66.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::hover());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
    }

    // Left file extension badge circle
    let badge_rect = Rect::from_center_size(
        Pos2::new(rect.min.x + 16.0 + 21.0, rect.center().y),
        Vec2::splat(42.0),
    );
    let badge_bg = match entry.status {
        DownloadStatus::Downloading { .. } => colors.color("activeButtonBg"),
        DownloadStatus::Paused { .. } => Color32::from_rgb(0xE0, 0x91, 0x2F),
        DownloadStatus::Completed => Color32::from_rgb(0x4E, 0xAE, 0x51),
        DownloadStatus::Failed { .. } => colors.color("attentionButtonFg"),
    };
    ui.painter()
        .circle_filled(badge_rect.center(), 21.0, badge_bg);
    ui.painter().text(
        badge_rect.center(),
        Align2::CENTER_CENTER,
        entry.ext_badge(),
        FontId::new(10.5, family_semibold()),
        Color32::WHITE,
    );

    // Right action buttons
    let del_rect = Rect::from_center_size(
        Pos2::new(rect.max.x - 36.0, rect.center().y),
        Vec2::new(52.0, 26.0),
    );
    let del_label = match entry.status {
        DownloadStatus::Downloading { .. } | DownloadStatus::Paused { .. } => "Cancel",
        DownloadStatus::Completed | DownloadStatus::Failed { .. } => "Delete",
    };
    let del_resp = ui.interact(
        del_rect,
        ui.make_persistent_id(("dl_row_del", entry.id)),
        Sense::click(),
    );
    del_resp.widget_info(|| {
        WidgetInfo::labeled(
            Role::Button,
            true,
            format!("{del_label} {}", entry.file_name),
        )
    });
    if del_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().rect_filled(
            del_rect,
            CornerRadius::same(6),
            colors.color("windowBgRipple"),
        );
    }
    ui.painter().text(
        del_rect.center(),
        Align2::CENTER_CENTER,
        del_label,
        FontId::new(11.5, family_semibold()),
        colors.color("attentionButtonFg"),
    );
    if del_resp.clicked() {
        action = Some(DownloadsAction::Delete {
            download_id: entry.id,
            delete_file: matches!(entry.status, DownloadStatus::Completed),
        });
    }

    let mut content_right = del_rect.min.x - 6.0;
    let pause_label = match entry.status {
        DownloadStatus::Downloading { .. } => Some("Pause"),
        DownloadStatus::Paused { .. } => Some("Resume"),
        DownloadStatus::Failed { .. } => Some("Retry"),
        DownloadStatus::Completed => None,
    };
    if let Some(plabel) = pause_label {
        let p_rect = Rect::from_center_size(
            Pos2::new(content_right - 28.0, rect.center().y),
            Vec2::new(56.0, 26.0),
        );
        let p_resp = ui.interact(
            p_rect,
            ui.make_persistent_id(("dl_row_pause", entry.id)),
            Sense::click(),
        );
        p_resp.widget_info(|| {
            WidgetInfo::labeled(Role::Button, true, format!("{plabel} {}", entry.file_name))
        });
        if p_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter().rect_filled(
                p_rect,
                CornerRadius::same(6),
                colors.color("windowBgRipple"),
            );
        }
        ui.painter().text(
            p_rect.center(),
            Align2::CENTER_CENTER,
            plabel,
            FontId::new(11.5, family_semibold()),
            colors.color("windowActiveTextFg"),
        );
        if p_resp.clicked() {
            action = Some(DownloadsAction::TogglePause(entry.id));
        }
        content_right = p_rect.min.x - 6.0;
    }

    // Center clickable info area (filename, subtitle, progress bar)
    let text_x = badge_rect.max.x + 12.0;
    let info_rect = Rect::from_min_max(
        Pos2::new(text_x, rect.min.y + 4.0),
        Pos2::new(content_right.max(text_x + 40.0), rect.max.y - 4.0),
    );
    let open_resp = ui.interact(
        info_rect,
        ui.make_persistent_id(("dl_row_open", entry.id)),
        Sense::click(),
    );
    open_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &entry.file_name));
    if open_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if open_resp.clicked() && action.is_none() {
        action = Some(DownloadsAction::OpenSourceChat(entry.peer_id));
    }

    let clip = ui.painter().with_clip_rect(info_rect);
    clip.text(
        Pos2::new(text_x, rect.min.y + 9.0),
        Align2::LEFT_TOP,
        &entry.file_name,
        font_id_from_style(st::semibold_font),
        colors.color("windowBoldFg"),
    );

    let (status_line, progress) = match &entry.status {
        DownloadStatus::Downloading {
            received_bytes,
            total_bytes,
        } => {
            let frac = if *total_bytes > 0 {
                (*received_bytes as f32 / *total_bytes as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (
                format!(
                    "{} / {} · {}",
                    format_file_size(*received_bytes),
                    format_file_size(*total_bytes),
                    entry.peer_title
                ),
                Some(frac),
            )
        }
        DownloadStatus::Paused {
            received_bytes,
            total_bytes,
        } => {
            let frac = if *total_bytes > 0 {
                (*received_bytes as f32 / *total_bytes as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (
                format!(
                    "Paused · {} / {} · {}",
                    format_file_size(*received_bytes),
                    format_file_size(*total_bytes),
                    entry.peer_title
                ),
                Some(frac),
            )
        }
        DownloadStatus::Completed => (
            format!(
                "{} · {}",
                format_file_size(entry.total_bytes),
                entry.peer_title
            ),
            None,
        ),
        DownloadStatus::Failed { reason } => (format!("Failed: {reason}"), None),
    };

    clip.text(
        Pos2::new(text_x, rect.min.y + 29.0),
        Align2::LEFT_TOP,
        status_line,
        FontId::new(12.0, family_regular()),
        colors.color("windowSubTextFg"),
    );

    if let Some(frac) = progress {
        let bar_w = (info_rect.width() - 4.0).max(20.0);
        let bar_rect =
            Rect::from_min_size(Pos2::new(text_x, rect.min.y + 49.0), Vec2::new(bar_w, 4.0));
        clip.rect_filled(
            bar_rect,
            CornerRadius::same(2),
            colors.color("windowBgRipple"),
        );
        let fill_rect = Rect::from_min_size(bar_rect.min, Vec2::new((bar_w * frac).max(2.0), 4.0));
        clip.rect_filled(fill_rect, CornerRadius::same(2), badge_bg);
    }

    ui.painter().line_segment(
        [
            Pos2::new(text_x, rect.max.y),
            Pos2::new(rect.max.x - 14.0, rect.max.y),
        ],
        Stroke::new(0.5, colors.color("shadowFg")),
    );

    action
}

fn small_pill_button(
    ui: &mut Ui,
    id_salt: &str,
    label: &str,
    fg: Color32,
    colors: &dyn Colors,
) -> bool {
    let font = FontId::new(11.5, family_semibold());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, fg);
    let size = Vec2::new(galley.size().x + 14.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), colors.color("windowBgOver"));
    }
    let _ = id_salt;
    ui.painter().galley(
        Pos2::new(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        fg,
    );
    resp.clicked()
}
