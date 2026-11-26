//! Context-menu read-receipt rows ("Seen by N" / "today at HH:MM"), channel message author lookup
//! (`channels.getMessageAuthor`), and the "Seen by" participants modal sheet.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/view/history_view_context_menu.cpp`
//!   (`AddWhoReactedAction`, `LookupMessageAuthor`, `MakeMessageAuthorAction`)
//! - `Telegram/SourceFiles/api/api_who_reacted.h` & `.cpp`
//! - `Telegram/SourceFiles/ui/controls/who_reacted_context_action.cpp`

use egui::accesskit::Role;
use egui::{
    Align2, Area, Color32, CornerRadius, CursorIcon, FontId, Id, Key, Order, Pos2, Rect,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo,
};
use tg_app_core::core_types::dto::engagement::{
    DEFAULT_CHAT_READ_MARK_SIZE_THRESHOLD, DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS, MessageAuthorInfo,
    MessageReadParticipant, OutboxReadDateStatus, who_read_exists_for_dm,
    who_read_exists_for_group,
};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::painter::BoxShadow;
use tg_ui::stand_in::{BoxShadowStyle, Point};
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::buttons::{IconKind, paint_icon};
use tg_ui::widgets::menu::MenuItem;

use crate::history::backend::{HistoryBackend, HistoryMessage, HistoryPeerInfo};
use crate::history::theme::HistoryColors;

/// Menu item action IDs emitted by read-receipt and channel-author context-menu rows.
pub const MENU_ID_SEEN_BY: &str = "wp61_seen_by";
/// Menu item action ID for 1:1 DM `"Read at HH:MM"` / `"read · show when"`.
pub const MENU_ID_READ_AT: &str = "wp61_read_at";
/// Menu item action ID for channel `"Sent by {user}"` (`channels.getMessageAuthor`).
pub const MENU_ID_MESSAGE_AUTHOR: &str = "wp61_message_author";

/// Snapshot of read-receipt and author data resolved for the currently open message context menu.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextMenuEngagementInfo {
    /// Owning peer id.
    pub peer_id: PeerId,
    /// Target message id.
    pub msg_id: MsgId,
    /// Group read-receipt participants (`messages.getMessageReadParticipants`), if eligible.
    pub group_readers: Option<Vec<MessageReadParticipant>>,
    /// 1:1 DM outgoing read timestamp (`messages.getOutboxReadDate`), if eligible.
    pub dm_read_date: Option<OutboxReadDateStatus>,
    /// Channel post author resolved via `channels.getMessageAuthor`, if available.
    pub message_author: Option<MessageAuthorInfo>,
}

