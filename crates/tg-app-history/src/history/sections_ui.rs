//! History section views: Scheduled messages, Pinned messages list, Replies/Comments thread
//! (with root channel post on top), Forum topics list & topic history, Saved Messages per-chat
//! sublists, and Channel Admin Log / Recent Actions with filters.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/view/history_view_scheduled_section.cpp`
//! - `Telegram/SourceFiles/history/view/history_view_pinned_section.cpp`
//! - `Telegram/SourceFiles/history/view/history_view_chat_section.cpp`
//! - `Telegram/SourceFiles/data/data_forum*.cpp` & `dialogs/ui/dialogs_layout.cpp` (topic rows)
//! - `Telegram/SourceFiles/data/data_saved_sublist.cpp` & `history_view_subsection_tabs.cpp`
//! - `Telegram/SourceFiles/history/admin_log/history_admin_log_*.cpp`

use std::hash::{Hash, Hasher};

use egui::epaint::{Mesh, Vertex};
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Key, Pos2, Rect, ScrollArea, Sense,
    Shape, Stroke, StrokeKind, TextEdit, Ui, Vec2,
};
use tg_emoji::EmojiTextureCache;
use tg_ui::anim::{SimpleAnimation, Transition, color_lerp, with_alpha};
use tg_ui::fonts::family_semibold;
use tg_ui::icons::paint_style_icon;
use tg_ui::tg_style::{DefaultPaletteResolver, st};
use tg_ui::userpic::{EmptyUserpic, peer_name_color};
use tg_ui::widgets::buttons::{IconKind, paint_icon};

use crate::history::HISTORY_TOP_BAR_HEIGHT;
use crate::history::backend::{HistoryBackend, HistoryMessage, SendMessageExtended, ThreadDraft};
use crate::history::bubble::format_compact_views;
use crate::history::compose_boxes::{ScheduleBoxAction, ScheduleBoxMode, ScheduleBoxState};
use crate::history::sections_model::{
    ADMIN_LOG_FILTER_CATEGORIES, AdminLogEntry, AdminLogFilter, AdminLogFilterFlags,
    FORUM_TOPIC_ICON_COLORS, ForumTopic, ForumTopicIconEntry, ScheduleDateTime,
    SubsectionTabsAppearance, SubsectionTabsMode, ThreadKey, extract_non_emoji_letter,
    format_schedule_badge, pack_subsection_tabs_state, rgb_from_hex, unpack_subsection_tabs_state,
};
use crate::history::text_bridge::parse_compose_markdown;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Reference "now" timestamp used by section schedule dialogs when no live wall clock is injected.
pub const DEFAULT_SECTION_NOW_UNIX: i32 = 1_795_341_600; // 2026-11-22 10:00:00 UTC

/// Active specialized history section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySectionKind {
    /// Regular chat history for `peer_id`.
    Main(PeerId),
    /// Scheduled messages section (`history_view_scheduled_section.cpp`).
    Scheduled {
        /// Target peer id.
        peer_id: PeerId,
        /// Optional forum topic root id.
        topic_root_id: Option<MsgId>,
    },
    /// Pinned messages full list section (`history_view_pinned_section.cpp`).
    Pinned {
        /// Target peer id.
        peer_id: PeerId,
        /// Optional forum topic root id.
        topic_root_id: Option<MsgId>,
    },
    /// Channel post discussion / comments thread (`history_view_chat_section.cpp`).
    Replies {
        /// Channel peer id.
        peer_id: PeerId,
        /// Root post message id shown on top.
        root_msg_id: MsgId,
    },
    /// Forum supergroup topics list (`data/data_forum*.cpp`).
    ForumTopics {
        /// Forum supergroup peer id.
        peer_id: PeerId,
    },
    /// Single forum topic message history.
    ForumTopicHistory {
        /// Forum supergroup peer id.
        peer_id: PeerId,
        /// Selected topic root message id.
        topic_root_id: MsgId,
    },
    /// Saved Messages per-chat sublists view (`data_saved_sublist.cpp`).
    SavedSublists {
        /// Saved Messages self peer id.
        peer_id: PeerId,
    },
    /// History of one Saved Messages per-chat sublist.
    SavedSublistHistory {
        /// Saved Messages self peer id.
        peer_id: PeerId,
        /// Original peer id of the sublist.
        sublist_peer_id: PeerId,
    },
    /// Channel / Supergroup Admin Log ("Recent Actions") with filters.
    AdminLog {
        /// Channel or supergroup peer id.
        peer_id: PeerId,
    },
}

impl HistorySectionKind {
    /// Returns the primary peer id associated with this history section.
    #[must_use]
    pub const fn peer_id(self) -> PeerId {
        match self {
            Self::Main(peer_id)
            | Self::Scheduled { peer_id, .. }
            | Self::Pinned { peer_id, .. }
            | Self::Replies { peer_id, .. }
            | Self::ForumTopics { peer_id }
            | Self::ForumTopicHistory { peer_id, .. }
            | Self::SavedSublists { peer_id }
            | Self::SavedSublistHistory { peer_id, .. }
            | Self::AdminLog { peer_id } => peer_id,
        }
    }
}

/// Events emitted by specialized history sections back to [`crate::history::HistoryWidget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistorySectionAction {
    /// Return to the parent section or main chat history.
    Back,
    /// Navigate into another history section.
    OpenSection(HistorySectionKind),
    /// Jump to a specific message in the main chat history.
    JumpToMessage {
        /// Owning peer id.
        peer_id: PeerId,
        /// Target message id.
        msg_id: MsgId,
    },
    /// Show a status toast notification.
    Toast(String),
}

// ---------------------------------------------------------------------------
// 1. Scheduled Messages Section (`history_view_scheduled_section.cpp`)
// ---------------------------------------------------------------------------

/// Stateful Scheduled Messages section with Send Now, Reschedule, and Delete controls.
#[derive(Debug, Default)]
pub struct ScheduledSectionWidget {
    /// Draft text for scheduling a new message right from this section.
    pub compose_text: String,
    /// Active schedule/reschedule modal box.
    pub schedule_box: Option<ScheduleBoxState>,
}

impl ScheduledSectionWidget {
    /// Renders the Scheduled Messages section inside `rect`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        peer_id: PeerId,
        topic_root_id: Option<MsgId>,
        backend: &mut dyn HistoryBackend,
        colors: &HistoryColors,
    ) -> Option<HistorySectionAction> {
        let mut action = None;
        let peer_info = backend.peer_info(peer_id);
        let is_saved = peer_info
            .as_ref()
            .is_some_and(|p| p.summary.is_saved_messages);
        let peer_title = peer_info
            .as_ref()
            .map_or_else(|| "Chat".to_owned(), |p| p.summary.title.clone());
        let can_when_online = peer_info.as_ref().is_some_and(|p| {
            matches!(p.summary.kind, tg_app_core::stand_in::types::PeerKind::User)
                && !p.summary.is_saved_messages
        });

        let messages = backend.scheduled_messages(peer_id, topic_root_id);

        // Top bar
        let top_rect =
            Rect::from_min_size(rect.min, Vec2::new(rect.width(), HISTORY_TOP_BAR_HEIGHT));
        let heading = if is_saved {
            "Reminders"
        } else {
            "Scheduled Messages"
        };
        let subtitle = format!("{} scheduled · {}", messages.len(), peer_title);
        if paint_section_top_bar(ui, top_rect, heading, &subtitle, None, colors) {
            action = Some(HistorySectionAction::Back);
        }

        // Bottom compose bar for scheduling a new message directly
        let compose_h = 52.0;
        let compose_rect = Rect::from_min_size(
            Pos2::new(rect.left(), rect.bottom() - compose_h),
            Vec2::new(rect.width(), compose_h),
        );
        let list_rect = Rect::from_min_max(
            Pos2::new(rect.left(), top_rect.bottom()),
            Pos2::new(rect.right(), compose_rect.top()),
        );

        ui.painter()
            .rect_filled(list_rect, CornerRadius::ZERO, colors.canvas_bg);

        if messages.is_empty() {
            paint_empty_section_pill(
                ui.painter(),
                list_rect,
                "No scheduled messages yet.\nCompose below to schedule one.",
                colors,
            );
        } else {
            let mut send_now_id = None;
            let mut reschedule_item = None;
            let mut delete_id = None;

            ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
                ScrollArea::vertical()
                    .id_salt(("tg_sched_list", peer_id.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(12.0);
                        for msg in &messages {
                            let sched_ts = msg.extra.scheduled_date.unwrap_or(msg.date_unix);
                            let badge = format_schedule_badge(sched_ts);

                            ui.horizontal(|ui| {
                                ui.add_space(18.0);
                                egui::Frame::NONE
                                    .fill(colors.msg_out_bg)
                                    .stroke(Stroke::new(1.0, colors.divider))
                                    .corner_radius(CornerRadius::same(12))
                                    .inner_margin(egui::Margin::symmetric(14, 10))
                                    .show(ui, |ui| {
                                        ui.vertical(|ui| {
                                            ui.set_max_width((list_rect.width() - 56.0).min(540.0));
                                            ui.set_min_width((list_rect.width() - 56.0).min(360.0));
                                            ui.horizontal(|ui| {
                                                tg_app_core::ui_icons::inline_icon_label(
                                                    ui,
                                                    13.0,
                                                    |p, c, s| {
                                                        tg_app_core::ui_icons::paint_style_tinted(
                                                            p,
                                                            Rect::from_center_size(
                                                                c,
                                                                Vec2::splat(s),
                                                            ),
                                                            tg_ui::tg_style::st::menu_icon_schedule,
                                                            colors.active_text_fg,
                                                        );
                                                    },
                                                    egui::RichText::new(&badge)
                                                        .strong()
                                                        .size(11.5)
                                                        .color(colors.active_text_fg),
                                                );
                                                if msg.extra.silent {
                                                    tg_app_core::ui_icons::inline_icon_label(
                                                        ui,
                                                        13.0,
                                                        |p, c, s| {
                                                            tg_app_core::ui_icons::paint_style_tinted(
                                                                p,
                                                                Rect::from_center_size(
                                                                    c,
                                                                    Vec2::splat(s),
                                                                ),
                                                                tg_ui::tg_style::st::menu_icon_mute,
                                                                colors.sub_text_fg,
                                                            );
                                                        },
                                                        egui::RichText::new("Silent")
                                                            .size(11.0)
                                                            .color(colors.sub_text_fg),
                                                    );
                                                }
                                            });
                                            ui.add_space(4.0);
                                            let body = if msg.text.is_empty() {
                                                msg.preview_snippet()
                                            } else {
                                                msg.text.text.clone()
                                            };
                                            ui.label(
                                                egui::RichText::new(body)
                                                    .size(13.5)
                                                    .color(colors.text_fg),
                                            );
                                            ui.add_space(8.0);
                                            ui.horizontal(|ui| {
                                                if ui.small_button("Send Now").clicked() {
                                                    send_now_id = Some(msg.id);
                                                }
                                                if ui.small_button("Reschedule").clicked() {
                                                    reschedule_item = Some((msg.id, sched_ts));
                                                }
                                                if ui
                                                    .small_button(
                                                        egui::RichText::new("Delete")
                                                            .color(colors.attention_fg),
                                                    )
                                                    .clicked()
                                                {
                                                    delete_id = Some(msg.id);
                                                }
                                            });
                                        });
                                    });
                            });
                            ui.add_space(8.0);
                        }
                        ui.add_space(12.0);
                    });
            });

            if let Some(id) = send_now_id {
                match backend.send_scheduled_now(peer_id, id) {
                    Ok(_) => {
                        action = Some(HistorySectionAction::Toast(
                            "Scheduled message sent immediately".to_owned(),
                        ));
                    }
                    Err(err) => action = Some(HistorySectionAction::Toast(err)),
                }
            }
            if let Some((id, current_ts)) = reschedule_item {
                self.schedule_box = Some(ScheduleBoxState::for_reschedule(
                    id,
                    current_ts,
                    DEFAULT_SECTION_NOW_UNIX,
                    can_when_online,
                    peer_title.clone(),
                ));
            }
            if let Some(id) = delete_id {
                let _ = backend.delete_scheduled_messages(peer_id, &[id]);
                action = Some(HistorySectionAction::Toast(
                    "Scheduled message deleted".to_owned(),
                ));
            }
        }

        // Compose strip to add a new scheduled message
        ui.painter()
            .rect_filled(compose_rect, CornerRadius::ZERO, colors.bar_bg);
        ui.painter().line_segment(
            [
                Pos2::new(compose_rect.left(), compose_rect.top()),
                Pos2::new(compose_rect.right(), compose_rect.top()),
            ],
            Stroke::new(1.0, colors.divider),
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(compose_rect.shrink2(Vec2::new(12.0, 10.0))),
            |ui| {
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        TextEdit::singleline(&mut self.compose_text)
                            .desired_width((compose_rect.width() - 124.0).max(120.0))
                            .hint_text("Write a message to schedule..."),
                    );
                    let enter = resp.lost_focus() && ui.ctx().input(|i| i.key_pressed(Key::Enter));
                    if (ui.button("Schedule").clicked() || enter)
                        && !self.compose_text.trim().is_empty()
                    {
                        self.schedule_box = Some(ScheduleBoxState::new(
                            if is_saved {
                                ScheduleBoxMode::SetReminder
                            } else {
                                ScheduleBoxMode::ScheduleMessage
                            },
                            DEFAULT_SECTION_NOW_UNIX,
                            can_when_online,
                            peer_title.clone(),
                        ));
                    }
                });
            },
        );

        // Schedule / Reschedule modal
        if let Some(ref mut box_state) = self.schedule_box
            && let Some(ev) = box_state.show(ui, colors, false)
        {
            match ev {
                ScheduleBoxAction::Confirmed { mode, target } => match mode {
                    ScheduleBoxMode::Reschedule(msg_id) => {
                        match backend.reschedule_message(peer_id, msg_id, target) {
                            Ok(_) => {
                                action = Some(HistorySectionAction::Toast(
                                    "Message rescheduled".to_owned(),
                                ));
                            }
                            Err(err) => action = Some(HistorySectionAction::Toast(err)),
                        }
                    }
                    ScheduleBoxMode::ScheduleMessage | ScheduleBoxMode::SetReminder => {
                        let raw = self.compose_text.trim().to_owned();
                        if !raw.is_empty() {
                            let req = SendMessageExtended {
                                text: parse_compose_markdown(&raw),
                                schedule_date: Some(target.to_time_id()),
                                thread_key: topic_root_id.map(|tid| ThreadKey::Topic {
                                    peer_id,
                                    topic_root_id: tid,
                                }),
                                ..SendMessageExtended::default()
                            };
                            if backend.send_message_extended(peer_id, req).is_ok() {
                                self.compose_text.clear();
                                action = Some(HistorySectionAction::Toast(format_schedule_badge(
                                    target.to_time_id(),
                                )));
                            }
                        }
                    }
                },
                ScheduleBoxAction::Cancelled => {}
            }
            self.schedule_box = None;
        }

        action
    }
}

// ---------------------------------------------------------------------------
// 2. Pinned Messages List Section (`history_view_pinned_section.cpp`)
// ---------------------------------------------------------------------------

/// Stateful Pinned Messages full-list section with Jump, Unpin, and Unpin All controls.
#[derive(Debug, Default)]
pub struct PinnedSectionWidget {
    confirm_unpin_all: bool,
}

impl PinnedSectionWidget {
    /// Renders the Pinned Messages list section inside `rect`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        peer_id: PeerId,
        topic_root_id: Option<MsgId>,
        backend: &mut dyn HistoryBackend,
        colors: &HistoryColors,
    ) -> Option<HistorySectionAction> {
        let mut action = None;
        let peer_info = backend.peer_info(peer_id);
        let can_pin = peer_info.as_ref().is_some_and(|p| p.can_pin);
        let peer_title = peer_info
            .as_ref()
            .map_or_else(|| "Chat".to_owned(), |p| p.summary.title.clone());
        let pinned = backend.pinned_full_messages(peer_id, topic_root_id);

        let top_rect =
            Rect::from_min_size(rect.min, Vec2::new(rect.width(), HISTORY_TOP_BAR_HEIGHT));
        let heading = format!("{} Pinned Message(s)", pinned.len());
        if paint_section_top_bar(ui, top_rect, &heading, &peer_title, None, colors) {
            action = Some(HistorySectionAction::Back);
        }

        let bottom_h = if can_pin && !pinned.is_empty() {
            48.0
        } else {
            0.0
        };
        let list_rect = Rect::from_min_max(
            Pos2::new(rect.left(), top_rect.bottom()),
            Pos2::new(rect.right(), rect.bottom() - bottom_h),
        );
        ui.painter()
            .rect_filled(list_rect, CornerRadius::ZERO, colors.canvas_bg);

        if pinned.is_empty() {
            paint_empty_section_pill(
                ui.painter(),
                list_rect,
                "No pinned messages in this chat.",
                colors,
            );
        } else {
            let mut jump_id = None;
            let mut unpin_id = None;

            ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
                ScrollArea::vertical()
                    .id_salt(("tg_pinned_section", peer_id.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(12.0);
                        for msg in &pinned {
                            ui.horizontal(|ui| {
                                ui.add_space(18.0);
                                let bg = if msg.outgoing {
                                    colors.msg_out_bg
                                } else {
                                    colors.msg_in_bg
                                };
                                egui::Frame::NONE
                                    .fill(bg)
                                    .stroke(Stroke::new(1.0, colors.divider))
                                    .corner_radius(CornerRadius::same(12))
                                    .inner_margin(egui::Margin::symmetric(14, 10))
                                    .show(ui, |ui| {
                                        ui.vertical(|ui| {
                                            ui.set_max_width((list_rect.width() - 56.0).min(540.0));
                                            ui.set_min_width((list_rect.width() - 56.0).min(360.0));
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    egui::RichText::new(&msg.sender_name)
                                                        .strong()
                                                        .size(12.5)
                                                        .color(colors.active_text_fg),
                                                );
                                                ui.with_layout(
                                                    egui::Layout::right_to_left(
                                                        egui::Align::Center,
                                                    ),
                                                    |ui| {
                                                        ui.label(
                                                            egui::RichText::new(
                                                                msg.formatted_time(),
                                                            )
                                                            .size(11.0)
                                                            .color(colors.sub_text_fg),
                                                        );
                                                    },
                                                );
                                            });
                                            ui.add_space(4.0);
                                            let body = if msg.text.is_empty() {
                                                msg.preview_snippet()
                                            } else {
                                                msg.text.text.clone()
                                            };
                                            ui.label(
                                                egui::RichText::new(body)
                                                    .size(13.5)
                                                    .color(colors.text_fg),
                                            );
                                            ui.add_space(6.0);
                                            ui.horizontal(|ui| {
                                                if ui.small_button("Jump to message").clicked() {
                                                    jump_id = Some(msg.id);
                                                }
                                                if can_pin
                                                    && ui
                                                        .small_button(
                                                            egui::RichText::new("Unpin")
                                                                .color(colors.attention_fg),
                                                        )
                                                        .clicked()
                                                {
                                                    unpin_id = Some(msg.id);
                                                }
                                            });
                                        });
                                    });
                            });
                            ui.add_space(8.0);
                        }
                        ui.add_space(10.0);
                    });
            });

            if let Some(id) = jump_id {
                action = Some(HistorySectionAction::JumpToMessage {
                    peer_id,
                    msg_id: id,
                });
            }
            if let Some(id) = unpin_id {
                let _ = backend.pin_message(peer_id, id, true);
                action = Some(HistorySectionAction::Toast("Message unpinned".to_owned()));
            }
        }

        if bottom_h > 0.0 {
            let bot_rect = Rect::from_min_size(
                Pos2::new(rect.left(), rect.bottom() - bottom_h),
                Vec2::new(rect.width(), bottom_h),
            );
            ui.painter()
                .rect_filled(bot_rect, CornerRadius::ZERO, colors.bar_bg);
            ui.painter().line_segment(
                [
                    Pos2::new(bot_rect.left(), bot_rect.top()),
                    Pos2::new(bot_rect.right(), bot_rect.top()),
                ],
                Stroke::new(1.0, colors.divider),
            );
            let resp = ui.interact(bot_rect, Id::unique("tg_unpin_all_btn"), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(bot_rect, CornerRadius::ZERO, colors.bar_bg_over);
            }
            let label = if self.confirm_unpin_all {
                format!("CONFIRM: UNPIN ALL {} MESSAGES", pinned.len())
            } else {
                format!("UNPIN ALL {} MESSAGES", pinned.len())
            };
            ui.painter().text(
                bot_rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(12.5),
                if self.confirm_unpin_all {
                    colors.attention_fg
                } else {
                    colors.active_text_fg
                },
            );
            if resp.clicked() {
                if self.confirm_unpin_all {
                    let _ = backend.unpin_all_messages(peer_id, topic_root_id);
                    self.confirm_unpin_all = false;
                    action = Some(HistorySectionAction::Toast(
                        "All messages unpinned".to_owned(),
                    ));
                } else {
                    self.confirm_unpin_all = true;
                }
            }
        }

        action
    }
}

