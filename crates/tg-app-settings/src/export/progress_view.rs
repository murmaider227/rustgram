//! Export progress panel widget with animated step/substep rows, delayed skip-file link,
//! stop button, and completion "Show my data" action.
//!
//! Port of `Telegram/SourceFiles/export/view/export_view_progress.h` and
//! `Telegram/SourceFiles/export/view/export_view_progress.cpp`.

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Ui, Vec2};
use tg_ui::anim::sine_in_out;
use tg_ui::style::{LinkButtonStyle, RoundButtonStyle};
use tg_ui::theme::Colors;
use tg_ui::widgets::buttons::{LinkButton, RoundButton};

use crate::export::content::{Content, ContentRow};
use tg_app_core::stand_in::theme::Palette;

/// Delay before showing the `"Skip this file"` link on a slow download (`kShowSkipFileTimeout = 5s`).
pub const SHOW_SKIP_FILE_TIMEOUT_SECS: f64 = 5.0;

/// Progress bar interpolation duration (`st::exportProgressDuration = 200ms`).
pub const PROGRESS_ANIM_DURATION_SECS: f64 = 0.2;

/// Height of one progress row (`st::exportProgressRowHeight = 30px`).
const ROW_HEIGHT: f32 = 30.0;

/// Vertical spacing between progress rows (`st::exportProgressRowSkip = 10px`).
const ROW_SKIP: f32 = 10.0;

/// Progress bar thickness (`st::exportProgressWidth = 3px`).
const PROGRESS_THICKNESS: f32 = 3.0;

/// Actions emitted by [`ProgressView::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressViewAction {
    /// User clicked `"Skip this file"` for the file with the given `random_id`.
    SkipFile(u64),
    /// User clicked `"Stop"` to request cancellation (triggers confirmation box).
    RequestCancel,
    /// User clicked `"Show my data"` after completion.
    ShowMyData,
}

/// Per-row progress bar animation state (port of `ProgressWidget::Row::Instance`).
#[derive(Debug, Clone)]
struct RowAnimState {
    row_id: String,
    target: f64,
    from: f64,
    started_at: f64,
    duration: f64,
}

impl Default for RowAnimState {
    fn default() -> Self {
        Self {
            row_id: String::new(),
            target: 0.0,
            from: 0.0,
            started_at: 0.0,
            duration: 0.0,
        }
    }
}

impl RowAnimState {
    fn update(&mut self, row: &ContentRow, now: f64) {
        let clamped = row.progress.clamp(0.0, 1.0);
        if self.row_id != row.id {
            self.row_id = row.id.clone();
            self.from = clamped;
            self.target = clamped;
            self.started_at = now;
            self.duration = 0.0;
            return;
        }
        if clamped > self.target {
            self.from = self.shown_progress(now);
            self.target = clamped;
            self.started_at = now;
            self.duration = PROGRESS_ANIM_DURATION_SECS;
        } else if clamped < self.target {
            self.from = clamped;
            self.target = clamped;
            self.duration = 0.0;
        }
    }

    fn shown_progress(&self, now: f64) -> f64 {
        if self.duration <= 0.0 || now - self.started_at >= self.duration {
            return self.target;
        }
        let dt = ((now - self.started_at) / self.duration).clamp(0.0, 1.0);
        (self.from + sine_in_out(self.target - self.from, dt)).clamp(0.0, 1.0)
    }

    fn is_animating(&self, now: f64) -> bool {
        self.duration > 0.0 && now - self.started_at < self.duration
    }
}

/// Export progress and completion screen widget (`Export::View::ProgressWidget`).
#[derive(Debug, Clone, Default)]
pub struct ProgressView {
    content: Content,
    row_anims: Vec<RowAnimState>,
    active_file_random_id: u64,
    file_started_at: Option<f64>,
    done: bool,
}

impl ProgressView {
    /// Creates a new `ProgressView` initialized with `content`.
    #[must_use]
    pub fn new(content: Content) -> Self {
        let mut view = Self::default();
        view.update_content(content, 0.0);
        view
    }

