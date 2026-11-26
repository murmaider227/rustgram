//! In-chat message search (`HistoryView::ComposeSearch`, history_view_compose_search.cpp).
//!
//! While searching, the chat's top bar is replaced by a back arrow and the search field (with a
//! "From: …" chip when a sender is chosen), and the composer turns into the results bar: jump to
//! date and choose-sender buttons plus the "N of M" counter on the left, previous / next arrows
//! on the right. Typing searches after `AutoSearchTimeout` (0.9 s) or at once on Enter through
//! [`HistoryBackend::search_chat_messages`] (`messages.search`); the newest match is shown
//! first and the arrows walk older / newer matches, loading further pages on demand.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Key, Pos2, Rect, RichText, Sense,
    Stroke, TextEdit, Ui, Vec2,
};
use tg_ui::tg_style::st;

use crate::history::backend::HistoryBackend;
use crate::history::theme::HistoryColors;
use tg_app_core::core_types::dto::rich_compose::MentionCandidate;
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// `AutoSearchTimeout` (config.h): delay between the last keystroke and the request.
pub const AUTO_SEARCH_TIMEOUT_SECS: f64 = 0.9;
/// `kSearchPerPage` of `Api::MessagesSearch`.
pub const SEARCH_PER_PAGE: i32 = 50;
/// `st::historyComposeButton.height`: height of the results bar replacing the composer.
pub const SEARCH_BOTTOM_BAR_HEIGHT: f32 = 46.0;
/// `st::topBarActionSkip`.
const TOP_BAR_ACTION_SKIP: f32 = 10.0;
/// `st::historyTopBarBack.width`.
const BACK_WIDTH: f32 = 52.0;
/// Bottom bar icon buttons (`st::dialogCalendar` / `st::calendarNext` hit areas).
const BOTTOM_BUTTON: f32 = 40.0;

/// Events the search asks the history widget to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeSearchEvent {
    /// Back arrow or Escape: leave the search and restore the top bar and composer.
    Close,
    /// Show this match (scroll to it and highlight it).
    Jump(MsgId),
    /// Open the jump-to-date calendar.
    ShowCalendar,
}

/// State of the in-chat search (top field, results, counter, sender filter).
#[derive(Debug, Default)]
pub struct ComposeSearch {
    /// Text in the search field.
    pub query: String,
    from: Option<(PeerId, String)>,
    pending_since: Option<f64>,
    searched: Option<(String, Option<PeerId>)>,
    ids: Vec<MsgId>,
    total: Option<usize>,
    next_offset: Option<MsgId>,
    current: usize,
    focus_pending: bool,
    from_picker: Option<String>,
    pending_step: Option<i32>,
    last_error: Option<String>,
}

impl ComposeSearch {
    /// Starts a fresh search with the field focused (`HistoryWidget::switchToSearch`).
    pub fn open(&mut self, query: &str) {
        *self = Self {
            query: query.to_owned(),
            focus_pending: true,
            pending_since: (!query.trim().is_empty()).then_some(f64::NEG_INFINITY),
            ..Self::default()
        };
    }

    /// Query whose matches are listed, for highlighting them in the chat.
    #[must_use]
    pub fn highlight_query(&self) -> &str {
        self.searched.as_ref().map_or("", |(q, _)| q.as_str())
    }

    /// Total matches reported by the backend; `None` before the first search.
    #[must_use]
    pub const fn total(&self) -> Option<usize> {
        self.total
    }

    /// 1-based index of the shown match (`0` when none).
    #[must_use]
    pub const fn current(&self) -> usize {
        self.current
    }

    /// Counter text of the results bar (`BottomBar::updateText`).
    #[must_use]
    pub fn counter_text(&self) -> String {
        match self.total {
            None => String::new(),
            Some(0) => tg_lang::tr::lng_search_messages_none(),
            Some(total) => tg_lang::tr::lng_search_messages_n_of_amount(
                &self.current.to_string(),
                &total.to_string(),
            ),
        }
    }