// ---------------------------------------------------------------------------
// 3. Replies / Comments Thread Section (`history_view_chat_section.cpp`)
// ---------------------------------------------------------------------------

/// Discussion / comments thread of a channel post, showing the root channel post on top,
/// comments divider, replies list, and a per-thread compose bar.
#[derive(Debug, Default)]
pub struct RepliesSectionWidget {
    loaded_thread: Option<(PeerId, MsgId)>,
    /// Compose input text for this comments thread.
    pub compose_text: String,
    /// Optional reply target comment id inside this thread.
    pub reply_to_comment: Option<(MsgId, String)>,
    /// Selected Send-As peer for comments.
    pub selected_send_as: Option<PeerId>,
}

impl RepliesSectionWidget {
    /// Renders the Comments Thread section inside `rect`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        peer_id: PeerId,
        root_msg_id: MsgId,
        backend: &mut dyn HistoryBackend,
        colors: &HistoryColors,
    ) -> Option<HistorySectionAction> {
        let mut action = None;
        let thread_key = ThreadKey::Replies {
            peer_id,
            root_msg_id,
        };

        // Load per-thread draft on thread switch.
        if self.loaded_thread != Some((peer_id, root_msg_id)) {
            if let Some((prev_p, prev_r)) = self.loaded_thread {
                let prev_key = ThreadKey::Replies {
                    peer_id: prev_p,
                    root_msg_id: prev_r,
                };
                let draft = (!self.compose_text.trim().is_empty()).then(|| ThreadDraft {
                    text: self.compose_text.clone(),
                    reply_to: self.reply_to_comment.as_ref().map(|(id, _)| *id),
                    reply_quote: None,
                    no_webpage: false,
                });
                backend.save_thread_draft(prev_key, draft);
            }
            self.loaded_thread = Some((peer_id, root_msg_id));
            if let Some(d) = backend.load_thread_draft(thread_key) {
                self.compose_text = d.text;
                self.reply_to_comment = d.reply_to.map(|id| (id, format!("Comment #{}", id.0)));
            } else {
                self.compose_text.clear();
                self.reply_to_comment = None;
            }
        }

        let peer_title = backend
            .peer_info(peer_id)
            .map_or_else(|| "Channel".to_owned(), |p| p.summary.title);
        let data = backend.replies_root_and_comments(peer_id, root_msg_id);
        let comment_count = data.as_ref().map_or(0, |(_, c)| c.len());

        // Top bar
        let top_rect =
            Rect::from_min_size(rect.min, Vec2::new(rect.width(), HISTORY_TOP_BAR_HEIGHT));
        let subtitle = format!("{comment_count} comments · {peer_title}");
        if paint_section_top_bar(ui, top_rect, "Comments", &subtitle, None, colors) {
            action = Some(HistorySectionAction::Back);
        }

        let reply_bar_h = if self.reply_to_comment.is_some() {
            30.0
        } else {
            0.0
        };
        let compose_h = 52.0 + reply_bar_h;
        let compose_rect = Rect::from_min_size(
            Pos2::new(rect.left(), rect.bottom() - compose_h),
            Vec2::new(rect.width(), compose_h),
        );
        let list_rect = Rect::from_min_max(
            Pos2::new(rect.left(), top_rect.bottom()),
            Pos2::new(rect.right(), compose_rect.top()),
        );
        ui.painter()
            .rect_filled(list_rect, CornerRadius::ZERO, colors.canvas_bg);

        let ui_colors = colors.ui_colors();

        if let Some((root_post, comments)) = data {
            let mut reply_pick = None;
            ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
                ScrollArea::vertical()
                    .id_salt(("tg_replies_scroll", peer_id.0, root_msg_id.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(12.0);

                        // 1. Root channel post card pinned at top of discussion!
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            egui::Frame::NONE
                                .fill(colors.msg_in_bg)
                                .stroke(Stroke::new(1.5, colors.accent))
                                .corner_radius(CornerRadius::same(14))
                                .inner_margin(egui::Margin::symmetric(16, 12))
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        let card_w = (list_rect.width() - 64.0).min(560.0);
                                        ui.set_max_width(card_w);
                                        ui.set_min_width(card_w.min(460.0));
                                        ui.horizontal(|ui| {
                                            let (av_r, _) = ui.allocate_exact_size(
                                                Vec2::splat(32.0),
                                                Sense::hover(),
                                            );
                                            EmptyUserpic::for_peer(
                                                root_post.peer_id.bare(),
                                                &root_post.sender_name,
                                                &ui_colors,
                                            )
                                            .paint_circle(ui.painter(), av_r);
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    egui::RichText::new(&root_post.sender_name)
                                                        .strong()
                                                        .size(13.5)
                                                        .color(colors.active_text_fg),
                                                );
                                                let views_str =
                                                    root_post.views.map_or_else(String::new, |v| {
                                                        format!(
                                                            " · {} views",
                                                            format_compact_views(v)
                                                        )
                                                    });
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "Channel Post · {}{}",
                                                        root_post.formatted_time(),
                                                        views_str
                                                    ))
                                                    .size(11.0)
                                                    .color(colors.sub_text_fg),
                                                );
                                            });
                                        });
                                        ui.add_space(8.0);
                                        ui.label(
                                            egui::RichText::new(&root_post.text.text)
                                                .size(13.5)
                                                .color(colors.text_fg),
                                        );
                                        if !root_post.reactions.is_empty() {
                                            ui.add_space(8.0);
                                            ui.horizontal_wrapped(|ui| {
                                                for r in &root_post.reactions {
                                                    let bg = if r.chosen {
                                                        colors.reaction_chosen_bg
                                                    } else {
                                                        colors.reaction_in_bg
                                                    };
                                                    let fg = if r.chosen {
                                                        colors.reaction_chosen_fg
                                                    } else {
                                                        colors.active_text_fg
                                                    };
                                                    egui::Frame::NONE
                                                        .fill(bg)
                                                        .corner_radius(CornerRadius::same(11))
                                                        .inner_margin(egui::Margin::symmetric(8, 3))
                                                        .show(ui, |ui| {
                                                            ui.label(
                                                                egui::RichText::new(format!(
                                                                    "{} {}",
                                                                    r.emoji, r.count
                                                                ))
                                                                .size(11.5)
                                                                .color(fg),
                                                            );
                                                        });
                                                }
                                            });
                                        }
                                    });
                                });
                        });

                        ui.add_space(10.0);

                        // 2. Centered Discussion / Comments separator pill
                        let sep_label = if comments.is_empty() {
                            "No comments yet".to_owned()
                        } else if comments.len() == 1 {
                            "1 Comment".to_owned()
                        } else {
                            format!("{} Comments", comments.len())
                        };
                        let (sep_row, _) = ui.allocate_exact_size(
                            Vec2::new(list_rect.width(), 26.0),
                            Sense::hover(),
                        );
                        let sep_galley = ui.painter().layout_no_wrap(
                            sep_label,
                            FontId::proportional(12.0),
                            colors.service_fg,
                        );
                        let pill_rect = Rect::from_center_size(
                            sep_row.center(),
                            sep_galley.size() + Vec2::new(26.0, 10.0),
                        );
                        ui.painter().rect_filled(
                            pill_rect,
                            CornerRadius::same(12),
                            colors.service_bg,
                        );
                        ui.painter().galley(
                            Pos2::new(
                                pill_rect.center().x - sep_galley.size().x * 0.5,
                                pill_rect.center().y - sep_galley.size().y * 0.5,
                            ),
                            sep_galley,
                            colors.service_fg,
                        );

                        ui.add_space(8.0);

                        // 3. Replies list
                        for msg in &comments {
                            let bubble_bg = if msg.outgoing {
                                colors.msg_out_bg
                            } else {
                                colors.msg_in_bg
                            };
                            let color_idx = tg_ui::userpic::decide_color_index(msg.from_id.bare());
                            let name_fg = peer_name_color(color_idx, &ui_colors);
                            let bubble_w = (list_rect.width() - 120.0).min(450.0);

                            ui.horizontal(|ui| {
                                if msg.outgoing {
                                    let spacer = (list_rect.width() - bubble_w - 40.0).max(48.0);
                                    ui.add_space(spacer);
                                } else {
                                    ui.add_space(14.0);
                                    let (av_r, _) =
                                        ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
                                    EmptyUserpic::for_peer(
                                        msg.from_id.bare(),
                                        &msg.sender_name,
                                        &ui_colors,
                                    )
                                    .paint_circle(ui.painter(), av_r);
                                    ui.add_space(4.0);
                                }

                                egui::Frame::NONE
                                    .fill(bubble_bg)
                                    .stroke(Stroke::new(0.8, colors.divider))
                                    .corner_radius(CornerRadius::same(13))
                                    .inner_margin(egui::Margin::symmetric(12, 8))
                                    .show(ui, |ui| {
                                        ui.vertical(|ui| {
                                            ui.set_max_width(bubble_w);
                                            ui.set_min_width(bubble_w.min(280.0));
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    egui::RichText::new(&msg.sender_name)
                                                        .strong()
                                                        .size(12.5)
                                                        .color(if msg.outgoing {
                                                            colors.msg_out_reply_bar
                                                        } else {
                                                            name_fg
                                                        }),
                                                );
                                                if let Some(ref sa) = msg.extra.send_as_name {
                                                    ui.label(
                                                        egui::RichText::new(format!("· as {sa}"))
                                                            .size(10.5)
                                                            .color(colors.sub_text_fg),
                                                    );
                                                }
                                            });
                                            if let Some(ref rep) = msg.reply {
                                                ui.add_space(3.0);
                                                egui::Frame::NONE
                                                    .fill(colors.msg_in_reply_bg)
                                                    .corner_radius(CornerRadius::same(4))
                                                    .inner_margin(egui::Margin::symmetric(8, 4))
                                                    .show(ui, |ui| {
                                                        ui.vertical(|ui| {
                                                            ui.label(
                                                                egui::RichText::new(
                                                                    &rep.author_name,
                                                                )
                                                                .strong()
                                                                .size(11.0)
                                                                .color(colors.active_text_fg),
                                                            );
                                                            ui.label(
                                                                egui::RichText::new(
                                                                    rep.quote_text
                                                                        .as_ref()
                                                                        .map(|q| format!("“{q}”"))
                                                                        .unwrap_or_else(|| {
                                                                            rep.preview_text.clone()
                                                                        }),
                                                                )
                                                                .size(11.5)
                                                                .color(colors.sub_text_fg),
                                                            );
                                                        });
                                                    });
                                                ui.add_space(4.0);
                                            }
                                            ui.label(
                                                egui::RichText::new(&msg.text.text)
                                                    .size(13.5)
                                                    .color(colors.text_fg),
                                            );
                                            ui.add_space(4.0);
                                            ui.horizontal(|ui| {
                                                for r in &msg.reactions {
                                                    egui::Frame::NONE
                                                        .fill(if r.chosen {
                                                            colors.reaction_chosen_bg
                                                        } else {
                                                            colors.reaction_in_bg
                                                        })
                                                        .corner_radius(CornerRadius::same(10))
                                                        .inner_margin(egui::Margin::symmetric(6, 2))
                                                        .show(ui, |ui| {
                                                            ui.label(
                                                                egui::RichText::new(format!(
                                                                    "{} {}",
                                                                    r.emoji, r.count
                                                                ))
                                                                .size(11.0)
                                                                .color(if r.chosen {
                                                                    colors.reaction_chosen_fg
                                                                } else {
                                                                    colors.active_text_fg
                                                                }),
                                                            );
                                                        });
                                                }
                                                ui.with_layout(
                                                    egui::Layout::right_to_left(
                                                        egui::Align::Center,
                                                    ),
                                                    |ui| {
                                                        ui.label(
                                                            egui::RichText::new(
                                                                msg.formatted_time(),
                                                            )
                                                            .size(10.5)
                                                            .color(colors.sub_text_fg),
                                                        );
                                                        if ui
                                                            .small_button("Reply")
                                                            .on_hover_text("Reply to this comment")
                                                            .clicked()
                                                        {
                                                            reply_pick = Some((
                                                                msg.id,
                                                                msg.sender_name.clone(),
                                                            ));
                                                        }
                                                    },
                                                );
                                            });
                                        });
                                    });
                            });
                            ui.add_space(8.0);
                        }
                        ui.add_space(12.0);
                    });
            });
            if let Some(pick) = reply_pick {
                self.reply_to_comment = Some(pick);
            }
        } else {
            paint_empty_section_pill(
                ui.painter(),
                list_rect,
                "Discussion thread not found.",
                colors,
            );
        }

        // Thread compose bar
        ui.painter()
            .rect_filled(compose_rect, CornerRadius::ZERO, colors.bar_bg);
        ui.painter().line_segment(
            [
                Pos2::new(compose_rect.left(), compose_rect.top()),
                Pos2::new(compose_rect.right(), compose_rect.top()),
            ],
            Stroke::new(1.0, colors.divider),
        );

        ui.scope_builder(
            egui::UiBuilder::new().max_rect(compose_rect.shrink2(Vec2::new(12.0, 8.0))),
            |ui| {
                if let Some((_, ref author)) = self.reply_to_comment.clone() {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("Replying to {author}"))
                                .size(11.5)
                                .color(colors.active_text_fg),
                        );
                        if ui.small_button("×").clicked() {
                            self.reply_to_comment = None;
                        }
                    });
                }
                ui.horizontal(|ui| {
                    let prev = self.compose_text.clone();
                    let resp = ui.add(
                        TextEdit::singleline(&mut self.compose_text)
                            .desired_width((compose_rect.width() - 104.0).max(120.0))
                            .hint_text("Leave a comment..."),
                    );
                    if self.compose_text != prev {
                        let draft = (!self.compose_text.trim().is_empty()).then(|| ThreadDraft {
                            text: self.compose_text.clone(),
                            reply_to: self.reply_to_comment.as_ref().map(|(id, _)| *id),
                            reply_quote: None,
                            no_webpage: false,
                        });
                        backend.save_thread_draft(thread_key, draft);
                    }
                    let enter = resp.lost_focus() && ui.ctx().input(|i| i.key_pressed(Key::Enter));
                    if (ui.button("Send").clicked() || enter)
                        && !self.compose_text.trim().is_empty()
                    {
                        let req = SendMessageExtended {
                            text: parse_compose_markdown(self.compose_text.trim()),
                            reply_to: self.reply_to_comment.as_ref().map(|(id, _)| *id),
                            send_as: self.selected_send_as,
                            thread_key: Some(thread_key),
                            ..SendMessageExtended::default()
                        };
                        if backend.send_message_extended(peer_id, req).is_ok() {
                            self.compose_text.clear();
                            self.reply_to_comment = None;
                            backend.save_thread_draft(thread_key, None);
                        }
                    }
                });
            },
        );

        action
    }
}

// ---------------------------------------------------------------------------
// 4. Forum Topics List & Topic History (`data/data_forum*.cpp`, dialogs topic rows)
// ---------------------------------------------------------------------------

/// Stateful Forum Topics list widget matching tdesktop's forum topic rows with exact 6
/// icon colors, custom emoji icons, General `#` icon, pinned/closed/unread/mention badges,
/// search, and topic creation.
pub struct ForumTopicsWidget {
    /// Filter query for topic search.
    pub search_query: String,
    /// True when the search box is expanded.
    pub search_open: bool,
    /// True when the inline "New Topic" composer is open.
    pub create_open: bool,
    /// New topic title input.
    pub new_title: String,
    /// Selected icon color hex from [`FORUM_TOPIC_ICON_COLORS`].
    pub new_color: u32,
    /// Optional custom emoji string or custom emoji document ID for new topic.
    pub new_emoji: String,
    /// Selected custom topic icon document ID from `Data::ForumIcons`, if any.
    pub new_icon_doc_id: Option<i64>,
    /// True when the custom topic icon picker grid is expanded.
    pub icon_picker_open: bool,
    /// Active inline topic editor `(topic_root_id, title_draft, selected_icon_doc_id)`.
    pub editing_topic: Option<(MsgId, String, Option<i64>)>,
    emoji_cache: EmojiTextureCache,
}

impl std::fmt::Debug for ForumTopicsWidget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForumTopicsWidget")
            .field("search_query", &self.search_query)
            .field("create_open", &self.create_open)
            .field("icon_picker_open", &self.icon_picker_open)
            .finish()
    }
}

impl Default for ForumTopicsWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl ForumTopicsWidget {
    /// Creates a new forum topics list widget.
    #[must_use]
    pub fn new() -> Self {
        Self {
            search_query: String::new(),
            search_open: false,
            create_open: false,
            new_title: String::new(),
            new_color: FORUM_TOPIC_ICON_COLORS[0].0,
            new_emoji: String::new(),
            new_icon_doc_id: None,
            icon_picker_open: false,
            editing_topic: None,
            emoji_cache: EmojiTextureCache::new(),
        }
    }

