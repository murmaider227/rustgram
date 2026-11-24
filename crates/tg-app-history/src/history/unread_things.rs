//! Floating unread `@` mentions, heart reactions, and poll-vote jump FABs with
//! right-click "Mark all read" popup menu.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/history_unread_things.h` & `.cpp`
//! - `Telegram/SourceFiles/history/history_widget.cpp` (`_unreadMentions`, `_unreadReactions`,
//!   `_unreadPollVotes`)
//! - `Telegram/SourceFiles/chat_helpers/chat_helpers.style` (`historyUnreadMentions`,
//!   `historyUnreadReactions`, `historyUnreadPollVotes`, `historyUnreadThingsSkip`)

use std::collections::VecDeque;

use egui::accesskit::Role;
use egui::{
    Align2, Area, Color32, CornerRadius, CursorIcon, FontId, Id, Key, Order, Pos2, Rect, Sense,
    Shape, Stroke, StrokeKind, Ui, Vec2, WidgetInfo,
};
use tg_app_core::core_types::dto::engagement::{
    UNREAD_THINGS_PRELOAD_IF_LESS, UnreadThingsCounts, UnreadThingsKind, UnreadThingsScope,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::painter::BoxShadow;
use tg_ui::stand_in::{Align, BoxShadowStyle, Point, UnreadBadgeStyle};
use tg_ui::tg_style::st;
use tg_ui::unread_badge::{UnreadBadgeKind, paint_unread_badge};

use crate::history::backend::HistoryBackend;
use crate::history::theme::HistoryColors;

/// Circular diameter of each floating history corner button (`historyToDown` = `42px`).
pub const UNREAD_FAB_DIAMETER: f32 = 42.0;
/// Vertical pitch between stacked corner FAB centers (42px circle + 12px badge/skip clearance).
pub const UNREAD_FAB_STEP_Y: f32 = 54.0;
/// Right margin from the history canvas edge (`historyToDownPosition.x`).
pub const UNREAD_FAB_RIGHT_MARGIN: f32 = 16.0;
/// Bottom margin of the lowest FAB from the history canvas bottom (`historyToDownPosition.y`).
pub const UNREAD_FAB_BOTTOM_MARGIN: f32 = 14.0;

/// Duration of FAB enter/exit opacity + scale transition (ms).
const FAB_FADE_DURATION_SECS: f64 = 0.16;

/// Event emitted when the user interacts with one of the unread engagement FABs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadThingsEvent {
    /// Left-clicked a FAB: jump to and highlight `msg_id` of `kind`.
    JumpTo {
        /// Category of unread item.
        kind: UnreadThingsKind,
        /// Target message id to jump to.
        msg_id: MsgId,
    },
    /// Right-click "Mark all read" triggered for `kind`.
    MarkAllRead {
        /// Category of unread items marked read.
        kind: UnreadThingsKind,
    },
}

#[derive(Debug, Default, Clone)]
struct KindQueueState {
    loaded_ids: VecDeque<MsgId>,
    has_more: bool,
    loaded_once: bool,
    /// Animated visibility progress in `0.0..=1.0`.
    visibility: f32,
    last_time: Option<f64>,
}

impl KindQueueState {
    fn clear(&mut self) {
        self.loaded_ids.clear();
        self.has_more = false;
        self.loaded_once = false;
    }

    fn step_visibility(
        &mut self,
        target_visible: bool,
        now_secs: f64,
        reduced_motion: bool,
    ) -> f32 {
        let target = if target_visible { 1.0 } else { 0.0 };
        if reduced_motion {
            self.visibility = target;
            self.last_time = Some(now_secs);
            return self.visibility;
        }
        let dt = self
            .last_time
            .map_or(0.0, |prev| (now_secs - prev).clamp(0.0, 0.1));
        self.last_time = Some(now_secs);
        if (self.visibility - target).abs() < 0.005 {
            self.visibility = target;
        } else {
            let step = (dt / FAB_FADE_DURATION_SECS) as f32;
            if target > self.visibility {
                self.visibility = (self.visibility + step.max(0.25)).min(1.0);
            } else {
                self.visibility = (self.visibility - step.max(0.25)).max(0.0);
            }
        }
        self.visibility
    }
}

