//! Layer-233 streamed bot drafts (`sendMessageTextDraftAction`, `sendMessageRichMessageDraftAction`,
//! `sendMessageStopDraftAction`) and seamless adoption of incoming server messages.
//!
//! Port origin: `Telegram/SourceFiles/history/history_streamed_drafts.h` and
//! `Telegram/SourceFiles/history/history_streamed_drafts.cpp`.

use egui::WidgetInfo;
use egui::accesskit::Role;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};

use tg_app_core::history::theme::HistoryColors;

pub use tg_app_core::core_types::dto::streamed_drafts::*;

/// Paints the "Stop generating" bar above the composer when a stoppable bot draft is active.
///
/// Returns `true` when the user clicks the Stop button.
pub fn paint_stop_streamed_draft_bar(ui: &mut Ui, rect: Rect, colors: &HistoryColors) -> bool {
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);
    ui.painter().line_segment(
        [
            Pos2::new(rect.left(), rect.top()),
            Pos2::new(rect.right(), rect.top()),
        ],
        Stroke::new(1.0, colors.divider),
    );

    ui.painter().text(
        Pos2::new(rect.left() + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        "Bot is generating a response...",
        FontId::proportional(12.5),
        colors.sub_text_fg,
    );

    let btn_rect = Rect::from_center_size(
        Pos2::new(rect.right() - 78.0, rect.center().y),
        Vec2::new(132.0, 24.0),
    );
    let resp = ui.interact(
        btn_rect,
        Id::unique("tg_hist_stop_streamed_draft"),
        Sense::click(),
    );
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Stop generating"));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let fill = if resp.hovered() {
        Color32::from_rgba_unmultiplied(colors.accent.r(), colors.accent.g(), colors.accent.b(), 38)
    } else {
        Color32::from_rgba_unmultiplied(colors.accent.r(), colors.accent.g(), colors.accent.b(), 22)
    };
    ui.painter()
        .rect_filled(btn_rect, CornerRadius::same(12), fill);

    // Small stop-square glyph + label
    let sq = Rect::from_center_size(
        Pos2::new(btn_rect.left() + 14.0, btn_rect.center().y),
        Vec2::splat(8.0),
    );
    ui.painter()
        .rect_filled(sq, CornerRadius::same(1), colors.accent);
    ui.painter().text(
        Pos2::new(sq.right() + 6.0, btn_rect.center().y),
        Align2::LEFT_CENTER,
        "Stop generating",
        FontId::proportional(12.0),
        colors.accent,
    );

    resp.on_hover_text("Stop bot draft generation").clicked()
}