    /// Renders the forum topics list inside `rect`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        peer_id: PeerId,
        backend: &mut dyn HistoryBackend,
        colors: &HistoryColors,
    ) -> Option<HistorySectionAction> {
        let mut action = None;
        let peer_title = backend
            .peer_info(peer_id)
            .map_or_else(|| "Forum".to_owned(), |p| p.summary.title);
        let mut topics = backend.forum_topics(peer_id);
        // Sort pinned topics first (General first among pinned), then newest message date.
        topics.sort_by(|a, b| {
            b.is_pinned
                .cmp(&a.is_pinned)
                .then_with(|| b.is_general.cmp(&a.is_general))
                .then_with(|| b.last_date_unix.cmp(&a.last_date_unix))
        });

        let top_rect =
            Rect::from_min_size(rect.min, Vec2::new(rect.width(), HISTORY_TOP_BAR_HEIGHT));
        let subtitle = format!("{} topics · Forum", topics.len());
        let right_action = paint_section_top_bar_with_actions(
            ui,
            top_rect,
            &peer_title,
            &subtitle,
            &[("Search", self.search_open), ("+ Topic", self.create_open)],
            colors,
        );
        match right_action {
            Some(0) => action = Some(HistorySectionAction::Back),
            Some(1) => {
                self.search_open = !self.search_open;
                if !self.search_open {
                    self.search_query.clear();
                }
            }
            Some(2) => self.create_open = !self.create_open,
            _ => {}
        }

        let mut y_top = top_rect.bottom();

        // Optional search bar
        if self.search_open {
            let sr =
                Rect::from_min_size(Pos2::new(rect.left(), y_top), Vec2::new(rect.width(), 38.0));
            ui.painter()
                .rect_filled(sr, CornerRadius::ZERO, colors.bar_bg);
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(sr.shrink2(Vec2::new(14.0, 6.0))),
                |ui| {
                    ui.add(
                        TextEdit::singleline(&mut self.search_query)
                            .desired_width(f32::INFINITY)
                            .hint_text("Search forum topics..."),
                    );
                },
            );
            ui.painter().line_segment(
                [
                    Pos2::new(sr.left(), sr.bottom()),
                    Pos2::new(sr.right(), sr.bottom()),
                ],
                Stroke::new(1.0, colors.divider),
            );
            y_top = sr.bottom();
        }

        // Optional inline "Create Topic" bar with custom topic icon picker (`Data::ForumIcons`)
        if self.create_open {
            let topic_icons = backend.forum_topic_icons();
            let picker_extra_h = if self.icon_picker_open { 72.0 } else { 0.0 };
            let cr = Rect::from_min_size(
                Pos2::new(rect.left(), y_top),
                Vec2::new(rect.width(), 82.0 + picker_extra_h),
            );
            ui.painter()
                .rect_filled(cr, CornerRadius::ZERO, colors.bar_bg);
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(cr.shrink2(Vec2::new(14.0, 8.0))),
                |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut self.new_title)
                                .desired_width(190.0)
                                .hint_text("New topic title..."),
                        );
                        ui.add(
                            TextEdit::singleline(&mut self.new_emoji)
                                .desired_width(44.0)
                                .hint_text("Emoji"),
                        );
                        let dir = if self.icon_picker_open {
                            tg_app_core::ui_icons::ArrowDir::Up
                        } else {
                            tg_app_core::ui_icons::ArrowDir::Down
                        };
                        let icons_resp = tg_app_core::ui_icons::icon_label_button(
                            ui,
                            "Icons",
                            false,
                            move |p, c, s, col| {
                                tg_app_core::ui_icons::paint_triangle(p, c, s * 0.72, dir, col);
                            },
                        );
                        if icons_resp.clicked() {
                            self.icon_picker_open = !self.icon_picker_open;
                        }
                        if ui.button("Create").clicked() && !self.new_title.trim().is_empty() {
                            let emoji_opt =
                                self.new_icon_doc_id.map(|id| id.to_string()).or_else(|| {
                                    (!self.new_emoji.trim().is_empty())
                                        .then(|| self.new_emoji.trim().to_owned())
                                });
                            if backend
                                .create_forum_topic(
                                    peer_id,
                                    self.new_title.trim().to_owned(),
                                    self.new_color,
                                    emoji_opt,
                                )
                                .is_ok()
                            {
                                self.new_title.clear();
                                self.new_emoji.clear();
                                self.new_icon_doc_id = None;
                                self.icon_picker_open = false;
                                self.create_open = false;
                                action = Some(HistorySectionAction::Toast(
                                    "Forum topic created".to_owned(),
                                ));
                            }
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Icon color:")
                                .size(11.5)
                                .color(colors.sub_text_fg),
                        );
                        for &(hex, name) in &FORUM_TOPIC_ICON_COLORS {
                            let [r, g, b] = rgb_from_hex(hex);
                            let (sw, resp) =
                                ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
                            ui.painter().circle_filled(
                                sw.center(),
                                8.0,
                                Color32::from_rgb(r, g, b),
                            );
                            if self.new_icon_doc_id.is_none() && self.new_color == hex {
                                ui.painter().circle_stroke(
                                    sw.center(),
                                    9.5,
                                    Stroke::new(2.0, colors.bold_fg),
                                );
                            }
                            if resp.on_hover_text(name).clicked() {
                                self.new_color = hex;
                                self.new_icon_doc_id = None;
                            }
                        }
                        if let Some(doc_id) = self.new_icon_doc_id {
                            let alt = topic_icons
                                .iter()
                                .find(|ic| ic.document_id == doc_id)
                                .map_or("✨", |ic| ic.alt_emoji.as_str());
                            ui.label(
                                egui::RichText::new(format!("Custom icon: {alt}"))
                                    .size(11.5)
                                    .color(colors.active_text_fg),
                            );
                        }
                    });
                    if self.icon_picker_open {
                        ui.add_space(4.0);
                        if let Some(picked) = render_forum_topic_icon_picker(
                            ui,
                            &topic_icons,
                            self.new_icon_doc_id,
                            colors,
                        ) {
                            self.new_icon_doc_id = picked;
                            if let Some(id) = picked
                                && let Some(entry) =
                                    topic_icons.iter().find(|ic| ic.document_id == id)
                            {
                                self.new_emoji = entry.alt_emoji.clone();
                            }
                        }
                    }
                },
            );
            ui.painter().line_segment(
                [
                    Pos2::new(cr.left(), cr.bottom()),
                    Pos2::new(cr.right(), cr.bottom()),
                ],
                Stroke::new(1.0, colors.divider),
            );
            y_top = cr.bottom();
        }

        // Optional inline "Edit Topic" bar (title + custom emoji icon picker)
        if let Some((edit_tid, mut edit_title, mut edit_icon_id)) = self.editing_topic.clone() {
            let topic_icons = backend.forum_topic_icons();
            let er = Rect::from_min_size(
                Pos2::new(rect.left(), y_top),
                Vec2::new(rect.width(), 108.0),
            );
            ui.painter()
                .rect_filled(er, CornerRadius::ZERO, colors.bar_bg);
            let mut keep_editing = true;
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(er.shrink2(Vec2::new(14.0, 6.0))),
                |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Edit Topic:")
                                .strong()
                                .size(12.0)
                                .color(colors.active_text_fg),
                        );
                        ui.add(
                            TextEdit::singleline(&mut edit_title)
                                .desired_width(180.0)
                                .hint_text("Topic title..."),
                        );
                        if ui.button("Save").clicked() && !edit_title.trim().is_empty() {
                            match backend.edit_forum_topic(
                                peer_id,
                                edit_tid,
                                Some(edit_title.trim().to_owned()),
                                Some(edit_icon_id.unwrap_or(0)),
                            ) {
                                Ok(_) => {
                                    action = Some(HistorySectionAction::Toast(
                                        "Forum topic updated".to_owned(),
                                    ));
                                    keep_editing = false;
                                }
                                Err(err) => {
                                    action = Some(HistorySectionAction::Toast(err));
                                }
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            keep_editing = false;
                        }
                    });
                    ui.add_space(4.0);
                    if let Some(picked) =
                        render_forum_topic_icon_picker(ui, &topic_icons, edit_icon_id, colors)
                    {
                        edit_icon_id = picked;
                    }
                },
            );
            ui.painter().line_segment(
                [
                    Pos2::new(er.left(), er.bottom()),
                    Pos2::new(er.right(), er.bottom()),
                ],
                Stroke::new(1.0, colors.divider),
            );
            self.editing_topic = keep_editing.then_some((edit_tid, edit_title, edit_icon_id));
            y_top = er.bottom();
        }

        let list_rect = Rect::from_min_max(Pos2::new(rect.left(), y_top), rect.max);
        ui.painter()
            .rect_filled(list_rect, CornerRadius::ZERO, colors.bar_bg);

        let q = self.search_query.trim().to_lowercase();
        let filtered: Vec<&ForumTopic> = topics
            .iter()
            .filter(|t| {
                q.is_empty()
                    || t.title.to_lowercase().contains(&q)
                    || t.last_preview.to_lowercase().contains(&q)
            })
            .collect();

        let mut open_topic_id = None;
        let mut toggle_pin_id = None;
        let mut toggle_close_id = None;
        let ctx = ui.ctx().clone();

        ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
            ScrollArea::vertical()
                .id_salt(("tg_forum_topics_list", peer_id.0))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for topic in &filtered {
                        let (row_r, resp) = ui.allocate_exact_size(
                            Vec2::new(list_rect.width(), 68.0),
                            Sense::click(),
                        );
                        resp.widget_info(|| {
                            let label = if topic.unread_count > 0 {
                                format!("{} ({} unread)", topic.title, topic.unread_count)
                            } else {
                                topic.title.clone()
                            };
                            egui::WidgetInfo::labeled(egui::Role::Button, true, label)
                        });
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            ui.painter()
                                .rect_filled(row_r, CornerRadius::ZERO, colors.bar_bg_over);
                        }

                        // Topic icon (General `#`, custom emoji sprite, or colored speech-bubble + letter)
                        let icon_rect = Rect::from_center_size(
                            Pos2::new(row_r.left() + 34.0, row_r.center().y),
                            Vec2::splat(38.0),
                        );
                        paint_forum_topic_icon(
                            ui.painter(),
                            &ctx,
                            &mut self.emoji_cache,
                            topic,
                            icon_rect,
                            colors,
                        );

                        // Right-side timestamp & badges
                        let dt = ScheduleDateTime::from_unix_utc(i64::from(topic.last_date_unix));
                        let time_str = format!("{:02}:{:02}", dt.hour, dt.minute);
                        ui.painter().text(
                            Pos2::new(row_r.right() - 16.0, row_r.top() + 14.0),
                            Align2::RIGHT_TOP,
                            time_str,
                            FontId::proportional(11.5),
                            colors.sub_text_fg,
                        );

                        // Unread / Mention / Pin badges on bottom-right
                        let mut badge_right = row_r.right() - 16.0;
                        let badge_y = row_r.bottom() - 20.0;
                        if topic.unread_count > 0 {
                            let label = topic.unread_count.to_string();
                            let w = (label.len() as f32 * 7.5 + 14.0).max(22.0);
                            let br = Rect::from_center_size(
                                Pos2::new(badge_right - w * 0.5, badge_y),
                                Vec2::new(w, 20.0),
                            );
                            let bg = if topic.muted {
                                colors.sub_text_fg
                            } else {
                                colors.accent
                            };
                            ui.painter().rect_filled(br, CornerRadius::same(10), bg);
                            ui.painter().text(
                                br.center(),
                                Align2::CENTER_CENTER,
                                label,
                                FontId::proportional(11.0),
                                Color32::WHITE,
                            );
                            badge_right = br.left() - 6.0;
                        }
                        if topic.unread_mentions > 0 {
                            let mr = Rect::from_center_size(
                                Pos2::new(badge_right - 10.0, badge_y),
                                Vec2::splat(20.0),
                            );
                            ui.painter().circle_filled(mr.center(), 10.0, colors.accent);
                            ui.painter().text(
                                mr.center(),
                                Align2::CENTER_CENTER,
                                "@",
                                FontId::proportional(11.0),
                                Color32::WHITE,
                            );
                            badge_right = mr.left() - 6.0;
                        }
                        if topic.is_pinned {
                            tg_app_core::ui_icons::paint_style_tinted(
                                ui.painter(),
                                Rect::from_center_size(
                                    Pos2::new(badge_right - 7.0, badge_y),
                                    Vec2::splat(13.0),
                                ),
                                tg_ui::tg_style::st::menu_icon_pin,
                                colors.sub_text_fg,
                            );
                            badge_right -= 20.0;
                        }

                        // Topic title + status markers
                        let text_x = icon_rect.right() + 14.0;
                        let title_galley = ui.painter().layout_no_wrap(
                            topic.title.clone(),
                            FontId::proportional(14.0),
                            colors.bold_fg,
                        );
                        let title_top = row_r.top() + 13.0;
                        let title_cy = title_top + title_galley.size().y * 0.5;
                        let mut marker_x = text_x + title_galley.size().x + 8.0;
                        ui.painter().galley(
                            Pos2::new(text_x, title_top),
                            title_galley,
                            colors.bold_fg,
                        );
                        if topic.is_closed {
                            tg_app_core::ui_icons::paint_lock(
                                ui.painter(),
                                Pos2::new(marker_x + 6.0, title_cy),
                                13.0,
                                colors.sub_text_fg,
                            );
                            marker_x += 18.0;
                        }
                        if topic.muted {
                            tg_app_core::ui_icons::paint_style_tinted(
                                ui.painter(),
                                Rect::from_center_size(
                                    Pos2::new(marker_x + 6.0, title_cy),
                                    Vec2::splat(13.0),
                                ),
                                tg_ui::tg_style::st::menu_icon_mute,
                                colors.sub_text_fg,
                            );
                        }

                        // Second line: Draft or Sender + Last Message Preview
                        let preview_w = (badge_right - text_x - 8.0).max(80.0);
                        let preview_clip = Rect::from_min_size(
                            Pos2::new(text_x, row_r.top() + 36.0),
                            Vec2::new(preview_w, 20.0),
                        );
                        if let Some(ref draft) = topic.draft_preview {
                            let draft_galley = ui.painter().layout_no_wrap(
                                format!("Draft: {draft}"),
                                FontId::proportional(12.5),
                                colors.attention_fg,
                            );
                            ui.painter().with_clip_rect(preview_clip).galley(
                                preview_clip.min,
                                draft_galley,
                                colors.attention_fg,
                            );
                        } else {
                            let line = format!("{}: {}", topic.last_sender, topic.last_preview);
                            let line_galley = ui.painter().layout_no_wrap(
                                line,
                                FontId::proportional(12.5),
                                colors.sub_text_fg,
                            );
                            ui.painter().with_clip_rect(preview_clip).galley(
                                preview_clip.min,
                                line_galley,
                                colors.sub_text_fg,
                            );
                        }

                        // Row divider
                        ui.painter().line_segment(
                            [
                                Pos2::new(text_x, row_r.bottom()),
                                Pos2::new(row_r.right(), row_r.bottom()),
                            ],
                            Stroke::new(0.8, colors.divider),
                        );

                        // Right-click quick actions (Edit / Pin / Close topic)
                        if resp.secondary_clicked() {
                            if !topic.is_general {
                                let icon_id = topic
                                    .custom_emoji
                                    .as_deref()
                                    .and_then(|s| s.parse::<i64>().ok());
                                self.editing_topic =
                                    Some((topic.root_id, topic.title.clone(), icon_id));
                            } else if topic.is_closed {
                                toggle_close_id = Some(topic.root_id);
                            } else {
                                toggle_pin_id = Some(topic.root_id);
                            }
                        } else if resp.clicked() {
                            open_topic_id = Some(topic.root_id);
                        }
                    }
                });
        });

        if let Some(tid) = open_topic_id {
            action = Some(HistorySectionAction::OpenSection(
                HistorySectionKind::ForumTopicHistory {
                    peer_id,
                    topic_root_id: tid,
                },
            ));
        }
        if let Some(tid) = toggle_pin_id {
            let _ = backend.toggle_forum_topic_pin(peer_id, tid);
        }
        if let Some(tid) = toggle_close_id {
            let _ = backend.toggle_forum_topic_closed(peer_id, tid);
        }

        action
    }
}

/// Paints a tdesktop forum topic icon (`#` for General, custom emoji sprite, or colored speech
/// bubble + first non-emoji letter).
pub fn paint_forum_topic_icon(
    painter: &egui::Painter,
    ctx: &egui::Context,
    emoji_cache: &mut EmojiTextureCache,
    topic: &ForumTopic,
    rect: Rect,
    colors: &HistoryColors,
) {
    if topic.is_general {
        painter.circle_filled(rect.center(), rect.width() * 0.48, colors.bar_bg_over);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "#",
            FontId::proportional(20.0),
            colors.sub_text_fg,
        );
        return;
    }
    if let Some(ref em) = topic.custom_emoji {
        emoji_cache.paint_emoji_str(painter, ctx, em, rect.shrink(2.0), Color32::WHITE);
        return;
    }
    let [r, g, b] = topic.icon_rgb();
    let fill = Color32::from_rgb(r, g, b);
    let body = rect.shrink(2.0);
    // Speech-bubble style forum topic icon matching `:/gui/topic_icons/*.svg`
    painter.rect_filled(body, CornerRadius::same(11), fill);
    let tail = vec![
        Pos2::new(body.left() + 6.0, body.bottom() - 2.0),
        Pos2::new(body.left() + 2.0, body.bottom() + 4.0),
        Pos2::new(body.left() + 13.0, body.bottom() - 1.0),
    ];
    painter.add(Shape::convex_polygon(tail, fill, Stroke::NONE));

    if let Some(letter) = extract_non_emoji_letter(&topic.title) {
        painter.text(
            body.center(),
            Align2::CENTER_CENTER,
            letter,
            FontId::proportional(15.0),
            Color32::WHITE,
        );
    }
}

/// Stateful single Forum Topic history view with per-topic draft, closed-topic protection,
/// and embedded [`SubsectionTabsWidget`] slider.
#[derive(Default)]
pub struct ForumTopicHistoryWidget {
    loaded_topic: Option<(PeerId, MsgId)>,
    /// Compose text for this forum topic.
    pub compose_text: String,
    /// Persistent subsection tabs slider state (`Top` / `Left` / `Bottom`).
    pub subsection_tabs: SubsectionTabsWidget,
    emoji_cache: EmojiTextureCache,
}

impl std::fmt::Debug for ForumTopicHistoryWidget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForumTopicHistoryWidget")
            .field("loaded_topic", &self.loaded_topic)
            .finish()
    }
}

impl ForumTopicHistoryWidget {
    /// Renders one forum topic's message history and compose bar inside `rect`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        peer_id: PeerId,
        topic_root_id: MsgId,
        backend: &mut dyn HistoryBackend,
        colors: &HistoryColors,
    ) -> Option<HistorySectionAction> {
        let mut action = None;
        let thread_key = ThreadKey::Topic {
            peer_id,
            topic_root_id,
        };

        if self.loaded_topic != Some((peer_id, topic_root_id)) {
            if let Some((prev_p, prev_t)) = self.loaded_topic {
                let prev_key = ThreadKey::Topic {
                    peer_id: prev_p,
                    topic_root_id: prev_t,
                };
                let draft = (!self.compose_text.trim().is_empty()).then(|| ThreadDraft {
                    text: self.compose_text.clone(),
                    ..ThreadDraft::default()
                });
                backend.save_thread_draft(prev_key, draft);
            }
            self.loaded_topic = Some((peer_id, topic_root_id));
            self.compose_text = backend
                .load_thread_draft(thread_key)
                .map(|d| d.text)
                .unwrap_or_default();
        }

        let topics = backend.forum_topics(peer_id);
        let topic = topics.iter().find(|t| t.root_id == topic_root_id).cloned();
        let forum_title = backend
            .peer_info(peer_id)
            .map_or_else(|| "Forum".to_owned(), |p| p.summary.title);
        let is_closed = topic.as_ref().is_some_and(|t| t.is_closed);

        let top_rect =
            Rect::from_min_size(rect.min, Vec2::new(rect.width(), HISTORY_TOP_BAR_HEIGHT));
        let heading = topic.as_ref().map_or_else(
            || format!("Topic #{}", topic_root_id.0),
            |t| t.title.clone(),
        );
        let subtitle = if is_closed {
            format!("Closed topic · {forum_title}")
        } else {
            forum_title
        };
        if paint_section_top_bar(ui, top_rect, &heading, &subtitle, Some("Topics"), colors) {
            action = Some(HistorySectionAction::OpenSection(
                HistorySectionKind::ForumTopics { peer_id },
            ));
        }

        let compose_h = 52.0;
        let content_rect = Rect::from_min_max(
            Pos2::new(rect.left(), top_rect.bottom()),
            Pos2::new(rect.right(), rect.bottom()),
        );
        let (inner_rect, tabs_act) = self.subsection_tabs.show_for_forum(
            ui,
            content_rect,
            peer_id,
            Some(topic_root_id),
            &topics,
            backend,
            colors,
        );
        if let Some(ev) = tabs_act {
            action = Some(ev);
        }

        let compose_rect = Rect::from_min_size(
            Pos2::new(inner_rect.left(), inner_rect.bottom() - compose_h),
            Vec2::new(inner_rect.width(), compose_h),
        );
        let list_rect = Rect::from_min_max(
            inner_rect.min,
            Pos2::new(inner_rect.right(), compose_rect.top()),
        );
        ui.painter()
            .rect_filled(list_rect, CornerRadius::ZERO, colors.canvas_bg);

        let messages = backend.load_topic_messages(peer_id, topic_root_id);
        paint_simple_message_feed(
            ui,
            list_rect,
            ("tg_topic_msgs", peer_id.0, topic_root_id.0),
            &messages,
            "No messages in this topic yet.",
            colors,
        );

        // Compose area
        ui.painter()
            .rect_filled(compose_rect, CornerRadius::ZERO, colors.bar_bg);
        ui.painter().line_segment(
            [
                Pos2::new(compose_rect.left(), compose_rect.top()),
                Pos2::new(compose_rect.right(), compose_rect.top()),
            ],
            Stroke::new(1.0, colors.divider),
        );
        if is_closed {
            let galley = ui.painter().layout_no_wrap(
                "This topic is closed".to_owned(),
                FontId::proportional(13.0),
                colors.sub_text_fg,
            );
            let total_w = 14.0 + 6.0 + galley.size().x;
            let left_x = compose_rect.center().x - total_w * 0.5;
            tg_app_core::ui_icons::paint_lock(
                ui.painter(),
                Pos2::new(left_x + 7.0, compose_rect.center().y),
                14.0,
                colors.sub_text_fg,
            );
            ui.painter().galley(
                Pos2::new(
                    left_x + 20.0,
                    compose_rect.center().y - galley.size().y * 0.5,
                ),
                galley,
                colors.sub_text_fg,
            );
        } else {
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(compose_rect.shrink2(Vec2::new(12.0, 10.0))),
                |ui| {
                    ui.horizontal(|ui| {
                        let prev = self.compose_text.clone();
                        let resp = ui.add(
                            TextEdit::singleline(&mut self.compose_text)
                                .desired_width((compose_rect.width() - 104.0).max(120.0))
                                .hint_text(format!("Message in {heading}...")),
                        );
                        if self.compose_text != prev {
                            let draft =
                                (!self.compose_text.trim().is_empty()).then(|| ThreadDraft {
                                    text: self.compose_text.clone(),
                                    ..ThreadDraft::default()
                                });
                            backend.save_thread_draft(thread_key, draft);
                        }
                        let enter =
                            resp.lost_focus() && ui.ctx().input(|i| i.key_pressed(Key::Enter));
                        if (ui.button("Send").clicked() || enter)
                            && !self.compose_text.trim().is_empty()
                        {
                            let req = SendMessageExtended {
                                text: parse_compose_markdown(self.compose_text.trim()),
                                thread_key: Some(thread_key),
                                ..SendMessageExtended::default()
                            };
                            if backend.send_message_extended(peer_id, req).is_ok() {
                                self.compose_text.clear();
                                backend.save_thread_draft(thread_key, None);
                            }
                        }
                    });
                },
            );
        }

        let _ = &self.emoji_cache;
        action
    }
}

