//! Toggleable AI Message Summary header & entry pill (`messages.summarizeText`).
//!
//! Port origin: `Telegram/SourceFiles/history/view/history_view_summary_header.h` & `.cpp`.

use egui::accesskit::Role;
use egui::{
    Color32, CornerRadius, CursorIcon, FontId, Id, Painter, Pos2, Rect, Sense, Shape, Stroke, Ui,
    Vec2, WidgetInfo,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::tg_style::st;

use crate::history::backend::MessageSummaryEntry;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::types::MsgId;

/// Vertical height reserved inside the bubble for the expanded AI summary header
/// (`historyReplyPadding.top + 2 * msgServiceNameFont->height + historyReplyPadding.bottom`).
pub const SUMMARY_HEADER_HEIGHT: i32 = 42;
/// Top + bottom margin around the expanded summary header (`historyReplyTop + historyReplyBottom`).
pub const SUMMARY_HEADER_TOTAL_HEIGHT: i32 = 48;
/// Compact collapsed entry pill height shown when a message has `summary_from_language`.
pub const SUMMARY_PILL_HEIGHT: i32 = 24;
/// Number of drifting star particles inside the expanded summary header (`StarParticles::Type::Right, 15`).
pub const SUMMARY_STAR_COUNT: usize = 15;
/// Normalized drift speed matching `_animation->particles.setSpeed(0.05)`.
pub const SUMMARY_STAR_SPEED: f64 = 0.05;

/// User interaction emitted by the AI summary pill or expanded [`paint_summary_header`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryHeaderAction {
    /// Toggle between original message text and AI summary (calling `messages.summarizeText` if uncached).
    Toggle(MsgId),
    /// User clicked the right-side Cocoon / AI info icon button (`Ui::AboutCocoonBox`).
    AboutAi(MsgId),
}

/// Returns the extra vertical height (in logical pixels) reserved inside the bubble for the AI
/// summary affordance on a message that has `summary_from_language`.
#[must_use]
pub fn summary_reserved_height(
    summary_from_language: Option<&str>,
    entry: Option<&MessageSummaryEntry>,
) -> i32 {
    if summary_from_language.is_none() && entry.is_none() {
        return 0;
    }
    if entry.is_some_and(|e| e.shown) {
        SUMMARY_HEADER_TOTAL_HEIGHT
    } else {
        SUMMARY_PILL_HEIGHT + 6
    }
}

/// Paints the compact collapsed `"AI summary"` entry pill on a summarizable message bubble.
pub fn paint_summary_pill(
    ui: &mut Ui,
    msg_id: MsgId,
    rect: Rect,
    outgoing: bool,
    entry: Option<&MessageSummaryEntry>,
    colors: &HistoryColors,
) -> Option<SummaryHeaderAction> {
    if !rect.is_positive() {
        return None;
    }
    let loading = entry.is_some_and(|e| e.loading);
    let premium_required = entry.is_some_and(|e| e.premium_required);
    let title = if loading {
        format!("{}...", tr::lng_summarize_header_title())
    } else if premium_required {
        format!("{} · Premium", tr::lng_summarize_header_title())
    } else {
        tr::lng_summarize_header_title()
    };

    let fg = if outgoing {
        colors.msg_out_reply_bar
    } else {
        colors.msg_in_reply_bar
    };
    let bg = if outgoing {
        colors.msg_out_reply_bg
    } else {
        colors.msg_in_reply_bg
    };

    let font = FontId::new(11.5, family_semibold());
    let galley = ui.painter().layout_no_wrap(title.clone(), font, fg);
    let pill_w = (galley.size().x + 28.0).min(rect.width()).max(72.0);
    let pill_h = (SUMMARY_PILL_HEIGHT as f32).min(rect.height()).max(20.0);
    let pill_rect = Rect::from_min_size(rect.min, Vec2::new(pill_w, pill_h));

    let resp = ui.interact(
        pill_rect,
        Id::unique(("tg_summary_pill", msg_id.0)),
        Sense::click(),
    );
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &title));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let fill = if resp.is_pointer_button_down_on() {
        Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), 48)
    } else if resp.hovered() {
        Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), 34)
    } else {
        bg
    };
    let cr = CornerRadius::same((pill_h * 0.5).round() as u8);
    ui.painter().rect_filled(pill_rect, cr, fill);
    if resp.has_focus() {
        ui.painter().rect_stroke(
            pill_rect,
            cr,
            Stroke::new(1.5, fg),
            egui::StrokeKind::Inside,
        );
    }

    // 4-point sparkle glyph on the left of the pill label.
    let icon_center = Pos2::new(pill_rect.left() + 12.0, pill_rect.center().y);
    paint_sparkle_glyph(ui.painter(), icon_center, 5.0, fg);

    let text_pos = Pos2::new(
        pill_rect.left() + 21.0,
        pill_rect.center().y - galley.size().y * 0.5,
    );
    let text_clip = Rect::from_min_max(
        Pos2::new(pill_rect.left() + 19.0, pill_rect.top()),
        Pos2::new(
            (pill_rect.right() - 6.0).max(pill_rect.left() + 22.0),
            pill_rect.bottom(),
        ),
    );
    ui.painter()
        .with_clip_rect(text_clip)
        .galley(text_pos, galley, fg);

    if resp.clicked() {
        Some(SummaryHeaderAction::Toggle(msg_id))
    } else {
        None
    }
}

