//! Export progress top bar displayed while an export runs in the background.
//!
//! Port of `Telegram/SourceFiles/export/view/export_view_top_bar.h` and
//! `Telegram/SourceFiles/export/view/export_view_top_bar.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};

use crate::export::content::Content;
use tg_app_core::stand_in::theme::Palette;

/// Height of the export top bar (`st::mediaPlayerHeight + st::lineWidth` = 36 + 1 = 37px).
pub const TOP_BAR_HEIGHT: f32 = 37.0;

/// Thickness of the bottom progress strip (`st::mediaPlayerPlayback.fullWidth` = 3px).
const TOP_BAR_PROGRESS_HEIGHT: f32 = 3.0;

/// Title prefix shown on the left (`tr::lng_export_progress_title` + `" –"`).
const TOP_BAR_TITLE_PREFIX: &str = "Exporting your data \u{2013}";

/// Persistent top-bar summary widget shown across the main window while export runs.
///
/// Clicking anywhere on the bar activates / restores the export panel.
#[derive(Debug, Clone, Default)]
pub struct ExportTopBar {
    /// Left bold prefix (`"Exporting your data –"`).
    pub left_text: String,
    /// Middle step label (e.g. `"Chats"`, `"Profile photos"`, `"Initializing..."`).
    pub middle_text: String,
    /// Right counter / info text (e.g. `"2 / 5"`).
    pub right_text: String,
    /// Overall progress in `[0.0, 1.0]`.
    pub progress: f64,
    /// Whether the bottom divider shadow and progress bar are visible.
    pub show_shadow: bool,
}

impl ExportTopBar {
    /// Creates a new `ExportTopBar` initialized from `content`.
    #[must_use]
    pub fn new(content: &Content) -> Self {
        let mut bar = Self {
            left_text: TOP_BAR_TITLE_PREFIX.to_owned(),
            middle_text: String::new(),
            right_text: String::new(),
            progress: 0.0,
            show_shadow: true,
        };
        bar.update_data(content);
        bar
    }

    /// Updates displayed text and progress from the primary row (`content.rows[0]`).
    pub fn update_data(&mut self, content: &Content) {
        let Some(row) = content.rows.first() else {
            return;
        };
        self.left_text = TOP_BAR_TITLE_PREFIX.to_owned();
        self.middle_text = row.label.clone();
        self.right_text = row.info.clone();
        self.progress = row.progress.clamp(0.0, 1.0);
    }

    /// Renders the top bar inside `ui` and returns `true` if clicked to open the export panel.
    pub fn show(&self, ui: &mut Ui, palette: &Palette) -> bool {
        let width = ui.available_width().max(120.0);
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(width, TOP_BAR_HEIGHT), Sense::click());

        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let bg = if response.hovered() {
                palette.window_bg_over
            } else {
                palette.window_bg
            };
            painter.rect_filled(rect, CornerRadius::ZERO, bg);

            // Layout matching `TopBar::resizeToWidthInfo` in `export_view_top_bar.cpp`.
            let padding = 16.0;
            let spacing = 5.0;
            let center_y = rect.min.y + (TOP_BAR_HEIGHT - TOP_BAR_PROGRESS_HEIGHT) * 0.5;

            let left_font = FontId::proportional(13.0);
            let left_galley =
                painter.layout_no_wrap(self.left_text.clone(), left_font, palette.window_bold_fg);
            let left_w = left_galley.size().x;
            let left_pos = Pos2::new(rect.min.x + padding, center_y - left_galley.size().y * 0.5);
            painter.galley(left_pos, left_galley, palette.window_bold_fg);

            let right_font = FontId::proportional(13.0);
            let right_galley = if self.right_text.is_empty() {
                None
            } else {
                Some(painter.layout_no_wrap(
                    self.right_text.clone(),
                    right_font,
                    palette.window_sub_text_fg,
                ))
            };
            let right_w = right_galley.as_ref().map_or(0.0, |g| g.size().x);

            let middle_x = rect.min.x + padding + left_w + spacing;
            let right_reserved = if right_w > 0.0 {
                right_w + spacing
            } else {
                0.0
            };
            let available_mid = (rect.max.x - padding - right_reserved - middle_x).max(0.0);

            let middle_font = FontId::proportional(13.0);
            let middle_display = elide_middle_to_fit(
                painter,
                &self.middle_text,
                middle_font.clone(),
                available_mid,
            );
            let middle_galley =
                painter.layout_no_wrap(middle_display, middle_font, palette.window_fg);
            let middle_w = middle_galley.size().x;
            let middle_pos = Pos2::new(middle_x, center_y - middle_galley.size().y * 0.5);
            painter.galley(middle_pos, middle_galley, palette.window_fg);

            if let Some(rg) = right_galley {
                let right_x = (middle_x + middle_w + spacing).min(rect.max.x - padding - right_w);
                let right_pos = Pos2::new(right_x.max(middle_x), center_y - rg.size().y * 0.5);
                painter.galley(right_pos, rg, palette.window_sub_text_fg);
            }

            if self.show_shadow {
                // 1px bottom divider hairline.
                painter.line_segment(
                    [
                        Pos2::new(rect.min.x, rect.max.y - 0.5),
                        Pos2::new(rect.max.x, rect.max.y - 0.5),
                    ],
                    Stroke::new(1.0, palette.divider),
                );

                // Bottom progress strip (`st::mediaPlayerPlayback`).
                let bar_y = rect.max.y - TOP_BAR_PROGRESS_HEIGHT;
                let fill_w = (rect.width() * self.progress as f32).clamp(0.0, rect.width());
                let inactive_bg = Color32::from_rgba_unmultiplied(
                    palette.window_bg_active.r(),
                    palette.window_bg_active.g(),
                    palette.window_bg_active.b(),
                    54,
                );
                painter.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(rect.min.x, bar_y),
                        Vec2::new(rect.width(), TOP_BAR_PROGRESS_HEIGHT),
                    ),
                    CornerRadius::ZERO,
                    inactive_bg,
                );
                if fill_w > 0.0 {
                    painter.rect_filled(
                        Rect::from_min_size(
                            Pos2::new(rect.min.x, bar_y),
                            Vec2::new(fill_w, TOP_BAR_PROGRESS_HEIGHT),
                        ),
                        CornerRadius::ZERO,
                        palette.window_bg_active,
                    );
                }
            }

            // Subtle right-side hint indicator on hover.
            if response.hovered() && rect.width() > 260.0 {
                painter.text(
                    Pos2::new(rect.max.x - 10.0, center_y),
                    Align2::RIGHT_CENTER,
                    "\u{2197}",
                    FontId::proportional(11.0),
                    palette.window_active_text_fg,
                );
            }
        }

        response.clicked()
    }
}

/// Elides `text` from the middle (`"abc…xyz"`) when it exceeds `max_width` pixels.
fn elide_middle_to_fit(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    max_width: f32,
) -> String {
    if text.is_empty() || max_width <= 8.0 {
        return String::new();
    }
    let full_w = painter
        .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    if full_w <= max_width {
        return text.to_owned();
    }

    if text.chars().count() <= 3 {
        return "\u{2026}".to_owned();
    }
    tg_ui::elide::elide_middle_by(text, |s| {
        tg_ui::elide::fits_width(painter, s, &font, max_width)
    })
}