/// Stateful controller for the floating `@` mention, heart reaction, and poll-vote jump FABs
/// in the active chat or topic (`HistoryUnreadThings` + `HistoryWidget` corner buttons).
#[derive(Debug)]
pub struct HistoryUnreadThingsWidget {
    active_scope: Option<UnreadThingsScope>,
    mentions: KindQueueState,
    reactions: KindQueueState,
    poll_votes: KindQueueState,
    /// Currently open right-click "Mark all read" context menu `(kind, anchor_rect)`.
    context_menu: Option<(UnreadThingsKind, Rect)>,
    /// Last painted button rects for deterministic `egui_kittest` hit-testing.
    last_rects: [(UnreadThingsKind, Option<Rect>); 3],
}

impl Default for HistoryUnreadThingsWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryUnreadThingsWidget {
    /// Creates an idle unread-things FAB controller.
    #[must_use]
    pub fn new() -> Self {
        Self {
            active_scope: None,
            mentions: KindQueueState::default(),
            reactions: KindQueueState::default(),
            poll_votes: KindQueueState::default(),
            context_menu: None,
            last_rects: [
                (UnreadThingsKind::PollVotes, None),
                (UnreadThingsKind::Reactions, None),
                (UnreadThingsKind::Mentions, None),
            ],
        }
    }

    /// Resets per-peer cached queues when switching chats or forum topics.
    pub fn sync_scope(&mut self, scope: UnreadThingsScope) {
        if self.active_scope != Some(scope) {
            self.active_scope = Some(scope);
            self.mentions.clear();
            self.reactions.clear();
            self.poll_votes.clear();
            self.context_menu = None;
        }
    }

    /// Closes any open right-click mark-all-read popup menu.
    pub fn close_menu(&mut self) -> bool {
        self.context_menu.take().is_some()
    }

    /// Returns the last painted rect for `kind`, if visible on the most recent frame.
    #[must_use]
    pub fn button_rect(&self, kind: UnreadThingsKind) -> Option<Rect> {
        self.last_rects
            .iter()
            .find(|(k, _)| *k == kind)
            .and_then(|(_, r)| *r)
    }

    /// Returns how many unread FABs are currently visible for `counts`.
    #[must_use]
    pub const fn visible_count(counts: UnreadThingsCounts) -> usize {
        (counts.poll_votes > 0) as usize
            + (counts.reactions > 0) as usize
            + (counts.mentions > 0) as usize
    }

    /// Computes the bottom-offset (in px from `list_rect.bottom()`) immediately above the entire
    /// corner FAB stack (scroll-to-bottom + unread poll-votes + reactions + mentions) so auxiliary
    /// corner buttons like Compose Stash never overlap the unread jump buttons.
    #[must_use]
    pub fn stack_top_offset(show_scroll_down: bool, counts: UnreadThingsCounts) -> f32 {
        let slots = usize::from(show_scroll_down) + Self::visible_count(counts);
        if slots == 0 {
            32.0
        } else {
            UNREAD_FAB_BOTTOM_MARGIN
                + UNREAD_FAB_DIAMETER * 0.5
                + (slots as f32) * UNREAD_FAB_STEP_Y
                + 8.0
        }
    }

    fn queue_mut(&mut self, kind: UnreadThingsKind) -> &mut KindQueueState {
        match kind {
            UnreadThingsKind::Mentions => &mut self.mentions,
            UnreadThingsKind::Reactions => &mut self.reactions,
            UnreadThingsKind::PollVotes => &mut self.poll_votes,
        }
    }

