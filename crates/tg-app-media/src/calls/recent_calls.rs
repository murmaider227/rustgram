//! Recent calls list box with call grouping, call type icons, call-back, and deletion.
//!
//! Port of `Telegram/SourceFiles/calls/calls_box_controller.cpp`.

use egui::{Align2, CornerRadius, FontId, Pos2, Rect, RichText, ScrollArea, Sense, Ui, Vec2};
use tg_lang::tr;

use crate::calls::backend::{CallPeerInfo, CallsBackend, RecentCallDirection, RecentCallEntry};
use crate::calls::format::format_recent_call_status;
use crate::calls::strings;
use crate::calls::theme::{
    CallPalette, paint_avatar, paint_call_arrow, paint_camera_icon, paint_close_icon,
    paint_phone_icon,
};
use tg_app_core::stand_in::types::PeerId;

/// Grouped recent call row (`BoxController::Row` in `calls_box_controller.cpp:264-384`).
///
/// Consecutive calls with the same peer, same direction (`In` / `Out` / `Missed`), and same
/// calendar day collapse into a single row sorted newest-first by `max_msg_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupedCallRow {
    /// Representative newest entry in the group.
    pub newest: RecentCallEntry,
    /// All message IDs belonging to this grouped row (descending).
    pub msg_ids: Vec<i32>,
}

/// Groups and sorts raw [`RecentCallEntry`] items identically to `Calls::BoxController::insertRow`.
#[must_use]
pub fn group_recent_calls(entries: &[RecentCallEntry]) -> Vec<GroupedCallRow> {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|a| std::cmp::Reverse(a.msg_id));

    let mut rows: Vec<GroupedCallRow> = Vec::new();
    for entry in sorted {
        if let Some(last) = rows.last_mut()
            && last.newest.peer.id == entry.peer.id
            && last.newest.direction == entry.direction
            && last.newest.days_ago == entry.days_ago
        {
            last.msg_ids.push(entry.msg_id);
            continue;
        }
        rows.push(GroupedCallRow {
            msg_ids: vec![entry.msg_id],
            newest: entry,
        });
    }
    rows
}

/// User actions emitted by [`RecentCallsBox`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecentCallsAction {
    /// Place a 1:1 voice or video call back to `peer_id`.
    CallBack {
        /// Target user peer.
        peer_id: PeerId,
        /// Whether to start with video enabled.
        video: bool,
    },
    /// Delete a grouped call log row (`msg_ids`).
    Delete(Vec<i32>),
    /// Join an active group call from the top section.
    JoinGroupCall(PeerId),
    /// Create a new E2E encrypted conference call (`phone.createConferenceCall`).
    CreateConferenceCall,
    /// Clear the entire call history (`messages.deletePhoneCallHistory`) after confirmation.
    ClearAll {
        /// Also delete for the other participants (`delete_for_everyone`).
        revoke: bool,
    },
}

/// Recent calls modal dialog (`Calls::BoxController`).
#[derive(Debug, Default)]
pub struct RecentCallsBox {
    /// Whether the dialog is open.
    pub open: bool,
    /// Whether the "Clear All" confirmation prompt is expanded.
    pub clear_confirm_open: bool,
    /// Checkbox state for "Delete for everyone" in the clear confirmation.
    pub clear_revoke_checked: bool,
}