/// Paints the expanded quote-style `SummaryHeader` bar above the summarized text
/// (`HistoryView::SummaryHeader::paint` in `history_view_summary_header.cpp:102-239`).
#[allow(clippy::too_many_arguments)]
pub fn paint_summary_header(
    ui: &mut Ui,
    msg_id: MsgId,
    rect: Rect,
    outgoing: bool,
    loading: bool,
    paused_effects: bool,
    colors: &HistoryColors,
) -> Option<SummaryHeaderAction> {
    if !rect.is_positive() {
        return None;
    }
    let header_h = (SUMMARY_HEADER_HEIGHT as f32).min(rect.height()).max(32.0);
    let box_rect = Rect::from_min_size(
        Pos2::new(rect.left(), rect.top() + 2.0),
        Vec2::new(rect.width(), header_h),
    );

    let bar_color = if outgoing {
        colors.msg_out_reply_bar
    } else {
        colors.msg_in_reply_bar
    };
    let bg_color = if outgoing {
        colors.msg_out_reply_bg
    } else {
        colors.msg_in_reply_bg
    };

    let icon_size = st::history_summary_header_icon_size.0 as f32;
    let icon_shift = st::history_summary_header_icon_size_inner.0 as f32;
    let icon_rect = Rect::from_min_size(
        Pos2::new(
            (box_rect.right() - icon_size - icon_shift).max(box_rect.left() + 4.0),
            box_rect.center().y - icon_size * 0.5,
        ),
        Vec2::splat(icon_size.min(box_rect.height())),
    );

    let icon_resp = ui.interact(
        icon_rect,
        Id::unique(("tg_summary_hdr_icon", msg_id.0)),
        Sense::click(),
    );
    icon_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "About AI summary"));
    if icon_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let main_rect = Rect::from_min_max(
        box_rect.min,
        Pos2::new(
            (icon_rect.left() - 2.0).max(box_rect.left() + 20.0),
            box_rect.max.y,
        ),
    );
    let main_resp = ui.interact(
        main_rect,
        Id::unique(("tg_summary_hdr_toggle", msg_id.0)),
        Sense::click(),
    );
    let toggle_a11y = format!(
        "{}: {}",
        tr::lng_summarize_header_title(),
        tr::lng_summarize_header_about()
    );
    main_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &toggle_a11y));
    if main_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    // Quote-style background + left 3px accent bar (`FillQuotePaint`).
    let radius = CornerRadius::same(4);
    let fill = if main_resp.hovered() {
        Color32::from_rgba_unmultiplied(bar_color.r(), bar_color.g(), bar_color.b(), 32)
    } else {
        bg_color
    };
    ui.painter().rect_filled(box_rect, radius, fill);
    let accent_bar = Rect::from_min_size(box_rect.min, Vec2::new(3.0, box_rect.height()));
    ui.painter()
        .rect_filled(accent_bar, CornerRadius::same(2), bar_color);
    if main_resp.has_focus() {
        ui.painter().rect_stroke(
            box_rect,
            radius,
            Stroke::new(1.5, bar_color),
            egui::StrokeKind::Inside,
        );
    }

    // Two-line header copy: `lng_summarize_header_title` + `lng_summarize_header_about`.
    // Lay out the text first so drifting sparkle particles stay strictly outside the text area.
    let text_left = box_rect.left() + 10.0;
    let text_max_w = (icon_rect.left() - text_left - 4.0).max(32.0);
    let title = tr::lng_summarize_header_title();
    let subtitle = if loading {
        "Summarizing message...".to_owned()
    } else {
        tr::lng_summarize_header_about()
    };

    let title_galley =
        ui.painter()
            .layout_no_wrap(title, FontId::new(12.5, family_semibold()), bar_color);
    let sub_galley = ui.painter().layout_no_wrap(
        subtitle,
        FontId::new(11.5, family_regular()),
        colors.sub_text_fg,
    );
    let text_occupied_w = title_galley
        .size()
        .x
        .max(sub_galley.size().x)
        .min(text_max_w);
    let text_occupied_right = text_left + text_occupied_w;

    // Drifting mini-star particles clipped strictly outside the text area (open right flank +
    // sparkle emblem zone), matching `StarParticles::Type::Right` while keeping copy crisp, and
    // strictly suppressed under reduced motion / `PowerSaving::kChatEffects`.
    if !paused_effects {
        let particle_rect = summary_particle_rect(box_rect, text_occupied_right, icon_rect);
        if particle_rect.width() >= 12.0 && particle_rect.height() >= 12.0 {
            let now_secs = ui.ctx().input(|i| i.time);
            paint_summary_star_particles(
                ui.painter(),
                particle_rect,
                msg_id.0 as u64,
                now_secs,
                bar_color,
            );
            ui.ctx().request_repaint();
        }
    }

    let text_clip = Rect::from_min_size(
        Pos2::new(text_left, box_rect.top()),
        Vec2::new(text_max_w, box_rect.height()),
    );
    let clipped = ui.painter().with_clip_rect(text_clip);
    clipped.galley(
        Pos2::new(text_left, box_rect.top() + 4.0),
        title_galley,
        bar_color,
    );
    clipped.galley(
        Pos2::new(text_left, box_rect.top() + 20.0),
        sub_galley,
        colors.sub_text_fg,
    );

    // Right-side sparkle/cocoon emblem button (`iconRect()`).
    if icon_resp.hovered() {
        ui.painter().circle_filled(
            icon_rect.center(),
            icon_rect.width() * 0.48,
            Color32::from_rgba_unmultiplied(bar_color.r(), bar_color.g(), bar_color.b(), 36),
        );
    }
    paint_sparkle_glyph(ui.painter(), icon_rect.center(), 7.5, bar_color);
    paint_sparkle_glyph(
        ui.painter(),
        icon_rect.center() + Vec2::new(6.0, -5.5),
        3.2,
        Color32::from_rgba_unmultiplied(bar_color.r(), bar_color.g(), bar_color.b(), 190),
    );

    if icon_resp.clicked() {
        Some(SummaryHeaderAction::AboutAi(msg_id))
    } else if main_resp.clicked() {
        Some(SummaryHeaderAction::Toggle(msg_id))
    } else {
        None
    }
}