    /// Pops the next unread message id for `kind`, loading a page from `backend` when the local
    /// queue is empty or falls below [`UNREAD_THINGS_PRELOAD_IF_LESS`].
    pub fn next_unread_target(
        &mut self,
        scope: UnreadThingsScope,
        kind: UnreadThingsKind,
        backend: &mut dyn HistoryBackend,
    ) -> Option<MsgId> {
        self.sync_scope(scope);
        let need_load = {
            let q = self.queue_mut(kind);
            !q.loaded_once || (q.loaded_ids.len() < UNREAD_THINGS_PRELOAD_IF_LESS && q.has_more)
        };
        if need_load && let Ok(slice) = backend.load_unread_things_page(scope, kind) {
            let q = self.queue_mut(kind);
            q.loaded_once = true;
            q.has_more = slice.has_more;
            for id in slice.loaded_ids {
                if !q.loaded_ids.contains(&id) {
                    q.loaded_ids.push_back(id);
                }
            }
        }
        let target = self.queue_mut(kind).loaded_ids.pop_front();
        // Top-up opportunistically if few items remain and server indicated more.
        let need_preload = {
            let q = self.queue_mut(kind);
            q.loaded_ids.len() < UNREAD_THINGS_PRELOAD_IF_LESS && q.has_more
        };
        if need_preload && let Ok(slice) = backend.load_unread_things_page(scope, kind) {
            let q = self.queue_mut(kind);
            q.has_more = slice.has_more;
            for id in slice.loaded_ids {
                if !q.loaded_ids.contains(&id) {
                    q.loaded_ids.push_back(id);
                }
            }
        }
        target
    }

    /// Marks all unread items of `kind` read via `backend` and clears the local queue.
    pub fn mark_all_read(
        &mut self,
        scope: UnreadThingsScope,
        kind: UnreadThingsKind,
        backend: &mut dyn HistoryBackend,
    ) -> Result<(), String> {
        self.sync_scope(scope);
        let res = match kind {
            UnreadThingsKind::Mentions => {
                backend.read_unread_mentions(scope.peer_id, scope.top_msg_id)
            }
            UnreadThingsKind::Reactions => {
                backend.read_unread_reactions(scope.peer_id, scope.top_msg_id, scope.saved_peer_id)
            }
            UnreadThingsKind::PollVotes => {
                backend.read_unread_poll_votes(scope.peer_id, scope.top_msg_id)
            }
        };
        if res.is_ok() {
            self.queue_mut(kind).clear();
            self.context_menu = None;
        }
        res.map(|_| ())
    }

