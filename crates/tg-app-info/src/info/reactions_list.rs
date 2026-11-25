//! Who-reacted peer list (`messages.getMessageReactionsList`) and poll voters list
//! (`messages.getPollVotes`) UI widgets and modal overlays.
//!
//! Port origin:
//! - `Telegram/SourceFiles/info/reactions_list/`
//! - `Telegram/SourceFiles/api/api_who_reacted.cpp`
//! - `Telegram/SourceFiles/info/polls/info_polls_results_widget.cpp`

use std::sync::{Arc, Mutex};

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Key, Margin, Pos2, Rect,
    RichText, ScrollArea, Sense, Stroke, Ui, Vec2,
};
use tg_emoji::EmojiTextureCache;
use tg_lang::tr;
use tg_stats::{ChartPalette, ChartViewType, StatisticalGraph};
use tg_ui::userpic::EmptyUserpic;

use crate::statistics::backend::{
    BoostStatusBundle, BotEarnBundle, ChannelEarnBundle, ChannelStatsBundle, MessageStatsBundle,
    StatsBackend, SupergroupStatsBundle,
};
use crate::statistics::chart_widget::ChartWidgetState;
use tg_app_core::chat_helpers::backend::PaidReactionPrivacy;
use tg_app_core::history::backend::{
    MessageReactionsPage, PollAttachment, PollVoterEntry, PollVotesPage, ReactionCounter,
    ReactionPeerEntry,
};
use tg_app_core::history::sections_model::ScheduleDateTime;
use tg_app_core::history::theme::HistoryColors;
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_core::ui_icons::{paint_close_cross, paint_select_check, paint_star};

const PAID_STAR_GOLD: Color32 = Color32::from_rgb(0xFF, 0xB8, 0x00);

/// Paints a reaction emoji (gold star for `"⭐"`, Twemoji sprite via per-context `tg-emoji` cache).
fn paint_reaction_glyph(
    painter: &egui::Painter,
    ctx: &egui::Context,
    emoji: &str,
    rect: Rect,
    fallback_color: Color32,
) {
    if emoji == "⭐" || emoji == "★" {
        paint_star(
            painter,
            rect.center(),
            rect.width().min(rect.height()) * 0.48,
            PAID_STAR_GOLD,
        );
        return;
    }
    let cache = ctx.data_mut(|d| {
        d.get_temp_mut_or_insert_with(Id::unique("tg_reactions_list_emoji_cache"), || {
            Arc::new(Mutex::new(EmojiTextureCache::new()))
        })
        .clone()
    });
    let painted = cache
        .lock()
        .map(|mut guard| guard.paint_emoji_str(painter, ctx, emoji, rect, Color32::WHITE))
        .unwrap_or(false);
    if !painted {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            emoji,
            FontId::proportional((rect.height() * 0.85).clamp(11.0, 18.0)),
            fallback_color,
        );
    }
}

/// Default page size requested for reactions list and poll voters pagination.
pub const PEER_LIST_PAGE_LIMIT: usize = 50;

// ---------------------------------------------------------------------------
// 1. Who-Reacted List UI (`messages.getMessageReactionsList`)
// ---------------------------------------------------------------------------

/// Events emitted by [`ReactionsListWidget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReactionsListAction {
    /// User closed the who-reacted list modal/panel.
    Close,
    /// User switched the reaction filter tab (`None` = All, `Some(emoji)` = single emoji).
    SelectFilter(Option<String>),
    /// User clicked "Load more" to fetch the next page using `next_offset`.
    LoadMore(String),
    /// User clicked a reacting peer row to open their profile/chat.
    OpenPeer(PeerId),
    /// User toggled paid-reaction leaderboard privacy (`messages.togglePaidReactionPrivacy`).
    TogglePaidPrivacy(PaidReactionPrivacy),
    /// User reported a participant's reaction (`messages.reportReaction`).
    ReportReaction(PeerId),
    /// User removed a participant's reaction from this message (`messages.deleteParticipantReaction`).
    DeleteParticipantReaction(PeerId),
    /// User removed all reactions by a participant in this chat (`messages.deleteParticipantReactions`).
    DeleteParticipantReactions(PeerId),
}

/// Stateful Who-Reacted list widget displaying per-reaction filter tabs, paid-reaction privacy
/// toggle, reaction moderation actions, and reacting peers.
#[derive(Debug, Clone)]
pub struct ReactionsListWidget {
    /// Peer owning the target message.
    pub peer_id: PeerId,
    /// Target message identifier.
    pub msg_id: MsgId,
    /// Summary reaction counters on the message (used for filter tabs).
    pub summary: Vec<ReactionCounter>,
    /// Active reaction filter (`None` = All).
    pub active_filter: Option<String>,
    /// Total reactions count reported by the server for the current filter.
    pub total_count: u32,
    /// Loaded peer reaction rows.
    pub entries: Vec<ReactionPeerEntry>,
    /// Pagination offset for loading the next page, if any.
    pub next_offset: Option<String>,
    /// Optional error message if loading failed.
    pub error: Option<String>,
    /// Active paid-reaction privacy state when the message has paid (`"⭐"`) reactions.
    pub paid_privacy: Option<PaidReactionPrivacy>,
    /// Whether the current user can moderate (delete) participant reactions in this chat.
    pub can_moderate: bool,
    /// Peer ID whose inline moderation actions menu is currently expanded, if any.
    pub active_row_menu: Option<PeerId>,
}