/// Computes the clip rectangle for drifting star particles so they stay strictly out of the
/// left-side title/subtitle copy and drift only across the right-hand open space and icon area.
#[must_use]
pub(crate) fn summary_particle_rect(
    box_rect: Rect,
    text_occupied_right: f32,
    icon_rect: Rect,
) -> Rect {
    let min_x = (text_occupied_right + 10.0)
        .max(box_rect.left() + 4.0)
        .min((icon_rect.left() - 2.0).max(box_rect.left() + 4.0));
    let max_x = (box_rect.right() - 2.0).max(min_x);
    let min_y = (box_rect.top() + 2.0).min(box_rect.bottom());
    let max_y = (box_rect.bottom() - 2.0).max(min_y);
    Rect::from_min_max(Pos2::new(min_x, min_y), Pos2::new(max_x, max_y))
}

/// Paints a crisp 4-point star / sparkle emblem using two strictly convex diamond quads.
///
/// Enforces a minimum waist half-width of `1.0px` matching tdesktop's `innerRadius = size / 5`
/// (`ministar_particles.cpp:78-79`) so small stars never collapse into thin anti-aliased streaks.
fn paint_sparkle_glyph(painter: &Painter, center: Pos2, radius: f32, color: Color32) {
    let r = radius.max(2.0);
    let half_w = (r * 0.40).max(1.0).min(r * 0.55);
    let vertical_diamond = vec![
        Pos2::new(center.x, center.y - r),
        Pos2::new(center.x + half_w, center.y),
        Pos2::new(center.x, center.y + r),
        Pos2::new(center.x - half_w, center.y),
    ];
    let horizontal_diamond = vec![
        Pos2::new(center.x - r, center.y),
        Pos2::new(center.x, center.y - half_w),
        Pos2::new(center.x + r, center.y),
        Pos2::new(center.x, center.y + half_w),
    ];
    painter.add(Shape::convex_polygon(vertical_diamond, color, Stroke::NONE));
    painter.add(Shape::convex_polygon(
        horizontal_diamond,
        color,
        Stroke::NONE,
    ));
}