    /// Paints the stacked `@` mention, heart reaction, and poll-vote FABs in the bottom-right
    /// corner of `list_rect` above the scroll-to-bottom button, returning any triggered event.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        list_rect: Rect,
        scope: UnreadThingsScope,
        show_scroll_down: bool,
        backend: &mut dyn HistoryBackend,
        colors: &HistoryColors,
        reduced_motion: bool,
    ) -> Option<UnreadThingsEvent> {
        self.sync_scope(scope);
        let counts = backend.unread_things_counts(scope);
        let now_secs = ui.ctx().input(|i| i.time);

        let mut event = None;
        let skip = (st::history_unread_things_skip.0 as f32).max(4.0);
        let step_y = UNREAD_FAB_DIAMETER + skip + 8.0; // 54px pitch for badge clearance
        let base_x = list_rect.right() - UNREAD_FAB_RIGHT_MARGIN - UNREAD_FAB_DIAMETER * 0.5;
        let mut center_y =
            list_rect.bottom() - UNREAD_FAB_BOTTOM_MARGIN - UNREAD_FAB_DIAMETER * 0.5;
        if show_scroll_down {
            center_y -= step_y;
        }

        // Order from bottom to top matching `history_widget.cpp`: PollVotes -> Reactions -> Mentions.
        let ordered = [
            (UnreadThingsKind::PollVotes, counts.poll_votes.max(0) as u32),
            (UnreadThingsKind::Reactions, counts.reactions.max(0) as u32),
            (UnreadThingsKind::Mentions, counts.mentions.max(0) as u32),
        ];

        self.last_rects = [
            (UnreadThingsKind::PollVotes, None),
            (UnreadThingsKind::Reactions, None),
            (UnreadThingsKind::Mentions, None),
        ];

        for (slot_idx, (kind, count)) in ordered.into_iter().enumerate() {
            let vis = self
                .queue_mut(kind)
                .step_visibility(count > 0, now_secs, reduced_motion);
            if vis <= 0.01 || count == 0 {
                continue;
            }
            if vis < 0.99 && !reduced_motion {
                ui.ctx().request_repaint();
            }

            let center = Pos2::new(base_x, center_y);
            let btn_rect = Rect::from_center_size(center, Vec2::splat(UNREAD_FAB_DIAMETER));
            self.last_rects[slot_idx] = (kind, Some(btn_rect));

            let id = Id::unique(("tg_hist_unread_fab", scope.peer_id.0, kind_tag(kind)));
            let resp = ui.interact(btn_rect, id, Sense::click());
            let a11y = accessible_label(kind, count);
            resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, a11y.clone()));

            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }

            // Ease-out entrance scale `0.94..1.0` + subtle `0.96` press feedback (respects reduced motion).
            let eased = 1.0 - (1.0 - vis).powi(3);
            let press_scale = if !reduced_motion && resp.is_pointer_button_down_on() {
                0.96
            } else {
                1.0
            };
            let scale = if reduced_motion {
                1.0
            } else {
                (0.94 + 0.06 * eased) * press_scale
            };
            let radius = (UNREAD_FAB_DIAMETER * 0.5) * scale;
            let draw_rect = Rect::from_center_size(center, Vec2::splat(radius * 2.0));

            let shadow = BoxShadow::new(BoxShadowStyle {
                blur_radius: 6.0,
                offset: Point::new(0.0, 2.0),
                opacity: 0.22 * eased,
            });
            shadow.paint(ui.ctx(), ui.painter(), draw_rect, radius);

            let menu_active = self.context_menu.is_some_and(|(k, _)| k == kind);
            let base_bg = if resp.hovered() || menu_active {
                colors.to_down_bg_over
            } else {
                colors.to_down_bg
            };
            let bg = scale_alpha(base_bg, eased);
            let fg = scale_alpha(colors.to_down_fg, eased);
            let stroke_col = scale_alpha(colors.divider, eased);

            ui.painter().circle_filled(center, radius, bg);
            ui.painter()
                .circle_stroke(center, radius, Stroke::new(0.5, stroke_col));

            if resp.has_focus() {
                ui.painter()
                    .circle_stroke(center, radius + 2.0, Stroke::new(2.0, colors.accent));
            }

            paint_unread_kind_icon(ui.painter(), center, kind, fg);

            // Unread counter pill floating above the circle (`paint_unread_badge`).
            let ui_colors = colors.ui_colors();
            let badge_style = UnreadBadgeStyle {
                align: Align::Center,
                ..UnreadBadgeStyle::default()
            };
            let count_str = if count > 999 {
                "999+".to_owned()
            } else {
                count.to_string()
            };
            paint_unread_badge(
                ui.painter(),
                &count_str,
                Pos2::new(center.x, draw_rect.top() - 9.0),
                &badge_style,
                UnreadBadgeKind::Dialogs,
                4,
                &ui_colors,
            );

            let tooltip = tooltip_text(kind, count);
            if resp.secondary_clicked() {
                self.context_menu = Some((kind, btn_rect));
            } else if resp.on_hover_text(tooltip).clicked() {
                self.context_menu = None;
                if let Some(msg_id) = self.next_unread_target(scope, kind, backend) {
                    event = Some(UnreadThingsEvent::JumpTo { kind, msg_id });
                }
            }

            center_y -= step_y;
        }

        // Right-click "Mark all read" popup menu anchored beside the target FAB.
        if let Some((menu_kind, anchor_rect)) = self.context_menu
            && let Some(menu_ev) = self.paint_mark_all_menu(
                ui,
                list_rect,
                scope.peer_id,
                menu_kind,
                anchor_rect,
                colors,
            )
            && self.mark_all_read(scope, menu_kind, backend).is_ok()
        {
            event = Some(menu_ev);
        }

        event
    }

    fn paint_mark_all_menu(
        &mut self,
        ui: &mut Ui,
        list_rect: Rect,
        peer_id: PeerId,
        kind: UnreadThingsKind,
        anchor_rect: Rect,
        colors: &HistoryColors,
    ) -> Option<UnreadThingsEvent> {
        let label = mark_all_read_label(kind);
        let galley = ui.painter().layout_no_wrap(
            label.clone(),
            FontId::new(13.0, family_regular()),
            colors.text_fg,
        );
        let menu_w = (galley.size().x + 32.0).clamp(168.0, (list_rect.width() - 24.0).max(168.0));
        let row_h = 34.0;
        let menu_h = row_h + 8.0;
        let menu_x = (anchor_rect.left() - menu_w - 8.0).max(list_rect.left() + 8.0);
        let menu_y = (anchor_rect.center().y - menu_h * 0.5).clamp(
            list_rect.top() + 8.0,
            (list_rect.bottom() - menu_h - 8.0).max(list_rect.top() + 8.0),
        );
        let menu_rect = Rect::from_min_size(Pos2::new(menu_x, menu_y), Vec2::new(menu_w, menu_h));

        let mut triggered = None;
        Area::new(Id::unique((
            "tg_hist_unread_mark_all_popup",
            peer_id.0,
            kind_tag(kind),
        )))
        .order(Order::Foreground)
        .fixed_pos(menu_rect.min)
        .show(ui.ctx(), |ui| {
            let (_, _) = ui.allocate_exact_size(menu_rect.size(), Sense::hover());
            let shadow = BoxShadow::new(BoxShadowStyle {
                blur_radius: 12.0,
                offset: Point::new(0.0, 4.0),
                opacity: 0.24,
            });
            shadow.paint(ui.ctx(), ui.painter(), menu_rect, 8.0);

            ui.painter()
                .rect_filled(menu_rect, CornerRadius::same(8), colors.bar_bg);
            ui.painter().rect_stroke(
                menu_rect,
                CornerRadius::same(8),
                Stroke::new(1.0, colors.divider),
                StrokeKind::Inside,
            );

            let item_rect = Rect::from_min_size(
                Pos2::new(menu_rect.left() + 4.0, menu_rect.top() + 4.0),
                Vec2::new(menu_rect.width() - 8.0, row_h),
            );
            let item_resp = ui.interact(
                item_rect,
                Id::unique(("tg_hist_unread_mark_all", peer_id.0, kind_tag(kind))),
                Sense::click(),
            );
            item_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label.clone()));
            if item_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(item_rect, CornerRadius::same(6), colors.bar_bg_over);
            }
            if item_resp.has_focus() {
                ui.painter().rect_stroke(
                    item_rect,
                    CornerRadius::same(6),
                    Stroke::new(1.5, colors.accent),
                    StrokeKind::Inside,
                );
            }
            ui.painter().galley(
                Pos2::new(
                    item_rect.left() + 12.0,
                    item_rect.center().y - galley.size().y * 0.5,
                ),
                galley,
                colors.text_fg,
            );

            if item_resp.clicked() {
                triggered = Some(UnreadThingsEvent::MarkAllRead { kind });
            }
        });

        if let Some(ev) = triggered {
            self.context_menu = None;
            return Some(ev);
        }

        // Escape or click outside closes the menu.
        let escape = ui.ctx().input(|i| i.key_pressed(Key::Escape));
        let outside_click = ui.ctx().input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !menu_rect.contains(p) && !anchor_rect.contains(p))
        });
        if escape || outside_click {
            self.context_menu = None;
        }

        None
    }
}