impl RecentCallsBox {
    /// Shows the recent calls modal window and returns any triggered [`RecentCallsAction`].
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        backend: &dyn CallsBackend,
        palette: &CallPalette,
    ) -> Option<RecentCallsAction> {
        if !self.open {
            return None;
        }
        let mut action = None;
        let mut keep_open = self.open;
        let grouped = group_recent_calls(backend.recent_calls());
        let active_groups: Vec<CallPeerInfo> = backend.active_chat_group_calls().to_vec();
        let screen = ctx.viewport_rect();
        let win_w = (screen.width() - 24.0).clamp(320.0, 380.0);
        let win_h = (screen.height() - 48.0).clamp(340.0, 480.0);

        egui::Window::new(tr::lng_call_box_title())
            .id(egui::Id::unique("recent_calls_box"))
            .collapsible(false)
            .resizable(false)
            .fixed_size(Vec2::new(win_w, win_h))
            .open(&mut keep_open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .button(
                            RichText::new("+ New Encrypted Call")
                                .strong()
                                .color(palette.call_answer_bg),
                        )
                        .clicked()
                    {
                        action = Some(RecentCallsAction::CreateConferenceCall);
                    }
                    if !grouped.is_empty() {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .button(
                                    RichText::new(tr::lng_call_box_clear_all())
                                        .color(palette.call_arrow_missed_fg),
                                )
                                .clicked()
                            {
                                self.clear_confirm_open = true;
                                self.clear_revoke_checked = false;
                            }
                        });
                    }
                });
                ui.add_space(6.0);

                if self.clear_confirm_open && !grouped.is_empty() {
                    egui::Frame::NONE
                        .fill(ui.visuals().faint_bg_color)
                        .stroke(egui::Stroke::new(1.0, palette.call_arrow_missed_fg))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add(
                                egui::Label::new(
                                    RichText::new(tr::lng_call_box_clear_sure())
                                        .size(12.5)
                                        .color(ui.visuals().text_color()),
                                )
                                .wrap(),
                            );
                            ui.add_space(6.0);
                            ui.checkbox(
                                &mut self.clear_revoke_checked,
                                tr::lng_delete_for_everyone_check(),
                            );
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new(tr::lng_call_box_clear_button())
                                                .strong()
                                                .color(egui::Color32::WHITE),
                                        )
                                        .fill(palette.call_arrow_missed_fg)
                                        .corner_radius(CornerRadius::same(6)),
                                    )
                                    .clicked()
                                {
                                    action = Some(RecentCallsAction::ClearAll {
                                        revoke: self.clear_revoke_checked,
                                    });
                                    self.clear_confirm_open = false;
                                }
                                if ui.button(tr::lng_cancel()).clicked() {
                                    self.clear_confirm_open = false;
                                }
                            });
                        });
                    ui.add_space(8.0);
                }
                ScrollArea::vertical().show(ui, |ui| {
                    // Active Voice Chats section (`GroupCalls::ListController`).
                    if !active_groups.is_empty() {
                        ui.label(
                            RichText::new(tr::lng_call_box_groupcalls_subtitle())
                                .strong()
                                .size(12.5)
                                .color(palette.group_active_fg),
                        );
                        ui.add_space(4.0);
                        for group in &active_groups {
                            if show_active_group_row(ui, group, palette) {
                                action = Some(RecentCallsAction::JoinGroupCall(group.id));
                            }
                        }
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(6.0);
                    }

                    ui.label(
                        RichText::new(strings::RECENT_CALLS_HEADING)
                            .strong()
                            .size(12.5)
                            .color(ui.visuals().weak_text_color()),
                    );
                    ui.add_space(4.0);

                    if grouped.is_empty() {
                        ui.centered_and_justified(|ui| {
                            ui.label(tr::lng_call_box_about());
                        });
                    } else {
                        for row in &grouped {
                            if let Some(a) = show_recent_call_row(ui, row, palette) {
                                action = Some(a);
                            }
                        }
                    }
                });
            });

        self.open = keep_open;
        action
    }
}