impl ReactionsListWidget {
    /// Creates a new [`ReactionsListWidget`] seeded with the first loaded page.
    #[must_use]
    pub fn new(
        peer_id: PeerId,
        msg_id: MsgId,
        summary: Vec<ReactionCounter>,
        initial_page: MessageReactionsPage,
    ) -> Self {
        let has_paid = summary.iter().any(|r| r.emoji == "⭐" || r.emoji == "★")
            || initial_page
                .entries
                .iter()
                .any(|e| e.emoji == "⭐" || e.emoji == "★");
        Self {
            peer_id,
            msg_id,
            summary,
            active_filter: None,
            total_count: initial_page.total_count,
            entries: initial_page.entries,
            next_offset: initial_page.next_offset,
            error: None,
            paid_privacy: has_paid.then_some(PaidReactionPrivacy::Default),
            can_moderate: false,
            active_row_menu: None,
        }
    }

    /// Configures initial paid-reaction leaderboard privacy state for this modal.
    #[must_use]
    pub fn with_paid_privacy(mut self, privacy: Option<PaidReactionPrivacy>) -> Self {
        self.paid_privacy = privacy;
        self
    }

    /// Enables or disables participant reaction deletion actions (admin moderation).
    #[must_use]
    pub const fn with_moderation(mut self, can_moderate: bool) -> Self {
        self.can_moderate = can_moderate;
        self
    }

    /// Returns `true` if the message carries a paid (`"⭐"`) reaction or an explicit privacy state.
    #[must_use]
    pub fn has_paid_reaction(&self) -> bool {
        self.paid_privacy.is_some()
            || self
                .summary
                .iter()
                .any(|r| r.emoji == "⭐" || r.emoji == "★")
            || self
                .entries
                .iter()
                .any(|e| e.emoji == "⭐" || e.emoji == "★")
    }

    /// Removes all rows belonging to `participant_id` after moderation deletion succeeds.
    pub fn remove_participant_entries(&mut self, participant_id: PeerId) {
        let before = self.entries.len();
        self.entries.retain(|e| e.peer_id != participant_id);
        let removed = before.saturating_sub(self.entries.len()) as u32;
        self.total_count = self.total_count.saturating_sub(removed);
        if self.active_row_menu == Some(participant_id) {
            self.active_row_menu = None;
        }
    }

    /// Replaces loaded entries when switching to `filter` with a fresh `page`.
    pub fn apply_filter_page(&mut self, filter: Option<String>, page: MessageReactionsPage) {
        self.active_filter = filter;
        self.total_count = page.total_count;
        self.entries = page.entries;
        self.next_offset = page.next_offset;
        self.error = None;
    }

    /// Appends a paginated page returned for `next_offset`.
    pub fn append_page(&mut self, page: MessageReactionsPage) {
        self.total_count = page.total_count.max(self.total_count);
        for entry in page.entries {
            let duplicate = self
                .entries
                .iter()
                .any(|e| e.peer_id == entry.peer_id && e.emoji == entry.emoji);
            if !duplicate {
                self.entries.push(entry);
            }
        }
        self.next_offset = page.next_offset;
        self.error = None;
    }

    /// Returns total reaction count across the message summary (falling back to `total_count`).
    #[must_use]
    pub fn total_summary_count(&self) -> u32 {
        let sum: u32 = self.summary.iter().map(|r| r.count).sum();
        if sum > 0 { sum } else { self.total_count }
    }

