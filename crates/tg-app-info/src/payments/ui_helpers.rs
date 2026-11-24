//! Shared Telegram Desktop-styled egui painting primitives and formatters for payments,
//! Stars, Star Gifts, Premium, Boosts, and Passport UI screens.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Response, Sense, Stroke, Ui,
    Vec2,
};
use tg_data::TimeId;
use tg_payments::card::CardBrand;
use tg_payments::star_gifts::RgbColor;

use tg_app_core::stand_in::theme::Palette;

/// Semantic accent colors matching Telegram Desktop's payments, credits, and premium styles.
pub const STAR_GOLD: Color32 = Color32::from_rgb(0xF5, 0xA6, 0x23);
/// Warm amber tint for Star highlights.
pub const STAR_AMBER: Color32 = Color32::from_rgb(0xFF, 0xB8, 0x33);
/// Soft light gold surface for Stars badges.
pub const STAR_SURFACE: Color32 = Color32::from_rgb(0xFF, 0xF4, 0xDC);
/// Telegram Premium gradient start (indigo-violet).
pub const PREMIUM_INDIGO: Color32 = Color32::from_rgb(0x6B, 0x67, 0xF7);
/// Telegram Premium gradient mid (purple-magenta).
pub const PREMIUM_PURPLE: Color32 = Color32::from_rgb(0x9D, 0x5B, 0xE5);
/// Telegram Premium gradient end (rose pink).
pub const PREMIUM_PINK: Color32 = Color32::from_rgb(0xE0, 0x5B, 0x9E);
/// Positive green accent (incoming Stars, ready Passport scope, valid card).
pub const SUCCESS_GREEN: Color32 = Color32::from_rgb(0x2E, 0xA6, 0x52);
/// Subtle light green tint for success chips.
pub const SUCCESS_SURFACE: Color32 = Color32::from_rgb(0xE8, 0xF8, 0xEE);

/// Converts `tg_payments::star_gifts::RgbColor` into an `egui::Color32`.
#[must_use]
pub const fn rgb_to_color32(c: RgbColor) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

/// Applies an alpha multiplier `0.0..=1.0` to `color`.
#[must_use]
pub fn with_alpha(color: Color32, alpha: f32) -> Color32 {
    let a = ((f32::from(color.a()) * alpha.clamp(0.0, 1.0)).round()) as u8;
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

/// Formats a Unix timestamp into a deterministic readable UTC date (`DD Mon YYYY, HH:MM`).
#[must_use]
pub fn format_unix_datetime(ts: TimeId) -> String {
    if ts <= 0 {
        return "—".to_owned();
    }
    let secs = i64::from(ts);
    let days = secs.div_euclid(86_400);
    let day_secs = secs.rem_euclid(86_400);
    let hour = day_secs / 3_600;
    let minute = (day_secs % 3_600) / 60;
    let (year, month, day) = civil_from_days(days);
    let month_name = match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        _ => "Dec",
    };
    format!("{day:02} {month_name} {year}, {hour:02}:{minute:02}")
}

/// Formats a Unix timestamp as `DD Mon YYYY`.
#[must_use]
pub fn format_unix_date(ts: TimeId) -> String {
    if ts <= 0 {
        return "—".to_owned();
    }
    let days = i64::from(ts).div_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let month_name = match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        _ => "Dec",
    };
    format!("{day:02} {month_name} {year}")
}

/// Howard Hinnant's civil-from-days algorithm (UTC, zero external dependencies).
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

/// Draws a Telegram Desktop-style horizontal section divider strip.
pub fn section_divider(ui: &mut Ui, palette: &Palette) {
    ui.add_space(6.0);
    let width = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 8.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, palette.window_bg_over);
    ui.painter().line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.0, palette.divider),
    );
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, palette.divider),
    );
    ui.add_space(6.0);
}

/// Draws a thin 1px hairline separator.
pub fn thin_divider(ui: &mut Ui, palette: &Palette) {
    let width = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
    ui.painter().line_segment(
        [rect.left_center(), rect.right_center()],
        Stroke::new(1.0, palette.divider),
    );
}

