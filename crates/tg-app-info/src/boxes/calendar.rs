//! Jump-to-date calendar modal box (`CalendarBox`) backed by `messages.getSearchResultsCalendar`.
//!
//! Port origin: `Telegram/SourceFiles/ui/boxes/calendar_box.h`,
//! `Telegram/SourceFiles/ui/boxes/calendar_box.cpp`, and
//! `Telegram/SourceFiles/data/data_search_calendar.cpp`.

use std::collections::{BTreeMap, HashMap};

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Key, Pos2, Rect, Role, Sense, Stroke,
    Ui, Vec2, WidgetInfo,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::st;

use tg_app_core::history::sections_model::{
    ScheduleDateTime, days_in_month, month_name, weekday_monday_zero,
};
use tg_app_core::history::theme::HistoryColors;
use tg_app_core::stand_in::types::{MsgId, PeerId};

pub use tg_app_core::core_types::dto::calendar::*;

/// Stateful jump-to-date calendar modal (`Ui::CalendarBox`).
#[derive(Debug, Clone)]
pub struct CalendarBoxState {
    /// Chat peer being navigated.
    pub peer_id: PeerId,
    /// Display title of the chat.
    pub peer_title: String,
    /// Currently displayed year.
    pub view_year: i32,
    /// Currently displayed month (`1..=12`).
    pub view_month: u32,
    /// Reference "today" `(year, month, day)`.
    pub today: (i32, u32, u32),
    /// Currently highlighted day `(year, month, day)`.
    pub selected_day: Option<(i32, u32, u32)>,
    /// Loaded per-month `messages.getSearchResultsCalendar` results.
    pub months: HashMap<(i32, u32), CalendarMonthData>,
    /// Optional status / error line.
    pub error: Option<String>,
}

impl CalendarBoxState {
    /// Creates a new [`CalendarBoxState`] positioned on the month containing `now_unix`.
    #[must_use]
    pub fn new(peer_id: PeerId, peer_title: impl Into<String>, now_unix: i32) -> Self {
        let dt = ScheduleDateTime::from_unix_utc(i64::from(now_unix));
        Self {
            peer_id,
            peer_title: peer_title.into(),
            view_year: dt.year,
            view_month: dt.month,
            today: (dt.year, dt.month, dt.day),
            selected_day: None,
            months: HashMap::new(),
            error: None,
        }
    }

    /// Returns `true` if the currently visible `(view_year, view_month)` has not been loaded yet.
    #[must_use]
    pub fn needs_load_current_month(&self) -> bool {
        !self
            .months
            .get(&(self.view_year, self.view_month))
            .is_some_and(|m| m.loaded)
    }

    /// Stores loaded `messages.getSearchResultsCalendar` period entries for `(year, month)`.
    pub fn set_month_days(&mut self, year: i32, month: u32, entries: Vec<CalendarDayInfo>) {
        let mut days = BTreeMap::new();
        let mut min_date = 0i32;
        let mut min_msg_id = 0i32;
        for entry in entries {
            if entry.year == year && entry.month == month && entry.day >= 1 && entry.day <= 31 {
                if min_date == 0 || entry.date_unix < min_date {
                    min_date = entry.date_unix;
                }
                if min_msg_id == 0 || entry.min_msg_id.0 < min_msg_id {
                    min_msg_id = entry.min_msg_id.0;
                }
                days.entry(entry.day).or_insert(entry);
            }
        }
        self.months.insert(
            (year, month),
            CalendarMonthData {
                year,
                month,
                days,
                loaded: true,
                min_date,
                min_msg_id,
            },
        );
        self.error = None;
    }

    /// Moves view to the previous month (clamped to [`CALENDAR_MIN_YEAR`]).
    pub fn prev_month(&mut self) {
        if self.view_month <= 1 {
            if self.view_year > CALENDAR_MIN_YEAR {
                self.view_year -= 1;
                self.view_month = 12;
            }
        } else {
            self.view_month -= 1;
        }
    }

    /// Moves view to the next month (clamped to [`Self::today`]'s month).
    pub fn next_month(&mut self) {
        let (max_y, max_m, _) = self.today;
        if (self.view_year, self.view_month) >= (max_y, max_m) {
            return;
        }
        if self.view_month >= 12 {
            self.view_year += 1;
            self.view_month = 1;
        } else {
            self.view_month += 1;
        }
    }

    /// Jumps directly to `(year, month)` clamped within `[CALENDAR_MIN_YEAR..=today]`.
    pub fn jump_to_month(&mut self, year: i32, month: u32) {
        let (max_y, max_m, _) = self.today;
        let mut y = year.clamp(CALENDAR_MIN_YEAR, max_y);
        let mut m = month.clamp(1, 12);
        if y == max_y && m > max_m {
            m = max_m;
        }
        if y < CALENDAR_MIN_YEAR {
            y = CALENDAR_MIN_YEAR;
            m = 1;
        }
        self.view_year = y;
        self.view_month = m;
    }