    /// Renders the who-reacted list contents inside `ui` (embedded in either an info column
    /// or a centered modal dialog).
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<ReactionsListAction> {
        let mut action = None;
        let ui_colors = colors.ui_colors();

        // Header bar
        ui.horizontal(|ui| {
            let total = self.total_summary_count();
            ui.label(
                RichText::new(format!("Reactions ({total})"))
                    .strong()
                    .size(15.0)
                    .color(colors.bold_fg),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (close_rect, close_resp) =
                    ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
                close_resp.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::Button, true, "Close reactions list")
                });
                if close_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    ui.painter()
                        .circle_filled(close_rect.center(), 13.0, colors.bar_bg_over);
                }
                paint_close_cross(ui.painter(), close_rect.center(), 14.0, colors.sub_text_fg);
                if close_resp.on_hover_text(tr::lng_close()).clicked() {
                    action = Some(ReactionsListAction::Close);
                }
            });
        });
        ui.add_space(6.0);

        // Paid-reaction privacy toggle (`messages.togglePaidReactionPrivacy`)
        if self.has_paid_reaction() {
            let current = self.paid_privacy.unwrap_or(PaidReactionPrivacy::Default);
            let show_in_top = !matches!(current, PaidReactionPrivacy::Anonymous);
            let row_w = ui.available_width().max(200.0);
            let (row_rect, row_resp) =
                ui.allocate_exact_size(Vec2::new(row_w, 34.0), Sense::click());
            let toggle_label = tr::lng_paid_react_show_in_top();
            row_resp.widget_info(|| {
                egui::WidgetInfo::selected(egui::Role::CheckBox, true, show_in_top, &toggle_label)
            });
            if row_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            ui.painter().rect_filled(
                row_rect,
                CornerRadius::same(8),
                if row_resp.hovered() {
                    colors.bar_bg_over
                } else {
                    Color32::from_rgba_unmultiplied(
                        colors.accent.r(),
                        colors.accent.g(),
                        colors.accent.b(),
                        14,
                    )
                },
            );
            let star_rect = Rect::from_center_size(
                Pos2::new(row_rect.left() + 16.0, row_rect.center().y),
                Vec2::splat(14.0),
            );
            paint_star(ui.painter(), star_rect.center(), 6.5, PAID_STAR_GOLD);

            let badge_text = if show_in_top {
                "Visible".to_owned()
            } else {
                tr::lng_paid_react_anonymous()
            };
            let badge_w = 72.0;
            let badge_rect = Rect::from_center_size(
                Pos2::new(row_rect.right() - badge_w * 0.5 - 8.0, row_rect.center().y),
                Vec2::new(badge_w, 22.0),
            );
            ui.painter().rect_filled(
                badge_rect,
                CornerRadius::same(11),
                if show_in_top {
                    colors.accent
                } else {
                    colors.bar_bg_over
                },
            );
            if show_in_top {
                paint_select_check(
                    ui.painter(),
                    Pos2::new(badge_rect.left() + 12.0, badge_rect.center().y),
                    11.0,
                    Color32::WHITE,
                );
            }
            ui.painter().text(
                Pos2::new(
                    badge_rect.center().x + if show_in_top { 6.0 } else { 0.0 },
                    badge_rect.center().y,
                ),
                Align2::CENTER_CENTER,
                &badge_text,
                FontId::proportional(10.5),
                if show_in_top {
                    Color32::WHITE
                } else {
                    colors.sub_text_fg
                },
            );

            let lbl_clip = Rect::from_min_max(
                Pos2::new(star_rect.right() + 8.0, row_rect.top()),
                Pos2::new(badge_rect.left() - 6.0, row_rect.bottom()),
            );
            ui.painter().with_clip_rect(lbl_clip).text(
                Pos2::new(lbl_clip.left(), row_rect.center().y),
                Align2::LEFT_CENTER,
                &toggle_label,
                FontId::proportional(12.0),
                colors.bold_fg,
            );
            if row_resp
                .on_hover_text("Toggle whether your account appears in Top Senders for this post")
                .clicked()
            {
                let next = if show_in_top {
                    PaidReactionPrivacy::Anonymous
                } else {
                    PaidReactionPrivacy::Default
                };
                self.paid_privacy = Some(next);
                action = Some(ReactionsListAction::TogglePaidPrivacy(next));
            }
            ui.add_space(6.0);
        }

        // Filter tabs: All + per-emoji tabs (rendered with Twemoji/star icons — zero missing glyphs)
        ScrollArea::horizontal()
            .id_salt(("tg_who_reacted_tabs", self.msg_id.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let all_active = self.active_filter.is_none();
                    if paint_filter_pill(
                        ui,
                        Id::unique(("tg_wr_tab_all", self.msg_id.0)),
                        &format!("All {}", self.total_summary_count()),
                        all_active,
                        colors,
                    ) && !all_active
                    {
                        action = Some(ReactionsListAction::SelectFilter(None));
                    }
                    for (idx, counter) in self.summary.iter().enumerate() {
                        let is_active =
                            self.active_filter.as_deref() == Some(counter.emoji.as_str());
                        if paint_reaction_filter_pill(
                            ui,
                            Id::unique(("tg_wr_tab", self.msg_id.0, idx)),
                            &counter.emoji,
                            counter.count,
                            is_active,
                            colors,
                        ) && !is_active
                        {
                            action = Some(ReactionsListAction::SelectFilter(Some(
                                counter.emoji.clone(),
                            )));
                        }
                    }
                });
            });

        ui.add_space(6.0);
        ui.separator();

        if let Some(ref err) = self.error {
            ui.add_space(8.0);
            ui.label(RichText::new(err).size(12.5).color(colors.attention_fg));
        } else if self.entries.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("No reactions to show yet.")
                        .size(13.0)
                        .color(colors.sub_text_fg),
                );
            });
        } else {
            let can_moderate = self.can_moderate;
            let mut toggle_menu_for: Option<PeerId> = None;
            let active_menu_peer = self.active_row_menu;
            ScrollArea::vertical()
                .id_salt(("tg_who_reacted_rows", self.msg_id.0))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (idx, entry) in self.entries.iter().enumerate() {
                        let menu_open = active_menu_peer == Some(entry.peer_id);
                        match paint_reaction_peer_row(ui, idx, entry, menu_open, colors, &ui_colors)
                        {
                            ReactionRowClick::OpenPeer => {
                                action = Some(ReactionsListAction::OpenPeer(entry.peer_id));
                            }
                            ReactionRowClick::ToggleModerationMenu => {
                                toggle_menu_for = Some(entry.peer_id);
                            }
                            ReactionRowClick::None => {}
                        }
                        if menu_open && !entry.is_my {
                            Frame::NONE
                                .fill(colors.bar_bg_over)
                                .stroke(Stroke::new(1.0, colors.divider))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::symmetric(10, 6))
                                .show(ui, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        let rep_lbl = tr::lng_report_reaction();
                                        if ui
                                            .button(
                                                RichText::new(&rep_lbl)
                                                    .size(11.5)
                                                    .color(colors.active_text_fg),
                                            )
                                            .clicked()
                                        {
                                            action = Some(ReactionsListAction::ReportReaction(
                                                entry.peer_id,
                                            ));
                                            toggle_menu_for = Some(entry.peer_id);
                                        }
                                        if can_moderate {
                                            let del_one_lbl =
                                                tr::lng_context_delete_this_reaction();
                                            if ui
                                                .button(
                                                    RichText::new(&del_one_lbl)
                                                        .size(11.5)
                                                        .color(colors.attention_fg),
                                                )
                                                .clicked()
                                            {
                                                action = Some(
                                                    ReactionsListAction::DeleteParticipantReaction(
                                                        entry.peer_id,
                                                    ),
                                                );
                                                toggle_menu_for = Some(entry.peer_id);
                                            }
                                            if ui
                                                .button(
                                                    RichText::new("Remove all from user")
                                                        .size(11.5)
                                                        .color(colors.attention_fg),
                                                )
                                                .clicked()
                                            {
                                                action = Some(
                                                    ReactionsListAction::DeleteParticipantReactions(
                                                        entry.peer_id,
                                                    ),
                                                );
                                                toggle_menu_for = Some(entry.peer_id);
                                            }
                                        }
                                    });
                                });
                            ui.add_space(4.0);
                        }
                    }
                    if let Some(ref cursor) = self.next_offset {
                        ui.add_space(6.0);
                        ui.vertical_centered(|ui| {
                            if ui
                                .button(
                                    RichText::new("Load more...")
                                        .size(12.5)
                                        .color(colors.active_text_fg),
                                )
                                .clicked()
                            {
                                action = Some(ReactionsListAction::LoadMore(cursor.clone()));
                            }
                        });
                    }
                });
            if let Some(pid) = toggle_menu_for {
                self.active_row_menu = if self.active_row_menu == Some(pid) {
                    None
                } else {
                    Some(pid)
                };
            }
        }

        action
    }

    /// Renders this widget inside a centered modal overlay above the chat column.
    pub fn show_modal(
        &mut self,
        ui: &mut Ui,
        colors: &HistoryColors,
    ) -> Option<ReactionsListAction> {
        let mut action = None;
        let screen = ui.ctx().viewport_rect();
        let modal_w = (screen.width() - 48.0).clamp(320.0, 420.0);
        let modal_h = (screen.height() - 80.0).clamp(300.0, 480.0);
        let modal_pos = Pos2::new(
            screen.center().x - modal_w * 0.5,
            screen.center().y - modal_h * 0.5,
        );

        egui::Area::new(Id::unique(("tg_who_reacted_backdrop", self.msg_id.0)))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ui.ctx(), |ui| {
                let (_, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter().rect_filled(
                    screen,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 110),
                );
                if resp.clicked() {
                    action = Some(ReactionsListAction::Close);
                }
            });

        egui::Area::new(Id::unique(("tg_who_reacted_modal", self.msg_id.0)))
            .order(egui::Order::Tooltip)
            .fixed_pos(modal_pos)
            .show(ui.ctx(), |ui| {
                Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.divider))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_min_size(Vec2::new(modal_w - 28.0, modal_h - 28.0));
                        ui.set_max_size(Vec2::new(modal_w - 28.0, modal_h - 28.0));
                        if let Some(ev) = self.show(ui, colors) {
                            action = Some(ev);
                        }
                    });
            });

        if ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
            action = Some(ReactionsListAction::Close);
        }

        action
    }
}