/// Returns the localized tdesktop label for marking all unread items of `kind` read.
#[must_use]
pub fn mark_all_read_label(kind: UnreadThingsKind) -> String {
    match kind {
        UnreadThingsKind::Mentions => tr::lng_context_mark_read_mentions_all(),
        UnreadThingsKind::Reactions => tr::lng_context_mark_read_reactions_all(),
        UnreadThingsKind::PollVotes => tr::lng_context_mark_read_poll_votes_all(),
    }
}

const fn kind_tag(kind: UnreadThingsKind) -> u8 {
    match kind {
        UnreadThingsKind::Mentions => 1,
        UnreadThingsKind::Reactions => 2,
        UnreadThingsKind::PollVotes => 3,
    }
}

fn accessible_label(kind: UnreadThingsKind, count: u32) -> String {
    match kind {
        UnreadThingsKind::Mentions => format!("Jump to unread mention ({count})"),
        UnreadThingsKind::Reactions => format!("Jump to unread reaction ({count})"),
        UnreadThingsKind::PollVotes => format!("Jump to unread poll vote ({count})"),
    }
}

fn tooltip_text(kind: UnreadThingsKind, count: u32) -> String {
    match kind {
        UnreadThingsKind::Mentions => {
            format!("Unread mentions ({count}) · Right-click to mark all read")
        }
        UnreadThingsKind::Reactions => {
            format!("Unread reactions ({count}) · Right-click to read all")
        }
        UnreadThingsKind::PollVotes => {
            format!("Unread poll votes ({count}) · Right-click to read all")
        }
    }
}