/// Draws a tdesktop subsection title in active accent blue.
pub fn subsection_title(ui: &mut Ui, text: &str, palette: &Palette) {
    ui.label(
        egui::RichText::new(text)
            .size(13.0)
            .strong()
            .color(palette.window_active_text_fg),
    );
}

/// Draws a full-width or sized rounded primary button (`st::paymentsPanelSubmit` / `defaultActiveButton`).
pub fn primary_button(
    ui: &mut Ui,
    label: &str,
    fill: Color32,
    fg: Color32,
    height: f32,
) -> Response {
    let width = ui.available_width().max(120.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.to_owned()));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let bg = if resp.is_pointer_button_down_on() {
        with_alpha(fill, 0.85)
    } else if resp.hovered() {
        with_alpha(fill, 0.92)
    } else {
        fill
    };
    ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(1.5),
            CornerRadius::same(9),
            Stroke::new(2.0, with_alpha(fill, 0.55)),
            egui::StrokeKind::Outside,
        );
    }
    let max_text_w = (rect.width() - 16.0).max(40.0);
    let galley =
        ui.fonts_mut(|f| f.layout(label.to_owned(), FontId::proportional(13.5), fg, max_text_w));
    ui.painter().galley(
        Pos2::new(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        fg,
    );
    resp
}

/// Draws a compact pill button with explicit width or auto-measured label width.
pub fn pill_button(
    ui: &mut Ui,
    label: &str,
    bg: Color32,
    fg: Color32,
    border: Option<Color32>,
) -> Response {
    let galley =
        ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), FontId::proportional(12.5), fg));
    let size = Vec2::new(galley.size().x + 22.0, 30.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.to_owned()));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let fill = if resp.is_pointer_button_down_on() {
        with_alpha(bg, 0.82)
    } else if resp.hovered() {
        with_alpha(bg, 0.90)
    } else {
        bg
    };
    ui.painter().rect_filled(rect, CornerRadius::same(15), fill);
    if let Some(stroke_col) = border {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(15),
            Stroke::new(1.0, stroke_col),
            egui::StrokeKind::Inside,
        );
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(1.5),
            CornerRadius::same(16),
            Stroke::new(1.5, fg),
            egui::StrokeKind::Outside,
        );
    }
    ui.painter().galley(
        Pos2::new(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        fg,
    );
    resp
}

/// Paints tdesktop's 24px radial progress spinner (`paymentsLoading` in `payments.style:140`).
///
/// Honors reduced-motion / deterministic test mode (`ui.style().animation_time == 0.0`) by
/// painting a clean static arc without requesting continuous repaints.
pub fn paint_radial_spinner(ui: &mut Ui, center: Pos2, radius: f32, color: Color32) {
    let animate = ui.style().animation_time > 0.0;
    let time = if animate {
        ui.ctx().request_repaint();
        ui.input(|i| i.time) as f32
    } else {
        0.0
    };
    // Background track
    ui.painter()
        .circle_stroke(center, radius, Stroke::new(3.0, with_alpha(color, 0.2)));
    // Rotating 240-degree arc (`paymentsLoading` thickness = 4px scaled cleanly to 3px)
    let start_angle = time * 4.2 - std::f32::consts::FRAC_PI_2;
    let sweep = std::f32::consts::PI * 1.25;
    let segments = 20usize;
    let mut pts = Vec::with_capacity(segments + 1);
    for i in 0..=segments {
        let a = start_angle + sweep * (i as f32 / segments as f32);
        pts.push(Pos2::new(
            center.x + radius * a.cos(),
            center.y + radius * a.sin(),
        ));
    }
    ui.painter()
        .add(egui::Shape::line(pts, Stroke::new(3.0, color)));
}

/// Computes an interruptible ease-out-cubic reveal progress (`0.0..=1.0`, 180ms budget)
/// for modal/panel entrances. Snaps immediately to `1.0` when `animation_time == 0.0`
/// (reduced motion or deterministic snapshot tests).
pub fn reveal_progress(ui: &Ui, id: egui::Id, visible: bool) -> f32 {
    if ui.style().animation_time <= 0.0 {
        return if visible { 1.0 } else { 0.0 };
    }
    let linear = ui.ctx().animate_bool_with_time(id, visible, 0.18);
    let inv = 1.0 - linear.clamp(0.0, 1.0);
    1.0 - inv * inv * inv
}