// ---------------------------------------------------------------------------
// 2. Poll Voters List UI (`messages.getPollVotes`)
// ---------------------------------------------------------------------------

/// Events emitted by [`PollVotersWidget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollVotersAction {
    /// User closed the poll voters modal/panel.
    Close,
    /// User switched the option filter (`None` = All, `Some(idx)` = single poll option).
    SelectOption(Option<usize>),
    /// User clicked "Load more" to fetch the next page using `next_offset`.
    LoadMore(String),
    /// User clicked a voter peer row to open their profile/chat.
    OpenPeer(PeerId),
    /// User clicked Refresh to reload poll results (`messages.getPollResults`).
    ReloadResults,
    /// User toggled the Poll Stats view (`stats.getPollStats`).
    LoadStats {
        /// Whether the active UI theme is dark mode.
        dark: bool,
    },
}

/// Stateful Poll Voters list widget displaying per-option tabs, voter rows, refresh, and
/// interactive poll statistics chart (`info_polls_results_widget.cpp`).
#[derive(Debug, Clone)]
pub struct PollVotersWidget {
    /// Peer owning the poll message.
    pub peer_id: PeerId,
    /// Poll message identifier.
    pub msg_id: MsgId,
    /// Poll metadata (question, options, vote counts).
    pub poll: PollAttachment,
    /// Active option filter (`None` = All options).
    pub active_option: Option<usize>,
    /// Total voters count reported by the server for the current filter.
    pub total_count: u32,
    /// Loaded voter rows.
    pub voters: Vec<PollVoterEntry>,
    /// Pagination offset for loading the next page, if any.
    pub next_offset: Option<String>,
    /// Optional error message if loading failed.
    pub error: Option<String>,
    /// Whether the modal is showing the Poll Stats chart tab instead of Voter rows.
    pub show_stats: bool,
    /// Hydrated interactive statistics chart (`stats.getPollStats`).
    pub stats_chart: Option<ChartWidgetState>,
    /// Optional error message if loading poll stats failed.
    pub stats_error: Option<String>,
}