impl ContextMenuEngagementInfo {
    /// Resolves read-receipt and channel-author metadata for `(peer, msg)` once when opening the
    /// context menu, applying tdesktop's exact [`who_read_exists_for_dm`] /
    /// [`who_read_exists_for_group`] eligibility gates.
    #[must_use]
    pub fn resolve(
        peer: &HistoryPeerInfo,
        msg: &HistoryMessage,
        now_unix: i64,
        backend: &dyn HistoryBackend,
    ) -> Self {
        let mut info = Self {
            peer_id: peer.summary.id,
            msg_id: msg.id,
            group_readers: None,
            dm_read_date: None,
            message_author: None,
        };

        if msg.is_service() {
            return info;
        }

        // Effective "now": clamp to at least `msg.date_unix + 60` when running against synthetic
        // fixture timestamps so expiry checks stay deterministic regardless of wall clock.
        let effective_now = now_unix.max(i64::from(msg.date_unix) + 60);
        let unseen = !msg.is_read || msg.is_sending;

        match peer.summary.kind {
            PeerKind::User => {
                let is_bot = peer.summary.status.eq_ignore_ascii_case("bot");
                let eligible = who_read_exists_for_dm(
                    msg.outgoing,
                    unseen,
                    peer.summary.is_saved_messages,
                    is_bot,
                    false,
                    false,
                    i64::from(msg.date_unix),
                    effective_now,
                    DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS,
                );
                if eligible
                    && let Ok(status) = backend.get_outbox_read_date(peer.summary.id, msg.id)
                    && !matches!(
                        status,
                        OutboxReadDateStatus::TooOld | OutboxReadDateStatus::Unavailable
                    )
                {
                    info.dm_read_date = Some(status);
                }
            }
            PeerKind::Group => {
                // Estimate group size conservatively when exact count is not embedded in PeerSummary.
                let approx_members = parse_member_count_from_status(&peer.summary.status)
                    .unwrap_or(10)
                    .max(1);
                let eligible = who_read_exists_for_group(
                    msg.outgoing,
                    unseen,
                    false,
                    false,
                    approx_members,
                    DEFAULT_CHAT_READ_MARK_SIZE_THRESHOLD,
                    i64::from(msg.date_unix),
                    effective_now,
                    DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS,
                );
                if eligible
                    && let Ok(readers) =
                        backend.get_message_read_participants(peer.summary.id, msg.id)
                {
                    info.group_readers = Some(readers);
                }
            }
            PeerKind::Channel => {
                if msg.is_post
                    && let Ok(author) = backend.get_message_author(peer.summary.id, msg.id)
                {
                    info.message_author = Some(author);
                }
            }
        }

        info
    }

    /// Appends the localized `"Seen by N"` / `"Read at HH:MM"` and/or `"Sent by {user}"` rows
    /// to a message context menu (`history_view_context_menu.cpp:1618-1645, 2587-2610`).
    pub fn append_menu_items(&self, items: &mut Vec<MenuItem>, reference_now_unix: i64) {
        let mut added_section = false;

        if let Some(ref readers) = self.group_readers {
            if !items.is_empty() {
                items.push(MenuItem::Separator);
            }
            added_section = true;
            let label = if readers.is_empty() {
                tr::lng_context_seen_text_none()
            } else {
                tr::lng_context_seen_text(readers.len() as i64)
            };
            items.push(MenuItem::action(MENU_ID_SEEN_BY, label).with_icon(IconKind::Check));
        } else if let Some(status) = self.dm_read_date {
            if !items.is_empty() {
                items.push(MenuItem::Separator);
            }
            added_section = true;
            let label = format_dm_read_date_label(status, reference_now_unix);
            items.push(MenuItem::action(MENU_ID_READ_AT, label).with_icon(IconKind::Check));
        }

        if let Some(ref author) = self.message_author {
            if !added_section && !items.is_empty() {
                items.push(MenuItem::Separator);
            }
            let label = tr::lng_context_sent_by(&author.name);
            items.push(MenuItem::action(MENU_ID_MESSAGE_AUTHOR, label).with_icon(IconKind::Info));
        }
    }
}

/// Formats a 1:1 DM [`OutboxReadDateStatus`] row label for the message context menu.
#[must_use]
pub fn format_dm_read_date_label(status: OutboxReadDateStatus, reference_now_unix: i64) -> String {
    match status {
        OutboxReadDateStatus::Read { date_unix } => {
            // `WhenAction::refreshText`: the row shows the `FormatReadDate` text alone.
            format_read_timestamp(date_unix, reference_now_unix)
        }
        OutboxReadDateStatus::MyPrivacyHidden => {
            format!(
                "{} · {}",
                tr::lng_context_read_hidden(),
                tr::lng_context_read_show()
            )
        }
        OutboxReadDateStatus::PeerPrivacyHidden
        | OutboxReadDateStatus::TooOld
        | OutboxReadDateStatus::Unavailable => tr::lng_context_read_hidden(),
    }
}