/// Draws a Telegram-style clickable settings row with left title, right value label, and hover highlight.
pub fn settings_value_row(
    ui: &mut Ui,
    title: &str,
    value: &str,
    icon_text: &str,
    icon_bg: Color32,
    palette: &Palette,
    interactive: bool,
) -> Response {
    let width = ui.available_width().max(120.0);
    let sense = if interactive {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 42.0), sense);
    if interactive && resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), palette.window_bg_over);
    }

    // Left icon badge
    let icon_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 4.0, rect.center().y - 13.0),
        Vec2::splat(26.0),
    );
    ui.painter()
        .rect_filled(icon_rect, CornerRadius::same(6), with_alpha(icon_bg, 0.16));
    ui.painter().text(
        icon_rect.center(),
        Align2::CENTER_CENTER,
        icon_text,
        FontId::proportional(11.5),
        icon_bg,
    );

    // Title
    ui.painter().text(
        Pos2::new(icon_rect.right() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(13.5),
        palette.window_fg,
    );

    // Right value + chevron
    let display_val = if value.is_empty() { "Add" } else { value };
    let val_col = if value.is_empty() {
        palette.window_active_text_fg
    } else {
        palette.window_sub_text_fg
    };
    let right_x = if interactive {
        rect.right() - 18.0
    } else {
        rect.right() - 6.0
    };
    ui.painter().text(
        Pos2::new(right_x, rect.center().y),
        Align2::RIGHT_CENTER,
        display_val,
        FontId::proportional(12.5),
        val_col,
    );
    if interactive {
        ui.painter().text(
            Pos2::new(rect.right() - 6.0, rect.center().y),
            Align2::RIGHT_CENTER,
            ">",
            FontId::proportional(13.0),
            palette.window_sub_text_fg,
        );
    }
    resp
}

/// Paints a crisp 5-pointed star emblem centered at `center` with outer `radius`.
pub fn paint_star_emblem(painter: &egui::Painter, center: Pos2, radius: f32, fill: Color32) {
    let mut pts = Vec::with_capacity(10);
    let inner_r = radius * 0.44;
    for i in 0..10 {
        let angle = -std::f32::consts::FRAC_PI_2 + (i as f32) * std::f32::consts::PI / 5.0;
        let r = if i % 2 == 0 { radius } else { inner_r };
        pts.push(Pos2::new(
            center.x + r * angle.cos(),
            center.y + r * angle.sin(),
        ));
    }
    // Paint as 10 radial triangles around center so the concave star points render sharply.
    for i in 0..10 {
        let next = (i + 1) % 10;
        painter.add(egui::Shape::convex_polygon(
            vec![center, pts[i], pts[next]],
            fill,
            Stroke::NONE,
        ));
    }
    // Inner highlight dot
    painter.circle_filled(
        Pos2::new(center.x - radius * 0.14, center.y - radius * 0.12),
        radius * 0.12,
        Color32::from_rgba_unmultiplied(255, 255, 255, 165),
    );
}