fn scale_alpha(color: Color32, factor: f32) -> Color32 {
    let a = (f32::from(color.a()) * factor.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

/// Paints the centered `@`, heart, or poll-vote glyph inside an unread FAB circle.
fn paint_unread_kind_icon(
    painter: &egui::Painter,
    center: Pos2,
    kind: UnreadThingsKind,
    fg: Color32,
) {
    match kind {
        UnreadThingsKind::Mentions => {
            painter.text(
                Pos2::new(center.x, center.y - 0.5),
                Align2::CENTER_CENTER,
                "@",
                FontId::new(19.0, family_semibold()),
                fg,
            );
        }
        UnreadThingsKind::Reactions => {
            // Crisp symmetric heart outline matching `history_unread_reaction`.
            let s = 8.5_f32;
            let pts = vec![
                Pos2::new(center.x, center.y + s * 0.78),
                Pos2::new(center.x - s * 0.88, center.y - s * 0.05),
                Pos2::new(center.x - s * 0.92, center.y - s * 0.52),
                Pos2::new(center.x - s * 0.48, center.y - s * 0.84),
                Pos2::new(center.x, center.y - s * 0.36),
                Pos2::new(center.x + s * 0.48, center.y - s * 0.84),
                Pos2::new(center.x + s * 0.92, center.y - s * 0.52),
                Pos2::new(center.x + s * 0.88, center.y - s * 0.05),
                Pos2::new(center.x, center.y + s * 0.78),
            ];
            painter.add(Shape::line(pts, Stroke::new(1.9, fg)));
        }
        UnreadThingsKind::PollVotes => {
            // Compact 3-row poll bars matching `history_unread_poll_vote-40x40`.
            let left = center.x - 7.5;
            let widths = [11.0_f32, 14.5, 8.5];
            let ys = [center.y - 5.0, center.y, center.y + 5.0];
            for (y, w) in ys.into_iter().zip(widths) {
                painter.circle_filled(Pos2::new(left + 1.5, y), 1.6, fg);
                painter.line_segment(
                    [
                        Pos2::new(left + 5.5, y),
                        Pos2::new(left + 5.5 + w * 0.68, y),
                    ],
                    Stroke::new(2.0, fg),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_top_offset_accounts_for_all_visible_fabs() {
        let none = UnreadThingsCounts::default();
        assert_eq!(HistoryUnreadThingsWidget::visible_count(none), 0);
        assert!(HistoryUnreadThingsWidget::stack_top_offset(false, none) < 40.0);

        let all = UnreadThingsCounts {
            mentions: 2,
            reactions: 3,
            poll_votes: 1,
        };
        assert_eq!(HistoryUnreadThingsWidget::visible_count(all), 3);
        let with_scroll = HistoryUnreadThingsWidget::stack_top_offset(true, all);
        let without_scroll = HistoryUnreadThingsWidget::stack_top_offset(false, all);
        assert!((with_scroll - without_scroll - UNREAD_FAB_STEP_Y).abs() < 0.01);
    }

    #[test]
    fn mark_all_read_labels_match_tg_lang_keys() {
        assert!(!mark_all_read_label(UnreadThingsKind::Mentions).is_empty());
        assert!(!mark_all_read_label(UnreadThingsKind::Reactions).is_empty());
        assert!(!mark_all_read_label(UnreadThingsKind::PollVotes).is_empty());
    }
}