impl PollVotersWidget {
    /// Creates a new [`PollVotersWidget`] seeded with the first loaded page.
    #[must_use]
    pub fn new(
        peer_id: PeerId,
        msg_id: MsgId,
        poll: PollAttachment,
        initial_page: PollVotesPage,
    ) -> Self {
        Self {
            peer_id,
            msg_id,
            poll,
            active_option: None,
            total_count: initial_page.total_count,
            voters: initial_page.voters,
            next_offset: initial_page.next_offset,
            error: None,
            show_stats: false,
            stats_chart: None,
            stats_error: None,
        }
    }

    /// Replaces poll metadata after `messages.getPollResults` reloads counts/options.
    pub fn apply_poll_results(&mut self, poll: PollAttachment) {
        self.poll = poll;
        self.error = None;
    }

    /// Hydrates the Poll Stats chart widget from a [`StatisticalGraph`] returned by `stats.getPollStats`.
    pub fn apply_poll_stats(&mut self, graph: StatisticalGraph, dark: bool) {
        self.show_stats = true;
        if !graph.error.is_empty() {
            self.stats_chart = None;
            self.stats_error = Some(graph.error);
            return;
        }
        if graph.chart.is_empty() {
            self.stats_chart = None;
            self.stats_error = Some("No poll statistics data available yet.".to_owned());
            return;
        }
        let palette = if dark {
            ChartPalette::dark()
        } else {
            ChartPalette::light()
        };
        let view_type = if graph.chart.lines.len() > 1 {
            ChartViewType::StackBar
        } else {
            ChartViewType::Linear
        };
        self.stats_chart = Some(ChartWidgetState::new(
            tr::lng_polls_stats_title(),
            graph,
            view_type,
            &palette,
        ));
        self.stats_error = None;
    }

    /// Replaces loaded voters when switching to `option_index` with a fresh `page`.
    pub fn apply_option_page(&mut self, option_index: Option<usize>, page: PollVotesPage) {
        self.active_option = option_index;
        self.total_count = page.total_count;
        self.voters = page.voters;
        self.next_offset = page.next_offset;
        self.error = None;
    }

    /// Appends a paginated page returned for `next_offset`.
    pub fn append_page(&mut self, page: PollVotesPage) {
        self.total_count = page.total_count.max(self.total_count);
        for voter in page.voters {
            let duplicate = self
                .voters
                .iter()
                .any(|v| v.peer_id == voter.peer_id && v.option_indices == voter.option_indices);
            if !duplicate {
                self.voters.push(voter);
            }
        }
        self.next_offset = page.next_offset;
        self.error = None;
    }