    /// Returns whether navigating to the previous month is permitted.
    #[must_use]
    pub const fn can_go_prev(&self) -> bool {
        self.view_year > CALENDAR_MIN_YEAR || self.view_month > 1
    }

    /// Returns whether navigating to the next month is permitted.
    #[must_use]
    pub const fn can_go_next(&self) -> bool {
        self.view_year < self.today.0
            || (self.view_year == self.today.0 && self.view_month < self.today.1)
    }

    /// Returns the currently selected day's [`CalendarDayInfo`], if available.
    #[must_use]
    pub fn selected_day_info(&self) -> Option<&CalendarDayInfo> {
        let (y, m, d) = self.selected_day?;
        self.months.get(&(y, m))?.day_info(d)
    }

    /// Resolves a jump action for the selected day (or nearest active day in the month).
    #[must_use]
    pub fn build_jump_action(&self) -> Option<CalendarBoxAction> {
        let (y, m, d) = self.selected_day?;
        if let Some(info) = self.months.get(&(y, m)).and_then(|md| md.day_info(d)) {
            return Some(CalendarBoxAction::JumpToDate {
                peer_id: self.peer_id,
                msg_id: info.max_msg_id,
                date_unix: info.date_unix,
            });
        }
        let dt = ScheduleDateTime {
            year: y,
            month: m,
            day: d,
            hour: 12,
            minute: 0,
        };
        Some(CalendarBoxAction::JumpToDate {
            peer_id: self.peer_id,
            msg_id: MsgId(0),
            date_unix: dt.to_unix_utc() as i32,
        })
    }