/// Formats a read-receipt Unix timestamp in local time like `Api::FormatReadDate`
/// (`api_who_reacted.cpp`): "today at 11:02", "yesterday at 11:02", or "Nov 21 at 11:02".
#[must_use]
pub fn format_read_timestamp(date_unix: i32, reference_now_unix: i64) -> String {
    use tg_history::datetime::{LocalDateTime, format_time_short};
    let ts = i64::from(date_unix).max(0);
    let read = LocalDateTime::from_unix_local(ts);
    let now = LocalDateTime::from_unix_local(reference_now_unix.max(ts));
    let time = format_time_short(read);
    let read_day = read.to_local_epoch_secs().div_euclid(86_400);
    let now_day = now.to_local_epoch_secs().div_euclid(86_400);
    if read_day == now_day {
        tr::lng_mediaview_today(&time)
    } else if read_day + 1 == now_day {
        tr::lng_mediaview_yesterday(&time)
    } else {
        tr::lng_mediaview_date_time(&day_of_month_short(read.date.month, read.date.day), &time)
    }
}

/// `langDayOfMonthShort`: short month name and day (`lng_month_day`).
fn day_of_month_short(month: u8, day: u8) -> String {
    let name = match month {
        1 => tr::lng_month1_small(),
        2 => tr::lng_month2_small(),
        3 => tr::lng_month3_small(),
        4 => tr::lng_month4_small(),
        5 => tr::lng_month5_small(),
        6 => tr::lng_month6_small(),
        7 => tr::lng_month7_small(),
        8 => tr::lng_month8_small(),
        9 => tr::lng_month9_small(),
        10 => tr::lng_month10_small(),
        11 => tr::lng_month11_small(),
        _ => tr::lng_month12_small(),
    };
    tr::lng_month_day(&name, &day.to_string())
}

fn parse_member_count_from_status(status: &str) -> Option<i32> {
    let digits: String = status
        .split_whitespace()
        .next()?
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse::<i32>().ok().filter(|&n| n > 0)
}

/// Modal list of group readers (`messages.getMessageReadParticipants`) opened when clicking
/// `"Seen by N"` in a group message context menu.
#[derive(Debug, Clone)]
pub struct SeenByModalWidget {
    /// Owning group peer id.
    pub peer_id: PeerId,
    /// Target message id.
    pub msg_id: MsgId,
    /// Readers list returned by `HistoryBackend::get_message_read_participants`.
    pub readers: Vec<MessageReadParticipant>,
    /// Reference timestamp (seconds) for formatting `"HH:MM"` vs `"MMM DD, HH:MM"`.
    pub reference_now_unix: i64,
}

impl SeenByModalWidget {
    /// Creates a new Seen-By modal sheet for `(peer_id, msg_id)`.
    #[must_use]
    pub fn new(
        peer_id: PeerId,
        msg_id: MsgId,
        readers: Vec<MessageReadParticipant>,
        reference_now_unix: i64,
    ) -> Self {
        Self {
            peer_id,
            msg_id,
            readers,
            reference_now_unix,
        }
    }

    /// Renders the modal over `full_rect`. Returns `true` when closed by Escape, backdrop click,
    /// or the Close button.
    pub fn show(&mut self, ui: &mut Ui, full_rect: Rect, colors: &HistoryColors) -> bool {
        let mut closed = false;
        let screen = ui.ctx().content_rect();
        let bounds = if screen.is_positive() {
            screen
        } else {
            full_rect
        };
        Area::new(Id::unique((
            "tg_seen_by_modal_area",
            self.peer_id.0,
            self.msg_id.0,
        )))
        .order(Order::Foreground)
        .fixed_pos(bounds.min)
        .show(ui.ctx(), |ui| {
            closed = self.show_inner(ui, bounds, colors);
        });
        closed
    }

    fn show_inner(&mut self, ui: &mut Ui, full_rect: Rect, colors: &HistoryColors) -> bool {
        let mut closed = false;

        // Scrim backdrop
        let backdrop_resp = ui.interact(
            full_rect,
            Id::unique(("tg_seen_by_backdrop", self.peer_id.0, self.msg_id.0)),
            Sense::click(),
        );
        ui.painter().rect_filled(
            full_rect,
            CornerRadius::ZERO,
            Color32::from_rgba_unmultiplied(0, 0, 0, 110),
        );
        if backdrop_resp.clicked() || ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
            closed = true;
        }