    /// Renders the poll voters list contents inside `ui`.
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<PollVotersAction> {
        let mut action = None;
        let ui_colors = colors.ui_colors();

        let is_dark = (u16::from(colors.bar_bg.r())
            + u16::from(colors.bar_bg.g())
            + u16::from(colors.bar_bg.b()))
            < 384;

        // Header bar
        ui.horizontal(|ui| {
            let heading = if self.show_stats {
                tr::lng_polls_stats_title()
            } else {
                "Poll Results".to_owned()
            };
            ui.label(
                RichText::new(heading)
                    .strong()
                    .size(15.0)
                    .color(colors.bold_fg),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(RichText::new("×").size(14.0).color(colors.text_fg))
                            .fill(colors.bar_bg_over)
                            .stroke(Stroke::new(1.0, colors.divider))
                            .corner_radius(CornerRadius::same(6)),
                    )
                    .clicked()
                {
                    action = Some(PollVotersAction::Close);
                }
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("Refresh")
                                .size(11.5)
                                .color(colors.active_text_fg),
                        )
                        .fill(colors.bar_bg_over)
                        .stroke(Stroke::new(1.0, colors.divider))
                        .corner_radius(CornerRadius::same(6)),
                    )
                    .clicked()
                {
                    action = Some(PollVotersAction::ReloadResults);
                }
            });
        });

        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(&self.poll.question)
                    .size(13.0)
                    .strong()
                    .color(colors.text_fg),
            )
            .wrap(),
        );
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("{} total votes", self.poll.total_voters))
                    .size(11.5)
                    .color(colors.sub_text_fg),
            );
            ui.add_space(6.0);
            if paint_filter_pill(
                ui,
                Id::unique(("tg_pv_mode_votes", self.msg_id.0)),
                "Voters",
                !self.show_stats,
                colors,
            ) {
                self.show_stats = false;
            }
            if paint_filter_pill(
                ui,
                Id::unique(("tg_pv_mode_stats", self.msg_id.0)),
                &tr::lng_polls_view_stats(),
                self.show_stats,
                colors,
            ) {
                self.show_stats = true;
                if self.stats_chart.is_none() {
                    action = Some(PollVotersAction::LoadStats { dark: is_dark });
                }
            }
        });
        ui.add_space(6.0);

        if self.show_stats {
            ui.separator();
            ui.add_space(6.0);
            if let Some(ref err) = self.stats_error {
                ui.label(RichText::new(err).size(12.5).color(colors.attention_fg));
                ui.add_space(6.0);
                if ui.button("Retry").clicked() {
                    action = Some(PollVotersAction::LoadStats { dark: is_dark });
                }
            } else if let Some(ref mut chart_widget) = self.stats_chart {
                let chart_palette = if is_dark {
                    ChartPalette::dark()
                } else {
                    ChartPalette::light()
                };
                let now_ms = (ui.ctx().input(|i| i.time) * 1000.0) as i64;
                ScrollArea::vertical()
                    .id_salt(("tg_poll_stats_scroll", self.msg_id.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        chart_widget.show(ui, &NoopPollStatsBackend, &chart_palette, now_ms);
                    });
            } else {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Loading poll statistics...")
                            .size(13.0)
                            .color(colors.sub_text_fg),
                    );
                });
            }
            return action;
        }

        // Option filter pills
        let total_opt_votes: u32 = self
            .poll
            .options
            .iter()
            .map(|o| o.votes)
            .sum::<u32>()
            .max(1);
        ScrollArea::horizontal()
            .id_salt(("tg_poll_voters_tabs", self.msg_id.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let all_active = self.active_option.is_none();
                    if paint_filter_pill(
                        ui,
                        Id::unique(("tg_pv_tab_all", self.msg_id.0)),
                        &format!("All ({})", self.poll.total_voters),
                        all_active,
                        colors,
                    ) && !all_active
                    {
                        action = Some(PollVotersAction::SelectOption(None));
                    }
                    for (idx, opt) in self.poll.options.iter().enumerate() {
                        let pct = ((f64::from(opt.votes) / f64::from(total_opt_votes)) * 100.0)
                            .round() as u32;
                        let short = truncate_label(&opt.text, 18);
                        let is_active = self.active_option == Some(idx);
                        if paint_filter_pill(
                            ui,
                            Id::unique(("tg_pv_tab", self.msg_id.0, idx)),
                            &format!("{short} · {pct}% ({})", opt.votes),
                            is_active,
                            colors,
                        ) && !is_active
                        {
                            action = Some(PollVotersAction::SelectOption(Some(idx)));
                        }
                    }
                });
            });

        ui.add_space(6.0);
        ui.separator();

        if let Some(ref err) = self.error {
            ui.add_space(8.0);
            ui.label(RichText::new(err).size(12.5).color(colors.attention_fg));
        } else if self.voters.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("No voters recorded for this option.")
                        .size(13.0)
                        .color(colors.sub_text_fg),
                );
            });
        } else {
            ScrollArea::vertical()
                .id_salt(("tg_poll_voters_rows", self.msg_id.0))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (idx, voter) in self.voters.iter().enumerate() {
                        let opt_summary = format_voter_options(voter, &self.poll);
                        if paint_voter_row(ui, idx, voter, &opt_summary, colors, &ui_colors) {
                            action = Some(PollVotersAction::OpenPeer(voter.peer_id));
                        }
                    }
                    if let Some(ref cursor) = self.next_offset {
                        ui.add_space(6.0);
                        ui.vertical_centered(|ui| {
                            if ui
                                .button(
                                    RichText::new("Load more...")
                                        .size(12.5)
                                        .color(colors.active_text_fg),
                                )
                                .clicked()
                            {
                                action = Some(PollVotersAction::LoadMore(cursor.clone()));
                            }
                        });
                    }
                });
        }

        action
    }

    /// Renders this widget inside a centered modal overlay above the chat column.
    pub fn show_modal(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<PollVotersAction> {
        let mut action = None;
        let screen = ui.ctx().viewport_rect();
        let max_w = if self.show_stats { 500.0 } else { 440.0 };
        let modal_w = (screen.width() - 24.0).clamp(320.0, max_w);
        let modal_h = (screen.height() - 64.0).clamp(320.0, 560.0);
        let modal_pos = Pos2::new(
            screen.center().x - modal_w * 0.5,
            screen.center().y - modal_h * 0.5,
        );

        egui::Area::new(Id::unique(("tg_poll_voters_backdrop", self.msg_id.0)))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ui.ctx(), |ui| {
                let (_, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter().rect_filled(
                    screen,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 110),
                );
                if resp.clicked() {
                    action = Some(PollVotersAction::Close);
                }
            });

        egui::Area::new(Id::unique(("tg_poll_voters_modal", self.msg_id.0)))
            .order(egui::Order::Tooltip)
            .fixed_pos(modal_pos)
            .show(ui.ctx(), |ui| {
                Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.divider))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_min_size(Vec2::new(modal_w - 28.0, modal_h - 28.0));
                        ui.set_max_size(Vec2::new(modal_w - 28.0, modal_h - 28.0));
                        if let Some(ev) = self.show(ui, colors) {
                            action = Some(ev);
                        }
                    });
            });

        if ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
            action = Some(PollVotersAction::Close);
        }

        action
    }
}

// ---------------------------------------------------------------------------
// Internal painting helpers
// ---------------------------------------------------------------------------

struct NoopPollStatsBackend;