fn show_active_group_row(ui: &mut Ui, group: &CallPeerInfo, palette: &CallPalette) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 52.0), Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, CornerRadius::same(8), ui.visuals().faint_bg_color);
    }
    paint_avatar(
        painter,
        Pos2::new(rect.left() + 26.0, rect.center().y),
        19.0,
        group.avatar_color,
        &group.initials,
        13.0,
    );
    painter.text(
        Pos2::new(rect.left() + 54.0, rect.top() + 17.0),
        Align2::LEFT_CENTER,
        &group.name,
        FontId::proportional(13.5),
        ui.visuals().text_color(),
    );
    let sub = if group.is_broadcast {
        strings::LIVE_STREAM_ACTIVE
    } else {
        strings::VOICE_CHAT_ACTIVE
    };
    painter.text(
        Pos2::new(rect.left() + 54.0, rect.top() + 35.0),
        Align2::LEFT_CENTER,
        sub,
        FontId::proportional(11.5),
        palette.group_active_fg,
    );
    let join_rect = Rect::from_min_size(
        Pos2::new(rect.right() - 62.0, rect.center().y - 13.0),
        Vec2::new(54.0, 26.0),
    );
    painter.rect_filled(join_rect, CornerRadius::same(13), palette.group_muted_1);
    painter.text(
        join_rect.center(),
        Align2::CENTER_CENTER,
        tr::lng_group_call_join(),
        FontId::proportional(12.0),
        egui::Color32::WHITE,
    );
    resp.clicked()
}

fn show_recent_call_row(
    ui: &mut Ui,
    row: &GroupedCallRow,
    palette: &CallPalette,
) -> Option<RecentCallsAction> {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 56.0), Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, CornerRadius::same(8), ui.visuals().faint_bg_color);
    }

    paint_avatar(
        painter,
        Pos2::new(rect.left() + 26.0, rect.center().y),
        20.0,
        row.newest.peer.avatar_color,
        &row.newest.peer.initials,
        13.5,
    );

    painter.text(
        Pos2::new(rect.left() + 56.0, rect.top() + 18.0),
        Align2::LEFT_CENTER,
        &row.newest.peer.name,
        FontId::proportional(13.5),
        ui.visuals().text_color(),
    );

    let arrow_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 56.0, rect.top() + 31.0),
        Vec2::new(11.0, 11.0),
    );
    let (outgoing, arrow_color) = match row.newest.direction {
        RecentCallDirection::Out => (true, palette.call_arrow_fg),
        RecentCallDirection::In => (false, palette.call_arrow_fg),
        RecentCallDirection::Missed => (false, palette.call_arrow_missed_fg),
    };
    paint_call_arrow(painter, arrow_rect, outgoing, arrow_color);

    let status = format_recent_call_status(
        row.newest.days_ago,
        row.newest.hour,
        row.newest.minute,
        row.msg_ids.len(),
        row.newest.duration_secs,
    );
    painter.text(
        Pos2::new(rect.left() + 72.0, rect.top() + 37.0),
        Align2::LEFT_CENTER,
        status,
        FontId::proportional(11.5),
        ui.visuals().weak_text_color(),
    );

    // Right action buttons: Call back + Delete.
    let call_btn = Rect::from_center_size(
        Pos2::new(rect.right() - 56.0, rect.center().y),
        Vec2::new(32.0, 32.0),
    );
    let del_btn = Rect::from_center_size(
        Pos2::new(rect.right() - 20.0, rect.center().y),
        Vec2::new(28.0, 28.0),
    );

    let pointer = ui.ctx().pointer_hover_pos();
    if pointer.is_some_and(|p| call_btn.contains(p)) {
        painter.circle_filled(
            call_btn.center(),
            16.0,
            ui.visuals().widgets.hovered.bg_fill,
        );
    }
    if row.newest.is_video {
        paint_camera_icon(
            painter,
            call_btn.center(),
            18.0,
            palette.group_active_fg,
            false,
        );
    } else {
        paint_phone_icon(painter, call_btn.center(), 18.0, 0.0, palette.call_arrow_fg);
    }

    if pointer.is_some_and(|p| del_btn.contains(p)) {
        painter.circle_filled(del_btn.center(), 14.0, ui.visuals().widgets.hovered.bg_fill);
    }
    paint_close_icon(
        painter,
        del_btn.center(),
        14.0,
        palette.call_arrow_missed_fg,
    );

    if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        if del_btn.contains(pos) {
            return Some(RecentCallsAction::Delete(row.msg_ids.clone()));
        }
        return Some(RecentCallsAction::CallBack {
            peer_id: row.newest.peer.id,
            video: row.newest.is_video,
        });
    }
    None
}