// ---------------------------------------------------------------------------
// 5. Saved Messages Sublists (per-chat) (`data_saved_sublist.cpp`)
// ---------------------------------------------------------------------------

/// Stateful Saved Messages per-chat sublists view and sublist history view.
#[derive(Debug, Default)]
pub struct SavedSublistsWidget {
    /// Filter query for sublists.
    pub search_query: String,
    /// Compose text when inside a sublist history.
    pub sublist_compose_text: String,
    /// Persistent subsection tabs slider for Saved Messages sublists.
    pub subsection_tabs: SubsectionTabsWidget,
}

impl SavedSublistsWidget {
    /// Renders the list of Saved Messages sublists grouped by original peer.
    pub fn show_list(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        peer_id: PeerId,
        backend: &dyn HistoryBackend,
        colors: &HistoryColors,
    ) -> Option<HistorySectionAction> {
        let mut action = None;
        let sublists = backend.saved_sublists(peer_id);

        let top_rect =
            Rect::from_min_size(rect.min, Vec2::new(rect.width(), HISTORY_TOP_BAR_HEIGHT));
        let subtitle = format!("{} saved chats", sublists.len());
        if paint_section_top_bar(
            ui,
            top_rect,
            "Saved Messages",
            &subtitle,
            Some("All Messages"),
            colors,
        ) {
            action = Some(HistorySectionAction::Back);
        }

        let list_rect = Rect::from_min_max(Pos2::new(rect.left(), top_rect.bottom()), rect.max);
        ui.painter()
            .rect_filled(list_rect, CornerRadius::ZERO, colors.bar_bg);

        let ui_colors = colors.ui_colors();
        ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
            ScrollArea::vertical()
                .id_salt(("tg_saved_sublists", peer_id.0))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for sub in &sublists {
                        let (row_r, resp) = ui.allocate_exact_size(
                            Vec2::new(list_rect.width(), 66.0),
                            Sense::click(),
                        );
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            ui.painter()
                                .rect_filled(row_r, CornerRadius::ZERO, colors.bar_bg_over);
                        }

                        let av_rect = Rect::from_center_size(
                            Pos2::new(row_r.left() + 34.0, row_r.center().y),
                            Vec2::splat(42.0),
                        );
                        if sub.is_my_notes {
                            EmptyUserpic::paint_saved_messages(ui.painter(), av_rect, &ui_colors);
                        } else {
                            let [r, g, b] = sub.avatar_rgb;
                            ui.painter().circle_filled(
                                av_rect.center(),
                                21.0,
                                Color32::from_rgb(r, g, b),
                            );
                            ui.painter().text(
                                av_rect.center(),
                                Align2::CENTER_CENTER,
                                &sub.initials,
                                FontId::proportional(14.5),
                                Color32::WHITE,
                            );
                        }

                        let text_x = av_rect.right() + 12.0;
                        let dt = ScheduleDateTime::from_unix_utc(i64::from(sub.last_date_unix));
                        ui.painter().text(
                            Pos2::new(row_r.right() - 16.0, row_r.top() + 13.0),
                            Align2::RIGHT_TOP,
                            format!("{:02}:{:02}", dt.hour, dt.minute),
                            FontId::proportional(11.5),
                            colors.sub_text_fg,
                        );
                        ui.painter().text(
                            Pos2::new(text_x, row_r.top() + 12.0),
                            Align2::LEFT_TOP,
                            &sub.title,
                            FontId::proportional(14.0),
                            colors.bold_fg,
                        );
                        ui.painter().text(
                            Pos2::new(text_x, row_r.top() + 34.0),
                            Align2::LEFT_TOP,
                            &sub.last_preview,
                            FontId::proportional(12.5),
                            colors.sub_text_fg,
                        );

                        // Message count badge
                        let count_str = format!("{} saved", sub.message_count);
                        ui.painter().text(
                            Pos2::new(row_r.right() - 16.0, row_r.bottom() - 16.0),
                            Align2::RIGHT_CENTER,
                            count_str,
                            FontId::proportional(11.0),
                            colors.active_text_fg,
                        );

                        ui.painter().line_segment(
                            [
                                Pos2::new(text_x, row_r.bottom()),
                                Pos2::new(row_r.right(), row_r.bottom()),
                            ],
                            Stroke::new(0.8, colors.divider),
                        );

                        if resp.clicked() {
                            action = Some(HistorySectionAction::OpenSection(
                                HistorySectionKind::SavedSublistHistory {
                                    peer_id,
                                    sublist_peer_id: sub.sublist_peer_id,
                                },
                            ));
                        }
                    }
                });
        });

        action
    }

    /// Renders messages inside one Saved Messages sublist.
    pub fn show_sublist_history(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        peer_id: PeerId,
        sublist_peer_id: PeerId,
        backend: &mut dyn HistoryBackend,
        colors: &HistoryColors,
    ) -> Option<HistorySectionAction> {
        let mut action = None;
        let sublists = backend.saved_sublists(peer_id);
        let title = sublists
            .iter()
            .find(|s| s.sublist_peer_id == sublist_peer_id)
            .map_or_else(|| "Saved Sublist".to_owned(), |s| s.title.clone());
        let messages = backend.load_saved_sublist_messages(peer_id, sublist_peer_id);

        let top_rect =
            Rect::from_min_size(rect.min, Vec2::new(rect.width(), HISTORY_TOP_BAR_HEIGHT));
        if paint_section_top_bar(
            ui,
            top_rect,
            &title,
            &format!("{} saved messages", messages.len()),
            Some("Sublists"),
            colors,
        ) {
            action = Some(HistorySectionAction::OpenSection(
                HistorySectionKind::SavedSublists { peer_id },
            ));
        }

        let compose_h = 52.0;
        let content_rect = Rect::from_min_max(
            Pos2::new(rect.left(), top_rect.bottom()),
            Pos2::new(rect.right(), rect.bottom()),
        );
        let (inner_rect, tabs_act) = self.subsection_tabs.show_for_saved(
            ui,
            content_rect,
            peer_id,
            Some(sublist_peer_id),
            &sublists,
            colors,
        );
        if let Some(ev) = tabs_act {
            action = Some(ev);
        }

        let compose_rect = Rect::from_min_size(
            Pos2::new(inner_rect.left(), inner_rect.bottom() - compose_h),
            Vec2::new(inner_rect.width(), compose_h),
        );
        let list_rect = Rect::from_min_max(
            inner_rect.min,
            Pos2::new(inner_rect.right(), compose_rect.top()),
        );
        ui.painter()
            .rect_filled(list_rect, CornerRadius::ZERO, colors.canvas_bg);

        paint_simple_message_feed(
            ui,
            list_rect,
            ("tg_saved_sub_msgs", peer_id.0, sublist_peer_id.0),
            &messages,
            "No messages saved from this chat yet.",
            colors,
        );

        ui.painter()
            .rect_filled(compose_rect, CornerRadius::ZERO, colors.bar_bg);
        ui.painter().line_segment(
            [
                Pos2::new(compose_rect.left(), compose_rect.top()),
                Pos2::new(compose_rect.right(), compose_rect.top()),
            ],
            Stroke::new(1.0, colors.divider),
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(compose_rect.shrink2(Vec2::new(12.0, 10.0))),
            |ui| {
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        TextEdit::singleline(&mut self.sublist_compose_text)
                            .desired_width((compose_rect.width() - 104.0).max(120.0))
                            .hint_text("Add a note to this saved sublist..."),
                    );
                    let enter = resp.lost_focus() && ui.ctx().input(|i| i.key_pressed(Key::Enter));
                    if (ui.button("Save").clicked() || enter)
                        && !self.sublist_compose_text.trim().is_empty()
                    {
                        let req = SendMessageExtended {
                            text: parse_compose_markdown(self.sublist_compose_text.trim()),
                            thread_key: Some(ThreadKey::SavedSublist {
                                peer_id,
                                sublist_peer_id,
                            }),
                            ..SendMessageExtended::default()
                        };
                        if backend.send_message_extended(peer_id, req).is_ok() {
                            self.sublist_compose_text.clear();
                        }
                    }
                });
            },
        );

        action
    }
}

// ---------------------------------------------------------------------------
// 6. Channel Admin Log / Recent Actions with Filters (`history/admin_log/*`)
// ---------------------------------------------------------------------------

/// Stateful Channel Admin Log ("Recent Actions") section with event-type & admin filters.
#[derive(Debug, Default)]
pub struct AdminLogSectionWidget {
    /// Active filter criteria (`AdminLog::FilterValue`).
    pub filter: AdminLogFilter,
    /// True when the filter drawer/modal is open.
    pub filter_modal_open: bool,
    /// True when inline search bar is open.
    pub search_open: bool,
}

impl AdminLogSectionWidget {
    /// Renders the Recent Actions feed and interactive Filter modal inside `rect`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        peer_id: PeerId,
        backend: &dyn HistoryBackend,
        colors: &HistoryColors,
    ) -> Option<HistorySectionAction> {
        let mut action = None;
        let peer_title = backend
            .peer_info(peer_id)
            .map_or_else(|| "Channel".to_owned(), |p| p.summary.title);
        let entries = backend.admin_log_entries(peer_id, &self.filter);
        let admins = backend.admin_log_admins(peer_id);

        let top_rect =
            Rect::from_min_size(rect.min, Vec2::new(rect.width(), HISTORY_TOP_BAR_HEIGHT));
        let subtitle = if self.filter.is_filtered() {
            format!("{} filtered events · {peer_title}", entries.len())
        } else {
            format!("{} recent actions · {peer_title}", entries.len())
        };
        let top_click = paint_section_top_bar_with_actions(
            ui,
            top_rect,
            "Recent Actions",
            &subtitle,
            &[
                ("Search", self.search_open),
                (
                    "Filter",
                    self.filter.is_filtered() || self.filter_modal_open,
                ),
            ],
            colors,
        );
        match top_click {
            Some(0) => action = Some(HistorySectionAction::Back),
            Some(1) => {
                self.search_open = !self.search_open;
                if !self.search_open {
                    self.filter.search_query.clear();
                }
            }
            Some(2) => self.filter_modal_open = !self.filter_modal_open,
            _ => {}
        }

        let mut y_top = top_rect.bottom();
        if self.search_open {
            let sr =
                Rect::from_min_size(Pos2::new(rect.left(), y_top), Vec2::new(rect.width(), 38.0));
            ui.painter()
                .rect_filled(sr, CornerRadius::ZERO, colors.bar_bg);
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(sr.shrink2(Vec2::new(14.0, 6.0))),
                |ui| {
                    ui.add(
                        TextEdit::singleline(&mut self.filter.search_query)
                            .desired_width(f32::INFINITY)
                            .hint_text("Search admin log actions..."),
                    );
                },
            );
            ui.painter().line_segment(
                [
                    Pos2::new(sr.left(), sr.bottom()),
                    Pos2::new(sr.right(), sr.bottom()),
                ],
                Stroke::new(1.0, colors.divider),
            );
            y_top = sr.bottom();
        }

        let list_rect = Rect::from_min_max(Pos2::new(rect.left(), y_top), rect.max);
        ui.painter()
            .rect_filled(list_rect, CornerRadius::ZERO, colors.canvas_bg);

        if entries.is_empty() {
            paint_empty_section_pill(
                ui.painter(),
                list_rect,
                "No recent actions match the active filter.",
                colors,
            );
        } else {
            let ui_colors = colors.ui_colors();
            ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
                ScrollArea::vertical()
                    .id_salt(("tg_admin_log_scroll", peer_id.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(12.0);
                        for entry in &entries {
                            paint_admin_log_card(ui, entry, list_rect.width(), &ui_colors, colors);
                            ui.add_space(8.0);
                        }
                        ui.add_space(12.0);
                    });
            });
        }

        // Interactive Admin Log Filter modal (`history_admin_log_filter.cpp`)
        if self.filter_modal_open {
            let screen = ui.ctx().viewport_rect();
            egui::Area::new(Id::unique("tg_admin_log_filter_backdrop"))
                .order(egui::Order::Middle)
                .fixed_pos(screen.min)
                .show(ui.ctx(), |ui| {
                    let (r, _) = ui.allocate_exact_size(screen.size(), Sense::click());
                    ui.painter().rect_filled(
                        r,
                        CornerRadius::ZERO,
                        Color32::from_rgba_unmultiplied(0, 0, 0, 110),
                    );
                });

            egui::Area::new(Id::unique("tg_admin_log_filter_modal"))
                .order(egui::Order::Foreground)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    egui::Frame::NONE
                        .fill(colors.bar_bg)
                        .stroke(Stroke::new(1.0, colors.divider))
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(egui::Margin::same(16))
                        .show(ui, |ui| {
                            ui.set_width(360.0);
                            ui.label(
                                egui::RichText::new("Recent Actions Filter")
                                    .strong()
                                    .size(16.0)
                                    .color(colors.bold_fg),
                            );
                            ui.add_space(8.0);

                            let mut flags = self.filter.flags.unwrap_or(AdminLogFilterFlags::ALL);
                            let mut all_events = flags.is_all();
                            if ui.checkbox(&mut all_events, "All actions").changed() {
                                flags = if all_events {
                                    AdminLogFilterFlags::ALL
                                } else {
                                    AdminLogFilterFlags(0)
                                };
                            }
                            ui.separator();

                            ScrollArea::vertical()
                                .id_salt("tg_admin_filter_flags")
                                .max_height(190.0)
                                .show(ui, |ui| {
                                    for &(label, mask) in ADMIN_LOG_FILTER_CATEGORIES {
                                        let mut checked = flags.contains(mask);
                                        if ui.checkbox(&mut checked, label).changed() {
                                            flags.set(mask, checked);
                                        }
                                    }
                                });
                            self.filter.flags = if flags.is_all() { None } else { Some(flags) };

                            ui.add_space(8.0);
                            ui.separator();
                            ui.label(
                                egui::RichText::new("By Admins")
                                    .strong()
                                    .size(12.5)
                                    .color(colors.active_text_fg),
                            );
                            let mut all_admins = self.filter.admin_ids.is_none();
                            if ui.checkbox(&mut all_admins, "All admins").changed() {
                                self.filter.admin_ids = if all_admins {
                                    None
                                } else {
                                    Some(admins.iter().map(|a| a.peer_id).collect())
                                };
                            }
                            if let Some(ref mut selected_admins) = self.filter.admin_ids {
                                for adm in &admins {
                                    let mut checked = selected_admins.contains(&adm.peer_id);
                                    if ui
                                        .checkbox(
                                            &mut checked,
                                            format!("{} ({})", adm.name, adm.role),
                                        )
                                        .changed()
                                    {
                                        if checked {
                                            if !selected_admins.contains(&adm.peer_id) {
                                                selected_admins.push(adm.peer_id);
                                            }
                                        } else {
                                            selected_admins.retain(|&id| id != adm.peer_id);
                                        }
                                    }
                                }
                            }

                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                if ui.button("Reset").clicked() {
                                    self.filter = AdminLogFilter::default();
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("Done").clicked() {
                                            self.filter_modal_open = false;
                                        }
                                    },
                                );
                            });
                        });
                });
            if ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
                self.filter_modal_open = false;
            }
        }

        action
    }
}

fn paint_admin_log_card(
    ui: &mut Ui,
    entry: &AdminLogEntry,
    avail_w: f32,
    ui_colors: &crate::history::theme::HistoryUiColors,
    colors: &HistoryColors,
) {
    let dt = ScheduleDateTime::from_unix_utc(i64::from(entry.date_unix));
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        egui::Frame::NONE
            .fill(colors.msg_in_bg)
            .stroke(Stroke::new(1.0, colors.divider))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_max_width((avail_w - 48.0).min(560.0));
                    ui.set_min_width((avail_w - 48.0).min(360.0));
                    ui.horizontal(|ui| {
                        let (av_r, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
                        EmptyUserpic::for_peer(entry.actor_id.bare(), &entry.actor_name, ui_colors)
                            .paint_circle(ui.painter(), av_r);
                        ui.label(
                            egui::RichText::new(&entry.actor_name)
                                .strong()
                                .size(12.5)
                                .color(colors.active_text_fg),
                        );
                        egui::Frame::NONE
                            .fill(colors.bar_bg_over)
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(egui::Margin::symmetric(6, 2))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(entry.flag.label())
                                        .size(10.5)
                                        .color(colors.sub_text_fg),
                                );
                            });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(dt.format_display())
                                    .size(10.5)
                                    .color(colors.sub_text_fg),
                            );
                        });
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(&entry.summary)
                            .size(13.0)
                            .color(colors.text_fg),
                    );
                    if let Some(ref before) = entry.before_text {
                        ui.add_space(6.0);
                        egui::Frame::NONE
                            .fill(Color32::from_rgba_unmultiplied(
                                colors.attention_fg.r(),
                                colors.attention_fg.g(),
                                colors.attention_fg.b(),
                                18,
                            ))
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(egui::Margin::symmetric(8, 5))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(format!("Before: {before}"))
                                        .size(12.0)
                                        .color(colors.text_fg),
                                );
                            });
                    }
                    if let Some(ref after) = entry.after_text {
                        ui.add_space(4.0);
                        egui::Frame::NONE
                            .fill(Color32::from_rgba_unmultiplied(
                                colors.accent.r(),
                                colors.accent.g(),
                                colors.accent.b(),
                                20,
                            ))
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(egui::Margin::symmetric(8, 5))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(format!("After: {after}"))
                                        .size(12.0)
                                        .color(colors.text_fg),
                                );
                            });
                    }
                });
            });
    });
}

