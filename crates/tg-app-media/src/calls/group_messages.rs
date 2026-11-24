//! Conference E2E emoji verification banner, in-call group chat feed, and paid Star reactions UI.
//!
//! Port origin:
//! - `Telegram/SourceFiles/calls/group/calls_group_messages_ui.cpp`
//! - `Telegram/SourceFiles/calls/group/ui/calls_group_stars_coloring.cpp`
//! - `Telegram/SourceFiles/calls/calls_emoji_fingerprint.cpp`

use egui::{
    Align2, Color32, CornerRadius, FontId, Key, Painter, Pos2, Rect, RichText, Sense, Stroke, Ui,
    Vec2,
};
use tg_calls::{StarsColoring, stars_coloring_for_count};
use tg_lang::tr;

use crate::calls::backend::{GroupCallMessageView, GroupCallSnapshot};
use crate::calls::theme::{CallPalette, paint_fingerprint_emoji_glyph, with_alpha};
use tg_app_core::stand_in::types::PeerId;

pub use tg_app_core::core_types::misc::default_stars_colorings;

/// Converts a 24-bit RGB hex color (`0xRRGGBB`) from [`StarsColoring`] into an [`egui::Color32`].
#[must_use]
pub const fn color_from_rgb_hex(hex: i32) -> Color32 {
    let r = ((hex >> 16) & 0xFF) as u8;
    let g = ((hex >> 8) & 0xFF) as u8;
    let b = (hex & 0xFF) as u8;
    Color32::from_rgb(r, g, b)
}

/// Persistent UI state for the conference E2E verification emoji banner.
#[derive(Debug, Default)]
pub struct E2eVerificationBannerState {
    /// Whether the user clicked the emoji verification pill to pin its explanatory tooltip.
    pub tooltip_pinned: bool,
    /// Last rendered pill hit-rect for drawing the tooltip above the panel body.
    last_banner_rect: Option<Rect>,
    /// Whether the banner was hovered on the current frame.
    last_hovered: bool,
}

impl E2eVerificationBannerState {
    /// Fixed height consumed by the conference E2E banner when visible.
    pub const BANNER_HEIGHT: f32 = 34.0;

    /// Paints the conference E2E verification banner bar inside `rect` and returns the vertical
    /// height consumed (`0.0` for non-conference calls).
    pub fn paint_banner(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        rect: Rect,
        snapshot: &GroupCallSnapshot,
        palette: &CallPalette,
    ) -> f32 {
        self.last_banner_rect = None;
        self.last_hovered = false;
        if !snapshot.is_conference {
            return 0.0;
        }

        let banner_rect = Rect::from_min_size(
            Pos2::new(rect.left(), rect.top()),
            Vec2::new(rect.width(), Self::BANNER_HEIGHT),
        );
        let resp = ui.allocate_rect(banner_rect, Sense::click());
        if resp.clicked() {
            self.tooltip_pinned = !self.tooltip_pinned;
        }
        self.last_banner_rect = Some(banner_rect);
        self.last_hovered = resp.hovered();

        let bg = if resp.hovered() || self.tooltip_pinned {
            with_alpha(palette.group_members_bg_over, 0.96)
        } else {
            with_alpha(palette.group_members_bg, 0.92)
        };
        painter.rect_filled(banner_rect, CornerRadius::same(10), bg);
        painter.rect_stroke(
            banner_rect,
            CornerRadius::same(10),
            Stroke::new(1.0, with_alpha(palette.call_answer_bg, 0.45)),
            egui::StrokeKind::Inside,
        );

        let cy = banner_rect.center().y;
        // Left: green lock status indicator + E2E label + verified count.
        let dot_center = Pos2::new(banner_rect.left() + 14.0, cy);
        painter.circle_filled(dot_center, 4.0, palette.call_answer_bg);

        let badge = if snapshot.e2e_verified_count > 0 {
            format!(
                "{} · {} verified",
                tr::lng_confcall_e2e_badge(),
                snapshot.e2e_verified_count
            )
        } else {
            tr::lng_confcall_e2e_badge()
        };
        painter.text(
            Pos2::new(dot_center.x + 10.0, cy),
            Align2::LEFT_CENTER,
            badge,
            FontId::proportional(11.5),
            palette.call_answer_bg,
        );

        // Right: 4-emoji verification pill (or "Verifying..." while commit-reveal finishes).
        if let Some(ref emojis) = snapshot.emoji_fingerprint {
            let pill_w = 104.0;
            let pill_rect = Rect::from_min_size(
                Pos2::new(banner_rect.right() - pill_w - 8.0, cy - 12.0),
                Vec2::new(pill_w, 24.0),
            );
            painter.rect_filled(pill_rect, CornerRadius::same(12), palette.call_bg_button);
            let slot_w = (pill_w - 10.0) / 4.0;
            for (idx, emoji) in emojis.iter().enumerate() {
                let cx = pill_rect.left() + 5.0 + slot_w * (idx as f32 + 0.5);
                paint_fingerprint_emoji_glyph(painter, Pos2::new(cx, cy), 8.0, emoji);
            }
        } else {
            painter.text(
                Pos2::new(banner_rect.right() - 12.0, cy),
                Align2::RIGHT_CENTER,
                "Verifying encryption...",
                FontId::proportional(11.0),
                palette.group_member_sub_fg,
            );
        }

        Self::BANNER_HEIGHT + 6.0
    }