/// Paints a vector card brand icon badge (Visa, MasterCard, Amex, Discover, JCB, Diners, UnionPay, Unknown).
pub fn paint_card_brand_icon(
    painter: &egui::Painter,
    rect: Rect,
    brand: CardBrand,
    palette: &Palette,
) {
    let (bg, fg, label) = match brand {
        CardBrand::Visa => (Color32::from_rgb(0x1A, 0x1F, 0x71), Color32::WHITE, "VISA"),
        CardBrand::MasterCard => (Color32::from_rgb(0x22, 0x27, 0x2E), Color32::WHITE, "MC"),
        CardBrand::Amex => (Color32::from_rgb(0x00, 0x6F, 0xCF), Color32::WHITE, "AMEX"),
        CardBrand::Discover => (Color32::from_rgb(0xE8, 0x6F, 0x1C), Color32::WHITE, "DISC"),
        CardBrand::Jcb => (Color32::from_rgb(0x0E, 0x7C, 0x3A), Color32::WHITE, "JCB"),
        CardBrand::DinersClub => (Color32::from_rgb(0x00, 0x79, 0xBE), Color32::WHITE, "DC"),
        CardBrand::UnionPay => (Color32::from_rgb(0xC6, 0x28, 0x28), Color32::WHITE, "UP"),
        CardBrand::Unknown => (palette.window_bg_over, palette.window_sub_text_fg, "CARD"),
    };
    painter.rect_filled(rect, CornerRadius::same(5), bg);
    if brand == CardBrand::MasterCard {
        let r = rect.height() * 0.28;
        let cy = rect.center().y;
        painter.circle_filled(
            Pos2::new(rect.center().x - r * 0.55, cy),
            r,
            Color32::from_rgb(0xEB, 0x00, 0x1B),
        );
        painter.circle_filled(
            Pos2::new(rect.center().x + r * 0.55, cy),
            r,
            Color32::from_rgba_unmultiplied(0xF7, 0x9E, 0x1B, 225),
        );
    } else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(10.0),
            fg,
        );
    }
    if brand == CardBrand::Unknown {
        painter.rect_stroke(
            rect,
            CornerRadius::same(5),
            Stroke::new(1.0, palette.divider),
            egui::StrokeKind::Inside,
        );
    }
}