// ---------------------------------------------------------------------------
// Shared section painting helpers
// ---------------------------------------------------------------------------

fn paint_section_top_bar(
    ui: &mut Ui,
    rect: Rect,
    title: &str,
    subtitle: &str,
    back_label: Option<&str>,
    colors: &HistoryColors,
) -> bool {
    let mut actions = Vec::new();
    if let Some(lbl) = back_label {
        actions.push((lbl, false));
    }
    matches!(
        paint_section_top_bar_with_actions(ui, rect, title, subtitle, &actions, colors),
        Some(0 | 1)
    )
}

/// Paints a section header bar (`54px`) with Back arrow (`Some(0)`) and optional right-side pill
/// buttons (`Some(1 + idx)`).
fn paint_section_top_bar_with_actions(
    ui: &mut Ui,
    rect: Rect,
    title: &str,
    subtitle: &str,
    right_buttons: &[(&str, bool)],
    colors: &HistoryColors,
) -> Option<usize> {
    let mut clicked = None;
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);

    // Back button
    let back_rect = Rect::from_center_size(
        Pos2::new(rect.left() + 24.0, rect.center().y),
        Vec2::splat(32.0),
    );
    let back_resp = ui.interact(
        back_rect,
        Id::unique(("tg_sec_back", title)),
        Sense::click(),
    );
    back_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Back"));
    if back_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .circle_filled(back_rect.center(), 16.0, colors.bar_bg_over);
    }
    paint_icon(
        ui.painter(),
        back_rect.shrink(7.0),
        IconKind::Back,
        colors.sub_text_fg,
    );
    if back_resp.clicked() {
        clicked = Some(0);
    }

    // Right action pills
    let mut right_x = rect.right() - 12.0;
    for (idx, &(label, active)) in right_buttons.iter().enumerate().rev() {
        let w = (label.len() as f32 * 7.2 + 20.0).max(56.0);
        let br = Rect::from_center_size(
            Pos2::new(right_x - w * 0.5, rect.center().y),
            Vec2::new(w, 28.0),
        );
        let resp = ui.interact(br, Id::unique(("tg_sec_btn", title, idx)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, active, label));
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let bg = if active {
            colors.accent
        } else if resp.hovered() {
            colors.bar_bg_over
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(br, CornerRadius::same(7), bg);
        if !active {
            ui.painter().rect_stroke(
                br,
                CornerRadius::same(7),
                Stroke::new(1.0, colors.divider),
                StrokeKind::Inside,
            );
        }
        ui.painter().text(
            br.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(12.0),
            if active {
                Color32::WHITE
            } else {
                colors.active_text_fg
            },
        );
        if resp.clicked() {
            clicked = Some(idx + 1);
        }
        right_x = br.left() - 6.0;
    }

    // Title & subtitle
    let tx = back_rect.right() + 10.0;
    ui.painter().text(
        Pos2::new(tx, rect.top() + 8.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(14.5),
        colors.bold_fg,
    );
    ui.painter().text(
        Pos2::new(tx, rect.top() + 28.0),
        Align2::LEFT_TOP,
        subtitle,
        FontId::proportional(11.5),
        colors.sub_text_fg,
    );

    ui.painter().line_segment(
        [
            Pos2::new(rect.left(), rect.bottom()),
            Pos2::new(rect.right(), rect.bottom()),
        ],
        Stroke::new(1.0, colors.divider),
    );
    clicked
}

fn paint_empty_section_pill(
    painter: &egui::Painter,
    rect: Rect,
    text: &str,
    colors: &HistoryColors,
) {
    let galley = painter.layout(
        text.to_owned(),
        FontId::proportional(13.0),
        colors.service_fg,
        320.0,
    );
    let pill = Rect::from_center_size(rect.center(), galley.size() + Vec2::new(32.0, 18.0));
    painter.rect_filled(pill, CornerRadius::same(14), colors.service_bg);
    painter.galley(
        Pos2::new(
            pill.center().x - galley.size().x * 0.5,
            pill.center().y - galley.size().y * 0.5,
        ),
        galley,
        colors.service_fg,
    );
}

fn paint_simple_message_feed(
    ui: &mut Ui,
    list_rect: Rect,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    messages: &[HistoryMessage],
    empty_label: &str,
    colors: &HistoryColors,
) {
    if messages.is_empty() {
        paint_empty_section_pill(ui.painter(), list_rect, empty_label, colors);
        return;
    }
    let ui_colors = colors.ui_colors();
    ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
        ScrollArea::vertical()
            .id_salt(id_salt)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(12.0);
                for msg in messages {
                    let bg = if msg.outgoing {
                        colors.msg_out_bg
                    } else {
                        colors.msg_in_bg
                    };
                    ui.horizontal(|ui| {
                        if msg.outgoing {
                            ui.add_space((list_rect.width() - 420.0).max(48.0));
                        } else {
                            ui.add_space(14.0);
                            let (av_r, _) =
                                ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
                            EmptyUserpic::for_peer(
                                msg.from_id.bare(),
                                &msg.sender_name,
                                &ui_colors,
                            )
                            .paint_circle(ui.painter(), av_r);
                            ui.add_space(4.0);
                        }
                        egui::Frame::NONE
                            .fill(bg)
                            .stroke(Stroke::new(0.8, colors.divider))
                            .corner_radius(CornerRadius::same(13))
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.vertical(|ui| {
                                    ui.set_max_width((list_rect.width() - 96.0).min(420.0));
                                    ui.label(
                                        egui::RichText::new(&msg.sender_name)
                                            .strong()
                                            .size(12.5)
                                            .color(colors.active_text_fg),
                                    );
                                    let body = if msg.text.is_empty() {
                                        msg.preview_snippet()
                                    } else {
                                        msg.text.text.clone()
                                    };
                                    ui.label(
                                        egui::RichText::new(body).size(13.5).color(colors.text_fg),
                                    );
                                    ui.label(
                                        egui::RichText::new(msg.formatted_time())
                                            .size(10.5)
                                            .color(colors.sub_text_fg),
                                    );
                                });
                            });
                    });
                    ui.add_space(7.0);
                }
                ui.add_space(10.0);
            });
    });
}

// ---------------------------------------------------------------------------
// 7. Forum Topic Custom Icon Picker (`data/data_forum_icons.cpp`)
// ---------------------------------------------------------------------------

/// Renders an interactive grid of default & custom emoji icons for forum topics.
///
/// Returns `Some(None)` when the user selects the default colored speech-bubble icon,
/// `Some(Some(doc_id))` when a custom emoji icon is picked, or `None` when unchanged.
pub fn render_forum_topic_icon_picker(
    ui: &mut Ui,
    icons: &[ForumTopicIconEntry],
    selected_doc_id: Option<i64>,
    colors: &HistoryColors,
) -> Option<Option<i64>> {
    let mut picked: Option<Option<i64>> = None;
    ScrollArea::horizontal()
        .id_salt("tg_forum_topic_icon_picker")
        .max_height(60.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // Default colored bubble option (`icon_emoji_id = 0`)
                let is_default = selected_doc_id.is_none();
                let (def_r, def_resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
                def_resp.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::Role::Button,
                        true,
                        is_default,
                        "Default Topic Bubble",
                    )
                });
                if def_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter().rect_filled(
                    def_r,
                    CornerRadius::same(7),
                    if is_default {
                        colors.accent
                    } else if def_resp.hovered() {
                        colors.bar_bg_over
                    } else {
                        Color32::TRANSPARENT
                    },
                );
                ui.painter().text(
                    def_r.center(),
                    Align2::CENTER_CENTER,
                    "#",
                    FontId::proportional(14.0),
                    if is_default {
                        Color32::WHITE
                    } else {
                        colors.bold_fg
                    },
                );
                if def_resp
                    .on_hover_text("Default colored topic bubble")
                    .clicked()
                {
                    picked = Some(None);
                }

                for icon in icons {
                    let selected = selected_doc_id == Some(icon.document_id);
                    let (tile, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::Role::Button,
                            true,
                            selected,
                            format!("Topic icon {}", icon.alt_emoji),
                        )
                    });
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    let bg = if selected {
                        colors.accent
                    } else if resp.hovered() {
                        colors.bar_bg_over
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(tile, CornerRadius::same(7), bg);
                    ui.painter().text(
                        tile.center(),
                        Align2::CENTER_CENTER,
                        &icon.alt_emoji,
                        FontId::proportional(15.0),
                        if selected {
                            Color32::WHITE
                        } else {
                            colors.text_fg
                        },
                    );
                    if resp
                        .on_hover_text(format!("{} (ID {})", icon.title, icon.document_id))
                        .clicked()
                    {
                        picked = Some(Some(icon.document_id));
                    }
                }
            });
        });
    picked
}

// ---------------------------------------------------------------------------
// 8. Subsection Tabs Slider (`history_view_subsection_tabs.cpp` + `subsection_tabs_slider.cpp`)
// ---------------------------------------------------------------------------

/// Height of the horizontal subsection tabs bar (`st::chatTabsSlider.height = 36px`).
pub const SUBSECTION_TABS_HORIZONTAL_HEIGHT: f32 = 36.0;

/// Width of the vertical subsection tabs sidebar (`st::chatTabsVertical.width = 64px`).
pub const SUBSECTION_TABS_VERTICAL_WIDTH: f32 = 64.0;

/// Width of the mode toggle button in horizontal (`Top`/`Bottom`) layouts.
const SUBSECTION_TAB_TOGGLE_WIDTH: f32 = 44.0;
/// Height of the mode toggle button in vertical (`Left`) layout (`st::chatTabsToggle.height`).
const SUBSECTION_TAB_TOGGLE_HEIGHT: f32 = 36.0;
/// Horizontal padding on each side of a tab (`st::chatTabsSlider.strictSkip / 2`).
const SUBSECTION_TAB_PAD_X: f32 = 10.0;
/// Gap between icon, title, and badges inside a single horizontal tab.
const SUBSECTION_TAB_INNER_GAP: f32 = 6.0;
/// Maximum title width in horizontal tabs on wide viewports before eliding with `…`.
const SUBSECTION_TAB_MAX_TITLE_WIDE: f32 = 144.0;
/// Active indicator slide duration (`st::chatTabsSlider.duration = 150ms`).
const SUBSECTION_TAB_ANIM_MS: u32 = 150;
/// Width/height of the edge fade gradient when tabs overflow the viewport.
const SUBSECTION_EDGE_FADE_PX: f32 = 28.0;
/// Unread pill height inside subsection tabs (`st::dialogsUnreadHeight` compact fit on 36px bar).
const SUBSECTION_BADGE_HEIGHT: f32 = 18.0;
/// Active indicator horizontal inset (`st::chatTabsOutlineHorizontal.skip = 8px`).
const SUBSECTION_INDICATOR_SKIP_H: f32 = 8.0;
/// Active indicator vertical inset in `Left` mode.
const SUBSECTION_INDICATOR_SKIP_V: f32 = 6.0;

/// One tab entry rendered inside [`SubsectionTabsWidget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsectionTabItem {
    /// Tab title (`"All Messages"`, topic title, or saved sublist name).
    pub title: String,
    /// Optional short glyph / letter / `#` icon.
    pub icon_glyph: String,
    /// Accent RGB for topic bubble / avatar dot.
    pub icon_rgb: [u8; 3],
    /// Whether this tab is currently active.
    pub active: bool,
    /// Whether this tab is pinned (eligible for reorder).
    pub pinned: bool,
    /// Unread message count badge.
    pub unread_count: u32,
    /// Unread mention count (`@` badge).
    pub unread_mentions: u32,
    /// True if notifications are muted for this tab.
    pub muted: bool,
    /// True if this is the synthetic `"All Messages"` root tab.
    pub is_all: bool,
    /// True if this is the Forum General (`#`) topic tab.
    pub is_general: bool,
    /// Optional Unicode emoji string for custom topic icons rendered via [`EmojiTextureCache`].
    pub custom_emoji: Option<String>,
    /// Optional forum topic root ID (`None` for `"All Messages"` or saved sublist).
    pub topic_root_id: Option<MsgId>,
    /// Optional saved sublist peer ID (`None` for `"All Messages"` or forum topic).
    pub sublist_peer_id: Option<PeerId>,
}

/// Stateful subsection tabs slider for Forum Topics and Saved Messages sublists
/// (`history_view_subsection_tabs.cpp` & `ui/controls/subsection_tabs_slider.cpp`).
pub struct SubsectionTabsWidget {
    /// Active dock position (`Top`, `Left`, `Bottom`).
    pub mode: SubsectionTabsMode,
    /// Active visual style (`TextAndIcons`, `TextOnly`, `IconsOnly`).
    pub appearance: SubsectionTabsAppearance,
    /// Whether the inline appearance/position menu popover is open.
    pub options_open: bool,
    /// Current scroll offset along the active slider axis (logical pixels).
    scroll_offset: f32,
    /// Maximum scroll offset from the most recent layout pass.
    last_max_scroll: f32,
    /// Whether a pointer drag gesture is actively scrolling the tab strip.
    drag_scroll_active: bool,
    /// Accumulated drag distance along the scroll axis during the current pointer press.
    drag_accum: f32,
    /// Animated start position of the active tab indicator (in content space).
    active_from: SimpleAnimation,
    /// Animated extent (width or height) of the active tab indicator.
    active_size: SimpleAnimation,
    /// True once the active tab indicator has been positioned at least once (skips first-render fly-in).
    anim_initialized: bool,
    /// Last active tab index observed (for detecting tab switches & auto-scrolling).
    last_active_idx: Option<usize>,
    /// Layout/peer fingerprint used to snap indicator & reset scroll on peer or mode switches.
    last_layout_key: Option<(u64, SubsectionTabsMode, SubsectionTabsAppearance)>,
    /// Bundled Noto emoji texture cache for custom forum topic icons.
    emoji_cache: EmojiTextureCache,
}

impl std::fmt::Debug for SubsectionTabsWidget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubsectionTabsWidget")
            .field("mode", &self.mode)
            .field("appearance", &self.appearance)
            .field("options_open", &self.options_open)
            .field("scroll_offset", &self.scroll_offset)
            .finish()
    }
}

impl Default for SubsectionTabsWidget {
    fn default() -> Self {
        Self::new()
    }
}

/// Elides `title` to fit within `max_width` using a real typographic ellipsis (`…`),
/// returning `(rendered_text, was_truncated)`.
pub(crate) fn elide_tab_title(
    painter: &egui::Painter,
    title: &str,
    font_id: &FontId,
    max_width: f32,
) -> (String, bool) {
    let full_w = painter
        .layout_no_wrap(title.to_owned(), font_id.clone(), Color32::WHITE)
        .size()
        .x;
    if full_w <= max_width + 0.5 {
        return (title.to_owned(), false);
    }
    let ellipsis = "\u{2026}";
    let chars: Vec<char> = title.chars().collect();
    if chars.is_empty() {
        return (String::new(), false);
    }
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        let prefix: String = chars[..mid].iter().collect();
        let candidate = format!("{}{ellipsis}", prefix.trim_end());
        let w = painter
            .layout_no_wrap(candidate, font_id.clone(), Color32::WHITE)
            .size()
            .x;
        if w <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        (ellipsis.to_owned(), true)
    } else {
        let prefix: String = chars[..lo].iter().collect();
        let trimmed = prefix.trim_end();
        if trimmed.is_empty() {
            (ellipsis.to_owned(), true)
        } else {
            (format!("{trimmed}{ellipsis}"), true)
        }
    }
}

/// Formats an unread counter matching `Ui::FormatUnreadCounter` compact conventions.
fn format_tab_unread_count(count: u32) -> String {
    if count >= 1_000_000 {
        format!("{}M", count / 1_000_000)
    } else if count >= 1_000 {
        format!("{}K", count / 1_000)
    } else {
        count.to_string()
    }
}