    /// Sets the sender filter (`TopBar::setFrom`) and re-runs the search after the timeout.
    pub fn set_from(&mut self, from: Option<(PeerId, String)>, now: f64) {
        self.from = from;
        self.from_picker = None;
        self.pending_since = Some(now);
    }

    /// Records a keystroke at `now` (`TopBar::requestSearchDelayed`).
    pub fn query_changed(&mut self, now: f64) {
        self.pending_since = Some(now);
    }

    /// Requests the search at once (Enter, `TopBar::requestSearch`).
    pub fn submit(&mut self) {
        self.pending_since = Some(f64::NEG_INFINITY);
    }

    /// Runs a due search and returns the match to show (`newFounds` -> first result).
    pub fn poll(
        &mut self,
        now: f64,
        backend: &dyn HistoryBackend,
        peer_id: PeerId,
    ) -> Option<ComposeSearchEvent> {
        let since = self.pending_since?;
        if now - since < AUTO_SEARCH_TIMEOUT_SECS {
            return None;
        }
        self.pending_since = None;
        let query = self.query.trim().to_owned();
        let from = self.from.as_ref().map(|(p, _)| *p);
        if self.searched.as_ref() == Some(&(query.clone(), from)) {
            return None;
        }
        self.ids.clear();
        self.next_offset = None;
        self.current = 0;
        self.last_error = None;
        if query.is_empty() && from.is_none() {
            self.searched = None;
            self.total = None;
            return None;
        }
        self.searched = Some((query.clone(), from));
        match backend.search_chat_messages(peer_id, &query, from, MsgId(0), SEARCH_PER_PAGE) {
            Ok(page) => {
                self.total = Some(page.total.max(page.ids.len()));
                self.next_offset = page.next_offset;
                self.ids = page.ids;
                if let Some(&first) = self.ids.first() {
                    self.current = 1;
                    return Some(ComposeSearchEvent::Jump(first));
                }
                self.total = Some(0);
                None
            }
            Err(err) => {
                self.total = Some(0);
                self.last_error = tg_app_core::user_error::chat_search(&err).into_text();
                None
            }
        }
    }

    /// Moves to the next older (`way = 1`) or newer (`way = -1`) match, loading the next page
    /// when the older end of the loaded matches is reached (`Inner`'s `showItemRequests`).
    pub fn step(
        &mut self,
        way: i32,
        backend: &dyn HistoryBackend,
        peer_id: PeerId,
    ) -> Option<ComposeSearchEvent> {
        let total = self.total?;
        let target = self.current as i64 + i64::from(way);
        if target < 1 || target as usize > total {
            return None;
        }
        let target = target as usize;
        if target > self.ids.len()
            && let (Some(offset), Some((query, from))) = (self.next_offset, self.searched.clone())
        {
            match backend.search_chat_messages(peer_id, &query, from, offset, SEARCH_PER_PAGE) {
                Ok(page) => {
                    self.next_offset = page.next_offset;
                    for id in page.ids {
                        if !self.ids.contains(&id) {
                            self.ids.push(id);
                        }
                    }
                }
                Err(err) => {
                    self.last_error = tg_app_core::user_error::chat_search(&err).into_text()
                }
            }
        }
        let id = *self.ids.get(target - 1)?;
        self.current = target;
        Some(ComposeSearchEvent::Jump(id))
    }

    /// Paints the search top bar over `rect` (`TopBar`: back arrow + search field).
    pub fn show_top_bar(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        colors: &HistoryColors,
        now: f64,
    ) -> Option<ComposeSearchEvent> {
        let mut event = None;
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);
        let base = Id::unique("tg_compose_search_top");