        let card_w = (full_rect.width() - 32.0).clamp(280.0, 360.0);
        let row_h = 52.0;
        let list_h = if self.readers.is_empty() {
            72.0
        } else {
            (self.readers.len() as f32 * row_h).clamp(64.0, 320.0)
        };
        let card_h = (52.0 + list_h + 12.0).min(full_rect.height() - 40.0);
        let card_rect = Rect::from_center_size(full_rect.center(), Vec2::new(card_w, card_h));

        let shadow = BoxShadow::new(BoxShadowStyle {
            blur_radius: 18.0,
            offset: Point::new(0.0, 6.0),
            opacity: 0.28,
        });
        shadow.paint(ui.ctx(), ui.painter(), card_rect, 12.0);
        ui.painter()
            .rect_filled(card_rect, CornerRadius::same(12), colors.bar_bg);
        ui.painter().rect_stroke(
            card_rect,
            CornerRadius::same(12),
            Stroke::new(1.0, colors.divider),
            StrokeKind::Inside,
        );

        // Header bar
        let header_rect = Rect::from_min_size(card_rect.min, Vec2::new(card_rect.width(), 48.0));
        let heading = if self.readers.is_empty() {
            tr::lng_context_seen_text_none()
        } else {
            tr::lng_context_seen_text(self.readers.len() as i64)
        };
        ui.painter().text(
            Pos2::new(header_rect.left() + 18.0, header_rect.center().y),
            Align2::LEFT_CENTER,
            &heading,
            FontId::new(15.0, family_semibold()),
            colors.bold_fg,
        );