/// Paints a smooth horizontal alpha gradient mask over `rect` using GPU vertex interpolation.
fn paint_horizontal_edge_fade(
    painter: &egui::Painter,
    rect: Rect,
    bg: Color32,
    opaque_on_left: bool,
    strength: f32,
) {
    let alpha = (strength.clamp(0.0, 1.0) * 255.0).round() as u8;
    if alpha == 0 || rect.width() <= 0.5 || rect.height() <= 0.5 {
        return;
    }
    let [r, g, b, _] = bg.to_srgba_unmultiplied();
    let opaque = Color32::from_rgba_unmultiplied(r, g, b, alpha);
    let clear = Color32::from_rgba_unmultiplied(r, g, b, 0);
    let (left_c, right_c) = if opaque_on_left {
        (opaque, clear)
    } else {
        (clear, opaque)
    };
    let mesh = Mesh {
        vertices: vec![
            Vertex {
                pos: rect.left_top(),
                uv: Pos2::ZERO,
                color: left_c,
            },
            Vertex {
                pos: rect.right_top(),
                uv: Pos2::ZERO,
                color: right_c,
            },
            Vertex {
                pos: rect.right_bottom(),
                uv: Pos2::ZERO,
                color: right_c,
            },
            Vertex {
                pos: rect.left_bottom(),
                uv: Pos2::ZERO,
                color: left_c,
            },
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        ..Default::default()
    };
    painter.add(Shape::mesh(mesh));
}

/// Paints a smooth vertical alpha gradient mask over `rect` using GPU vertex interpolation.
fn paint_vertical_edge_fade(
    painter: &egui::Painter,
    rect: Rect,
    bg: Color32,
    opaque_on_top: bool,
    strength: f32,
) {
    let alpha = (strength.clamp(0.0, 1.0) * 255.0).round() as u8;
    if alpha == 0 || rect.width() <= 0.5 || rect.height() <= 0.5 {
        return;
    }
    let [r, g, b, _] = bg.to_srgba_unmultiplied();
    let opaque = Color32::from_rgba_unmultiplied(r, g, b, alpha);
    let clear = Color32::from_rgba_unmultiplied(r, g, b, 0);
    let (top_c, bot_c) = if opaque_on_top {
        (opaque, clear)
    } else {
        (clear, opaque)
    };
    let mesh = Mesh {
        vertices: vec![
            Vertex {
                pos: rect.left_top(),
                uv: Pos2::ZERO,
                color: top_c,
            },
            Vertex {
                pos: rect.right_top(),
                uv: Pos2::ZERO,
                color: top_c,
            },
            Vertex {
                pos: rect.right_bottom(),
                uv: Pos2::ZERO,
                color: bot_c,
            },
            Vertex {
                pos: rect.left_bottom(),
                uv: Pos2::ZERO,
                color: bot_c,
            },
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        ..Default::default()
    };
    painter.add(Shape::mesh(mesh));
}

/// Paints tdesktop's compact forum topic speech-bubble icon with a bold white initial letter.
fn paint_compact_topic_bubble(painter: &egui::Painter, rect: Rect, fill: Color32, letter: &str) {
    let body = Rect::from_min_max(
        Pos2::new(rect.left(), rect.top()),
        Pos2::new(rect.right(), rect.bottom() - 1.5),
    );
    let radius = (body.height() * 0.44).round().clamp(4.0, 10.0) as u8;
    painter.rect_filled(body, CornerRadius::same(radius), fill);
    let tail_w = (rect.width() * 0.28).clamp(3.0, 7.0);
    let tail = vec![
        Pos2::new(body.left() + tail_w * 0.45, body.bottom() - 1.5),
        Pos2::new(body.left() + 0.5, rect.bottom()),
        Pos2::new(body.left() + tail_w * 1.45, body.bottom() - 0.5),
    ];
    painter.add(Shape::convex_polygon(tail, fill, Stroke::NONE));
    if !letter.is_empty() {
        let font_size = (body.height() * 0.62).clamp(9.0, 14.0);
        painter.text(
            Pos2::new(body.center().x, body.center().y - 0.3),
            Align2::CENTER_CENTER,
            letter,
            FontId::new(font_size, family_semibold()),
            Color32::WHITE,
        );
    }
}

/// Measures a rounded pill unread badge matching `CountUnreadBadgeSize`.
fn measure_tab_unread_pill(painter: &egui::Painter, text: &str, font_id: &FontId) -> f32 {
    let text_w = painter
        .layout_no_wrap(text.to_owned(), font_id.clone(), Color32::WHITE)
        .size()
        .x;
    (text_w + 10.0).max(SUBSECTION_BADGE_HEIGHT)
}

/// Paints a vertically centered rounded pill unread badge and returns its width.
fn paint_tab_unread_pill(
    painter: &egui::Painter,
    left: f32,
    center_y: f32,
    text: &str,
    font_id: &FontId,
    bg: Color32,
    fg: Color32,
) -> f32 {
    let galley = painter.layout_no_wrap(text.to_owned(), font_id.clone(), fg);
    let w = (galley.size().x + 10.0).max(SUBSECTION_BADGE_HEIGHT);
    let pill = Rect::from_min_size(
        Pos2::new(left, center_y - SUBSECTION_BADGE_HEIGHT * 0.5),
        Vec2::new(w, SUBSECTION_BADGE_HEIGHT),
    );
    let radius = (SUBSECTION_BADGE_HEIGHT * 0.5).ceil() as u8;
    painter.rect_filled(pill, CornerRadius::same(radius), bg);
    painter.galley(
        Pos2::new(
            pill.center().x - galley.size().x * 0.5,
            pill.center().y - galley.size().y * 0.5 - 0.3,
        ),
        galley,
        fg,
    );
    w
}

struct MeasuredTab {
    elided_title: String,
    truncated: bool,
    show_icon: bool,
    icon_w: f32,
    title_w: f32,
    mention_w: f32,
    unread_text: Option<String>,
    unread_w: f32,
    start: f32,
    extent: f32,
}

impl SubsectionTabsWidget {
    /// Creates a new subsection tabs slider defaulting to `Top` + `TextAndIcons`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            mode: SubsectionTabsMode::Top,
            appearance: SubsectionTabsAppearance::TextAndIcons,
            options_open: false,
            scroll_offset: 0.0,
            last_max_scroll: 0.0,
            drag_scroll_active: false,
            drag_accum: 0.0,
            active_from: SimpleAnimation::new(),
            active_size: SimpleAnimation::new(),
            anim_initialized: false,
            last_active_idx: None,
            last_layout_key: None,
            emoji_cache: EmojiTextureCache::default(),
        }
    }

    /// Returns the current scroll offset along the active tab strip axis.
    #[must_use]
    pub const fn scroll_offset(&self) -> f32 {
        self.scroll_offset
    }

    /// Returns the maximum scroll offset from the most recent layout pass.
    #[must_use]
    pub const fn max_scroll_offset(&self) -> f32 {
        self.last_max_scroll
    }

    /// Loads `(mode, appearance)` from tdesktop's packed `subsection_tabs_modes` value.
    pub fn apply_packed_state(&mut self, stored: i32) {
        let (mode, appearance) = unpack_subsection_tabs_state(stored);
        self.mode = mode;
        self.appearance = appearance;
    }

    /// Packs the current `(mode, appearance)` for persistence in `SessionSettings`.
    #[must_use]
    pub const fn packed_state(&self) -> i32 {
        pack_subsection_tabs_state(self.mode, self.appearance)
    }

    /// Cycles the slider layout mode (`Top -> Left -> Bottom -> Top`).
    pub fn cycle_mode(&mut self) {
        self.mode = self.mode.next();
        self.scroll_offset = 0.0;
    }

    /// Renders the subsection tabs slider for a Forum supergroup around `outer_rect`,
    /// returning `(remaining_content_rect, optional_section_action)`.
    #[allow(clippy::too_many_arguments)]
    pub fn show_for_forum(
        &mut self,
        ui: &mut Ui,
        outer_rect: Rect,
        peer_id: PeerId,
        active_topic: Option<MsgId>,
        topics: &[ForumTopic],
        backend: &mut dyn HistoryBackend,
        colors: &HistoryColors,
    ) -> (Rect, Option<HistorySectionAction>) {
        if topics.is_empty() {
            return (outer_rect, None);
        }
        let mut items = Vec::with_capacity(topics.len() + 1);
        items.push(SubsectionTabItem {
            title: "All Messages".to_owned(),
            icon_glyph: String::new(),
            icon_rgb: [colors.accent.r(), colors.accent.g(), colors.accent.b()],
            active: active_topic.is_none(),
            pinned: false,
            unread_count: 0,
            unread_mentions: 0,
            muted: false,
            is_all: true,
            is_general: false,
            custom_emoji: None,
            topic_root_id: None,
            sublist_peer_id: None,
        });
        let mut sorted = topics.to_vec();
        sorted.sort_by(|a, b| {
            b.is_pinned
                .cmp(&a.is_pinned)
                .then_with(|| b.is_general.cmp(&a.is_general))
                .then_with(|| b.last_date_unix.cmp(&a.last_date_unix))
        });
        for t in &sorted {
            let letter = if t.is_general {
                "#".to_owned()
            } else {
                extract_non_emoji_letter(&t.title).unwrap_or_else(|| "T".to_owned())
            };
            let custom_emoji = t
                .custom_emoji
                .as_ref()
                .filter(|em| !em.is_empty() && !em.chars().all(|c| c.is_ascii_digit()))
                .cloned();
            items.push(SubsectionTabItem {
                title: t.title.clone(),
                icon_glyph: letter,
                icon_rgb: t.icon_rgb(),
                active: active_topic == Some(t.root_id),
                pinned: t.is_pinned,
                unread_count: t.unread_count,
                unread_mentions: t.unread_mentions,
                muted: t.muted,
                is_all: false,
                is_general: t.is_general,
                custom_emoji,
                topic_root_id: Some(t.root_id),
                sublist_peer_id: None,
            });
        }

        let (remaining, click) = self.show_slider(
            ui,
            outer_rect,
            ("tg_subtabs_forum", peer_id.0),
            &items,
            colors,
        );
        let mut action = None;
        if let Some(ev) = click {
            match ev {
                SliderEvent::Activate(idx) => {
                    if let Some(item) = items.get(idx) {
                        if let Some(tid) = item.topic_root_id {
                            action = Some(HistorySectionAction::OpenSection(
                                HistorySectionKind::ForumTopicHistory {
                                    peer_id,
                                    topic_root_id: tid,
                                },
                            ));
                        } else {
                            action = Some(HistorySectionAction::Back);
                        }
                    }
                }
                SliderEvent::MovePinnedEarlier(idx) => {
                    if let Some(item) = items.get(idx)
                        && let Some(tid) = item.topic_root_id
                    {
                        let mut pinned_order: Vec<MsgId> = sorted
                            .iter()
                            .filter(|t| t.is_pinned)
                            .map(|t| t.root_id)
                            .collect();
                        if let Some(pos) = pinned_order.iter().position(|&id| id == tid)
                            && pos > 0
                        {
                            pinned_order.swap(pos, pos - 1);
                            let _ = backend.reorder_pinned_forum_topics(peer_id, &pinned_order);
                            action = Some(HistorySectionAction::Toast(
                                "Pinned topics reordered".to_owned(),
                            ));
                        }
                    }
                }
                SliderEvent::TogglePin(idx) => {
                    if let Some(item) = items.get(idx)
                        && let Some(tid) = item.topic_root_id
                    {
                        let _ = backend.toggle_forum_topic_pin(peer_id, tid);
                    }
                }
            }
        }
        (remaining, action)
    }

    /// Renders the subsection tabs slider for Saved Messages sublists around `outer_rect`.
    pub fn show_for_saved(
        &mut self,
        ui: &mut Ui,
        outer_rect: Rect,
        peer_id: PeerId,
        active_sublist: Option<PeerId>,
        sublists: &[crate::history::sections_model::SavedSublist],
        colors: &HistoryColors,
    ) -> (Rect, Option<HistorySectionAction>) {
        if sublists.is_empty() {
            return (outer_rect, None);
        }
        let mut items = Vec::with_capacity(sublists.len() + 1);
        items.push(SubsectionTabItem {
            title: "All Messages".to_owned(),
            icon_glyph: String::new(),
            icon_rgb: [colors.accent.r(), colors.accent.g(), colors.accent.b()],
            active: active_sublist.is_none(),
            pinned: false,
            unread_count: 0,
            unread_mentions: 0,
            muted: false,
            is_all: true,
            is_general: false,
            custom_emoji: None,
            topic_root_id: None,
            sublist_peer_id: None,
        });
        for s in sublists {
            items.push(SubsectionTabItem {
                title: s.title.clone(),
                icon_glyph: if s.is_my_notes {
                    String::new()
                } else {
                    s.initials.clone()
                },
                icon_rgb: s.avatar_rgb,
                active: active_sublist == Some(s.sublist_peer_id),
                pinned: s.is_pinned,
                unread_count: 0,
                unread_mentions: 0,
                muted: false,
                is_all: false,
                is_general: false,
                custom_emoji: s.is_my_notes.then(|| "🔖".to_owned()),
                topic_root_id: None,
                sublist_peer_id: Some(s.sublist_peer_id),
            });
        }

        let (remaining, click) = self.show_slider(
            ui,
            outer_rect,
            ("tg_subtabs_saved", peer_id.0),
            &items,
            colors,
        );
        let mut action = None;
        if let Some(SliderEvent::Activate(idx)) = click
            && let Some(item) = items.get(idx)
        {
            if let Some(sub_id) = item.sublist_peer_id {
                action = Some(HistorySectionAction::OpenSection(
                    HistorySectionKind::SavedSublistHistory {
                        peer_id,
                        sublist_peer_id: sub_id,
                    },
                ));
            } else {
                action = Some(HistorySectionAction::Back);
            }
        }
        (remaining, action)
    }

    fn paint_tab_icon(
        &mut self,
        painter: &egui::Painter,
        ctx: &egui::Context,
        item: &SubsectionTabItem,
        icon_rect: Rect,
        fg: Color32,
        colors: &HistoryColors,
    ) {
        if item.is_all {
            let _ = paint_style_icon(
                painter,
                icon_rect,
                st::folders_all,
                &DefaultPaletteResolver,
                Some(fg),
            );
            return;
        }
        if item.is_general {
            let font_size = (icon_rect.height() * 0.88).clamp(13.0, 20.0);
            painter.text(
                Pos2::new(icon_rect.center().x, icon_rect.center().y - 0.5),
                Align2::CENTER_CENTER,
                "#",
                FontId::new(font_size, family_semibold()),
                fg,
            );
            return;
        }
        if let Some(ref em) = item.custom_emoji
            && self
                .emoji_cache
                .paint_emoji_str(painter, ctx, em, icon_rect, Color32::WHITE)
        {
            return;
        }
        if item.sublist_peer_id.is_some() && item.icon_glyph.is_empty() {
            let _ = paint_style_icon(
                painter,
                icon_rect,
                st::menu_icon_saved_messages,
                &DefaultPaletteResolver,
                Some(colors.accent),
            );
            return;
        }
        let [r, g, b] = item.icon_rgb;
        let fill = Color32::from_rgb(r, g, b);
        if item.sublist_peer_id.is_some() {
            let rad = (icon_rect.width().min(icon_rect.height()) * 0.5).max(4.0);
            painter.circle_filled(icon_rect.center(), rad, fill);
            let font_size = (rad * 1.15).clamp(8.5, 13.0);
            painter.text(
                icon_rect.center(),
                Align2::CENTER_CENTER,
                &item.icon_glyph,
                FontId::new(font_size, family_semibold()),
                Color32::WHITE,
            );
        } else {
            paint_compact_topic_bubble(painter, icon_rect, fill, &item.icon_glyph);
        }
    }

    fn show_slider(
        &mut self,
        ui: &mut Ui,
        outer_rect: Rect,
        id_salt: impl Hash + std::fmt::Debug + Copy,
        items: &[SubsectionTabItem],
        colors: &HistoryColors,
    ) -> (Rect, Option<SliderEvent>) {
        let (bar_rect, remaining) = match self.mode {
            SubsectionTabsMode::Top => {
                let h = SUBSECTION_TABS_HORIZONTAL_HEIGHT.min(outer_rect.height() * 0.35);
                (
                    Rect::from_min_size(outer_rect.min, Vec2::new(outer_rect.width(), h)),
                    Rect::from_min_max(
                        Pos2::new(outer_rect.left(), outer_rect.top() + h),
                        outer_rect.max,
                    ),
                )
            }
            SubsectionTabsMode::Bottom => {
                let h = SUBSECTION_TABS_HORIZONTAL_HEIGHT.min(outer_rect.height() * 0.35);
                (
                    Rect::from_min_size(
                        Pos2::new(outer_rect.left(), outer_rect.bottom() - h),
                        Vec2::new(outer_rect.width(), h),
                    ),
                    Rect::from_min_max(
                        outer_rect.min,
                        Pos2::new(outer_rect.right(), outer_rect.bottom() - h),
                    ),
                )
            }
            SubsectionTabsMode::Left => {
                let w = SUBSECTION_TABS_VERTICAL_WIDTH.min(outer_rect.width() * 0.35);
                (
                    Rect::from_min_size(outer_rect.min, Vec2::new(w, outer_rect.height())),
                    Rect::from_min_max(
                        Pos2::new(outer_rect.left() + w, outer_rect.top()),
                        outer_rect.max,
                    ),
                )
            }
        };

        ui.painter()
            .rect_filled(bar_rect, CornerRadius::ZERO, colors.bar_bg);

        let vertical = matches!(self.mode, SubsectionTabsMode::Left);
        let mut event = None;

        // 1. Mode toggle button (`chatTabsToggle` in `history_view_subsection_tabs.cpp` + `chat.style`).
        let toggle_rect = if vertical {
            Rect::from_min_size(
                bar_rect.min,
                Vec2::new(
                    bar_rect.width(),
                    SUBSECTION_TAB_TOGGLE_HEIGHT.min(bar_rect.height()),
                ),
            )
        } else {
            Rect::from_min_size(
                bar_rect.min,
                Vec2::new(
                    SUBSECTION_TAB_TOGGLE_WIDTH.min(bar_rect.width()),
                    bar_rect.height(),
                ),
            )
        };
        let toggle_resp = ui.interact(
            toggle_rect,
            Id::unique((id_salt, "mode_toggle")),
            Sense::click(),
        );
        toggle_resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Button,
                true,
                format!("Subsection Tabs Layout ({})", self.mode.label()),
            )
        });
        let toggle_hover = toggle_resp.hovered();
        let toggle_focus = toggle_resp.has_focus();
        if toggle_hover {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter().rect_filled(
                toggle_rect.shrink2(Vec2::new(4.0, 4.0)),
                CornerRadius::same(6),
                colors.bar_bg_over,
            );
        }
        if toggle_focus {
            ui.painter().rect_stroke(
                toggle_rect.shrink2(Vec2::new(3.0, 3.0)),
                CornerRadius::same(6),
                Stroke::new(2.0, colors.accent),
                StrokeKind::Inside,
            );
        }
        let toggle_icon = match self.mode {
            SubsectionTabsMode::Top => st::chat_tabs_toggle_icon_top,
            SubsectionTabsMode::Left => st::chat_tabs_toggle_icon_left,
            SubsectionTabsMode::Bottom => st::chat_tabs_toggle_icon_bottom,
        };
        let toggle_icon_color = if toggle_hover || toggle_focus {
            colors.text_fg
        } else {
            colors.sub_text_fg
        };
        let toggle_icon_rect = Rect::from_center_size(toggle_rect.center(), Vec2::splat(22.0));
        let _ = paint_style_icon(
            ui.painter(),
            toggle_icon_rect,
            toggle_icon,
            &DefaultPaletteResolver,
            Some(toggle_icon_color),
        );
        let toggle_resp =
            toggle_resp.on_hover_text("Switch tabs layout (right-click for appearance)");
        if toggle_resp.clicked() {
            self.cycle_mode();
        }
        if toggle_resp.secondary_clicked() {
            self.options_open = !self.options_open;
        }

        let scroll_rect = if vertical {
            Rect::from_min_max(
                Pos2::new(bar_rect.left(), toggle_rect.bottom()),
                bar_rect.max,
            )
        } else {
            Rect::from_min_max(Pos2::new(toggle_rect.right(), bar_rect.top()), bar_rect.max)
        };

        let show_icons = !matches!(self.appearance, SubsectionTabsAppearance::TextOnly);
        let show_text = !matches!(self.appearance, SubsectionTabsAppearance::IconsOnly);

        let label_font = if vertical {
            FontId::new(10.5, family_semibold())
        } else {
            FontId::new(13.0, family_semibold())
        };
        let badge_font = FontId::new(11.0, family_semibold());
        let max_title_w = if vertical {
            (scroll_rect.width() - 10.0).max(24.0)
        } else {
            (scroll_rect.width() * 0.44).clamp(88.0, SUBSECTION_TAB_MAX_TITLE_WIDE)
        };

        // 2. Measure every tab using real glyph metrics (matching `HorizontalButton::updateSize`).
        let mut measured = Vec::with_capacity(items.len());
        let mut cursor = 0.0_f32;
        for item in items {
            let item_show_icon = if vertical {
                show_icons
            } else {
                // In tdesktop horizontal mode, "All Messages" is text-only unless `IconsOnly` is chosen.
                show_icons && (!item.is_all || !show_text)
            };
            let (elided_title, truncated) = if show_text {
                elide_tab_title(ui.painter(), &item.title, &label_font, max_title_w)
            } else {
                (String::new(), false)
            };
            let title_w = if show_text {
                ui.painter()
                    .layout_no_wrap(elided_title.clone(), label_font.clone(), Color32::WHITE)
                    .size()
                    .x
            } else {
                0.0
            };
            let icon_w = if !item_show_icon {
                0.0
            } else if vertical {
                26.0
            } else if item.is_general {
                12.0
            } else {
                17.0
            };
            let mention_w = if item.unread_mentions > 0 {
                SUBSECTION_BADGE_HEIGHT
            } else {
                0.0
            };
            let unread_text =
                (item.unread_count > 0).then(|| format_tab_unread_count(item.unread_count));
            let unread_w = unread_text.as_deref().map_or(0.0, |t| {
                measure_tab_unread_pill(ui.painter(), t, &badge_font)
            });

            let extent = if vertical {
                if item_show_icon && show_text {
                    58.0
                } else if item_show_icon {
                    44.0
                } else {
                    34.0
                }
            } else {
                let mut content_w = 0.0_f32;
                if icon_w > 0.0 {
                    content_w += icon_w;
                }
                if show_text && title_w > 0.0 {
                    if content_w > 0.0 {
                        content_w += SUBSECTION_TAB_INNER_GAP;
                    }
                    content_w += title_w;
                }
                let badges_w = match (mention_w > 0.0, unread_w > 0.0) {
                    (true, true) => mention_w + 4.0 + unread_w,
                    (true, false) => mention_w,
                    (false, true) => unread_w,
                    (false, false) => 0.0,
                };
                if badges_w > 0.0 {
                    if content_w > 0.0 {
                        content_w += SUBSECTION_TAB_INNER_GAP;
                    }
                    content_w += badges_w;
                }
                let pad_x = if show_text { SUBSECTION_TAB_PAD_X } else { 8.0 };
                (content_w + pad_x * 2.0).max(38.0)
            };

            measured.push(MeasuredTab {
                elided_title,
                truncated,
                show_icon: item_show_icon,
                icon_w,
                title_w,
                mention_w,
                unread_text,
                unread_w,
                start: cursor,
                extent,
            });
            cursor += extent;
        }

        let total_extent = cursor;
        let viewport_extent = if vertical {
            scroll_rect.height().max(1.0)
        } else {
            scroll_rect.width().max(1.0)
        };
        let max_scroll = (total_extent - viewport_extent).max(0.0);
        self.last_max_scroll = max_scroll;

        // Detect peer / mode / appearance transitions so scroll and indicator snap cleanly.
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        id_salt.hash(&mut hasher);
        let salt_hash = hasher.finish();
        let layout_key = (salt_hash, self.mode, self.appearance);
        let layout_changed = self.last_layout_key != Some(layout_key);
        if layout_changed {
            self.last_layout_key = Some(layout_key);
            self.scroll_offset = 0.0;
            self.drag_scroll_active = false;
            self.drag_accum = 0.0;
        }
        self.scroll_offset = self.scroll_offset.clamp(0.0, max_scroll);

        // 3. Wheel scrolling (`history_view_subsection_tabs.cpp:184-193`: vertical wheel scrolls horizontal bar).
        let pointer_in_scroll = ui.ctx().input(|i| {
            i.pointer
                .hover_pos()
                .is_some_and(|p| scroll_rect.contains(p))
        });
        if pointer_in_scroll && max_scroll > 0.0 {
            let wheel = ui.ctx().input(|i| i.smooth_scroll_delta);
            let delta = if vertical {
                wheel.y
            } else if wheel.x.abs() > 0.1 {
                wheel.x
            } else {
                wheel.y
            };
            if delta.abs() > 0.01 {
                self.scroll_offset = (self.scroll_offset - delta).clamp(0.0, max_scroll);
            }
        }

        // 4. Active tab indicator animation (`SubsectionSlider` 150ms interruptible EaseOutCubic).
        let active_idx = items.iter().position(|it| it.active);
        let now = ui.ctx().input(|i| i.time);
        let anim_disabled = tg_ui::anim::disabled() || ui.style().animation_time <= 0.0;
        let active_changed = self.last_active_idx != active_idx;

        if let Some(aidx) = active_idx
            && let Some(mt) = measured.get(aidx)
        {
            // Auto-scroll newly active tab into view with neighbor context (`SubsectionSlider::requestShown`).
            if active_changed && !layout_changed && max_scroll > 0.0 {
                let context_margin = (viewport_extent - mt.extent).clamp(0.0, mt.extent) * 0.5;
                let want_min = (mt.start - context_margin).max(0.0);
                let want_max = (mt.start + mt.extent + context_margin).min(total_extent);
                if want_min < self.scroll_offset {
                    self.scroll_offset = want_min.clamp(0.0, max_scroll);
                } else if want_max > self.scroll_offset + viewport_extent {
                    let target = want_min.min(want_max - viewport_extent);
                    self.scroll_offset = target.clamp(0.0, max_scroll);
                }
            }

            let target_from = f64::from(mt.start);
            let target_size = f64::from(mt.extent);
            if !self.anim_initialized || layout_changed || anim_disabled {
                self.active_from
                    .start(now, target_from, target_from, 0, Transition::EaseOutCubic);
                self.active_size
                    .start(now, target_size, target_size, 0, Transition::EaseOutCubic);
            } else if active_changed {
                self.active_from.change(
                    now,
                    target_from,
                    SUBSECTION_TAB_ANIM_MS,
                    Transition::EaseOutCubic,
                );
                self.active_size.change(
                    now,
                    target_size,
                    SUBSECTION_TAB_ANIM_MS,
                    Transition::EaseOutCubic,
                );
                ui.ctx().request_repaint();
            }
        }
        self.anim_initialized = true;
        self.last_active_idx = active_idx;

        let (ind_from, ind_size) = if let Some(aidx) = active_idx
            && let Some(mt) = measured.get(aidx)
        {
            let cur_from = self.active_from.sample(now, f64::from(mt.start)) as f32;
            let cur_size = self.active_size.sample(now, f64::from(mt.extent)) as f32;
            if self.active_from.animating() || self.active_size.animating() {
                ui.ctx().request_repaint();
            }
            (Some(cur_from), Some(cur_size))
        } else {
            (None, None)
        };

        // 5. Paint & interact tabs inside the clipped `scroll_rect`.
        let clipped = ui.painter().with_clip_rect(scroll_rect);
        let ctx = ui.ctx().clone();
        let mut any_dragged = false;
        let pinned_bg = if colors.is_night {
            Color32::from_rgba_unmultiplied(255, 255, 255, 11)
        } else {
            Color32::from_rgba_unmultiplied(0, 0, 0, 10)
        };
        let muted_badge_bg = if colors.is_night {
            Color32::from_rgb(0x46, 0x5B, 0x70)
        } else {
            colors.sub_text_fg
        };

        // Paint continuous rounded background behind pinned tabs (`HorizontalButton::paintPinnedBackground`).
        let first_pinned = items.iter().position(|it| it.pinned && !it.is_all);
        let last_pinned = items.iter().rposition(|it| it.pinned && !it.is_all);
        if let (Some(fp), Some(lp)) = (first_pinned, last_pinned) {
            let start = measured[fp].start;
            let end = measured[lp].start + measured[lp].extent;
            let bg_rect = if vertical {
                Rect::from_min_max(
                    Pos2::new(
                        scroll_rect.left() + 4.0,
                        scroll_rect.top() + start - self.scroll_offset + 2.0,
                    ),
                    Pos2::new(
                        scroll_rect.right() - 4.0,
                        scroll_rect.top() + end - self.scroll_offset - 2.0,
                    ),
                )
            } else {
                Rect::from_min_max(
                    Pos2::new(
                        scroll_rect.left() + start - self.scroll_offset + 2.0,
                        scroll_rect.top() + 4.0,
                    ),
                    Pos2::new(
                        scroll_rect.left() + end - self.scroll_offset - 2.0,
                        scroll_rect.bottom() - 4.0,
                    ),
                )
            };
            if bg_rect.intersects(scroll_rect) {
                clipped.rect_filled(bg_rect, CornerRadius::same(6), pinned_bg);
            }
        }

        for (idx, (item, mt)) in items.iter().zip(measured.iter()).enumerate() {
            let tab_r = if vertical {
                Rect::from_min_size(
                    Pos2::new(
                        scroll_rect.left(),
                        scroll_rect.top() + mt.start - self.scroll_offset,
                    ),
                    Vec2::new(scroll_rect.width(), mt.extent),
                )
            } else {
                Rect::from_min_size(
                    Pos2::new(
                        scroll_rect.left() + mt.start - self.scroll_offset,
                        scroll_rect.top(),
                    ),
                    Vec2::new(mt.extent, scroll_rect.height()),
                )
            };
            let visible_r = tab_r.intersect(scroll_rect);
            if visible_r.width() <= 0.5 || visible_r.height() <= 0.5 {
                continue;
            }

            let resp = ui.interact(
                visible_r,
                Id::unique((id_salt, "tab", idx)),
                Sense::click_and_drag(),
            );
            resp.widget_info(|| {
                egui::WidgetInfo::selected(egui::Role::Tab, true, item.active, &item.title)
            });

            // Handle horizontal/vertical drag-to-scroll when content overflows.
            if resp.dragged() && max_scroll > 0.0 {
                any_dragged = true;
                let delta = if vertical {
                    resp.drag_delta().y
                } else {
                    resp.drag_delta().x
                };
                self.drag_accum += delta.abs();
                if self.drag_accum > 4.0 {
                    self.drag_scroll_active = true;
                    self.scroll_offset = (self.scroll_offset - delta).clamp(0.0, max_scroll);
                    ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                }
            }

            let hovered = resp.hovered() && !self.drag_scroll_active;
            if hovered {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                let hover_r = if vertical {
                    tab_r.shrink2(Vec2::new(4.0, 2.0))
                } else {
                    tab_r.shrink2(Vec2::new(2.0, 3.0))
                };
                clipped.rect_filled(hover_r, CornerRadius::same(6), colors.bar_bg_over);
            }

            if resp.has_focus() {
                let focus_r = tab_r.shrink2(Vec2::new(2.0, 2.0));
                clipped.rect_stroke(
                    focus_r,
                    CornerRadius::same(6),
                    Stroke::new(2.0, colors.accent),
                    StrokeKind::Inside,
                );
                // Keyboard navigation within the tablist (WCAG / ARIA APG Tabs pattern).
                let (prev_key, next_key) = if vertical {
                    (Key::ArrowUp, Key::ArrowDown)
                } else {
                    (Key::ArrowLeft, Key::ArrowRight)
                };
                let key_target = ui.ctx().input(|i| {
                    if i.key_pressed(next_key) && idx + 1 < items.len() {
                        Some(idx + 1)
                    } else if i.key_pressed(prev_key) && idx > 0 {
                        Some(idx - 1)
                    } else if i.key_pressed(Key::Home) {
                        Some(0)
                    } else if i.key_pressed(Key::End) && !items.is_empty() {
                        Some(items.len() - 1)
                    } else if i.key_pressed(Key::Enter) || i.key_pressed(Key::Space) {
                        Some(idx)
                    } else {
                        None
                    }
                });
                if let Some(target_idx) = key_target {
                    event = Some(SliderEvent::Activate(target_idx));
                }
            }

            // Active color blend matching `SubsectionSlider::buttonActive`.
            let active_ratio = match (ind_from, ind_size) {
                (Some(f), Some(s)) => {
                    let check = mt.extent.min(s);
                    if check > 0.0 {
                        (1.0 - (f - mt.start).abs() / check).clamp(0.0, 1.0)
                    } else if item.active {
                        1.0
                    } else {
                        0.0
                    }
                }
                _ => {
                    if item.active {
                        1.0
                    } else {
                        0.0
                    }
                }
            };
            let base_fg = if hovered && !item.active {
                color_lerp(colors.sub_text_fg, colors.text_fg, 0.35)
            } else {
                colors.sub_text_fg
            };
            let fg = color_lerp(base_fg, colors.active_text_fg, f64::from(active_ratio));

            if vertical {
                let cx = tab_r.center().x;
                if mt.show_icon {
                    let iy = if show_text {
                        tab_r.top() + 20.0
                    } else {
                        tab_r.center().y
                    };
                    let icon_rect = Rect::from_center_size(Pos2::new(cx, iy), Vec2::splat(26.0));
                    self.paint_tab_icon(&clipped, &ctx, item, icon_rect, fg, colors);
                }
                if show_text {
                    let ty = if mt.show_icon {
                        tab_r.top() + 45.0
                    } else {
                        tab_r.center().y
                    };
                    clipped.text(
                        Pos2::new(cx, ty),
                        Align2::CENTER_CENTER,
                        &mt.elided_title,
                        label_font.clone(),
                        fg,
                    );
                }
                // Compact top-right unread pill inside vertical tab (`VerticalButton::paintEvent`).
                let badge_y = tab_r.top() + 12.0;
                if let Some(ref count_str) = mt.unread_text {
                    let badge_bg = if item.muted {
                        muted_badge_bg
                    } else {
                        colors.accent
                    };
                    let pw = measure_tab_unread_pill(&clipped, count_str, &badge_font);
                    let left = (tab_r.right() - pw - 4.0).max(tab_r.left() + 4.0);
                    let _ = paint_tab_unread_pill(
                        &clipped,
                        left,
                        badge_y,
                        count_str,
                        &badge_font,
                        badge_bg,
                        Color32::WHITE,
                    );
                } else if item.unread_mentions > 0 {
                    let left =
                        (tab_r.right() - SUBSECTION_BADGE_HEIGHT - 4.0).max(tab_r.left() + 4.0);
                    let _ = paint_tab_unread_pill(
                        &clipped,
                        left,
                        badge_y,
                        "@",
                        &badge_font,
                        colors.accent,
                        Color32::WHITE,
                    );
                }
            } else {
                // Horizontal tab: `[pad] [icon] [gap] [title] [gap] [@] [unread pill] [pad]`,
                // all vertically centered as one cohesive unit (`HorizontalButton::paintEvent`).
                let cy = tab_r.center().y;
                let pad_x = if show_text { SUBSECTION_TAB_PAD_X } else { 8.0 };
                let mut x = tab_r.left() + pad_x;

                if mt.show_icon {
                    let icon_rect = Rect::from_center_size(
                        Pos2::new(x + mt.icon_w * 0.5, cy),
                        Vec2::new(mt.icon_w, 17.0),
                    );
                    self.paint_tab_icon(&clipped, &ctx, item, icon_rect, fg, colors);
                    x += mt.icon_w;
                    if show_text && mt.title_w > 0.0 {
                        x += SUBSECTION_TAB_INNER_GAP;
                    }
                }
                if show_text && mt.title_w > 0.0 {
                    clipped.text(
                        Pos2::new(x, cy - 0.3),
                        Align2::LEFT_CENTER,
                        &mt.elided_title,
                        label_font.clone(),
                        fg,
                    );
                    x += mt.title_w;
                }
                let has_badges = mt.mention_w > 0.0 || mt.unread_w > 0.0;
                if has_badges {
                    if mt.show_icon || (show_text && mt.title_w > 0.0) {
                        x += SUBSECTION_TAB_INNER_GAP;
                    }
                    if mt.mention_w > 0.0 {
                        let w = paint_tab_unread_pill(
                            &clipped,
                            x,
                            cy,
                            "@",
                            &badge_font,
                            colors.accent,
                            Color32::WHITE,
                        );
                        x += w + 4.0;
                    }
                    if let Some(ref count_str) = mt.unread_text {
                        let badge_bg = if item.muted {
                            muted_badge_bg
                        } else {
                            colors.accent
                        };
                        let _ = paint_tab_unread_pill(
                            &clipped,
                            x,
                            cy,
                            count_str,
                            &badge_font,
                            badge_bg,
                            Color32::WHITE,
                        );
                    }
                }
            }

            let resp = if mt.truncated || !show_text {
                resp.on_hover_text(&item.title)
            } else {
                resp
            };
            if resp.clicked() && !self.drag_scroll_active {
                event = Some(SliderEvent::Activate(idx));
            } else if resp.secondary_clicked() {
                if item.pinned {
                    event = Some(SliderEvent::MovePinnedEarlier(idx));
                } else if item.topic_root_id.is_some() {
                    event = Some(SliderEvent::TogglePin(idx));
                }
            }
        }

        if !any_dragged && !ui.ctx().input(|i| i.pointer.primary_down()) {
            self.drag_scroll_active = false;
            self.drag_accum = 0.0;
        }

        // 6. Active tab indicator pill (`SubsectionSlider::setupBar`, `chatTabsOutlineHorizontal`).
        if let (Some(cur_from), Some(cur_size)) = (ind_from, ind_size) {
            match self.mode {
                SubsectionTabsMode::Top => {
                    let from = scroll_rect.left() + cur_from - self.scroll_offset
                        + SUBSECTION_INDICATOR_SKIP_H;
                    let w = (cur_size - 2.0 * SUBSECTION_INDICATOR_SKIP_H).max(12.0);
                    let bar = Rect::from_min_size(
                        Pos2::new(from, scroll_rect.bottom() - 3.5),
                        Vec2::new(w, 3.5),
                    );
                    clipped.rect_filled(
                        bar,
                        CornerRadius {
                            nw: 3,
                            ne: 3,
                            sw: 0,
                            se: 0,
                        },
                        colors.accent,
                    );
                }
                SubsectionTabsMode::Bottom => {
                    let from = scroll_rect.left() + cur_from - self.scroll_offset
                        + SUBSECTION_INDICATOR_SKIP_H;
                    let w = (cur_size - 2.0 * SUBSECTION_INDICATOR_SKIP_H).max(12.0);
                    let bar =
                        Rect::from_min_size(Pos2::new(from, scroll_rect.top()), Vec2::new(w, 3.5));
                    clipped.rect_filled(
                        bar,
                        CornerRadius {
                            nw: 0,
                            ne: 0,
                            sw: 3,
                            se: 3,
                        },
                        colors.accent,
                    );
                }
                SubsectionTabsMode::Left => {
                    let from = scroll_rect.top() + cur_from - self.scroll_offset
                        + SUBSECTION_INDICATOR_SKIP_V;
                    let h = (cur_size - 2.0 * SUBSECTION_INDICATOR_SKIP_V).max(12.0);
                    let bar =
                        Rect::from_min_size(Pos2::new(scroll_rect.left(), from), Vec2::new(3.5, h));
                    clipped.rect_filled(
                        bar,
                        CornerRadius {
                            nw: 0,
                            ne: 3,
                            sw: 0,
                            se: 3,
                        },
                        colors.accent,
                    );
                }
            }
        }

        // 7. Scroll-linked edge fade masks + tdesktop scroll shadow next to toggle button.
        if vertical {
            let top_strength = (self.scroll_offset / 24.0).clamp(0.0, 1.0);
            let bot_strength = ((max_scroll - self.scroll_offset) / 24.0).clamp(0.0, 1.0);
            let fade_h = SUBSECTION_EDGE_FADE_PX.min(scroll_rect.height() * 0.25);
            if top_strength > 0.01 {
                paint_vertical_edge_fade(
                    &clipped,
                    Rect::from_min_size(scroll_rect.min, Vec2::new(scroll_rect.width(), fade_h)),
                    colors.bar_bg,
                    true,
                    top_strength,
                );
                ui.painter().line_segment(
                    [
                        Pos2::new(bar_rect.left(), toggle_rect.bottom()),
                        Pos2::new(bar_rect.right(), toggle_rect.bottom()),
                    ],
                    Stroke::new(1.0, with_alpha(colors.divider, f64::from(top_strength))),
                );
            }
            if bot_strength > 0.01 {
                paint_vertical_edge_fade(
                    &clipped,
                    Rect::from_min_size(
                        Pos2::new(scroll_rect.left(), scroll_rect.bottom() - fade_h),
                        Vec2::new(scroll_rect.width(), fade_h),
                    ),
                    colors.bar_bg,
                    false,
                    bot_strength,
                );
            }
        } else {
            let left_strength = (self.scroll_offset / 24.0).clamp(0.0, 1.0);
            let right_strength = ((max_scroll - self.scroll_offset) / 24.0).clamp(0.0, 1.0);
            let fade_w = SUBSECTION_EDGE_FADE_PX.min(scroll_rect.width() * 0.25);
            if left_strength > 0.01 {
                paint_horizontal_edge_fade(
                    &clipped,
                    Rect::from_min_size(scroll_rect.min, Vec2::new(fade_w, scroll_rect.height())),
                    colors.bar_bg,
                    true,
                    left_strength,
                );
                // Matches `shadow->showOn(scrollLeft() > 0)` in `history_view_subsection_tabs.cpp:173-200`.
                ui.painter().line_segment(
                    [
                        Pos2::new(toggle_rect.right(), bar_rect.top()),
                        Pos2::new(toggle_rect.right(), bar_rect.bottom()),
                    ],
                    Stroke::new(1.0, with_alpha(colors.divider, f64::from(left_strength))),
                );
            }
            if right_strength > 0.01 {
                paint_horizontal_edge_fade(
                    &clipped,
                    Rect::from_min_size(
                        Pos2::new(scroll_rect.right() - fade_w, scroll_rect.top()),
                        Vec2::new(fade_w, scroll_rect.height()),
                    ),
                    colors.bar_bg,
                    false,
                    right_strength,
                );
            }
        }

        // 8. Subtle 1px divider separating the slider from the history feed.
        let div_seg = match self.mode {
            SubsectionTabsMode::Top => [
                Pos2::new(bar_rect.left(), bar_rect.bottom() - 0.5),
                Pos2::new(bar_rect.right(), bar_rect.bottom() - 0.5),
            ],
            SubsectionTabsMode::Bottom => [
                Pos2::new(bar_rect.left(), bar_rect.top() + 0.5),
                Pos2::new(bar_rect.right(), bar_rect.top() + 0.5),
            ],
            SubsectionTabsMode::Left => [
                Pos2::new(bar_rect.right() - 0.5, bar_rect.top()),
                Pos2::new(bar_rect.right() - 0.5, bar_rect.bottom()),
            ],
        };
        ui.painter()
            .line_segment(div_seg, Stroke::new(1.0, colors.divider));

        // Optional appearance/position quick popover opened by right-clicking the toggle button.
        if self.options_open {
            egui::Area::new(Id::unique((id_salt, "subtabs_opts")))
                .order(egui::Order::Foreground)
                .fixed_pos(Pos2::new(
                    toggle_rect.left() + 4.0,
                    toggle_rect.bottom() + 4.0,
                ))
                .show(ui.ctx(), |ui| {
                    egui::Frame::NONE
                        .fill(colors.bar_bg)
                        .stroke(Stroke::new(1.0, colors.divider))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::same(8))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("Tab Position")
                                    .strong()
                                    .size(11.5)
                                    .color(colors.active_text_fg),
                            );
                            for m in [
                                SubsectionTabsMode::Top,
                                SubsectionTabsMode::Left,
                                SubsectionTabsMode::Bottom,
                            ] {
                                if ui.selectable_label(self.mode == m, m.label()).clicked() {
                                    self.mode = m;
                                    self.options_open = false;
                                }
                            }
                            ui.separator();
                            ui.label(
                                egui::RichText::new("Appearance")
                                    .strong()
                                    .size(11.5)
                                    .color(colors.active_text_fg),
                            );
                            for a in [
                                SubsectionTabsAppearance::TextAndIcons,
                                SubsectionTabsAppearance::TextOnly,
                                SubsectionTabsAppearance::IconsOnly,
                            ] {
                                if ui
                                    .selectable_label(self.appearance == a, a.label())
                                    .clicked()
                                {
                                    self.appearance = a;
                                    self.options_open = false;
                                }
                            }
                        });
                });
        }

        (remaining, event)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SliderEvent {
    Activate(usize),
    MovePinnedEarlier(usize),
    TogglePin(usize),
}