    /// Returns the currently displayed [`Content`].
    #[must_use]
    pub fn content(&self) -> &Content {
        &self.content
    }

    /// Returns `true` once the done state has been reached.
    #[must_use]
    pub const fn is_done(&self) -> bool {
        self.done
    }

    /// Returns `true` if the `"Skip this file"` link should be visible at timestamp `now_secs`.
    #[must_use]
    pub fn should_show_skip_file(&self, now_secs: f64) -> bool {
        if self.done || self.active_file_random_id == 0 {
            return false;
        }
        self.file_started_at
            .is_some_and(|start| now_secs - start >= SHOW_SKIP_FILE_TIMEOUT_SECS)
    }

    /// Updates progress rows and file skip-timer state at timestamp `now_secs`.
    pub fn update_content(&mut self, content: Content, now_secs: f64) {
        if content.is_done() {
            self.done = true;
            self.active_file_random_id = 0;
            self.file_started_at = None;
        } else {
            let file_id = content.active_file_random_id();
            if self.active_file_random_id != file_id {
                self.active_file_random_id = file_id;
                self.file_started_at = if file_id != 0 { Some(now_secs) } else { None };
            }
        }

        if self.row_anims.len() < content.rows.len() {
            self.row_anims
                .resize_with(content.rows.len(), RowAnimState::default);
        }
        for (idx, row) in content.rows.iter().enumerate() {
            self.row_anims[idx].update(row, now_secs);
        }
        self.content = content;
    }

    /// Renders the progress or done view inside `ui` and returns any clicked action.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        palette: &Palette,
        colors: &dyn Colors,
        now_secs: f64,
    ) -> Option<ProgressViewAction> {
        let mut action = None;
        let full_rect = ui.available_rect_before_wrap();
        let pad_x = 22.0;
        let inner_w = (full_rect.width() - pad_x * 2.0).max(160.0);

        // Request repaint while any row bar animates or while counting toward the 5s skip timer.
        if self.row_anims.iter().any(|a| a.is_animating(now_secs))
            || (!self.done
                && self.active_file_random_id != 0
                && !self.should_show_skip_file(now_secs))
        {
            ui.ctx().request_repaint();
        }

        // Top section: progress rows + skip link + explanatory text.
        ui.vertical(|ui| {
            ui.add_space(14.0);

            for (idx, row) in self.content.rows.iter().enumerate() {
                if idx > 0 {
                    ui.add_space(ROW_SKIP);
                }
                let shown_progress = self
                    .row_anims
                    .get(idx)
                    .map_or(row.progress, |anim| anim.shown_progress(now_secs));
                ui.horizontal(|ui| {
                    ui.add_space(pad_x);
                    paint_progress_row(ui, inner_w, row, shown_progress, palette);
                });
            }

            // Reserved line for `"Skip this file"` link (`export_view_progress.cpp:301-311`).
            ui.add_space(10.0);
            let show_skip = self.should_show_skip_file(now_secs);
            ui.horizontal(|ui| {
                ui.add_space(pad_x);
                if show_skip {
                    let link_style = LinkButtonStyle::default_link(colors);
                    if LinkButton::new("Skip this file", link_style)
                        .show(ui)
                        .clicked()
                    {
                        action = Some(ProgressViewAction::SkipFile(self.active_file_random_id));
                    }
                } else {
                    ui.allocate_exact_size(Vec2::new(1.0, 20.0), egui::Sense::hover());
                }
            });

            // About / completion explanation (`lng_export_progress` vs `lng_export_about_done`).
            ui.add_space(10.0);
            let about_text = if self.done {
                "Your data was successfully exported."
            } else {
                "You can close this window now. Please don't quit Telegram until the data export is completed."
            };
            ui.horizontal(|ui| {
                ui.add_space(pad_x);
                ui.vertical(|ui| {
                    ui.set_max_width(inner_w);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(about_text)
                                .size(13.5)
                                .color(palette.window_sub_text_fg),
                        )
                        .wrap(),
                    );
                });
            });
        });

        // Bottom centered action button (`exportCancelBottom = 30px`, 200x44px).
        let btn_w = 200.0_f32.min((full_rect.width() - 60.0).max(140.0));
        let btn_h = 44.0;
        let btn_y = (full_rect.max.y - 30.0 - btn_h).max(ui.cursor().min.y + 18.0);
        let btn_rect = Rect::from_min_size(
            Pos2::new(full_rect.center().x - btn_w * 0.5, btn_y),
            Vec2::new(btn_w, btn_h),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(btn_rect), |ui| {
            if self.done {
                let mut st = RoundButtonStyle::default_active(colors);
                st.height = btn_h;
                st.full_radius = true;
                st.style.font.size = 15.0;
                st.style.font.semibold = true;
                if RoundButton::new("Show my data", st)
                    .full_width(btn_w)
                    .full_radius(true)
                    .show(ui)
                    .clicked()
                {
                    action = Some(ProgressViewAction::ShowMyData);
                }
            } else {
                let mut st = RoundButtonStyle::attention_box(colors);
                st.height = btn_h;
                st.style.font.size = 15.0;
                st.style.font.semibold = true;
                if RoundButton::new("Stop", st)
                    .full_width(btn_w)
                    .show(ui)
                    .clicked()
                {
                    action = Some(ProgressViewAction::RequestCancel);
                }
            }
        });

        action
    }
}