        // Back arrow (`st::historyTopBarBack`).
        let back = Rect::from_min_size(rect.min, Vec2::new(BACK_WIDTH, rect.height()));
        let back_resp = ui.interact(back, base.with("back"), Sense::click());
        back_resp
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Close search"));
        if back_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(back.center(), 21.0, colors.bar_bg_over);
        }
        tg_app_core::ui_icons::paint_style_tinted(
            ui.painter(),
            Rect::from_center_size(back.center(), Vec2::splat(24.0)),
            st::history_top_bar_back.icon,
            colors.sub_text_fg,
        );
        if back_resp.clicked() {
            event = Some(ComposeSearchEvent::Close);
        }

        // Search field (`st::searchInChatMultiSelect`): magnifier, "From:" chip, input, cancel.
        let field = Rect::from_min_max(
            Pos2::new(back.right(), rect.top()),
            Pos2::new(rect.right() - 8.0, rect.bottom()),
        );
        tg_app_core::ui_icons::paint_style_tinted(
            ui.painter(),
            // The icon carries its `point(10px, 9px)` offset inside the field.
            Rect::from_min_size(
                Pos2::new(field.left(), field.center().y - 17.0),
                Vec2::splat(34.0),
            ),
            st::box_field_search_icon,
            colors.sub_text_fg,
        );
        let mut text_left = field.left() + 36.0;
        if let Some((_, name)) = self.from.clone() {
            let label = tg_lang::tr::lng_dlg_search_from(&name);
            let galley =
                ui.painter()
                    .layout_no_wrap(label, FontId::proportional(13.0), Color32::WHITE);
            let chip = Rect::from_min_size(
                Pos2::new(text_left, field.center().y - 14.0),
                Vec2::new(galley.size().x + 24.0, 28.0),
            );
            let chip_resp = ui.interact(chip, base.with("from_chip"), Sense::click());
            ui.painter()
                .rect_filled(chip, CornerRadius::same(14), colors.active_text_fg);
            ui.painter().galley(
                Pos2::new(chip.left() + 12.0, chip.center().y - galley.size().y / 2.0),
                galley,
                Color32::WHITE,
            );
            if chip_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if chip_resp.on_hover_text("Remove").clicked() {
                self.set_from(None, now);
            }
            text_left = chip.right() + 8.0;
        }
        let cancel_w = if self.query.is_empty() { 0.0 } else { 44.0 };
        let input_rect = Rect::from_min_max(
            Pos2::new(text_left, field.center().y - 12.0),
            Pos2::new(field.right() - cancel_w, field.center().y + 12.0),
        );
        let input = ui.put(
            input_rect,
            TextEdit::singleline(&mut self.query)
                .id(base.with("field"))
                .frame(egui::Frame::NONE)
                .background_color(Color32::TRANSPARENT)
                .text_color(colors.text_fg)
                .font(FontId::proportional(15.0))
                .hint_text(RichText::new(tg_lang::tr::lng_dlg_filter()).color(colors.sub_text_fg))
                .desired_width(input_rect.width()),
        );
        if self.focus_pending {
            input.request_focus();
            self.focus_pending = false;
        }
        if input.changed() {
            self.query_changed(now);
        }
        let (enter, escape, f3, shift) = ui.input(|i| {
            (
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::F3),
                i.modifiers.shift,
            )
        });
        if input.lost_focus() && enter {
            self.submit();
            input.request_focus();
        }
        if escape {
            if self.from_picker.is_some() {
                self.from_picker = None;
            } else {
                event = Some(ComposeSearchEvent::Close);
            }
        }
        if f3 {
            // Mapped by the caller through `step` once the backend is at hand.
            self.pending_step = Some(if shift { -1 } else { 1 });
        }
        if cancel_w > 0.0 {
            let cancel = Rect::from_min_size(
                Pos2::new(field.right() - cancel_w, field.center().y - 22.0),
                Vec2::splat(44.0),
            );
            let resp = ui.interact(cancel, base.with("cancel"), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let c = cancel.center();
            let stroke = Stroke::new(1.5, colors.sub_text_fg);
            ui.painter()
                .line_segment([c - Vec2::splat(6.0), c + Vec2::splat(6.0)], stroke);
            ui.painter()
                .line_segment([c + Vec2::new(-6.0, 6.0), c + Vec2::new(6.0, -6.0)], stroke);
            if resp.on_hover_text("Clear").clicked() {
                self.query.clear();
                self.query_changed(now);
                self.focus_pending = true;
            }
        }
        ui.painter().line_segment(
            [
                Pos2::new(rect.left(), rect.bottom()),
                Pos2::new(rect.right(), rect.bottom()),
            ],
            Stroke::new(1.0, colors.divider),
        );
        event
    }

    /// Paints the results bar in place of the composer (`BottomBar`). `can_choose_from` is
    /// `HasChooseFrom` (groups and channels); `members` feed the sender picker.
    #[allow(clippy::too_many_arguments)]
    pub fn show_bottom_bar(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        colors: &HistoryColors,
        can_choose_from: bool,
        members: &dyn Fn() -> Vec<MentionCandidate>,
        backend: &dyn HistoryBackend,
        peer_id: PeerId,
        now: f64,
    ) -> Option<ComposeSearchEvent> {
        let mut event = None;
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);
        ui.painter().line_segment(
            [
                Pos2::new(rect.left(), rect.top()),
                Pos2::new(rect.right(), rect.top()),
            ],
            Stroke::new(1.0, colors.divider),
        );
        let base = Id::unique("tg_compose_search_bottom");
        let button = |ui: &mut Ui, r: Rect, key: &str, icon, enabled: bool, label: &str| {
            let resp = ui.interact(r, base.with(key), Sense::click());
            let owned = label.to_owned();
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, enabled, owned.clone())
            });
            if enabled && resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .circle_filled(r.center(), 17.0, colors.bar_bg_over);
            }
            let fg = if enabled {
                colors.sub_text_fg
            } else {
                colors.sub_text_fg.gamma_multiply(0.45)
            };
            tg_app_core::ui_icons::paint_style_tinted(
                ui.painter(),
                Rect::from_center_size(r.center(), Vec2::splat(24.0)),
                icon,
                fg,
            );
            enabled && resp.on_hover_text(label).clicked()
        };

        // Left: jump to date and choose sender (hidden once a sender is chosen), then "N of M".
        let mut left = rect.left() + TOP_BAR_ACTION_SKIP;
        let cy = rect.center().y;
        if self.from.is_none() {
            let r = Rect::from_center_size(
                Pos2::new(left + BOTTOM_BUTTON / 2.0, cy),
                Vec2::splat(BOTTOM_BUTTON),
            );
            if button(
                ui,
                r,
                "calendar",
                st::dialog_calendar.icon,
                true,
                "Jump to date",
            ) {
                event = Some(ComposeSearchEvent::ShowCalendar);
            }
            left += BOTTOM_BUTTON;
            if can_choose_from {
                let r = Rect::from_center_size(
                    Pos2::new(left + BOTTOM_BUTTON / 2.0, cy),
                    Vec2::splat(BOTTOM_BUTTON),
                );
                if button(
                    ui,
                    r,
                    "from",
                    st::dialog_search_from.icon,
                    true,
                    "Search messages from",
                ) {
                    self.from_picker = Some(String::new());
                }
                left += BOTTOM_BUTTON;
            }
        }
        let counter = self.counter_text();
        if !counter.is_empty() {
            ui.painter().text(
                Pos2::new(left + 6.0, cy),
                Align2::LEFT_CENTER,
                &counter,
                FontId::proportional(13.0),
                colors.sub_text_fg,
            );
        }

        // Right: previous (newer) at the edge, next (older) left of it; icons are swapped.
        let total = self.total.unwrap_or(0);
        let next_enabled = self.current > 0 && self.current < total;
        let prev_enabled = self.current > 1;
        let prev = Rect::from_center_size(
            Pos2::new(rect.right() - BOTTOM_BUTTON / 2.0 - 4.0, cy),
            Vec2::splat(BOTTOM_BUTTON),
        );
        let next = prev.translate(Vec2::new(-BOTTOM_BUTTON, 0.0));
        let prev_icon = if prev_enabled {
            st::calendar_next.icon
        } else {
            st::calendar_next_disabled
        };
        let next_icon = if next_enabled {
            st::calendar_previous.icon
        } else {
            st::calendar_previous_disabled
        };
        if button(ui, next, "next", next_icon, next_enabled, "Older result")
            && let Some(ev) = self.step(1, backend, peer_id)
        {
            event = Some(ev);
        }
        if button(ui, prev, "prev", prev_icon, prev_enabled, "Newer result")
            && let Some(ev) = self.step(-1, backend, peer_id)
        {
            event = Some(ev);
        }
        if let Some(way) = self.pending_step.take()
            && let Some(ev) = self.step(way, backend, peer_id)
        {
            event = Some(ev);
        }

        if self.from_picker.is_some() {
            self.show_from_picker(ui, rect, colors, members, now);
        }
        event
    }

    /// Sender picker above the results bar (`Dialogs::SearchFromBox`, simplified to a list).
    fn show_from_picker(
        &mut self,
        ui: &mut Ui,
        bar: Rect,
        colors: &HistoryColors,
        members: &dyn Fn() -> Vec<MentionCandidate>,
        now: f64,
    ) {
        let Some(mut filter) = self.from_picker.take() else {
            return;
        };
        let needle = filter.to_lowercase();
        let list: Vec<MentionCandidate> = members()
            .into_iter()
            .filter(|m| {
                needle.is_empty()
                    || m.display_name.to_lowercase().contains(&needle)
                    || m.username
                        .as_deref()
                        .is_some_and(|u| u.to_lowercase().contains(&needle))
            })
            .take(8)
            .collect();
        let row_h = 44.0;
        let height = 40.0 + row_h * list.len().max(1) as f32;
        let panel = Rect::from_min_size(
            Pos2::new(bar.left() + TOP_BAR_ACTION_SKIP, bar.top() - height - 6.0),
            Vec2::new(280.0_f32.min(bar.width() - 20.0), height),
        );
        let mut chosen = None;
        let mut keep = true;
        egui::Area::new(Id::unique("tg_compose_search_from_picker"))
            .order(egui::Order::Foreground)
            .fixed_pos(panel.min)
            .show(ui.ctx(), |ui| {
                ui.painter()
                    .rect_filled(panel, CornerRadius::same(8), colors.bar_bg);
                ui.painter().rect_stroke(
                    panel,
                    CornerRadius::same(8),
                    Stroke::new(1.0, colors.divider),
                    egui::StrokeKind::Inside,
                );
                let field = Rect::from_min_size(
                    panel.min + Vec2::new(12.0, 8.0),
                    Vec2::new(panel.width() - 24.0, 24.0),
                );
                let resp = ui.put(
                    field,
                    TextEdit::singleline(&mut filter)
                        .frame(egui::Frame::NONE)
                        .text_color(colors.text_fg)
                        .hint_text(
                            RichText::new(tg_lang::tr::lng_dlg_filter()).color(colors.sub_text_fg),
                        ),
                );
                resp.request_focus();
                for (i, m) in list.iter().enumerate() {
                    let row = Rect::from_min_size(
                        Pos2::new(panel.left(), panel.top() + 40.0 + row_h * i as f32),
                        Vec2::new(panel.width(), row_h),
                    );
                    let r = ui.interact(
                        row,
                        Id::unique(("tg_cs_from_row", m.user_id)),
                        Sense::click(),
                    );
                    if r.hovered() {
                        ui.painter()
                            .rect_filled(row, CornerRadius::ZERO, colors.bar_bg_over);
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    let [cr, cg, cb] = m.avatar_rgb;
                    ui.painter().circle_filled(
                        Pos2::new(row.left() + 30.0, row.center().y),
                        16.0,
                        Color32::from_rgb(cr, cg, cb),
                    );
                    ui.painter().text(
                        Pos2::new(row.left() + 56.0, row.center().y),
                        Align2::LEFT_CENTER,
                        &m.display_name,
                        FontId::proportional(14.0),
                        colors.text_fg,
                    );
                    if r.clicked() {
                        chosen = Some((PeerId::from_user(m.user_id), m.display_name.clone()));
                    }
                }
                if ui.input(|i| i.pointer.any_click())
                    && ui
                        .ctx()
                        .pointer_interact_pos()
                        .is_some_and(|p| !panel.contains(p))
                {
                    keep = false;
                }
            });
        if let Some(from) = chosen {
            self.set_from(Some(from), now);
            self.focus_pending = true;
        } else if keep {
            self.from_picker = Some(filter);
        }
    }
}