impl StatsBackend for NoopPollStatsBackend {
    fn channel_statistics(&self) -> Result<ChannelStatsBundle, String> {
        Err("Not used for poll statistics".to_owned())
    }
    fn supergroup_statistics(&self) -> Result<SupergroupStatsBundle, String> {
        Err("Not used for poll statistics".to_owned())
    }
    fn message_statistics(&self, _message_id: i32) -> Result<MessageStatsBundle, String> {
        Err("Not used for poll statistics".to_owned())
    }
    fn story_statistics(&self, _story_id: i32) -> Result<MessageStatsBundle, String> {
        Err("Not used for poll statistics".to_owned())
    }
    fn load_zoom_graph(
        &self,
        _zoom_token: &str,
        _x_timestamp: f64,
    ) -> Result<StatisticalGraph, String> {
        Err("Zoom is not available for poll statistics".to_owned())
    }
    fn boost_status(&self) -> Result<BoostStatusBundle, String> {
        Err("Not used for poll statistics".to_owned())
    }
    fn load_more_boosts(
        &self,
        _token: &tg_stats::BoostsOffsetToken,
    ) -> Result<tg_stats::BoostsListSlice, String> {
        Err("Not used for poll statistics".to_owned())
    }
    fn channel_earn(&self) -> Result<ChannelEarnBundle, String> {
        Err("Not used for poll statistics".to_owned())
    }
    fn load_more_channel_earn_history(
        &self,
        _token: &str,
    ) -> Result<tg_stats::EarnHistorySlice, String> {
        Err("Not used for poll statistics".to_owned())
    }
    fn bot_earn(&self) -> Result<BotEarnBundle, String> {
        Err("Not used for poll statistics".to_owned())
    }
}

fn paint_filter_pill(
    ui: &mut Ui,
    id: Id,
    label: &str,
    active: bool,
    colors: &HistoryColors,
) -> bool {
    let fg = if active {
        Color32::WHITE
    } else {
        colors.text_fg
    };
    let bg = if active {
        colors.accent
    } else {
        colors.bar_bg_over
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::proportional(12.0), fg);
    let size = Vec2::new((galley.size().x + 18.0).max(46.0), 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, active, label));
    let _ = ui.interact(rect, id, Sense::hover());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().rect_filled(rect, CornerRadius::same(13), bg);
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

fn paint_reaction_filter_pill(
    ui: &mut Ui,
    id: Id,
    emoji: &str,
    count: u32,
    active: bool,
    colors: &HistoryColors,
) -> bool {
    let fg = if active {
        Color32::WHITE
    } else {
        colors.text_fg
    };
    let bg = if active {
        colors.accent
    } else {
        colors.bar_bg_over
    };
    let count_str = count.to_string();
    let galley = ui
        .painter()
        .layout_no_wrap(count_str.clone(), FontId::proportional(12.0), fg);
    let icon_side = 15.0;
    let size = Vec2::new((icon_side + 5.0 + galley.size().x + 18.0).max(52.0), 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let accessible = format!("{emoji} {count_str}");
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, active, &accessible));
    let _ = ui.interact(rect, id, Sense::hover());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().rect_filled(rect, CornerRadius::same(13), bg);
    let content_w = icon_side + 5.0 + galley.size().x;
    let start_x = rect.center().x - content_w * 0.5;
    let icon_rect = Rect::from_center_size(
        Pos2::new(start_x + icon_side * 0.5, rect.center().y),
        Vec2::splat(icon_side),
    );
    paint_reaction_glyph(ui.painter(), ui.ctx(), emoji, icon_rect, fg);
    ui.painter().galley(
        Pos2::new(
            icon_rect.right() + 5.0,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        fg,
    );
    resp.clicked()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReactionRowClick {
    None,
    OpenPeer,
    ToggleModerationMenu,
}

fn paint_reaction_peer_row(
    ui: &mut Ui,
    idx: usize,
    entry: &ReactionPeerEntry,
    menu_open: bool,
    colors: &HistoryColors,
    ui_colors: &dyn tg_ui::stand_in::Colors,
) -> ReactionRowClick {
    let width = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 48.0), Sense::click());
    let _ = ui.interact(
        rect,
        Id::unique(("tg_wr_row", entry.peer_id.0, idx)),
        Sense::hover(),
    );
    if resp.hovered() || menu_open {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), colors.bar_bg_over);
    }

    let av_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 8.0, rect.center().y - 17.0),
        Vec2::splat(34.0),
    );
    EmptyUserpic::for_color_index(entry.color_index, &entry.name, ui_colors)
        .paint_circle(ui.painter(), av_rect);

    // Optional overflow actions button (`...`) on non-self rows for reaction moderation/reporting.
    let mut right_edge = rect.right() - 8.0;
    let mut menu_clicked = false;
    if !entry.is_my {
        let more_rect = Rect::from_center_size(
            Pos2::new(right_edge - 12.0, rect.center().y),
            Vec2::splat(24.0),
        );
        let more_resp = ui.interact(
            more_rect,
            Id::unique(("tg_wr_row_more", entry.peer_id.0, idx)),
            Sense::click(),
        );
        more_resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Button,
                true,
                format!("Reaction actions for {}", entry.name),
            )
        });
        if more_resp.hovered() || menu_open {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(more_rect.center(), 11.0, colors.bar_bg);
        }
        if more_resp.has_focus() {
            ui.painter()
                .circle_stroke(more_rect.center(), 11.0, Stroke::new(1.5, colors.accent));
        }
        let c = more_rect.center();
        for dx in [-4.0_f32, 0.0, 4.0] {
            ui.painter()
                .circle_filled(Pos2::new(c.x + dx, c.y), 1.5, colors.sub_text_fg);
        }
        if more_resp
            .on_hover_text("Report or remove reaction")
            .clicked()
        {
            menu_clicked = true;
        }
        right_edge = more_rect.left() - 6.0;
    }

    let emoji_rect = Rect::from_center_size(
        Pos2::new(right_edge - 11.0, rect.center().y),
        Vec2::splat(20.0),
    );
    paint_reaction_glyph(
        ui.painter(),
        ui.ctx(),
        &entry.emoji,
        emoji_rect,
        colors.text_fg,
    );

    let text_x = av_rect.right() + 10.0;
    let max_text_w = (emoji_rect.left() - text_x - 8.0).max(48.0);

    let name_galley = ui.painter().layout_no_wrap(
        entry.name.clone(),
        FontId::proportional(13.0),
        colors.bold_fg,
    );
    ui.painter()
        .with_clip_rect(Rect::from_min_size(
            Pos2::new(text_x, rect.top() + 7.0),
            Vec2::new(max_text_w, 18.0),
        ))
        .galley(
            Pos2::new(text_x, rect.top() + 7.0),
            name_galley,
            colors.bold_fg,
        );

    let subtitle = format_peer_subtitle(entry.username.as_deref(), entry.date_unix);
    let sub_galley =
        ui.painter()
            .layout_no_wrap(subtitle, FontId::proportional(11.0), colors.sub_text_fg);
    ui.painter()
        .with_clip_rect(Rect::from_min_size(
            Pos2::new(text_x, rect.top() + 25.0),
            Vec2::new(max_text_w, 16.0),
        ))
        .galley(
            Pos2::new(text_x, rect.top() + 25.0),
            sub_galley,
            colors.sub_text_fg,
        );

    if menu_clicked || (!entry.is_my && resp.secondary_clicked()) {
        ReactionRowClick::ToggleModerationMenu
    } else if resp.clicked() {
        ReactionRowClick::OpenPeer
    } else {
        ReactionRowClick::None
    }
}