    /// Paints the conference E2E verification explanatory popover above the panel body when hovered
    /// or pinned.
    pub fn paint_tooltip(
        &self,
        painter: &Painter,
        panel_rect: Rect,
        snapshot: &GroupCallSnapshot,
        palette: &CallPalette,
    ) {
        if !snapshot.is_conference || (!self.last_hovered && !self.tooltip_pinned) {
            return;
        }
        let Some(anchor) = self.last_banner_rect else {
            return;
        };

        let body = if snapshot.emoji_fingerprint.is_some() {
            "If everyone in this call sees the same 4 emojis, the call is 100% end-to-end encrypted."
        } else {
            "Completing end-to-end key verification with active conference participants..."
        };
        let tip_w = (panel_rect.width() - 36.0).clamp(240.0, 420.0);
        let tip_rect = Rect::from_min_size(
            Pos2::new(panel_rect.center().x - tip_w * 0.5, anchor.bottom() + 6.0),
            Vec2::new(tip_w, 52.0),
        );
        painter.rect_filled(
            tip_rect,
            CornerRadius::same(8),
            Color32::from_rgba_unmultiplied(16, 20, 24, 242),
        );
        painter.rect_stroke(
            tip_rect,
            CornerRadius::same(8),
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 36)),
            egui::StrokeKind::Inside,
        );
        painter.text(
            Pos2::new(tip_rect.center().x, tip_rect.top() + 16.0),
            Align2::CENTER_CENTER,
            tr::lng_confcall_e2e_badge(),
            FontId::proportional(11.5),
            palette.call_answer_bg,
        );
        painter.text(
            Pos2::new(tip_rect.center().x, tip_rect.top() + 35.0),
            Align2::CENTER_CENTER,
            body,
            FontId::proportional(10.5),
            palette.call_name_fg,
        );
    }
}

/// User action emitted by [`GroupMessagesPanelState`] for `CallsBackend` dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupMessagesAction {
    /// Send a text message (optionally attaching paid Stars).
    SendMessage {
        /// Plain text message body.
        text: String,
        /// Paid Stars attached (0 for free / conference messages).
        paid_stars: i64,
    },
    /// Delete a message (or all messages from `delete_all_from`).
    DeleteMessage {
        /// Message ID to delete.
        msg_id: i32,
        /// Optional participant peer ID to delete all messages from.
        delete_all_from: Option<PeerId>,
        /// Whether to report as spam.
        report_spam: bool,
    },
    /// Queue `count` paid Stars into the batched reaction counter.
    AddPaidStars(i64),
    /// Flush locally queued paid Star reactions immediately.
    FlushPaidStars,
    /// Cancel locally queued paid Star reactions.
    CancelPaidStars,
    /// Refresh paid Stars leaderboard from server (`phone.getGroupCallStars`).
    RefreshStars,
}

/// Persistent UI state for the in-call chat feed and paid Star reactions composer.
#[derive(Debug, Default)]
pub struct GroupMessagesPanelState {
    /// Draft message text currently typed in the composer.
    pub draft_text: String,
    /// Paid Stars to attach to the next sent text message (0 for free).
    pub attach_paid_stars: i64,
}