/// Paints gentle right-drifting mini-stars clipped inside `rect` (outside the header text area),
/// with smooth edge fade-in/out and low alpha so they accentuate the AI icon without visual noise.
fn paint_summary_star_particles(
    painter: &Painter,
    rect: Rect,
    seed: u64,
    now_secs: f64,
    color: Color32,
) {
    let clipped = painter.with_clip_rect(rect);
    let w = f64::from(rect.width().max(1.0));
    let h = f64::from(rect.height().max(1.0));
    // Scale visible count proportionally to the right-side particle zone so narrow bubbles stay airy.
    let count = ((rect.width() / 22.0).round() as usize).clamp(4, SUMMARY_STAR_COUNT);
    for i in 0..count {
        let hash = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add((i as u64 + 1).wrapping_mul(1442695040888963407));
        let x0 = ((hash & 0xFFFF) as f64) / 65535.0;
        let y0 = (((hash >> 16) & 0xFFFF) as f64) / 65535.0;
        let phase = (((hash >> 32) & 0xFFFF) as f64) / 65535.0;
        let x_frac = (x0 + now_secs * SUMMARY_STAR_SPEED * (0.7 + 0.6 * phase)) % 1.0;
        // Smooth edge fade over the leading/trailing 20% of `rect` so stars never pop at clip bounds.
        let edge_fade = (x_frac / 0.20).min((1.0 - x_frac) / 0.20).clamp(0.0, 1.0);
        let alpha_wave =
            0.35 + 0.65 * (0.5 + 0.5 * ((now_secs * 1.4 + phase * std::f64::consts::TAU).sin()));
        let alpha = (alpha_wave * edge_fade * 58.0).round().clamp(0.0, 60.0) as u8;
        if alpha < 8 {
            continue;
        }
        let pos = Pos2::new(
            rect.left() + (x_frac * w) as f32,
            rect.top() + (0.16 + y0 * 0.68) as f32 * h as f32,
        );
        let r = 2.2 + (phase as f32) * 1.4;
        let col = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);
        paint_sparkle_glyph(&clipped, pos, r, col);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_text::TextWithEntities;

    #[test]
    fn reserved_height_matches_collapsed_and_expanded_states() {
        assert_eq!(summary_reserved_height(None, None), 0);
        assert_eq!(
            summary_reserved_height(Some("en"), None),
            SUMMARY_PILL_HEIGHT + 6
        );
        let shown = MessageSummaryEntry {
            text: TextWithEntities::simple("Short summary."),
            loading: false,
            shown: true,
            premium_required: false,
            to_lang: String::new(),
        };
        assert_eq!(
            summary_reserved_height(Some("en"), Some(&shown)),
            SUMMARY_HEADER_TOTAL_HEIGHT
        );
    }

    #[test]
    fn particle_rect_stays_clear_of_header_text_area() {
        // Desktop-width bubble header (320px wide) with subtitle ending around x=165.
        let wide_box = Rect::from_min_size(Pos2::new(20.0, 10.0), Vec2::new(320.0, 42.0));
        let wide_icon = Rect::from_min_size(Pos2::new(306.0, 17.0), Vec2::splat(28.0));
        let wide_text_right = 165.0;
        let wide_pr = summary_particle_rect(wide_box, wide_text_right, wide_icon);
        assert!(
            wide_pr.left() >= wide_text_right + 10.0,
            "particle clip rect must start strictly to the right of text (left={}, text_right={wide_text_right})",
            wide_pr.left()
        );
        assert!(wide_pr.right() <= wide_box.right());

        // Compact mobile-width bubble header (210px wide) where subtitle reaches near the icon.
        let narrow_box = Rect::from_min_size(Pos2::new(16.0, 10.0), Vec2::new(210.0, 42.0));
        let narrow_icon = Rect::from_min_size(Pos2::new(192.0, 17.0), Vec2::splat(28.0));
        let narrow_text_right = 186.0;
        let narrow_pr = summary_particle_rect(narrow_box, narrow_text_right, narrow_icon);
        assert!(
            narrow_pr.left() >= narrow_text_right,
            "on narrow bubbles, particle clip rect must not enter the text area (left={}, text_right={narrow_text_right})",
            narrow_pr.left()
        );
        assert!(narrow_pr.right() <= narrow_box.right());
    }
}