    /// Builds a `tg-ui` [`BoxModal`] descriptor when hosted inside [`crate::boxes::BoxHost`].
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let mut modal = BoxModal::new(id, "Jump to Date")
            .width(st::box_width.0 as f32)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()));
        if self.selected_day.is_some() {
            modal = modal.add_button(BoxButton::default_btn("jump", "Jump to Date"));
        }
        modal
    }

    /// Renders the calendar body inside [`crate::boxes::BoxHost`].
    ///
    /// Returns `Some(CalendarBoxAction::JumpToDate { .. })` immediately when the user clicks an
    /// active day with messages (matching tdesktop's single-click jump in `calendar_box.cpp`).
    pub fn show_box_body(&mut self, ui: &mut Ui, colors: &dyn Colors) -> Option<CalendarBoxAction> {
        let palette = CalendarPalette {
            bg: colors.color("boxBg"),
            bg_over: colors.color("windowBgOver"),
            accent: colors.color("windowBgActive"),
            bold_fg: colors.color("windowBoldFg"),
            text_fg: colors.color("windowFg"),
            sub_text_fg: colors.color("windowSubTextFg"),
            attention_fg: colors.color("attentionButtonFg"),
            divider: colors.color("shadowFg"),
        };
        self.render_calendar_grid(ui, &palette)
    }

    /// Renders a standalone modal overlay centered inside `HistoryWidget`, invoking `load_month`
    /// lazily whenever the visible `(year, month)` has not yet been fetched via
    /// `messages.getSearchResultsCalendar`.
    pub fn show_in_history(
        &mut self,
        ui: &mut Ui,
        colors: &HistoryColors,
        mut load_month: impl FnMut(PeerId, i32, u32) -> Result<Vec<CalendarDayInfo>, String>,
    ) -> Option<CalendarBoxAction> {
        if self.needs_load_current_month() {
            match load_month(self.peer_id, self.view_year, self.view_month) {
                Ok(days) => self.set_month_days(self.view_year, self.view_month, days),
                Err(err) => {
                    self.set_month_days(self.view_year, self.view_month, Vec::new());
                    self.error = Some(err);
                }
            }
        }

        let palette = CalendarPalette {
            bg: colors.bar_bg,
            bg_over: colors.bar_bg_over,
            accent: colors.accent,
            bold_fg: colors.bold_fg,
            text_fg: colors.text_fg,
            sub_text_fg: colors.sub_text_fg,
            attention_fg: colors.attention_fg,
            divider: colors.divider,
        };

        let mut action = None;
        let full = ui.ctx().content_rect();
        let mut clicked_outside = false;
        let mut card_rect = Rect::NOTHING;

        egui::Area::new(Id::unique("tg_calendar_box_scrim"))
            .order(egui::Order::Middle)
            .fixed_pos(full.min)
            .show(ui.ctx(), |ui| {
                let (r, resp) = ui.allocate_exact_size(full.size(), Sense::click());
                ui.painter().rect_filled(
                    r,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 110),
                );
                if resp.clicked() {
                    clicked_outside = true;
                }
            });

        egui::Area::new(Id::unique("tg_calendar_box_card"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                let frame_resp = egui::Frame::NONE
                    .fill(palette.bg)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(18))
                    .show(ui, |ui| {
                        ui.set_width(336.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Jump to Date")
                                    .font(FontId::new(16.0, family_semibold()))
                                    .color(palette.bold_fg),
                            );
                            if !self.peer_title.is_empty() {
                                ui.label(
                                    egui::RichText::new(format!("· {}", self.peer_title))
                                        .font(FontId::new(12.5, family_regular()))
                                        .color(palette.sub_text_fg),
                                );
                            }
                        });
                        ui.add_space(10.0);

                        if let Some(grid_act) = self.render_calendar_grid(ui, &palette) {
                            action = Some(grid_act);
                        }

                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.small_button("Today").clicked() {
                                let (ty, tm, _) = self.today;
                                self.jump_to_month(ty, tm);
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if self.selected_day.is_some()
                                        && ui
                                            .add(
                                                egui::Button::new(
                                                    egui::RichText::new("Jump")
                                                        .color(Color32::WHITE)
                                                        .strong(),
                                                )
                                                .fill(palette.accent)
                                                .corner_radius(CornerRadius::same(6)),
                                            )
                                            .clicked()
                                    {
                                        action = self.build_jump_action();
                                    }
                                    ui.add_space(6.0);
                                    if ui.button("Cancel").clicked() {
                                        action = Some(CalendarBoxAction::Cancelled);
                                    }
                                },
                            );
                        });
                    });
                card_rect = frame_resp.response.rect;
            });

        if action.is_none() {
            let escaped = ui.ctx().input(|i| i.key_pressed(Key::Escape));
            let outside = clicked_outside
                && ui
                    .ctx()
                    .pointer_interact_pos()
                    .is_none_or(|p| !card_rect.contains(p));
            if escaped || outside {
                action = Some(CalendarBoxAction::Cancelled);
            }
        }

        // If user switched months inside the grid, eagerly fetch the newly visible month.
        if action.is_none() && self.needs_load_current_month() {
            match load_month(self.peer_id, self.view_year, self.view_month) {
                Ok(days) => self.set_month_days(self.view_year, self.view_month, days),
                Err(err) => {
                    self.set_month_days(self.view_year, self.view_month, Vec::new());
                    self.error = Some(err);
                }
            }
        }

        action
    }

    fn render_calendar_grid(
        &mut self,
        ui: &mut Ui,
        palette: &CalendarPalette,
    ) -> Option<CalendarBoxAction> {
        let mut action = None;

        // Month & year navigation bar
        ui.horizontal(|ui| {
            let prev_btn = tg_app_core::ui_icons::small_icon_button(
                ui,
                "Previous month",
                self.can_go_prev(),
                |p, c, s, col| {
                    tg_app_core::ui_icons::paint_triangle(
                        p,
                        c,
                        s,
                        tg_app_core::ui_icons::ArrowDir::Left,
                        col,
                    );
                },
            );
            if prev_btn.clicked() {
                self.prev_month();
            }
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!(
                    "{} {}",
                    month_name(self.view_month),
                    self.view_year
                ))
                .font(FontId::new(14.0, family_semibold()))
                .color(palette.bold_fg),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let next_btn = tg_app_core::ui_icons::small_icon_button(
                    ui,
                    "Next month",
                    self.can_go_next(),
                    |p, c, s, col| {
                        tg_app_core::ui_icons::paint_triangle(
                            p,
                            c,
                            s,
                            tg_app_core::ui_icons::ArrowDir::Right,
                            col,
                        );
                    },
                );
                if next_btn.clicked() {
                    self.next_month();
                }
            });
        });

        ui.add_space(8.0);

        let month_data = self.months.get(&(self.view_year, self.view_month)).cloned();
        let has_any_calendar_marks = month_data.as_ref().is_some_and(|m| !m.days.is_empty());

        let cell_w = 42.0;
        let cell_h = 36.0;
        let (ty, tm, td) = self.today;

        egui::Grid::new(("tg_calendar_days_grid", self.peer_id.0))
            .spacing(Vec2::new(4.0, 4.0))
            .show(ui, |ui| {
                for wd in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(cell_w, 20.0), Sense::hover());
                    ui.painter().text(
                        r.center(),
                        Align2::CENTER_CENTER,
                        wd,
                        FontId::new(11.5, family_regular()),
                        palette.sub_text_fg,
                    );
                }
                ui.end_row();

                let first_wd = weekday_monday_zero(self.view_year, self.view_month, 1);
                let total_days = days_in_month(self.view_year, self.view_month);
                let mut col = 0u32;

                for _ in 0..first_wd {
                    ui.allocate_exact_size(Vec2::new(cell_w, cell_h), Sense::hover());
                    col += 1;
                }

                for day in 1..=total_days {
                    let is_future = (self.view_year, self.view_month, day) > (ty, tm, td);
                    let is_today = self.view_year == ty && self.view_month == tm && day == td;
                    let is_selected =
                        self.selected_day == Some((self.view_year, self.view_month, day));
                    let day_info = month_data.as_ref().and_then(|m| m.day_info(day));
                    let has_msgs = day_info.is_some();
                    let clickable = !is_future && (has_msgs || !has_any_calendar_marks);

                    let sense = if clickable {
                        Sense::click()
                    } else {
                        Sense::hover()
                    };
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(cell_w, cell_h), sense);
                    resp.widget_info(|| {
                        let lbl = match day_info {
                            Some(info) => format!(
                                "{} {} ({} messages)",
                                month_name(self.view_month),
                                day,
                                info.count
                            ),
                            None => format!("{} {}", month_name(self.view_month), day),
                        };
                        WidgetInfo::labeled(Role::Button, clickable, lbl)
                    });

                    if clickable && resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }

                    let circle_r = 15.0;
                    if is_selected {
                        ui.painter()
                            .circle_filled(r.center(), circle_r, palette.accent);
                    } else if let Some(info) = day_info {
                        let fill = if let Some([cr, cg, cb]) = info.preview_rgb {
                            Color32::from_rgba_unmultiplied(cr, cg, cb, 190)
                        } else if resp.hovered() {
                            palette.accent
                        } else {
                            Color32::from_rgba_unmultiplied(
                                palette.accent.r(),
                                palette.accent.g(),
                                palette.accent.b(),
                                38,
                            )
                        };
                        ui.painter().circle_filled(r.center(), circle_r, fill);
                    } else if clickable && resp.hovered() {
                        ui.painter()
                            .circle_filled(r.center(), circle_r, palette.bg_over);
                    }

                    if is_today && !is_selected {
                        ui.painter().circle_stroke(
                            r.center(),
                            circle_r,
                            Stroke::new(1.3, palette.accent),
                        );
                    }

                    let text_col = if is_selected || (has_msgs && resp.hovered()) {
                        Color32::WHITE
                    } else if is_future || (has_any_calendar_marks && !has_msgs) {
                        Color32::from_rgba_unmultiplied(
                            palette.sub_text_fg.r(),
                            palette.sub_text_fg.g(),
                            palette.sub_text_fg.b(),
                            110,
                        )
                    } else if has_msgs {
                        palette.bold_fg
                    } else {
                        palette.text_fg
                    };

                    ui.painter().text(
                        Pos2::new(
                            r.center().x,
                            r.center().y - if has_msgs { 2.0 } else { 0.0 },
                        ),
                        Align2::CENTER_CENTER,
                        day.to_string(),
                        FontId::new(
                            12.5,
                            if has_msgs {
                                family_semibold()
                            } else {
                                family_regular()
                            },
                        ),
                        text_col,
                    );

                    // Small activity dot under days containing messages.
                    if has_msgs {
                        let dot_col = if is_selected || resp.hovered() {
                            Color32::WHITE
                        } else {
                            palette.accent
                        };
                        ui.painter().circle_filled(
                            Pos2::new(r.center().x, r.center().y + 10.5),
                            2.2,
                            dot_col,
                        );
                    }

                    if clickable && resp.clicked() {
                        self.selected_day = Some((self.view_year, self.view_month, day));
                        if let Some(info) = day_info {
                            action = Some(CalendarBoxAction::JumpToDate {
                                peer_id: self.peer_id,
                                msg_id: info.max_msg_id,
                                date_unix: info.date_unix,
                            });
                        }
                    }

                    col += 1;
                    if col.is_multiple_of(7) {
                        ui.end_row();
                    }
                }
            });

        // Month summary footer
        if let Some(md) = month_data.as_ref() {
            ui.add_space(6.0);
            let total = md.total_messages();
            let days_with_msgs = md.days.len();
            let summary = if days_with_msgs == 0 {
                format!(
                    "No messages found in {} {}",
                    month_name(self.view_month),
                    self.view_year
                )
            } else {
                format!(
                    "{total} message(s) across {days_with_msgs} day(s) in {} {}",
                    month_name(self.view_month),
                    self.view_year
                )
            };
            ui.label(
                egui::RichText::new(summary)
                    .font(FontId::new(11.5, family_regular()))
                    .color(palette.sub_text_fg),
            );
        }

        if let Some(ref err) = self.error {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(err)
                    .font(FontId::new(11.5, family_regular()))
                    .color(palette.attention_fg),
            );
        }

        action
    }
}

struct CalendarPalette {
    bg: Color32,
    bg_over: Color32,
    accent: Color32,
    bold_fg: Color32,
    text_fg: Color32,
    sub_text_fg: Color32,
    attention_fg: Color32,
    divider: Color32,
}