impl GroupMessagesPanelState {
    /// Renders the in-call messages feed, paid Star donors leaderboard, and message composer inside
    /// `ui`, returning any user-triggered [`GroupMessagesAction`].
    pub fn show(
        &mut self,
        ui: &mut Ui,
        snapshot: &GroupCallSnapshot,
        palette: &CallPalette,
    ) -> Option<GroupMessagesAction> {
        let mut action: Option<GroupMessagesAction> = None;
        let colorings = default_stars_colorings();
        let gold = Color32::from_rgb(0xFF, 0xC8, 0x3D);

        // 1. Paid Stars leaderboard & scheduled reaction batch banner (non-E2E calls/streams).
        if !snapshot.is_conference
            && (snapshot.stars_total > 0
                || snapshot.stars_scheduled > 0
                || snapshot.is_video_stream
                || !snapshot.stars_top.is_empty())
        {
            egui::Frame::new()
                .fill(palette.group_bg)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        tg_app_core::ui_icons::inline_icon_label(
                            ui,
                            13.0,
                            |p, c, s| tg_app_core::ui_icons::paint_star(p, c, s * 0.45, gold),
                            RichText::new(format!("{} Stars", snapshot.stars_total))
                                .strong()
                                .size(12.0)
                                .color(gold),
                        );
                        if snapshot.stars_my_total > 0 {
                            ui.label(
                                RichText::new(format!("(You: {})", snapshot.stars_my_total))
                                    .size(11.0)
                                    .color(palette.group_member_sub_fg),
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Refresh").clicked() {
                                action = Some(GroupMessagesAction::RefreshStars);
                            }
                        });
                    });

                    if !snapshot.stars_top.is_empty() {
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            for (idx, donor) in snapshot.stars_top.iter().take(3).enumerate() {
                                let name = donor
                                    .peer
                                    .as_ref()
                                    .map_or("Anonymous", |p| p.short_name.as_str());
                                let chip_bg = if donor.is_my {
                                    with_alpha(gold, 0.24)
                                } else {
                                    palette.group_members_bg_over
                                };
                                egui::Frame::new()
                                    .fill(chip_bg)
                                    .corner_radius(CornerRadius::same(10))
                                    .inner_margin(egui::Margin::symmetric(6, 2))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 3.0;
                                            ui.label(
                                                RichText::new(format!("#{} {name}", idx + 1))
                                                    .size(10.5)
                                                    .color(palette.group_members_fg),
                                            );
                                            let (sr, _) = ui.allocate_exact_size(
                                                Vec2::splat(10.0),
                                                egui::Sense::hover(),
                                            );
                                            tg_app_core::ui_icons::paint_star(
                                                ui.painter(),
                                                sr.center(),
                                                4.5,
                                                gold,
                                            );
                                            ui.label(
                                                RichText::new(format!("{}", donor.stars))
                                                    .size(10.5)
                                                    .color(palette.group_members_fg),
                                            );
                                        });
                                    });
                            }
                        });
                    }

                    if snapshot.stars_scheduled > 0 {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            tg_app_core::ui_icons::inline_icon_label(
                                ui,
                                12.0,
                                |p, c, s| tg_app_core::ui_icons::paint_star(p, c, s * 0.45, gold),
                                RichText::new(format!("+{} queued", snapshot.stars_scheduled))
                                    .strong()
                                    .size(11.5)
                                    .color(gold),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Cancel").clicked() {
                                        action = Some(GroupMessagesAction::CancelPaidStars);
                                    }
                                    if ui.small_button("Send Stars").clicked() {
                                        action = Some(GroupMessagesAction::FlushPaidStars);
                                    }
                                },
                            );
                        });
                    }
                });
            ui.add_space(6.0);
        }

        // 2. Scrollable in-call messages list.
        let composer_reserve = if snapshot.is_conference { 42.0 } else { 70.0 };
        let feed_h = (ui.available_height() - composer_reserve).max(60.0);
        egui::ScrollArea::vertical()
            .id_salt("group_call_messages_scroll")
            .max_height(feed_h)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                if snapshot.chat_messages.is_empty() {
                    let hint = if snapshot.is_conference {
                        "Messages sent here are end-to-end encrypted."
                    } else {
                        "No messages yet. Say hello or send a Star reaction!"
                    };
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(hint)
                            .size(11.5)
                            .color(palette.group_member_sub_fg),
                    );
                } else {
                    for msg in &snapshot.chat_messages {
                        if let Some(del) = render_message_row(
                            ui,
                            msg,
                            snapshot.can_manage,
                            &colorings,
                            palette,
                            gold,
                        ) {
                            action = Some(del);
                        }
                        ui.add_space(4.0);
                    }
                }
            });

        ui.add_space(6.0);

        // 3. Paid Star quick-reaction pills & optional message Stars attachment (non-E2E calls).
        if !snapshot.is_conference {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("React:")
                        .size(11.0)
                        .color(palette.group_member_sub_fg),
                );
                for count in [1_i64, 10, 50, 100] {
                    if tg_app_core::ui_icons::icon_label_button(
                        ui,
                        &format!("+{count}"),
                        true,
                        |p, c, s, _| tg_app_core::ui_icons::paint_star(p, c, s * 0.44, gold),
                    )
                    .clicked()
                    {
                        action = Some(GroupMessagesAction::AddPaidStars(count));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let attach_label = if self.attach_paid_stars > 0 {
                        format!("Msg {}", self.attach_paid_stars)
                    } else {
                        "Msg".to_owned()
                    };
                    if tg_app_core::ui_icons::icon_label_button(
                        ui,
                        &attach_label,
                        true,
                        |p, c, s, _| tg_app_core::ui_icons::paint_star(p, c, s * 0.44, gold),
                    )
                    .on_hover_text("Cycle paid Stars attached to your next message")
                    .clicked()
                    {
                        self.attach_paid_stars = match self.attach_paid_stars {
                            0 => 10,
                            10 => 50,
                            50 => 100,
                            _ => 0,
                        };
                    }
                });
            });
            ui.add_space(4.0);
        }

        // 4. Message composer input + Send button.
        let can_send = snapshot.is_conference || snapshot.messages_enabled;
        if !can_send {
            ui.label(
                RichText::new("Chat messages are disabled in this voice chat.")
                    .size(11.0)
                    .color(palette.group_member_sub_fg),
            );
            return action;
        }

        let effective_stars = if snapshot.is_conference {
            0
        } else {
            self.attach_paid_stars.max(snapshot.min_paid_stars)
        };
        let hint = if snapshot.is_conference {
            "Encrypted message..."
        } else if effective_stars > 0 {
            "Message (paid Stars)..."
        } else {
            "Message..."
        };

        ui.horizontal(|ui| {
            let input_w = (ui.available_width() - 62.0).max(80.0);
            let edit_resp = ui.add(
                egui::TextEdit::singleline(&mut self.draft_text)
                    .desired_width(input_w)
                    .hint_text(hint),
            );
            let enter_pressed = edit_resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let send_btn = ui.button("Send").clicked();
            if (enter_pressed || send_btn) && !self.draft_text.trim().is_empty() {
                let text = self.draft_text.trim().to_owned();
                self.draft_text.clear();
                self.attach_paid_stars = 0;
                action = Some(GroupMessagesAction::SendMessage {
                    text,
                    paid_stars: effective_stars,
                });
            }
        });

        action
    }
}

