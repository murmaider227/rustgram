//! Business Opening Hours editor with time zones and per-day interval ranges.
//!
//! Port origin: `Telegram/SourceFiles/settings/business/settings_working_hours.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Sense, Stroke,
    Ui, Vec2,
};

use crate::business::backend::BusinessBackend;
use crate::business::model::{
    IN_NEXT_DAY_MAX, SECONDS_PER_DAY, Timezone, WEEKDAY_NAMES, WorkingHours, WorkingInterval,
    WorkingIntervals, extract_day_intervals, find_closest_timezone_id, format_day_time,
    is_full_open, join_intervals, replace_day_intervals,
};
use crate::business::ui_helpers::{
    BUSINESS_GOLD, SUCCESS_GREEN, business_section_intro, pill_button, primary_button, radio_row,
    section_divider, settings_toggle_row, subsection_title, thin_divider, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Active day interval editor state (`EditDayBox`).
#[derive(Debug, Clone)]
pub struct DayEditorState {
    /// Day index `0..=6` (`0` = Monday .. `6` = Sunday).
    pub day_index: i32,
    /// Draft day-local intervals (`[0, kDay + kInNextDayMax]`).
    pub intervals: WorkingIntervals,
}

/// Interactive view-model for Opening Hours (`settings_working_hours.cpp`).
pub struct WorkingHoursView {
    /// Current draft working hours.
    pub hours: WorkingHours,
    /// Whether "Show Opening Hours" toggle is on.
    pub enabled: bool,
    /// Available timezones sorted by `(utc_offset, name)`.
    pub timezones: Vec<Timezone>,
    /// Open per-day interval editor, if any.
    pub editing_day: Option<DayEditorState>,
    /// Whether the timezone chooser drawer is open.
    pub choosing_timezone: bool,
    /// Search query inside the timezone chooser.
    pub timezone_filter: String,
}

impl WorkingHoursView {
    /// Creates a [`WorkingHoursView`] from saved `hours` and available `timezones`.
    #[must_use]
    pub fn new(mut hours: WorkingHours, mut timezones: Vec<Timezone>) -> Self {
        timezones.sort_by(|a, b| a.utc_offset.cmp(&b.utc_offset).then(a.name.cmp(&b.name)));
        let enabled = hours.is_enabled();
        if !timezones.iter().any(|tz| tz.id == hours.timezone_id)
            && let Some(closest) = find_closest_timezone_id(&timezones, -5 * 3600)
        {
            hours.timezone_id = closest;
        }
        Self {
            hours,
            enabled,
            timezones,
            editing_day: None,
            choosing_timezone: false,
            timezone_filter: String::new(),
        }
    }

    /// Returns the display label `(UTC±H:MM) Name` for the currently chosen timezone.
    #[must_use]
    pub fn selected_timezone_label(&self) -> String {
        self.timezones
            .iter()
            .find(|tz| tz.id == self.hours.timezone_id)
            .map_or_else(|| self.hours.timezone_id.clone(), Timezone::full_name)
    }

    /// Returns the status string shown under weekday `day_index` (`Closed`, `Open 24 hours`,
    /// or comma-separated `HH:MM - HH:MM` ranges).
    #[must_use]
    pub fn day_status_label(&self, day_index: i32) -> (String, bool) {
        let extracted = extract_day_intervals(&self.hours.intervals, day_index);
        if !extracted.is_non_empty() {
            ("Closed".to_owned(), false)
        } else if is_full_open(&extracted) {
            ("Open 24 hours".to_owned(), true)
        } else {
            (join_intervals(&extracted), true)
        }
    }

    /// Toggles `day_index` between Closed and Open 24 hours (matching `toggleButton` in `AddWeekButton`).
    pub fn toggle_day(&mut self, day_index: i32, open: bool) {
        let replacement = if open {
            WorkingIntervals::new(vec![WorkingInterval::new(0, SECONDS_PER_DAY)])
        } else {
            WorkingIntervals::default()
        };
        self.hours.intervals = replace_day_intervals(&self.hours.intervals, day_index, replacement);
        self.hours = self.hours.normalized();
    }

    /// Opens the inline interval editor for `day_index`.
    pub fn open_day_editor(&mut self, day_index: i32) {
        let extracted = extract_day_intervals(&self.hours.intervals, day_index);
        self.editing_day = Some(DayEditorState {
            day_index,
            intervals: extracted,
        });
    }

    /// Commits the open [`DayEditorState`] back into `self.hours`.
    pub fn apply_day_editor(&mut self) {
        if let Some(editor) = self.editing_day.take() {
            self.hours.intervals =
                replace_day_intervals(&self.hours.intervals, editor.day_index, editor.intervals);
            self.hours = self.hours.normalized();
        }
    }

    /// Adds a new interval inside the open day editor using the exact `EditDayBox` add rule
    /// (`8:00-20:00` if empty, else `max(min(last + 30m, 23:30), last + 1m)` for 4 hours).
    pub fn add_interval_in_editor(&mut self) {
        let Some(ref mut editor) = self.editing_day else {
            return;
        };
        if editor.intervals.list.is_empty() {
            editor
                .intervals
                .list
                .push(WorkingInterval::new(8 * 3600, 20 * 3600));
        } else if let Some(last) = editor.intervals.list.last()
            && last.end + 60 < SECONDS_PER_DAY
        {
            let from = (last.end + 30 * 60)
                .min(SECONDS_PER_DAY - 30 * 60)
                .max(last.end + 60);
            let to = (from + 4 * 3600).min(SECONDS_PER_DAY + IN_NEXT_DAY_MAX);
            editor.intervals.list.push(WorkingInterval::new(from, to));
        }
    }

    /// Persists current opening hours through `backend`.
    pub fn save_to_backend(&self, backend: &mut dyn BusinessBackend) -> Result<String, String> {
        let payload = if self.enabled {
            self.hours.normalized()
        } else {
            WorkingHours::default()
        };
        backend.save_working_hours(payload)?;
        Ok(if self.enabled {
            "Opening hours saved.".to_owned()
        } else {
            "Opening hours hidden from profile.".to_owned()
        })
    }

    /// Renders the Opening Hours editor UI and returns an optional toast notification.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn BusinessBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;

        business_section_intro(
            ui,
            "HR",
            BUSINESS_GOLD,
            "Opening Hours",
            "Let customers see when your business is open in their local time zone.",
            palette,
        );

        Frame::NONE
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                if settings_toggle_row(
                    ui,
                    "Show Opening Hours",
                    Some("Display weekly opening hours on your profile page."),
                    &mut self.enabled,
                    palette,
                ) {
                    if self.enabled && !self.hours.intervals.is_non_empty() {
                        // Seed sensible Mon-Fri 09:00-18:00 default when enabling from empty.
                        let list = (0..5)
                            .map(|d| {
                                WorkingInterval::new(
                                    d * SECONDS_PER_DAY + 9 * 3600,
                                    d * SECONDS_PER_DAY + 18 * 3600,
                                )
                            })
                            .collect();
                        self.hours.intervals = WorkingIntervals::new(list).normalized();
                    }
                    match self.save_to_backend(backend) {
                        Ok(msg) => toast = Some(msg),
                        Err(e) => toast = Some(e),
                    }
                }
            });

        if !self.enabled {
            return toast;
        }

        section_divider(ui, palette);

        // Weekly 7-day schedule (`AddWeekButton` loop `0..7`)
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    subsection_title(ui, "Weekly Schedule", palette);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if pill_button(
                            ui,
                            "24/7",
                            palette.window_bg_over,
                            palette.window_active_text_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            self.hours.intervals =
                                WorkingIntervals::new(vec![WorkingInterval::new(
                                    0,
                                    7 * SECONDS_PER_DAY,
                                )])
                                .normalized();
                            if let Ok(msg) = self.save_to_backend(backend) {
                                toast = Some(format!("Set 24/7 schedule. {msg}"));
                            }
                        }
                        if pill_button(
                            ui,
                            "Mon-Fri 9-18",
                            palette.window_bg_over,
                            palette.window_active_text_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            let list = (0..5)
                                .map(|d| {
                                    WorkingInterval::new(
                                        d * SECONDS_PER_DAY + 9 * 3600,
                                        d * SECONDS_PER_DAY + 18 * 3600,
                                    )
                                })
                                .collect();
                            self.hours.intervals = WorkingIntervals::new(list).normalized();
                            if let Ok(msg) = self.save_to_backend(backend) {
                                toast = Some(format!("Applied Mon-Fri 09:00-18:00. {msg}"));
                            }
                        }
                    });
                });
                ui.add_space(6.0);

                for day_index in 0..7 {
                    let (status_text, is_open) = self.day_status_label(day_index);
                    let width = ui.available_width().max(220.0);
                    let (row_rect, _) =
                        ui.allocate_exact_size(Vec2::new(width, 50.0), Sense::hover());

                    // Left clickable zone opens EditDayBox; right zone toggles day on/off.
                    let toggle_zone_w = 60.0;
                    let left_rect = Rect::from_min_max(
                        row_rect.min,
                        Pos2::new(row_rect.right() - toggle_zone_w, row_rect.bottom()),
                    );
                    let right_rect = Rect::from_min_max(
                        Pos2::new(row_rect.right() - toggle_zone_w, row_rect.top()),
                        row_rect.max,
                    );

                    let left_resp = ui.interact(
                        left_rect,
                        ui.scope_id().with(("day_edit", day_index)),
                        Sense::click(),
                    );
                    let right_resp = ui.interact(
                        right_rect,
                        ui.scope_id().with(("day_toggle", day_index)),
                        Sense::click(),
                    );
                    if left_resp.hovered() || right_resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if left_resp.hovered() {
                        ui.painter().rect_filled(
                            left_rect,
                            CornerRadius::same(6),
                            palette.window_bg_over,
                        );
                    }

                    let is_editing_this = self
                        .editing_day
                        .as_ref()
                        .is_some_and(|ed| ed.day_index == day_index);
                    if is_editing_this {
                        ui.painter().rect_filled(
                            left_rect,
                            CornerRadius::same(6),
                            with_alpha(palette.window_bg_active, 0.10),
                        );
                    }

                    ui.painter().text(
                        Pos2::new(left_rect.left() + 8.0, left_rect.top() + 16.0),
                        Align2::LEFT_CENTER,
                        WEEKDAY_NAMES[day_index as usize],
                        FontId::proportional(13.5),
                        palette.window_bold_fg,
                    );
                    ui.painter().text(
                        Pos2::new(left_rect.left() + 8.0, left_rect.top() + 35.0),
                        Align2::LEFT_CENTER,
                        &status_text,
                        FontId::proportional(11.5),
                        if is_open {
                            SUCCESS_GREEN
                        } else {
                            palette.window_sub_text_fg
                        },
                    );

                    // Vertical separator between edit area and toggle switch (matching `AddWeekButton`)
                    ui.painter().line_segment(
                        [
                            Pos2::new(right_rect.left(), row_rect.top() + 10.0),
                            Pos2::new(right_rect.left(), row_rect.bottom() - 10.0),
                        ],
                        Stroke::new(1.0, palette.divider),
                    );

                    // Toggle switch visual
                    let sw_rect =
                        Rect::from_center_size(right_rect.center(), Vec2::new(38.0, 21.0));
                    let track_col = if is_open {
                        palette.window_bg_active
                    } else {
                        with_alpha(palette.window_sub_text_fg, 0.35)
                    };
                    ui.painter()
                        .rect_filled(sw_rect, CornerRadius::same(11), track_col);
                    let knob_x = if is_open {
                        sw_rect.right() - 10.5
                    } else {
                        sw_rect.left() + 10.5
                    };
                    ui.painter().circle_filled(
                        Pos2::new(knob_x, sw_rect.center().y),
                        8.0,
                        Color32::WHITE,
                    );

                    if left_resp.clicked() {
                        if is_editing_this {
                            self.editing_day = None;
                        } else {
                            self.open_day_editor(day_index);
                        }
                    }
                    if right_resp.clicked() {
                        self.toggle_day(day_index, !is_open);
                        if is_editing_this {
                            self.open_day_editor(day_index);
                        }
                        if let Ok(msg) = self.save_to_backend(backend) {
                            toast = Some(msg);
                        }
                    }

                    // Inline EditDayBox card right below the selected day row
                    if is_editing_this {
                        if let Some(msg) = self.show_day_editor_card(ui, backend, palette) {
                            toast = Some(msg);
                        }
                        ui.add_space(6.0);
                    }

                    thin_divider(ui, palette);
                }
            });

        section_divider(ui, palette);

        // Time Zone selector (`lng_hours_time_zone` + `ChooseTimezoneBox`)
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 4))
            .show(ui, |ui| {
                subsection_title(ui, "Time Zone", palette);
                ui.add_space(6.0);
                let tz_label = self.selected_timezone_label();
                let width = ui.available_width().max(200.0);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 42.0), Sense::click());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), palette.window_bg_over);
                }
                ui.painter().text(
                    Pos2::new(rect.left() + 6.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    "Time Zone",
                    FontId::proportional(13.5),
                    palette.window_fg,
                );
                ui.painter().text(
                    Pos2::new(rect.right() - 18.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    &tz_label,
                    FontId::proportional(12.5),
                    palette.window_active_text_fg,
                );
                ui.painter().text(
                    Pos2::new(rect.right() - 6.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    if self.choosing_timezone { "v" } else { ">" },
                    FontId::proportional(12.5),
                    palette.window_sub_text_fg,
                );
                if resp.clicked() {
                    self.choosing_timezone = !self.choosing_timezone;
                }

                if self.choosing_timezone {
                    ui.add_space(6.0);
                    Frame::NONE
                        .fill(palette.window_bg_over)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.timezone_filter)
                                    .hint_text("Filter time zones...")
                                    .desired_width(ui.available_width()),
                            );
                            ui.add_space(6.0);
                            let q = self.timezone_filter.trim().to_lowercase();
                            let mut picked = None;
                            for tz in &self.timezones {
                                let full = tz.full_name();
                                if !q.is_empty()
                                    && !full.to_lowercase().contains(&q)
                                    && !tz.id.to_lowercase().contains(&q)
                                {
                                    continue;
                                }
                                let sel = tz.id == self.hours.timezone_id;
                                if radio_row(ui, &full, sel, palette) {
                                    picked = Some(tz.id.clone());
                                }
                            }
                            if let Some(new_id) = picked {
                                self.hours.timezone_id = new_id;
                                self.choosing_timezone = false;
                                match self.save_to_backend(backend) {
                                    Ok(msg) => toast = Some(msg),
                                    Err(e) => toast = Some(e),
                                }
                            }
                        });
                }

                ui.add_space(12.0);
                if primary_button(
                    ui,
                    "Save Opening Hours",
                    palette.window_bg_active,
                    palette.window_fg_active,
                    38.0,
                )
                .clicked()
                {
                    if self.editing_day.is_some() {
                        self.apply_day_editor();
                    }
                    match self.save_to_backend(backend) {
                        Ok(msg) => toast = Some(msg),
                        Err(e) => toast = Some(e),
                    }
                }
            });

        toast
    }

    fn show_day_editor_card(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn BusinessBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        let mut save_clicked = false;
        let mut cancel_clicked = false;
        let mut add_clicked = false;

        let editor = self.editing_day.as_mut()?;

        Frame::NONE
            .fill(palette.window_bg_over)
            .stroke(Stroke::new(1.0, palette.window_bg_active))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                let day_name = WEEKDAY_NAMES[editor.day_index as usize];
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("Edit {day_name} Hours"))
                            .size(13.0)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if pill_button(
                            ui,
                            "Late Night (18:00-02:00)",
                            palette.window_bg,
                            palette.window_active_text_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            editor.intervals = WorkingIntervals::new(vec![WorkingInterval::new(
                                18 * 3600,
                                SECONDS_PER_DAY + 2 * 3600,
                            )]);
                        }
                        if pill_button(
                            ui,
                            "Split Shift",
                            palette.window_bg,
                            palette.window_active_text_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            editor.intervals = WorkingIntervals::new(vec![
                                WorkingInterval::new(9 * 3600, 13 * 3600),
                                WorkingInterval::new(14 * 3600, 18 * 3600),
                            ]);
                        }
                    });
                });
                ui.add_space(6.0);

                let count = editor.intervals.list.len();
                let mut remove_idx = None;
                for i in 0..count {
                    let min_t = if i == 0 {
                        0
                    } else {
                        editor.intervals.list[i - 1].end + 60
                    };
                    let max_t = if i + 1 == count {
                        SECONDS_PER_DAY + IN_NEXT_DAY_MAX
                    } else {
                        editor.intervals.list[i + 1].start - 60
                    };
                    let iv = &mut editor.intervals.list[i];

                    Frame::NONE
                        .fill(palette.window_bg)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(8))
                        .show(ui, |ui| {
                            // Opening row
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("Opens:")
                                        .size(12.0)
                                        .color(palette.window_sub_text_fg),
                                );
                                let start_max = (iv.end - 60).max(min_t);
                                if ui.small_button("-30m").clicked() {
                                    iv.start = (iv.start - 1800).clamp(min_t, start_max);
                                }
                                ui.label(
                                    egui::RichText::new(format_day_time(iv.start, true))
                                        .size(12.5)
                                        .strong()
                                        .color(palette.window_bold_fg),
                                );
                                if ui.small_button("+30m").clicked() {
                                    iv.start = (iv.start + 1800).clamp(min_t, start_max);
                                }

                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new("Closes:")
                                        .size(12.0)
                                        .color(palette.window_sub_text_fg),
                                );
                                let end_min = (iv.start + 60).min(max_t);
                                if ui.small_button("-30m ").clicked() {
                                    iv.end = (iv.end - 1800).clamp(end_min, max_t);
                                }
                                ui.label(
                                    egui::RichText::new(format_day_time(iv.end, true))
                                        .size(12.5)
                                        .strong()
                                        .color(palette.window_bold_fg),
                                );
                                if ui.small_button("+30m ").clicked() {
                                    iv.end = (iv.end + 1800).clamp(end_min, max_t);
                                }

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .button(
                                                egui::RichText::new("Remove")
                                                    .size(11.0)
                                                    .color(palette.attention_fg),
                                            )
                                            .clicked()
                                        {
                                            remove_idx = Some(i);
                                        }
                                    },
                                );
                            });
                        });
                    ui.add_space(4.0);
                }

                if let Some(idx) = remove_idx {
                    editor.intervals.list.remove(idx);
                }

                let can_add = editor.intervals.list.is_empty()
                    || editor
                        .intervals
                        .list
                        .last()
                        .is_some_and(|iv| iv.end + 60 < SECONDS_PER_DAY);

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if can_add
                        && pill_button(
                            ui,
                            "+ Add Hours",
                            with_alpha(palette.window_bg_active, 0.14),
                            palette.window_active_text_fg,
                            None,
                        )
                        .clicked()
                    {
                        add_clicked = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if pill_button(
                            ui,
                            "Save Day",
                            palette.window_bg_active,
                            palette.window_fg_active,
                            None,
                        )
                        .clicked()
                        {
                            save_clicked = true;
                        }
                        if pill_button(
                            ui,
                            "Cancel",
                            palette.window_bg,
                            palette.window_sub_text_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            cancel_clicked = true;
                        }
                    });
                });
            });

        if add_clicked {
            self.add_interval_in_editor();
        }
        if save_clicked {
            self.apply_day_editor();
            match self.save_to_backend(backend) {
                Ok(msg) => toast = Some(msg),
                Err(e) => toast = Some(e),
            }
        } else if cancel_clicked {
            self.editing_day = None;
        }
        toast
    }
}