/// Paints a single progress row (`ProgressWidget::Row::paintEvent` in `export_view_progress.cpp`).
fn paint_progress_row(ui: &mut Ui, width: f32, row: &ContentRow, progress: f64, palette: &Palette) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, ROW_HEIGHT), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let painter = ui.painter();
    let bar_top = rect.max.y - PROGRESS_THICKNESS;

    // Subtle track shadow line (`st::shadowFg`) always painted even for empty placeholder rows.
    painter.rect_filled(
        Rect::from_min_size(
            Pos2::new(rect.min.x, bar_top),
            Vec2::new(rect.width(), PROGRESS_THICKNESS),
        ),
        CornerRadius::same(1),
        palette.divider,
    );

    if row.id.is_empty() {
        return;
    }

    // Right info label (`st::exportProgressInfoLabel`).
    let info_w = if row.info.is_empty() {
        0.0
    } else {
        let info_galley = painter.layout_no_wrap(
            row.info.clone(),
            FontId::proportional(13.5),
            palette.window_sub_text_fg,
        );
        let w = info_galley.size().x;
        painter.galley(
            Pos2::new(rect.max.x - w, rect.min.y + 1.0),
            info_galley,
            palette.window_sub_text_fg,
        );
        w + 8.0
    };

    // Left bold label (`st::exportProgressLabel`), clipped so it never overlaps right info.
    let label_max_w = (rect.width() - info_w).max(20.0);
    let label_clip = Rect::from_min_size(rect.min, Vec2::new(label_max_w, 22.0));
    painter.with_clip_rect(label_clip).text(
        Pos2::new(rect.min.x, rect.min.y + 1.0),
        Align2::LEFT_TOP,
        &row.label,
        FontId::proportional(14.0),
        palette.window_bold_fg,
    );

    // Active/inactive progress bar (`st::exportProgressFg` / `st::exportProgressBg`).
    let bar_bg = Color32::from_rgba_unmultiplied(
        palette.window_bg_active.r(),
        palette.window_bg_active.g(),
        palette.window_bg_active.b(),
        58,
    );
    let filled_w = (rect.width() * progress.clamp(0.0, 1.0) as f32).round();
    if filled_w < rect.width() {
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(rect.min.x + filled_w, bar_top),
                Vec2::new((rect.width() - filled_w).max(0.0), PROGRESS_THICKNESS),
            ),
            CornerRadius::same(1),
            bar_bg,
        );
    }
    if filled_w > 0.0 {
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(rect.min.x, bar_top),
                Vec2::new(filled_w.min(rect.width()), PROGRESS_THICKNESS),
            ),
            CornerRadius::same(1),
            palette.window_bg_active,
        );
    }
}