// ---------------------------------------------------------------------------
// 9. Inline Self-Forwards Tagger (`history_view_self_forwards_tagger.cpp`)
// ---------------------------------------------------------------------------

/// Initial auto-dismiss duration (`kInitTimer = 3000ms` in `history_view_self_forwards_tagger.cpp`).
pub const SELF_FORWARDS_TAGGER_INIT_SECS: f64 = 3.0;
/// Auto-dismiss duration after pointer leaves (`kTimerOnLeave = 2000ms`).
pub const SELF_FORWARDS_TAGGER_LEAVE_SECS: f64 = 2.0;

/// Active mode of [`SelfForwardsTaggerState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfForwardsTaggerPhase {
    /// Showing `"Forwarded to Saved Messages"` toast + inline reaction tag strip.
    SelectingTag {
        /// Saved Messages peer ID owning the forwarded copies.
        saved_peer_id: PeerId,
        /// Newly forwarded message IDs inside Saved Messages.
        forwarded_msg_ids: Vec<MsgId>,
    },
    /// Showing `"Message tagged with <emoji>"` confirmation with `"View"` button.
    Tagged {
        /// Saved Messages peer ID to jump to when `"View"` is clicked.
        saved_peer_id: PeerId,
        /// Applied reaction emoji.
        emoji: String,
    },
    /// Showing `"Add channel/group to a folder?"` quick toast (`showChannelFilterToast`).
    ChannelFilterPrompt {
        /// Newly joined peer ID.
        joined_peer_id: PeerId,
        /// True for broadcast channel, false for group.
        is_channel: bool,
    },
}

