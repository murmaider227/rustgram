//! Interactive Birthday Suggestion & Userpic Suggestion service action cards.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/view/media/history_view_birthday_suggestion.h` & `.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_userpic_suggestion.h` & `.cpp`

use egui::accesskit::Role;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Painter, Pos2, Rect, Sense, Shape,
    Stroke, Ui, Vec2, WidgetInfo,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::tg_style::st;

use crate::history::backend::HistoryMessage;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::types::MsgId;

/// Centered service card width matching `st::birthdaySuggestStickerWidth` (`236px`).
pub const SUGGESTION_CARD_WIDTH: i32 = 236;

/// Interactive action emitted when the user clicks the CTA button on a Birthday or Userpic
/// suggestion card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestionCardAction {
    /// Recipient clicked the birthday suggestion card button (`internal:edit_birthday:suggestion_*`
    /// -> applies `account.updateBirthday`).
    ApplyBirthday {
        /// Service message id.
        msg_id: MsgId,
        /// Suggested day of month (`1..=31`).
        day: u8,
        /// Suggested month (`1..=12`).
        month: u8,
        /// Optional suggested birth year.
        year: Option<u16>,
    },
    /// Recipient clicked `"Set Photo"` on an incoming userpic suggestion card
    /// (`photos.updateProfilePhoto`).
    ApplyUserpic {
        /// Service message id.
        msg_id: MsgId,
        /// Suggested photo id.
        photo_id: i64,
        /// Suggested photo access hash.
        access_hash: i64,
        /// Suggested photo file reference.
        file_reference: Vec<u8>,
        /// True if the suggested userpic is an animated video avatar.
        is_video: bool,
    },
    /// Sender clicked `"View Photo"` on their outgoing userpic suggestion card.
    ViewUserpic {
        /// Service message id.
        msg_id: MsgId,
    },
}

/// Returns the English month name for `month` in `1..=12`.
#[must_use]
pub fn month_name(month: u8) -> &'static str {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    MONTHS
        .get(usize::from(month.saturating_sub(1)))
        .copied()
        .unwrap_or("Unknown")
}

/// Computes `(width, height)` for a Birthday Suggestion service card
/// (`GenerateSuggetsBirthdayMedia` + `BirthdayTable`).
///
/// Kept within a `1:1` aspect ratio (`236×236` incoming, `236×196` outgoing) so
/// `tg-history`'s centered service-media box allocates the exact card height without
/// clamping vertically.
#[must_use]
pub fn measure_birthday_suggestion_card(outgoing: bool) -> (i32, i32) {
    let w = st::birthday_suggest_sticker_width
        .0
        .max(SUGGESTION_CARD_WIDTH);
    let h = if outgoing { 196 } else { 236 };
    (w, h)
}

/// Computes `(width, height)` for a Userpic Suggestion service card (`HistoryView::UserpicSuggestion`).
#[must_use]
pub fn measure_userpic_suggestion_card() -> (i32, i32) {
    let w = st::birthday_suggest_sticker_width
        .0
        .max(SUGGESTION_CARD_WIDTH);
    // 96px circular avatar + top/bottom padding + 2-line subtitle + 30px CTA button.
    (w, 232)
}

/// Paints the interactive Birthday Suggestion card (`history_view_birthday_suggestion.cpp`),
/// rendering the real `cake.tgs` Lottie emblem when available and falling back to the vector cake
/// only when `cake.tgs` is absent or invalid.
#[allow(clippy::too_many_arguments)]
pub fn paint_birthday_suggestion_card(
    ui: &mut Ui,
    card_rect: Rect,
    msg: &HistoryMessage,
    peer_name: &str,
    day: u8,
    month: u8,
    year: Option<u16>,
    colors: &HistoryColors,
) -> Option<SuggestionCardAction> {
    paint_birthday_suggestion_card_with_tgs(
        ui, card_rect, msg, peer_name, day, month, year, None, false, colors,
    )
}