fn render_message_row(
    ui: &mut Ui,
    msg: &GroupCallMessageView,
    can_manage: bool,
    colorings: &[StarsColoring],
    palette: &CallPalette,
    gold: Color32,
) -> Option<GroupMessagesAction> {
    let mut action = None;
    let fill = if msg.stars > 0 {
        stars_coloring_for_count(colorings, msg.stars.min(i64::from(i32::MAX)) as i32)
            .map_or(palette.group_members_bg_over, |tier| {
                with_alpha(color_from_rgb_hex(tier.bg_dark), 0.92)
            })
    } else {
        palette.group_bg
    };

    egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&msg.peer.name)
                        .strong()
                        .size(11.5)
                        .color(if msg.mine {
                            palette.group_active_fg
                        } else {
                            msg.peer.avatar_color
                        }),
                );
                if msg.admin {
                    ui.label(
                        RichText::new("Admin")
                            .size(10.0)
                            .color(palette.group_member_active),
                    );
                }
                if msg.stars > 0 {
                    tg_app_core::ui_icons::inline_icon_label(
                        ui,
                        11.0,
                        |p, c, s| tg_app_core::ui_icons::paint_star(p, c, s * 0.45, gold),
                        RichText::new(format!("{}", msg.stars))
                            .strong()
                            .size(10.5)
                            .color(gold),
                    );
                }
                if msg.pinned {
                    ui.label(
                        RichText::new("PIN")
                            .size(9.5)
                            .color(palette.group_members_fg),
                    );
                }
                if msg.failed {
                    ui.label(
                        RichText::new("Failed")
                            .size(10.0)
                            .color(palette.group_member_muted_icon),
                    );
                }
                if (msg.mine || can_manage) && msg.id > 0 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button(
                                RichText::new("×")
                                    .size(11.0)
                                    .color(palette.group_member_sub_fg),
                            )
                            .on_hover_text("Delete message")
                            .clicked()
                        {
                            action = Some(GroupMessagesAction::DeleteMessage {
                                msg_id: msg.id,
                                delete_all_from: None,
                                report_spam: false,
                            });
                        }
                    });
                }
            });

            if msg.text.is_empty() && msg.stars > 0 {
                tg_app_core::ui_icons::inline_icon_label(
                    ui,
                    12.0,
                    |p, c, s| tg_app_core::ui_icons::paint_star(p, c, s * 0.45, gold),
                    RichText::new(format!("Sent {} Stars", msg.stars))
                        .size(12.0)
                        .color(palette.group_members_fg),
                );
            } else {
                ui.label(
                    RichText::new(&msg.text)
                        .size(12.0)
                        .color(palette.group_members_fg),
                );
            }
        });

    action
}