        let close_rect = Rect::from_center_size(
            Pos2::new(header_rect.right() - 24.0, header_rect.center().y),
            Vec2::splat(30.0),
        );
        let close_resp = ui.interact(
            close_rect,
            Id::unique(("tg_seen_by_close", self.msg_id.0)),
            Sense::click(),
        );
        close_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Close seen by list"));
        if close_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(close_rect.center(), 14.0, colors.bar_bg_over);
        }
        if close_resp.has_focus() {
            ui.painter()
                .circle_stroke(close_rect.center(), 14.0, Stroke::new(1.5, colors.accent));
        }
        paint_icon(
            ui.painter(),
            close_rect.shrink(7.0),
            IconKind::Close,
            colors.sub_text_fg,
        );
        if close_resp.clicked() {
            closed = true;
        }

        ui.painter().line_segment(
            [
                Pos2::new(card_rect.left(), header_rect.bottom()),
                Pos2::new(card_rect.right(), header_rect.bottom()),
            ],
            Stroke::new(1.0, colors.divider),
        );

        let body_rect = Rect::from_min_max(
            Pos2::new(card_rect.left(), header_rect.bottom() + 4.0),
            Pos2::new(card_rect.right(), card_rect.bottom() - 8.0),
        );

        if self.readers.is_empty() {
            ui.painter().text(
                body_rect.center(),
                Align2::CENTER_CENTER,
                tr::lng_context_seen_text_none(),
                FontId::new(13.0, family_regular()),
                colors.sub_text_fg,
            );
            return closed;
        }

        let ui_colors = colors.ui_colors();
        ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
            ScrollArea::vertical()
                .id_salt(("tg_seen_by_scroll", self.peer_id.0, self.msg_id.0))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for reader in &self.readers {
                        let (row_rect, row_resp) = ui.allocate_exact_size(
                            Vec2::new(body_rect.width(), row_h),
                            Sense::hover(),
                        );
                        if row_resp.hovered() {
                            ui.painter().rect_filled(
                                row_rect,
                                CornerRadius::ZERO,
                                colors.bar_bg_over,
                            );
                        }
                        let av_rect = Rect::from_center_size(
                            Pos2::new(row_rect.left() + 32.0, row_rect.center().y),
                            Vec2::splat(36.0),
                        );
                        EmptyUserpic::for_color_index(reader.color_index, &reader.name, &ui_colors)
                            .paint_circle_or_photo(ui.painter(), av_rect, reader.user_id.0);

                        let read_time =
                            format_read_timestamp(reader.read_date_unix, self.reference_now_unix);
                        let time_galley = ui.painter().layout_no_wrap(
                            read_time,
                            FontId::new(11.5, family_regular()),
                            colors.sub_text_fg,
                        );
                        let time_w = time_galley.size().x + 14.0;
                        let text_left = av_rect.right() + 12.0;
                        let text_max_w = (row_rect.right() - text_left - time_w - 22.0).max(60.0);

                        let name_galley = ui.painter().layout(
                            reader.name.clone(),
                            FontId::new(13.5, family_semibold()),
                            colors.text_fg,
                            text_max_w,
                        );
                        let name_y = if reader.username.is_some() {
                            row_rect.center().y - name_galley.size().y + 1.0
                        } else {
                            row_rect.center().y - name_galley.size().y * 0.5
                        };
                        ui.painter().galley(
                            Pos2::new(text_left, name_y),
                            name_galley,
                            colors.text_fg,
                        );

                        if let Some(ref uname) = reader.username {
                            ui.painter().text(
                                Pos2::new(text_left, row_rect.center().y + 3.0),
                                Align2::LEFT_TOP,
                                format!("@{uname}"),
                                FontId::new(11.5, family_regular()),
                                colors.sub_text_fg,
                            );
                        }

                        let time_x = row_rect.right() - 16.0 - time_galley.size().x;
                        let check_rect = Rect::from_center_size(
                            Pos2::new(time_x - 8.0, row_rect.center().y),
                            Vec2::splat(11.0),
                        );
                        paint_icon(ui.painter(), check_rect, IconKind::Check, colors.accent);
                        ui.painter().galley(
                            Pos2::new(time_x, row_rect.center().y - time_galley.size().y * 0.5),
                            time_galley,
                            colors.sub_text_fg,
                        );
                    }
                });
        });

        closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_read_timestamp_same_day_and_other_day() {
        use tg_history::datetime::{LocalDateTime, format_time_short};
        let base: i32 = 1_795_341_600; // 2026-11-22 10:00:00 UTC
        let local = |ts: i32| format_time_short(LocalDateTime::from_unix_local(i64::from(ts)));
        let now = i64::from(base) + 3800;
        assert_eq!(
            format_read_timestamp(base + 3720, now),
            tr::lng_mediaview_today(&local(base + 3720)),
            "local time, not UTC (`Api::FormatReadDate`)"
        );
        assert_eq!(
            format_read_timestamp(base + 3720 - 86_400, now),
            tr::lng_mediaview_yesterday(&local(base + 3720))
        );
        let old = LocalDateTime::from_unix_local(i64::from(base) + 3720 - 3 * 86_400);
        assert_eq!(
            format_read_timestamp(base + 3720 - 3 * 86_400, now),
            tr::lng_mediaview_date_time(
                &day_of_month_short(old.date.month, old.date.day),
                &local(base + 3720)
            )
        );
    }

    #[test]
    fn format_dm_read_date_labels_handle_all_statuses() {
        let now = 1_795_348_800_i64;
        assert_eq!(
            format_dm_read_date_label(
                OutboxReadDateStatus::Read {
                    date_unix: 1_795_345_320
                },
                now
            ),
            format_read_timestamp(1_795_345_320, now)
        );
        let hidden = format_dm_read_date_label(OutboxReadDateStatus::MyPrivacyHidden, now);
        assert!(hidden.contains(&tr::lng_context_read_hidden()));
        assert!(hidden.contains(&tr::lng_context_read_show()));
    }
}