/// Paints the interactive Birthday Suggestion card with explicit `cake.tgs` Lottie override and
/// reduced-motion flag (`history_view_birthday_suggestion.cpp`).
#[allow(clippy::too_many_arguments)]
pub fn paint_birthday_suggestion_card_with_tgs(
    ui: &mut Ui,
    card_rect: Rect,
    msg: &HistoryMessage,
    peer_name: &str,
    day: u8,
    month: u8,
    year: Option<u16>,
    cake_tgs_override: Option<&[u8]>,
    reduced_motion: bool,
    colors: &HistoryColors,
) -> Option<SuggestionCardAction> {
    if !card_rect.is_positive() {
        return None;
    }
    ui.painter()
        .rect_filled(card_rect, CornerRadius::same(16), colors.service_bg);

    let cx = card_rect.center().x;
    let mut y = card_rect.top() + 10.0;

    // 1. Real `cake.tgs` Lottie emblem (`ChatHelpers::GenerateLocalTgsSticker(session, "cake")`),
    //    falling back to the vector birthday cake illustration only when unavailable.
    let cake_side = (st::birthday_suggest_sticker_size.0 as f32 * 0.76)
        .min(card_rect.width() * 0.36)
        .max(60.0);
    let cake_rect = Rect::from_min_size(Pos2::new(cx - cake_side * 0.5, y), Vec2::splat(cake_side));
    let now_secs = ui.ctx().input(|i| i.time);
    if let Some(tex) = crate::history::birthday_cake::resolve_cake_lottie_texture(
        ui.ctx(),
        msg.id.0,
        cake_tgs_override,
        now_secs,
        reduced_motion,
    ) {
        ui.painter().image(
            tex.id(),
            cake_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        paint_birthday_cake_emblem(ui.painter(), cake_rect, colors.accent);
    }
    y += cake_side + 8.0;

    // 2. Prompt text (`lng_action_suggested_birthday[_me]`).
    let target_name = if msg.outgoing {
        peer_name
    } else {
        &msg.sender_name
    };
    let prompt = if msg.outgoing {
        tr::lng_action_suggested_birthday_me(target_name)
    } else {
        tr::lng_action_suggested_birthday(target_name)
    };
    let inner_w = (card_rect.width() - 24.0).max(80.0);
    let prompt_galley = ui.painter().layout(
        prompt,
        FontId::new(12.5, family_regular()),
        colors.service_fg,
        inner_w,
    );
    ui.painter().galley(
        Pos2::new(cx - prompt_galley.size().x * 0.5, y),
        prompt_galley.clone(),
        colors.service_fg,
    );
    y += prompt_galley.size().y + 10.0;

    // 3. `BirthdayTable`: Day | Month | [Year] columns with 70% opacity labels over bold values.
    let mut cols: Vec<(String, String)> = vec![
        (tr::lng_date_input_day(), day.to_string()),
        (tr::lng_date_input_month(), month_name(month).to_owned()),
    ];
    if let Some(y_val) = year {
        cols.push((tr::lng_date_input_year(), y_val.to_string()));
    }
    let col_w = inner_w / (cols.len().max(1) as f32);
    let label_fg = Color32::from_rgba_unmultiplied(
        colors.service_fg.r(),
        colors.service_fg.g(),
        colors.service_fg.b(),
        (f32::from(colors.service_fg.a()) * 0.7).round() as u8,
    );
    let table_left = card_rect.left() + 12.0;
    let table_skip = (st::birthday_suggest_table_skip.0 as f32).min(4.0);
    for (i, (lbl, val)) in cols.iter().enumerate() {
        let col_cx = table_left + col_w * (i as f32 + 0.5);
        ui.painter().text(
            Pos2::new(col_cx, y),
            Align2::CENTER_TOP,
            lbl,
            FontId::new(11.0, family_regular()),
            label_fg,
        );
        ui.painter().text(
            Pos2::new(col_cx, y + 14.0 + table_skip),
            Align2::CENTER_TOP,
            val,
            FontId::new(13.0, family_semibold()),
            colors.service_fg,
        );
    }
    let table_bottom = y + 32.0 + table_skip;

    // 4. Interactive pill CTA button when incoming (`!isSelf`).
    if msg.outgoing {
        return None;
    }

    let btn_h = (st::msg_service_gift_box_button_height.0 as f32).min(30.0);
    let btn_w = (card_rect.width() - 36.0).clamp(96.0, 176.0);
    let btn_top = (card_rect.bottom() - btn_h - 12.0).max(table_bottom + 8.0);
    let btn_rect = Rect::from_min_size(
        Pos2::new(cx - btn_w * 0.5, btn_top),
        Vec2::new(btn_w, btn_h),
    );
    let label = tr::lng_sticker_premium_view();
    let a11y_label = format!("Add suggested birthday ({day} {})", month_name(month));
    if paint_service_pill_button(
        ui,
        Id::unique(("tg_bday_suggest_btn", msg.id.0)),
        btn_rect,
        &label,
        &a11y_label,
        colors,
    ) {
        return Some(SuggestionCardAction::ApplyBirthday {
            msg_id: msg.id,
            day,
            month,
            year,
        });
    }
    None
}

/// Paints the interactive Userpic Suggestion card (`history_view_userpic_suggestion.cpp`).
#[allow(clippy::too_many_arguments)]
pub fn paint_userpic_suggestion_card(
    ui: &mut Ui,
    card_rect: Rect,
    msg: &HistoryMessage,
    peer_name: &str,
    photo_id: i64,
    access_hash: i64,
    file_reference: &[u8],
    is_video: bool,
    fallback_rgb: ([u8; 3], [u8; 3]),
    colors: &HistoryColors,
) -> Option<SuggestionCardAction> {
    if !card_rect.is_positive() {
        return None;
    }
    ui.painter()
        .rect_filled(card_rect, CornerRadius::same(16), colors.service_bg);

    let cx = card_rect.center().x;
    let avatar_dia = 96.0_f32.min(card_rect.width() * 0.45);
    let avatar_center = Pos2::new(cx, card_rect.top() + 18.0 + avatar_dia * 0.5);

    // 1. Circular userpic preview with subtle double-tone depth and crisp ring.
    let top_col = Color32::from_rgb(fallback_rgb.0[0], fallback_rgb.0[1], fallback_rgb.0[2]);
    let bot_col = Color32::from_rgb(fallback_rgb.1[0], fallback_rgb.1[1], fallback_rgb.1[2]);
    ui.painter()
        .circle_filled(avatar_center, avatar_dia * 0.5, top_col);
    ui.painter().circle_filled(
        avatar_center + Vec2::new(0.0, avatar_dia * 0.14),
        avatar_dia * 0.34,
        Color32::from_rgba_unmultiplied(bot_col.r(), bot_col.g(), bot_col.b(), 180),
    );
    // Clean geometric portrait silhouette inside the circular preview.
    let head_r = avatar_dia * 0.16;
    ui.painter().circle_filled(
        avatar_center - Vec2::new(0.0, avatar_dia * 0.10),
        head_r,
        Color32::from_rgba_unmultiplied(255, 255, 255, 225),
    );
    ui.painter().circle_filled(
        avatar_center + Vec2::new(0.0, avatar_dia * 0.24),
        avatar_dia * 0.24,
        Color32::from_rgba_unmultiplied(255, 255, 255, 210),
    );
    ui.painter().circle_stroke(
        avatar_center,
        avatar_dia * 0.5,
        Stroke::new(2.0, Color32::from_rgba_unmultiplied(255, 255, 255, 165)),
    );
    if is_video {
        let badge = Pos2::new(
            avatar_center.x + avatar_dia * 0.32,
            avatar_center.y + avatar_dia * 0.32,
        );
        ui.painter().circle_filled(badge, 11.0, colors.accent);
        tg_app_core::ui_icons::paint_triangle(
            ui.painter(),
            badge + Vec2::new(1.0, 0.0),
            10.0,
            tg_app_core::ui_icons::ArrowDir::Right,
            Color32::WHITE,
        );
    }

    // 2. Subtitle text (`lng_action_suggested_{photo,video}[_me]`).
    let target_name = if msg.outgoing {
        peer_name
    } else {
        &msg.sender_name
    };
    let subtitle = match (msg.outgoing, is_video) {
        (true, true) => tr::lng_action_suggested_video_me(target_name),
        (true, false) => tr::lng_action_suggested_photo_me(target_name),
        (false, true) => tr::lng_action_suggested_video(target_name),
        (false, false) => tr::lng_action_suggested_photo(target_name),
    };
    let inner_w = (card_rect.width() - 28.0).max(80.0);
    let sub_galley = ui.painter().layout(
        subtitle,
        FontId::new(12.5, family_regular()),
        colors.service_fg,
        inner_w,
    );
    let sub_y = avatar_center.y + avatar_dia * 0.5 + 12.0;
    ui.painter().galley(
        Pos2::new(cx - sub_galley.size().x * 0.5, sub_y),
        sub_galley,
        colors.service_fg,
    );

    // 3. Action button (`Set Photo` for incoming, `View Photo` for outgoing).
    let btn_label = if msg.outgoing {
        if is_video {
            tr::lng_action_suggested_video_button()
        } else {
            tr::lng_action_suggested_photo_button()
        }
    } else if is_video {
        tr::lng_profile_set_video_button()
    } else {
        tr::lng_profile_set_photo_button()
    };
    let btn_h = st::msg_service_gift_box_button_height.0 as f32;
    let btn_w = (card_rect.width() - 36.0).clamp(108.0, 180.0);
    let btn_rect = Rect::from_min_size(
        Pos2::new(cx - btn_w * 0.5, card_rect.bottom() - btn_h - 14.0),
        Vec2::new(btn_w, btn_h),
    );
    if paint_service_pill_button(
        ui,
        Id::unique(("tg_userpic_suggest_btn", msg.id.0)),
        btn_rect,
        &btn_label,
        &btn_label,
        colors,
    ) {
        if msg.outgoing {
            return Some(SuggestionCardAction::ViewUserpic { msg_id: msg.id });
        }
        return Some(SuggestionCardAction::ApplyUserpic {
            msg_id: msg.id,
            photo_id,
            access_hash,
            file_reference: file_reference.to_vec(),
            is_video,
        });
    }
    None
}

/// Shared service-card pill button (`msgServiceGiftBoxButtonHeight = 30px`) with hover, press,
/// focus ring, and accessible name.
fn paint_service_pill_button(
    ui: &mut Ui,
    id: Id,
    rect: Rect,
    label: &str,
    accessible_name: &str,
    colors: &HistoryColors,
) -> bool {
    let resp = ui.interact(rect, id, Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, accessible_name));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let cr = CornerRadius::same((rect.height() * 0.5).round() as u8);
    let fill = if resp.is_pointer_button_down_on() {
        Color32::from_rgba_unmultiplied(255, 255, 255, 72)
    } else if resp.hovered() {
        Color32::from_rgba_unmultiplied(255, 255, 255, 54)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 36)
    };
    ui.painter().rect_filled(rect, cr, fill);
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect,
            cr,
            Stroke::new(1.5, colors.service_fg),
            egui::StrokeKind::Inside,
        );
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::new(12.5, family_semibold()),
        colors.service_fg,
    );
    resp.clicked()
}