/// Action emitted by [`SelfForwardsTaggerState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfForwardsTaggerAction {
    /// User clicked a reaction tag; tag all forwarded saved messages with `emoji`.
    ApplyTag {
        /// Saved Messages peer ID.
        saved_peer_id: PeerId,
        /// Message IDs to tag via `messages.sendReaction`.
        msg_ids: Vec<MsgId>,
        /// Chosen emoji.
        emoji: String,
    },
    /// User renamed a saved reaction tag's custom title (`messages.updateSavedReactionTag`).
    RenameTag {
        /// Reaction emoji.
        emoji: String,
        /// New custom title (`None` clears title).
        title: Option<String>,
    },
    /// User clicked `"View"` to open Saved Messages.
    OpenSavedMessages(PeerId),
    /// User requested opening Chat Folders settings for a joined channel/group.
    OpenFoldersForPeer(PeerId),
}

/// Inline self-forwards tagger overlay (`HistoryView::SelfForwardsTagger`).
#[derive(Debug, Clone, Default)]
pub struct SelfForwardsTaggerState {
    /// Current active phase (`None` when hidden).
    pub phase: Option<SelfForwardsTaggerPhase>,
    /// Monotonic expiry timestamp in seconds (`None` while hovered or renaming).
    pub expires_at: Option<f64>,
    /// True if pointer was hovering the tagger during the previous frame.
    pub was_hovered: bool,
    /// Optional inline tag-label rename state `(emoji, title_draft)`.
    pub renaming_tag: Option<(String, String)>,
    /// Server-provided default Saved Messages tag reaction emojis loaded via
    /// `messages.getDefaultTagReactions` (`history_view_self_forwards_tagger.cpp`).
    pub cached_default_tags: Vec<String>,
    /// Whether `default_tag_reactions` has already been queried for this session.
    default_tags_loaded: bool,
}

impl SelfForwardsTaggerState {
    /// Returns `true` once server default tag reactions have been fetched via
    /// `ChatHelpersBackend::default_tag_reactions`.
    #[must_use]
    pub const fn has_loaded_default_tags(&self) -> bool {
        self.default_tags_loaded
    }

    /// Populates `cached_default_tags` from `messages.getDefaultTagReactions`.
    pub fn seed_default_tag_reactions(
        &mut self,
        reactions: &[tg_app_core::chat_helpers::backend::DefaultTagReaction],
    ) {
        self.default_tags_loaded = true;
        self.cached_default_tags.clear();
        for item in reactions {
            let em = match item {
                tg_app_core::chat_helpers::backend::DefaultTagReaction::Emoji(s) => s.trim(),
                tg_app_core::chat_helpers::backend::DefaultTagReaction::Paid => "⭐",
                tg_app_core::chat_helpers::backend::DefaultTagReaction::CustomEmoji(_) => continue,
            };
            if !em.is_empty() && !self.cached_default_tags.iter().any(|e| e == em) {
                self.cached_default_tags.push(em.to_owned());
            }
        }
    }
    /// Opens the inline tag selector for messages just forwarded to Saved Messages.
    pub fn show_for_forwarded(
        &mut self,
        saved_peer_id: PeerId,
        forwarded_msg_ids: Vec<MsgId>,
        now_secs: f64,
    ) {
        if forwarded_msg_ids.is_empty() {
            return;
        }
        self.phase = Some(SelfForwardsTaggerPhase::SelectingTag {
            saved_peer_id,
            forwarded_msg_ids,
        });
        self.expires_at = Some(now_secs + SELF_FORWARDS_TAGGER_INIT_SECS);
        self.was_hovered = false;
        self.renaming_tag = None;
    }

    /// Transitions into the `"Message tagged with <emoji>"` confirmation toast.
    pub fn show_tagged(&mut self, saved_peer_id: PeerId, emoji: String, now_secs: f64) {
        self.phase = Some(SelfForwardsTaggerPhase::Tagged {
            saved_peer_id,
            emoji,
        });
        self.expires_at = Some(now_secs + SELF_FORWARDS_TAGGER_INIT_SECS);
        self.was_hovered = false;
        self.renaming_tag = None;
    }

    /// Opens the `"Add channel/group to a folder?"` toast (`showChannelFilterToast`).
    pub fn show_channel_filter_toast(
        &mut self,
        joined_peer_id: PeerId,
        is_channel: bool,
        now_secs: f64,
    ) {
        self.phase = Some(SelfForwardsTaggerPhase::ChannelFilterPrompt {
            joined_peer_id,
            is_channel,
        });
        self.expires_at = Some(now_secs + SELF_FORWARDS_TAGGER_INIT_SECS);
        self.was_hovered = false;
        self.renaming_tag = None;
    }

    /// Hides the tagger immediately.
    pub fn hide(&mut self) {
        self.phase = None;
        self.expires_at = None;
        self.was_hovered = false;
        self.renaming_tag = None;
    }

    /// Returns `true` when the tagger toast/strip is currently visible.
    #[must_use]
    pub const fn is_visible(&self) -> bool {
        self.phase.is_some()
    }

    /// Renders the floating top self-forwards tagger overlay inside `host_rect`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        host_rect: Rect,
        saved_tags: &[crate::history::backend::SavedReactionTagItem],
        now_secs: f64,
        colors: &HistoryColors,
    ) -> Option<SelfForwardsTaggerAction> {
        let phase = self.phase.clone()?;

        // Auto-dismiss check (paused while hovered or editing a tag title).
        if self.renaming_tag.is_none()
            && let Some(exp) = self.expires_at
            && now_secs >= exp
        {
            self.hide();
            return None;
        }

        let mut action = None;
        let card_w = (host_rect.width() - 32.0).clamp(260.0, 420.0);
        let anchor = Pos2::new(host_rect.center().x - card_w * 0.5, host_rect.top() + 10.0);

        let area_resp = egui::Area::new(Id::unique("tg_self_forwards_tagger_overlay"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.accent))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width(card_w);
                        match phase {
                            SelfForwardsTaggerPhase::SelectingTag {
                                saved_peer_id,
                                forwarded_msg_ids,
                            } => {
                                let count = forwarded_msg_ids.len();
                                let header = if count == 1 {
                                    "Forwarded to Saved Messages".to_owned()
                                } else {
                                    format!("Forwarded {count} messages to Saved Messages")
                                };
                                ui.horizontal(|ui| {
                                    tg_app_core::ui_icons::inline_icon_label(
                                        ui,
                                        14.0,
                                        |p, c, s| {
                                            tg_app_core::ui_icons::paint_style_tinted(
                                                p,
                                                Rect::from_center_size(c, Vec2::splat(s)),
                                                tg_ui::tg_style::st::menu_icon_saved_messages,
                                                colors.active_text_fg,
                                            );
                                        },
                                        egui::RichText::new(&header)
                                            .strong()
                                            .size(12.5)
                                            .color(colors.bold_fg),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let close_btn = egui::Button::new(
                                                egui::RichText::new("×")
                                                    .size(12.5)
                                                    .color(colors.sub_text_fg),
                                            )
                                            .fill(colors.bar_bg_over)
                                            .stroke(Stroke::new(1.0, colors.divider))
                                            .corner_radius(CornerRadius::same(6));
                                            if ui.add(close_btn).clicked() {
                                                self.hide();
                                            }
                                            let view_btn = egui::Button::new(
                                                egui::RichText::new("View")
                                                    .strong()
                                                    .size(11.5)
                                                    .color(colors.active_text_fg),
                                            )
                                            .fill(colors.bar_bg_over)
                                            .stroke(Stroke::new(1.0, colors.divider))
                                            .corner_radius(CornerRadius::same(6));
                                            if ui.add(view_btn).clicked() {
                                                action = Some(
                                                    SelfForwardsTaggerAction::OpenSavedMessages(
                                                        saved_peer_id,
                                                    ),
                                                );
                                                self.hide();
                                            }
                                        },
                                    );
                                });
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(tg_lang::tr::lng_add_tag_selector(
                                        count as i64,
                                    ))
                                    .size(11.5)
                                    .color(colors.sub_text_fg),
                                );
                                ui.add_space(6.0);

                                // Build reaction strip from saved tags + server default tag reactions
                                // (`messages.getDefaultTagReactions`), falling back to built-in quick
                                // emojis only when the backend catalog is empty.
                                let mut strip: Vec<(String, Option<String>)> = saved_tags
                                    .iter()
                                    .map(|t| (t.emoji.clone(), t.title.clone()))
                                    .collect();
                                if !self.cached_default_tags.is_empty() {
                                    for em in &self.cached_default_tags {
                                        if !strip.iter().any(|(e, _)| e == em) {
                                            strip.push((em.clone(), None));
                                        }
                                    }
                                } else {
                                    for fallback in ["❤️", "👍", "🔥", "⭐", "🎉", "😁", "🤔", "📌"]
                                    {
                                        if !strip.iter().any(|(e, _)| e == fallback) {
                                            strip.push((fallback.to_owned(), None));
                                        }
                                    }
                                }
                                strip.truncate(8);

                                let emoji_cache = ui.ctx().data_mut(|d| {
                                    d.get_temp_mut_or_insert_with(
                                        Id::unique("tg_self_forwards_tagger_emoji_cache"),
                                        || {
                                            std::sync::Arc::new(std::sync::Mutex::new(
                                                EmojiTextureCache::new(),
                                            ))
                                        },
                                    )
                                    .clone()
                                });
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                                    for (em, title_opt) in &strip {
                                        let label = match title_opt {
                                            Some(t) if !t.is_empty() => format!("{em} {t}"),
                                            _ => em.clone(),
                                        };
                                        let title_w = title_opt
                                            .as_deref()
                                            .filter(|t| !t.is_empty())
                                            .map(|t| {
                                                ui.painter()
                                                    .layout_no_wrap(
                                                        t.to_owned(),
                                                        FontId::proportional(12.0),
                                                        colors.text_fg,
                                                    )
                                                    .size()
                                                    .x
                                                    + 6.0
                                            })
                                            .unwrap_or(0.0);
                                        let chip_w = (32.0 + title_w).max(34.0);
                                        let (chip_rect, resp) = ui.allocate_exact_size(
                                            Vec2::new(chip_w, 30.0),
                                            Sense::click(),
                                        );
                                        resp.widget_info(|| {
                                            egui::WidgetInfo::labeled(
                                                egui::Role::Button,
                                                true,
                                                label.clone(),
                                            )
                                        });
                                        if resp.hovered() {
                                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                        }
                                        let chip_fill = if resp.hovered() {
                                            color_lerp(colors.bar_bg_over, colors.accent, 0.14)
                                        } else {
                                            colors.bar_bg_over
                                        };
                                        let chip_stroke = if resp.hovered() || resp.has_focus() {
                                            Stroke::new(1.2, colors.accent)
                                        } else {
                                            Stroke::new(1.0, colors.divider)
                                        };
                                        ui.painter().rect_filled(
                                            chip_rect,
                                            CornerRadius::same(15),
                                            chip_fill,
                                        );
                                        ui.painter().rect_stroke(
                                            chip_rect,
                                            CornerRadius::same(15),
                                            chip_stroke,
                                            StrokeKind::Inside,
                                        );
                                        let icon_center = if title_w > 0.0 {
                                            Pos2::new(chip_rect.left() + 16.0, chip_rect.center().y)
                                        } else {
                                            chip_rect.center()
                                        };
                                        let icon_rect =
                                            Rect::from_center_size(icon_center, Vec2::splat(18.0));
                                        if let Ok(mut cache) = emoji_cache.lock() {
                                            let _ = cache.paint_emoji_str(
                                                ui.painter(),
                                                ui.ctx(),
                                                em,
                                                icon_rect,
                                                Color32::WHITE,
                                            );
                                        }
                                        if let Some(t) =
                                            title_opt.as_deref().filter(|t| !t.is_empty())
                                        {
                                            ui.painter().text(
                                                Pos2::new(
                                                    icon_rect.right() + 4.0,
                                                    chip_rect.center().y,
                                                ),
                                                Align2::LEFT_CENTER,
                                                t,
                                                FontId::proportional(12.0),
                                                colors.text_fg,
                                            );
                                        }
                                        let activated = resp.clicked()
                                            || (resp.has_focus()
                                                && ui.input(|i| {
                                                    i.key_pressed(Key::Enter)
                                                        || i.key_pressed(Key::Space)
                                                }));
                                        if activated {
                                            action = Some(SelfForwardsTaggerAction::ApplyTag {
                                                saved_peer_id,
                                                msg_ids: forwarded_msg_ids.clone(),
                                                emoji: em.clone(),
                                            });
                                        } else if resp.secondary_clicked() {
                                            self.renaming_tag = Some((
                                                em.clone(),
                                                title_opt.clone().unwrap_or_default(),
                                            ));
                                        }
                                    }
                                });

                                if let Some((ref rename_em, ref mut draft)) =
                                    self.renaming_tag.clone()
                                {
                                    let mut edit_buf = draft.clone();
                                    let mut close_rename = false;
                                    ui.add_space(6.0);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!("Label for {rename_em}:"))
                                                .size(11.5)
                                                .color(colors.active_text_fg),
                                        );
                                        ui.add(
                                            TextEdit::singleline(&mut edit_buf)
                                                .desired_width(130.0)
                                                .hint_text("Tag name (max 12)..."),
                                        );
                                        if ui.small_button("Save").clicked() {
                                            let trimmed = edit_buf.trim();
                                            let title = (!trimmed.is_empty())
                                                .then(|| trimmed.chars().take(12).collect());
                                            action = Some(SelfForwardsTaggerAction::RenameTag {
                                                emoji: rename_em.clone(),
                                                title,
                                            });
                                            close_rename = true;
                                        }
                                        if ui.small_button("Cancel").clicked() {
                                            close_rename = true;
                                        }
                                    });
                                    if close_rename {
                                        self.renaming_tag = None;
                                    } else {
                                        self.renaming_tag = Some((rename_em.clone(), edit_buf));
                                    }
                                }
                            }
                            SelfForwardsTaggerPhase::Tagged {
                                saved_peer_id,
                                emoji,
                            } => {
                                ui.horizontal(|ui| {
                                    let (ic_r, _) = ui.allocate_exact_size(
                                        egui::Vec2::splat(13.0),
                                        egui::Sense::hover(),
                                    );
                                    tg_app_core::ui_icons::paint_sparkle(
                                        ui.painter(),
                                        ic_r.center(),
                                        12.0,
                                        colors.accent,
                                    );
                                    ui.label(
                                        egui::RichText::new(format!("Message tagged with {emoji}"))
                                            .strong()
                                            .size(12.5)
                                            .color(colors.bold_fg),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button("×").clicked() {
                                                self.hide();
                                            }
                                            if ui.small_button("View Saved").clicked() {
                                                action = Some(
                                                    SelfForwardsTaggerAction::OpenSavedMessages(
                                                        saved_peer_id,
                                                    ),
                                                );
                                                self.hide();
                                            }
                                        },
                                    );
                                });
                            }
                            SelfForwardsTaggerPhase::ChannelFilterPrompt {
                                joined_peer_id,
                                is_channel,
                            } => {
                                let prompt = if is_channel {
                                    "Add this channel to a folder?"
                                } else {
                                    "Add this group to a folder?"
                                };
                                ui.horizontal(|ui| {
                                    tg_app_core::ui_icons::inline_icon_label(
                                        ui,
                                        14.0,
                                        |p, c, s| {
                                            tg_app_core::ui_icons::paint_style_tinted(
                                                p,
                                                Rect::from_center_size(c, Vec2::splat(s)),
                                                tg_ui::tg_style::st::menu_icon_show_in_folder,
                                                colors.active_text_fg,
                                            );
                                        },
                                        egui::RichText::new(prompt)
                                            .strong()
                                            .size(12.5)
                                            .color(colors.bold_fg),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button("×").clicked() {
                                                self.hide();
                                            }
                                            if ui.small_button("Add to Folder").clicked() {
                                                action = Some(
                                                    SelfForwardsTaggerAction::OpenFoldersForPeer(
                                                        joined_peer_id,
                                                    ),
                                                );
                                                self.hide();
                                            }
                                        },
                                    );
                                });
                            }
                        }
                    });
            });

        // Hover pauses timer; leaving restarts `kTimerOnLeave` (`2.0s`).
        let hovered = area_resp.response.hovered();
        if hovered {
            self.expires_at = None;
        } else if self.was_hovered && self.renaming_tag.is_none() {
            self.expires_at = Some(now_secs + SELF_FORWARDS_TAGGER_LEAVE_SECS);
        }
        self.was_hovered = hovered;

        action
    }
}