fn paint_voter_row(
    ui: &mut Ui,
    idx: usize,
    voter: &PollVoterEntry,
    option_summary: &str,
    colors: &HistoryColors,
    ui_colors: &dyn tg_ui::stand_in::Colors,
) -> bool {
    let width = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 50.0), Sense::click());
    let _ = ui.interact(
        rect,
        Id::unique(("tg_pv_row", voter.peer_id.0, idx)),
        Sense::hover(),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), colors.bar_bg_over);
    }

    let av_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 8.0, rect.center().y - 17.0),
        Vec2::splat(34.0),
    );
    EmptyUserpic::for_color_index(voter.color_index, &voter.name, ui_colors)
        .paint_circle(ui.painter(), av_rect);

    let text_x = av_rect.right() + 10.0;
    let max_text_w = (rect.right() - text_x - 10.0).max(60.0);

    let name_galley = ui.painter().layout_no_wrap(
        voter.name.clone(),
        FontId::proportional(13.0),
        colors.bold_fg,
    );
    ui.painter()
        .with_clip_rect(Rect::from_min_size(
            Pos2::new(text_x, rect.top() + 7.0),
            Vec2::new(max_text_w, 18.0),
        ))
        .galley(
            Pos2::new(text_x, rect.top() + 7.0),
            name_galley,
            colors.bold_fg,
        );

    let base_sub = format_peer_subtitle(voter.username.as_deref(), voter.date_unix);
    let sub_line = if option_summary.is_empty() {
        base_sub
    } else if base_sub.is_empty() {
        option_summary.to_owned()
    } else {
        format!("{option_summary} · {base_sub}")
    };
    let sub_galley =
        ui.painter()
            .layout_no_wrap(sub_line, FontId::proportional(11.0), colors.sub_text_fg);
    ui.painter()
        .with_clip_rect(Rect::from_min_size(
            Pos2::new(text_x, rect.top() + 26.0),
            Vec2::new(max_text_w, 16.0),
        ))
        .galley(
            Pos2::new(text_x, rect.top() + 26.0),
            sub_galley,
            colors.sub_text_fg,
        );

    resp.clicked()
}

fn format_peer_subtitle(username: Option<&str>, date_unix: i32) -> String {
    let mut parts = Vec::new();
    if let Some(u) = username
        && !u.is_empty()
    {
        parts.push(format!("@{u}"));
    }
    if date_unix > 0 {
        let dt = ScheduleDateTime::from_unix_utc(i64::from(date_unix));
        parts.push(dt.format_display());
    }
    parts.join(" · ")
}

fn format_voter_options(voter: &PollVoterEntry, poll: &PollAttachment) -> String {
    let labels: Vec<String> = voter
        .option_indices
        .iter()
        .filter_map(|&idx| poll.options.get(idx).map(|o| truncate_label(&o.text, 20)))
        .collect();
    labels.join(", ")
}

fn truncate_label(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}