/// Paints a stylized vector gift artwork inside `rect` for Star Gifts cards and detail headers.
pub fn paint_gift_artwork(painter: &egui::Painter, rect: Rect, kind: &str, accent: Color32) {
    let center = rect.center();
    let r = rect.width().min(rect.height()) * 0.34;
    let light = Color32::from_rgba_unmultiplied(255, 255, 255, 235);
    let soft = Color32::from_rgba_unmultiplied(255, 255, 255, 90);

    // Subtle halo ring behind artwork
    painter.circle_filled(center, r * 1.15, soft);

    match kind {
        "gem" => {
            let pts = vec![
                Pos2::new(center.x, center.y - r),
                Pos2::new(center.x + r * 0.85, center.y - r * 0.2),
                Pos2::new(center.x, center.y + r),
                Pos2::new(center.x - r * 0.85, center.y - r * 0.2),
            ];
            painter.add(egui::Shape::convex_polygon(
                pts,
                light,
                Stroke::new(1.5, accent),
            ));
            painter.line_segment(
                [
                    Pos2::new(center.x - r * 0.85, center.y - r * 0.2),
                    Pos2::new(center.x + r * 0.85, center.y - r * 0.2),
                ],
                Stroke::new(1.2, accent),
            );
        }
        "crown" => {
            let base_y = center.y + r * 0.55;
            let top_y = center.y - r * 0.55;
            let box_rect = Rect::from_min_max(
                Pos2::new(center.x - r * 0.8, center.y + r * 0.2),
                Pos2::new(center.x + r * 0.8, base_y),
            );
            painter.rect_filled(box_rect, CornerRadius::same(3), light);
            let left_peak = vec![
                Pos2::new(center.x - r * 0.8, center.y + r * 0.25),
                Pos2::new(center.x - r * 0.85, top_y),
                Pos2::new(center.x - r * 0.2, center.y + r * 0.25),
            ];
            let mid_peak = vec![
                Pos2::new(center.x - r * 0.35, center.y + r * 0.25),
                Pos2::new(center.x, top_y - r * 0.2),
                Pos2::new(center.x + r * 0.35, center.y + r * 0.25),
            ];
            let right_peak = vec![
                Pos2::new(center.x + r * 0.2, center.y + r * 0.25),
                Pos2::new(center.x + r * 0.85, top_y),
                Pos2::new(center.x + r * 0.8, center.y + r * 0.25),
            ];
            painter.add(egui::Shape::convex_polygon(left_peak, light, Stroke::NONE));
            painter.add(egui::Shape::convex_polygon(mid_peak, light, Stroke::NONE));
            painter.add(egui::Shape::convex_polygon(right_peak, light, Stroke::NONE));
            painter.circle_filled(Pos2::new(center.x, top_y - r * 0.2), 3.0, STAR_GOLD);
        }
        "cake" => {
            let base = Rect::from_min_max(
                Pos2::new(center.x - r * 0.78, center.y - r * 0.05),
                Pos2::new(center.x + r * 0.78, center.y + r * 0.75),
            );
            painter.rect_filled(base, CornerRadius::same(5), light);
            let icing = Rect::from_min_max(
                Pos2::new(center.x - r * 0.78, center.y - r * 0.05),
                Pos2::new(center.x + r * 0.78, center.y + r * 0.2),
            );
            painter.rect_filled(icing, CornerRadius::same(4), with_alpha(accent, 0.45));
            let candle = Rect::from_min_max(
                Pos2::new(center.x - 3.0, center.y - r * 0.55),
                Pos2::new(center.x + 3.0, center.y - r * 0.05),
            );
            painter.rect_filled(candle, CornerRadius::same(1), light);
            painter.circle_filled(Pos2::new(center.x, center.y - r * 0.72), 4.5, STAR_GOLD);
        }
        "rocket" => {
            let body = vec![
                Pos2::new(center.x, center.y - r),
                Pos2::new(center.x + r * 0.45, center.y - r * 0.1),
                Pos2::new(center.x + r * 0.35, center.y + r * 0.65),
                Pos2::new(center.x - r * 0.35, center.y + r * 0.65),
                Pos2::new(center.x - r * 0.45, center.y - r * 0.1),
            ];
            painter.add(egui::Shape::convex_polygon(body, light, Stroke::NONE));
            painter.circle_filled(Pos2::new(center.x, center.y - r * 0.1), r * 0.22, accent);
            let flame = vec![
                Pos2::new(center.x - r * 0.22, center.y + r * 0.65),
                Pos2::new(center.x + r * 0.22, center.y + r * 0.65),
                Pos2::new(center.x, center.y + r * 1.05),
            ];
            painter.add(egui::Shape::convex_polygon(flame, STAR_GOLD, Stroke::NONE));
        }
        "trophy" => {
            let cup = Rect::from_min_max(
                Pos2::new(center.x - r * 0.65, center.y - r * 0.7),
                Pos2::new(center.x + r * 0.65, center.y + r * 0.15),
            );
            painter.rect_filled(cup, CornerRadius::same(6), light);
            let stem = Rect::from_min_max(
                Pos2::new(center.x - r * 0.15, center.y + r * 0.15),
                Pos2::new(center.x + r * 0.15, center.y + r * 0.55),
            );
            painter.rect_filled(stem, CornerRadius::ZERO, light);
            let pedestal = Rect::from_min_max(
                Pos2::new(center.x - r * 0.55, center.y + r * 0.55),
                Pos2::new(center.x + r * 0.55, center.y + r * 0.8),
            );
            painter.rect_filled(pedestal, CornerRadius::same(3), light);
            paint_star_emblem(painter, cup.center(), r * 0.28, STAR_GOLD);
        }
        "rose" => {
            painter.line_segment(
                [
                    Pos2::new(center.x, center.y + r * 0.1),
                    Pos2::new(center.x, center.y + r * 0.85),
                ],
                Stroke::new(3.0, light),
            );
            painter.circle_filled(Pos2::new(center.x, center.y - r * 0.2), r * 0.52, light);
            painter.circle_filled(
                Pos2::new(center.x - r * 0.18, center.y - r * 0.25),
                r * 0.28,
                with_alpha(accent, 0.35),
            );
            painter.circle_filled(
                Pos2::new(center.x + r * 0.18, center.y - r * 0.18),
                r * 0.24,
                with_alpha(accent, 0.25),
            );
        }
        _ => {
            paint_star_emblem(painter, center, r * 0.9, light);
        }
    }
}

/// Paints a rounded avatar circle with centered initials.
pub fn paint_avatar(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    bg: Color32,
    initials: &str,
) {
    painter.circle_filled(center, radius, bg);
    painter.text(
        center,
        Align2::CENTER_CENTER,
        initials,
        FontId::proportional((radius * 0.78).max(10.0)),
        Color32::WHITE,
    );
}