/// Paints a festive multi-tier vector birthday cake illustration (`cake.tgs` visual analog).
fn paint_birthday_cake_emblem(painter: &Painter, rect: Rect, accent: Color32) {
    let c = rect.center();
    let s = rect.width().min(rect.height());

    // Soft celebratory halo backdrop
    painter.circle_filled(
        c,
        s * 0.46,
        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 36),
    );

    // Cake plate
    let plate_y = c.y + s * 0.30;
    painter.line_segment(
        [
            Pos2::new(c.x - s * 0.34, plate_y),
            Pos2::new(c.x + s * 0.34, plate_y),
        ],
        Stroke::new(3.0, Color32::from_rgba_unmultiplied(255, 255, 255, 190)),
    );

    // Bottom cake layer
    let base_rect = Rect::from_min_max(
        Pos2::new(c.x - s * 0.28, c.y + s * 0.04),
        Pos2::new(c.x + s * 0.28, plate_y - 2.0),
    );
    painter.rect_filled(
        base_rect,
        CornerRadius::same(6),
        Color32::from_rgb(0xF7, 0xA8, 0xB8),
    );

    // Top frosting layer
    let frost_rect = Rect::from_min_max(
        Pos2::new(c.x - s * 0.26, c.y - s * 0.08),
        Pos2::new(c.x + s * 0.26, c.y + s * 0.08),
    );
    painter.rect_filled(
        frost_rect,
        CornerRadius::same(6),
        Color32::from_rgb(0xFF, 0xF5, 0xEB),
    );

    // Three candles + warm glowing flames
    for offset in [-0.13_f32, 0.0, 0.13] {
        let cx = c.x + s * offset;
        let candle = Rect::from_min_max(
            Pos2::new(cx - 2.5, c.y - s * 0.25),
            Pos2::new(cx + 2.5, c.y - s * 0.07),
        );
        painter.rect_filled(
            candle,
            CornerRadius::same(2),
            Color32::from_rgb(0x4E, 0xB6, 0xF5),
        );
        let flame = vec![
            Pos2::new(cx, c.y - s * 0.36),
            Pos2::new(cx + 4.0, c.y - s * 0.28),
            Pos2::new(cx, c.y - s * 0.25),
            Pos2::new(cx - 4.0, c.y - s * 0.28),
        ];
        painter.add(Shape::convex_polygon(
            flame,
            Color32::from_rgb(0xFF, 0xC8, 0x3D),
            Stroke::NONE,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn birthday_card_dimensions_differ_by_outgoing_cta() {
        let (w_in, h_in) = measure_birthday_suggestion_card(false);
        let (w_out, h_out) = measure_birthday_suggestion_card(true);
        assert_eq!(w_in, SUGGESTION_CARD_WIDTH);
        assert_eq!(w_out, SUGGESTION_CARD_WIDTH);
        assert!(h_in > h_out);
        assert_eq!(month_name(5), "May");
    }

    #[test]
    fn userpic_card_dimensions_are_stable() {
        let (w, h) = measure_userpic_suggestion_card();
        assert_eq!(w, SUGGESTION_CARD_WIDTH);
        assert!(h >= 200);
    }
}
