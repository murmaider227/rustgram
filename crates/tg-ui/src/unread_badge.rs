//! Pill-shaped unread counter badges.
//!
//! Port of `Telegram/SourceFiles/ui/unread_badge_paint.h` and
//! `Telegram/SourceFiles/ui/unread_badge_paint.cpp`.

use egui::{Align2, Color32, CornerRadius, Painter, Pos2, Rect, Vec2};

use crate::style::{Align, UnreadBadgeStyle};
use crate::theme::Colors;

/// Badge context variants mirroring `Ui::UnreadBadgeSize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnreadBadgeKind {
    #[default]
    Dialogs,
    ReactionInDialogs,
    PollInDialogs,
}

/// Format an unread count string with optional `allow_digits` truncation.
///
/// Exact port of `ComputeUnreadBadgeText` in `unread_badge_paint.cpp`:
/// if `allow_digits > 0 && count.len() > allow_digits + 1`, returns `".."` + last `allow_digits` chars.
pub fn format_unread_badge_text(unread_count: &str, allow_digits: usize) -> String {
    let char_count = unread_count.chars().count();
    if allow_digits > 0 && char_count > allow_digits + 1 {
        let skip = char_count - allow_digits;
        let tail: String = unread_count.chars().skip(skip).collect();
        format!("..{tail}")
    } else {
        unread_count.to_owned()
    }
}

/// Compute the pill dimensions for `text_width` and `style`.
///
/// Matches `CountUnreadBadgeSize`: width = `max(text_width + 2 * padding, size)`.
#[inline]
pub fn unread_badge_size_for_text_width(text_width: f32, style: &UnreadBadgeStyle) -> Vec2 {
    let h = style.size;
    let w = (text_width + 2.0 * style.padding).max(h);
    Vec2::new(w, h)
}

/// Resolve background and foreground colors for a badge state.
pub fn resolve_unread_badge_colors(
    style: &UnreadBadgeStyle,
    kind: UnreadBadgeKind,
    colors: &dyn Colors,
) -> (Color32, Color32) {
    let bg_key = match kind {
        UnreadBadgeKind::ReactionInDialogs => match (style.muted, style.active, style.selected) {
            (false, true, _) => "dialogsDraftFgActive",
            (false, false, true) => "dialogsDraftFgOver",
            (false, false, false) => "dialogsDraftFg",
            (true, true, _) => "dialogsUnreadBgMutedActive",
            (true, false, true) => "dialogsUnreadBgMutedOver",
            (true, false, false) => "dialogsUnreadBgMuted",
        },
        UnreadBadgeKind::PollInDialogs => match (style.muted, style.active, style.selected) {
            (false, _, _) => "dialogsPollIconFg",
            (true, true, _) => "dialogsUnreadBgMutedActive",
            (true, false, true) => "dialogsUnreadBgMutedOver",
            (true, false, false) => "dialogsUnreadBgMuted",
        },
        UnreadBadgeKind::Dialogs => match (style.muted, style.active, style.selected) {
            (false, true, _) => "dialogsUnreadBgActive",
            (false, false, true) => "dialogsUnreadBgOver",
            (false, false, false) => "dialogsUnreadBg",
            (true, true, _) => "dialogsUnreadBgMutedActive",
            (true, false, true) => "dialogsUnreadBgMutedOver",
            (true, false, false) => "dialogsUnreadBgMuted",
        },
    };
    let fg_key = if style.active {
        "dialogsUnreadFgActive"
    } else if style.selected {
        "dialogsUnreadFgOver"
    } else {
        "dialogsUnreadFg"
    };
    (colors.color(bg_key), colors.color(fg_key))
}

/// Paint an unread counter pill badge anchored at `(x, y)` per `style.align`.
pub fn paint_unread_badge(
    painter: &Painter,
    unread_count: &str,
    anchor: Pos2,
    style: &UnreadBadgeStyle,
    kind: UnreadBadgeKind,
    allow_digits: usize,
    colors: &dyn Colors,
) -> Rect {
    let text = format_unread_badge_text(unread_count, allow_digits);
    let font_id = style.font.font_id();
    let (bg, fg) = resolve_unread_badge_colors(style, kind, colors);
    let galley = painter.layout_no_wrap(text, font_id, fg);
    let size = unread_badge_size_for_text_width(galley.size().x, style);

    let left = match style.align {
        Align::Center | Align::Top | Align::Bottom => anchor.x - size.x * 0.5,
        Align::Right | Align::TopRight | Align::BottomRight => anchor.x - size.x,
        Align::Left | Align::TopLeft | Align::BottomLeft => anchor.x,
    };
    let rect = Rect::from_min_size(Pos2::new(left, anchor.y), size);
    let radius = (size.y * 0.5).ceil() as u8;
    painter.rect_filled(rect, CornerRadius::same(radius), bg);

    painter.galley(
        Pos2::new(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        fg,
    );
    rect
}

/// Allocate and draw an inline unread badge in an egui `Ui`.
pub fn unread_badge(
    ui: &mut egui::Ui,
    unread_count: &str,
    style: &UnreadBadgeStyle,
    colors: &dyn Colors,
) -> egui::Response {
    let text = format_unread_badge_text(unread_count, 0);
    let font_id = style.font.font_id();
    let (bg, fg) = resolve_unread_badge_colors(style, UnreadBadgeKind::Dialogs, colors);
    let galley = ui
        .painter()
        .layout_no_wrap(text.clone(), font_id.clone(), fg);
    let size = unread_badge_size_for_text_width(galley.size().x, style);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let radius = (size.y * 0.5).ceil() as u8;
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius), bg);
        ui.painter()
            .text(rect.center(), Align2::CENTER_CENTER, text, font_id, fg);
    }
    response
}
