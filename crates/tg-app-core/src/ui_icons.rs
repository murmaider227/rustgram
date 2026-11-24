//! Shared crisp vector and `tg_style::st` icon painting helpers for `tg-app*` crates.
//!
//! Replaces raw Unicode emoji and symbol glyphs (`✓`, `✕`, `★`, `🔒`, `📍`, `🖼`, `🎬`, `📄`,
//! directional arrows, triangles, pause bars, checkboxes, etc.) that are missing from the bundled
//! Open Sans font (`crates/tg-ui/src/fonts.rs`) so UI controls never render missing-glyph gaps.

use egui::accesskit::Role;
use egui::{
    Color32, CornerRadius, CursorIcon, FontId, Painter, Pos2, Rect, Response, Sense, Shape, Stroke,
    StrokeKind, Ui, Vec2, WidgetInfo,
};
use tg_ui::fonts::family_regular;
use tg_ui::icons::paint_style_icon;
use tg_ui::tg_style::{DefaultPaletteResolver, Icon, st};

/// Cardinal direction for painted arrows and disclosure triangles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrowDir {
    /// Points left (`←` / `◀`).
    Left,
    /// Points right (`→` / `▶` / `▸`).
    Right,
    /// Points up (`↑` / `▲`).
    Up,
    /// Points down (`↓` / `▼` / `▾`).
    Down,
}

/// Paints a `tg_style::Icon` tinted with `color` centered inside `rect`.
///
/// Returns `true` if rasterization succeeded.
pub fn paint_style_tinted(painter: &Painter, rect: Rect, icon: Icon, color: Color32) -> bool {
    paint_style_icon(painter, rect, icon, &DefaultPaletteResolver, Some(color))
}

/// Paints a crisp 2-segment checkmark centered at `center` inside a `size × size` box.
pub fn paint_check(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let r = size * 0.42;
    let stroke = Stroke::new((size * 0.14).clamp(1.3, 2.4), color);
    let pts = vec![
        Pos2::new(center.x - r * 0.85, center.y + r * 0.05),
        Pos2::new(center.x - r * 0.15, center.y + r * 0.72),
        Pos2::new(center.x + r * 0.95, center.y - r * 0.68),
    ];
    painter.add(Shape::line(pts, stroke));
}

/// Paints `st::menu_icon_select` (tdesktop's circular checkmark), falling back to [`paint_check`].
pub fn paint_select_check(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let rect = Rect::from_center_size(center, Vec2::splat(size));
    if !paint_style_tinted(painter, rect, st::menu_icon_select, color) {
        paint_check(painter, center, size, color);
    }
}

/// Paints an outgoing double-checkmark read receipt anchored on its right edge at `right_center`.
pub fn paint_double_check(painter: &Painter, right_center: Pos2, size: f32, color: Color32) {
    let c2 = Pos2::new(right_center.x - size * 0.45, right_center.y);
    let c1 = Pos2::new(c2.x - size * 0.42, right_center.y);
    paint_check(painter, c1, size, color);
    paint_check(painter, c2, size, color);
}

/// Paints a crisp X close cross centered at `center`.
pub fn paint_close_cross(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let d = size * 0.34;
    let stroke = Stroke::new((size * 0.13).clamp(1.3, 2.2), color);
    painter.line_segment(
        [
            Pos2::new(center.x - d, center.y - d),
            Pos2::new(center.x + d, center.y + d),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(center.x - d, center.y + d),
            Pos2::new(center.x + d, center.y - d),
        ],
        stroke,
    );
}

/// Paints a filled 5-point star centered at `center` with outer radius `outer_r`.
pub fn paint_star(painter: &Painter, center: Pos2, outer_r: f32, color: Color32) {
    let inner_r = outer_r * 0.44;
    let mut pts = Vec::with_capacity(10);
    for i in 0..10 {
        let angle = -std::f32::consts::FRAC_PI_2 + (i as f32) * std::f32::consts::PI / 5.0;
        let r = if i % 2 == 0 { outer_r } else { inner_r };
        pts.push(Pos2::new(
            center.x + r * angle.cos(),
            center.y + r * angle.sin(),
        ));
    }
    // Draw as a triangle fan from center so concave star fills cleanly in egui.
    for i in 0..10 {
        let next = (i + 1) % 10;
        painter.add(Shape::convex_polygon(
            vec![center, pts[i], pts[next]],
            color,
            Stroke::NONE,
        ));
    }
}

/// Paints a crisp directional arrow (`←`, `→`, `↑`, `↓`) centered at `center`.
pub fn paint_arrow(painter: &Painter, center: Pos2, size: f32, dir: ArrowDir, color: Color32) {
    let r = size * 0.36;
    let head = r * 0.78;
    let stroke = Stroke::new((size * 0.13).clamp(1.3, 2.2), color);
    match dir {
        ArrowDir::Left => {
            let tip = Pos2::new(center.x - r, center.y);
            let tail = Pos2::new(center.x + r, center.y);
            painter.line_segment([tail, tip], stroke);
            painter.add(Shape::line(
                vec![
                    Pos2::new(tip.x + head, tip.y - head),
                    tip,
                    Pos2::new(tip.x + head, tip.y + head),
                ],
                stroke,
            ));
        }
        ArrowDir::Right => {
            let tip = Pos2::new(center.x + r, center.y);
            let tail = Pos2::new(center.x - r, center.y);
            painter.line_segment([tail, tip], stroke);
            painter.add(Shape::line(
                vec![
                    Pos2::new(tip.x - head, tip.y - head),
                    tip,
                    Pos2::new(tip.x - head, tip.y + head),
                ],
                stroke,
            ));
        }
        ArrowDir::Up => {
            let tip = Pos2::new(center.x, center.y - r);
            let tail = Pos2::new(center.x, center.y + r);
            painter.line_segment([tail, tip], stroke);
            painter.add(Shape::line(
                vec![
                    Pos2::new(tip.x - head, tip.y + head),
                    tip,
                    Pos2::new(tip.x + head, tip.y + head),
                ],
                stroke,
            ));
        }
        ArrowDir::Down => {
            let tip = Pos2::new(center.x, center.y + r);
            let tail = Pos2::new(center.x, center.y - r);
            painter.line_segment([tail, tip], stroke);
            painter.add(Shape::line(
                vec![
                    Pos2::new(tip.x - head, tip.y - head),
                    tip,
                    Pos2::new(tip.x + head, tip.y - head),
                ],
                stroke,
            ));
        }
    }
}

/// Paints a crisp filled triangle (`◀`, `▶`, `▲`, `▼`, `▸`, `▾`) centered at `center`.
pub fn paint_triangle(painter: &Painter, center: Pos2, size: f32, dir: ArrowDir, color: Color32) {
    let r = size * 0.36;
    let pts = match dir {
        ArrowDir::Left => vec![
            Pos2::new(center.x - r, center.y),
            Pos2::new(center.x + r * 0.75, center.y - r * 0.9),
            Pos2::new(center.x + r * 0.75, center.y + r * 0.9),
        ],
        ArrowDir::Right => vec![
            Pos2::new(center.x + r, center.y),
            Pos2::new(center.x - r * 0.75, center.y - r * 0.9),
            Pos2::new(center.x - r * 0.75, center.y + r * 0.9),
        ],
        ArrowDir::Up => vec![
            Pos2::new(center.x, center.y - r * 0.8),
            Pos2::new(center.x - r * 0.95, center.y + r * 0.7),
            Pos2::new(center.x + r * 0.95, center.y + r * 0.7),
        ],
        ArrowDir::Down => vec![
            Pos2::new(center.x, center.y + r * 0.8),
            Pos2::new(center.x - r * 0.95, center.y - r * 0.7),
            Pos2::new(center.x + r * 0.95, center.y - r * 0.7),
        ],
    };
    painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// Paints crisp vertical pause bars (`❚❚`) centered at `center`.
pub fn paint_pause(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let bar_w = (size * 0.22).max(2.0);
    let bar_h = size * 0.72;
    let gap = (size * 0.16).max(1.8);
    let left_bar = Rect::from_center_size(
        Pos2::new(center.x - (bar_w + gap) * 0.5, center.y),
        Vec2::new(bar_w, bar_h),
    );
    let right_bar = Rect::from_center_size(
        Pos2::new(center.x + (bar_w + gap) * 0.5, center.y),
        Vec2::new(bar_w, bar_h),
    );
    painter.rect_filled(left_bar, CornerRadius::same(1), color);
    painter.rect_filled(right_bar, CornerRadius::same(1), color);
}

/// Paints a crisp rounded checkbox glyph (`☑` / `☐`) centered at `center`.
pub fn paint_checkbox_glyph(
    painter: &Painter,
    center: Pos2,
    size: f32,
    checked: bool,
    color: Color32,
) {
    let box_rect = Rect::from_center_size(center, Vec2::splat(size * 0.78));
    let stroke = Stroke::new((size * 0.1).clamp(1.2, 1.8), color);
    painter.rect_stroke(box_rect, CornerRadius::same(2), stroke, StrokeKind::Inside);
    if checked {
        paint_check(painter, center, size * 0.62, color);
    }
}

/// Paints `st::menu_icon_lock`, falling back to a crisp vector padlock.
pub fn paint_lock(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let rect = Rect::from_center_size(center, Vec2::splat(size));
    if paint_style_tinted(painter, rect, st::menu_icon_lock, color) {
        return;
    }
    let body = Rect::from_center_size(
        Pos2::new(center.x, center.y + size * 0.14),
        Vec2::new(size * 0.62, size * 0.46),
    );
    let stroke = Stroke::new((size * 0.11).clamp(1.2, 2.0), color);
    painter.rect_stroke(body, CornerRadius::same(2), stroke, StrokeKind::Inside);
    painter.circle_stroke(
        Pos2::new(center.x, body.top() - size * 0.06),
        size * 0.2,
        stroke,
    );
}

/// Paints a horizontal flip icon (`st::media_menu_icon_flip` with vector fallback).
pub fn paint_flip_horizontal(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let rect = Rect::from_center_size(center, Vec2::splat(size));
    if paint_style_tinted(painter, rect, st::media_menu_icon_flip, color) {
        return;
    }
    paint_arrow(
        painter,
        Pos2::new(center.x, center.y - size * 0.18),
        size * 0.68,
        ArrowDir::Right,
        color,
    );
    paint_arrow(
        painter,
        Pos2::new(center.x, center.y + size * 0.18),
        size * 0.68,
        ArrowDir::Left,
        color,
    );
}

/// Paints a clockwise 90° rotation icon (`↻`).
pub fn paint_rotate_cw(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let r = size * 0.34;
    let stroke = Stroke::new((size * 0.12).clamp(1.3, 2.0), color);
    let steps = 14usize;
    let start = -std::f32::consts::FRAC_PI_2;
    let sweep = std::f32::consts::PI * 1.55;
    let mut pts = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let a = start + sweep * (i as f32 / steps as f32);
        pts.push(Pos2::new(center.x + r * a.cos(), center.y + r * a.sin()));
    }
    painter.add(Shape::line(pts, stroke));
    let tip = Pos2::new(center.x + r * 0.35, center.y - r);
    paint_triangle(painter, tip, size * 0.42, ArrowDir::Right, color);
}

/// Paints a crisp eye icon for channel post view counts (`👁`).
pub fn paint_eye(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let rx = size * 0.44;
    let ry = size * 0.25;
    let stroke = Stroke::new((size * 0.1).clamp(1.0, 1.6), color);
    let mut top = Vec::with_capacity(9);
    let mut bot = Vec::with_capacity(9);
    for i in 0..=8 {
        let t = (i as f32 / 8.0) * 2.0 - 1.0;
        let y_off = (1.0 - t * t) * ry;
        top.push(Pos2::new(center.x + t * rx, center.y - y_off));
        bot.push(Pos2::new(center.x + t * rx, center.y + y_off));
    }
    painter.add(Shape::line(top, stroke));
    painter.add(Shape::line(bot, stroke));
    painter.circle_filled(center, (size * 0.14).max(1.4), color);
}

/// Paints a crisp trophy cup icon for giveaway results badges (`🏆`).
pub fn paint_trophy(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let cup = Rect::from_center_size(
        Pos2::new(center.x, center.y - size * 0.12),
        Vec2::new(size * 0.56, size * 0.44),
    );
    painter.rect_filled(
        cup,
        CornerRadius {
            nw: 1,
            ne: 1,
            sw: 4,
            se: 4,
        },
        color,
    );
    let stroke = Stroke::new((size * 0.12).clamp(1.2, 2.0), color);
    painter.line_segment(
        [
            Pos2::new(center.x, cup.bottom()),
            Pos2::new(center.x, center.y + size * 0.34),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(center.x - size * 0.26, center.y + size * 0.34),
            Pos2::new(center.x + size * 0.26, center.y + size * 0.34),
        ],
        stroke,
    );
}

/// Paints a crisp diamond / gem polygon (`💎`, `♦`).
pub fn paint_diamond(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let rx = size * 0.38;
    let ry = size * 0.42;
    let pts = vec![
        Pos2::new(center.x, center.y - ry),
        Pos2::new(center.x + rx, center.y),
        Pos2::new(center.x, center.y + ry),
        Pos2::new(center.x - rx, center.y),
    ];
    painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// Paints a crisp lightning bolt (`⚡`).
pub fn paint_bolt(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let r = size * 0.42;
    let top = vec![
        Pos2::new(center.x + r * 0.15, center.y - r),
        Pos2::new(center.x - r * 0.55, center.y + r * 0.08),
        Pos2::new(center.x + r * 0.15, center.y + r * 0.08),
    ];
    let bot = vec![
        Pos2::new(center.x - r * 0.15, center.y - r * 0.08),
        Pos2::new(center.x + r * 0.55, center.y - r * 0.08),
        Pos2::new(center.x - r * 0.15, center.y + r),
    ];
    painter.add(Shape::convex_polygon(top, color, Stroke::NONE));
    painter.add(Shape::convex_polygon(bot, color, Stroke::NONE));
}

/// Paints a crisp 5-petal bloom / flower (`✿`).
pub fn paint_bloom(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let pr = size * 0.18;
    let orbit = size * 0.22;
    for i in 0..5 {
        let a = -std::f32::consts::FRAC_PI_2 + (i as f32) * std::f32::consts::TAU / 5.0;
        painter.circle_filled(
            Pos2::new(center.x + orbit * a.cos(), center.y + orbit * a.sin()),
            pr,
            color,
        );
    }
    painter.circle_filled(center, pr * 0.75, color);
}

/// Paints a crisp cloud silhouette (`☁`).
pub fn paint_cloud(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let r = size * 0.2;
    painter.circle_filled(
        Pos2::new(center.x - r * 0.85, center.y + r * 0.25),
        r * 0.85,
        color,
    );
    painter.circle_filled(Pos2::new(center.x, center.y - r * 0.25), r * 1.15, color);
    painter.circle_filled(
        Pos2::new(center.x + r * 0.9, center.y + r * 0.25),
        r * 0.8,
        color,
    );
    let base = Rect::from_min_max(
        Pos2::new(center.x - r * 0.9, center.y + r * 0.2),
        Pos2::new(center.x + r * 0.95, center.y + r * 1.05),
    );
    painter.rect_filled(base, CornerRadius::same(2), color);
}

/// Paints a crisp "none / empty" slashed circle (`∅`).
pub fn paint_none_slash(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let r = size * 0.34;
    let stroke = Stroke::new((size * 0.11).clamp(1.2, 2.0), color);
    painter.circle_stroke(center, r, stroke);
    let d = r * 0.85;
    painter.line_segment(
        [
            Pos2::new(center.x - d, center.y + d),
            Pos2::new(center.x + d, center.y - d),
        ],
        stroke,
    );
}

/// Paints a crisp 4-point sparkle (`✦`, `✨`).
pub fn paint_sparkle(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let outer = size * 0.44;
    let inner = outer * 0.28;
    let mut pts = Vec::with_capacity(8);
    for i in 0..8 {
        let a = (i as f32) * std::f32::consts::FRAC_PI_4;
        let r = if i % 2 == 0 { outer } else { inner };
        pts.push(Pos2::new(center.x + r * a.cos(), center.y + r * a.sin()));
    }
    for i in 0..8 {
        let next = (i + 1) % 8;
        painter.add(Shape::convex_polygon(
            vec![center, pts[i], pts[next]],
            color,
            Stroke::NONE,
        ));
    }
}

/// Paints a crisp speech bubble outline (`💬`).
pub fn paint_speech_bubble(painter: &Painter, center: Pos2, size: f32, color: Color32) {
    let rect = Rect::from_center_size(
        Pos2::new(center.x, center.y - size * 0.06),
        Vec2::new(size * 0.72, size * 0.52),
    );
    let stroke = Stroke::new((size * 0.11).clamp(1.2, 1.8), color);
    painter.rect_stroke(rect, CornerRadius::same(3), stroke, StrokeKind::Inside);
    painter.line_segment(
        [
            Pos2::new(rect.left() + size * 0.18, rect.bottom()),
            Pos2::new(rect.left() + size * 0.08, rect.bottom() + size * 0.18),
        ],
        stroke,
    );
}

/// Paints a standard Telegram-style "← Back" (or custom label) button with a crisp painted left arrow.
pub fn back_button(ui: &mut Ui, label: &str) -> Response {
    let font = FontId::new(13.0, family_regular());
    let fg = ui.visuals().text_color();
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, fg);
    let icon_w = 13.0;
    let gap = 5.0;
    let pad_x = 8.0;
    let h = (galley.size().y + 8.0).max(24.0);
    let w = pad_x * 2.0 + icon_w + gap + galley.size().x;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    let visuals = ui.style().interact(&resp);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter()
        .rect_filled(rect, visuals.corner_radius, visuals.bg_fill);
    if visuals.bg_stroke.width > 0.0 {
        ui.painter().rect_stroke(
            rect,
            visuals.corner_radius,
            visuals.bg_stroke,
            StrokeKind::Inside,
        );
    }
    let arrow_c = Pos2::new(rect.left() + pad_x + icon_w * 0.5, rect.center().y);
    paint_arrow(
        ui.painter(),
        arrow_c,
        icon_w,
        ArrowDir::Left,
        visuals.text_color(),
    );
    let text_pos = Pos2::new(
        rect.left() + pad_x + icon_w + gap,
        rect.center().y - galley.size().y * 0.5,
    );
    ui.painter().galley(text_pos, galley, visuals.text_color());
    resp
}

/// Paints a compact icon-only clickable button (replacement for `ui.small_button("✕")` / `"◀"` / etc.).
pub fn small_icon_button(
    ui: &mut Ui,
    accessible_label: &str,
    enabled: bool,
    paint_fn: impl FnOnce(&Painter, Pos2, f32, Color32),
) -> Response {
    let size = Vec2::new(24.0, 20.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, accessible_label));
    let visuals = if enabled {
        ui.style().interact(&resp)
    } else {
        &ui.style().visuals.widgets.noninteractive
    };
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter()
        .rect_filled(rect, visuals.corner_radius, visuals.bg_fill);
    if visuals.bg_stroke.width > 0.0 {
        ui.painter().rect_stroke(
            rect,
            visuals.corner_radius,
            visuals.bg_stroke,
            StrokeKind::Inside,
        );
    }
    let icon_color = if enabled {
        visuals.text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    paint_fn(ui.painter(), rect.center(), 12.0, icon_color);
    resp
}

/// Paints a compact button with a leading painted icon and a text label (e.g. `↻ Rotate 90°`).
pub fn icon_label_button(
    ui: &mut Ui,
    label: &str,
    small: bool,
    paint_fn: impl FnOnce(&Painter, Pos2, f32, Color32),
) -> Response {
    let font_size = if small { 11.5 } else { 13.0 };
    let font = FontId::new(font_size, family_regular());
    let fg = ui.visuals().text_color();
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, fg);
    let icon_w = if small { 12.0 } else { 14.0 };
    let gap = 5.0;
    let pad_x = if small { 6.0 } else { 8.0 };
    let h = if small {
        (galley.size().y + 5.0).max(20.0)
    } else {
        (galley.size().y + 8.0).max(24.0)
    };
    let w = pad_x * 2.0 + icon_w + gap + galley.size().x;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    let visuals = ui.style().interact(&resp);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter()
        .rect_filled(rect, visuals.corner_radius, visuals.bg_fill);
    if visuals.bg_stroke.width > 0.0 {
        ui.painter().rect_stroke(
            rect,
            visuals.corner_radius,
            visuals.bg_stroke,
            StrokeKind::Inside,
        );
    }
    let icon_c = Pos2::new(rect.left() + pad_x + icon_w * 0.5, rect.center().y);
    paint_fn(ui.painter(), icon_c, icon_w, visuals.text_color());
    let text_pos = Pos2::new(
        rect.left() + pad_x + icon_w + gap,
        rect.center().y - galley.size().y * 0.5,
    );
    ui.painter().galley(text_pos, galley, visuals.text_color());
    resp
}

/// Paints a clickable popup menu row with a leading `tg_style::Icon` and plain text label.
pub fn style_icon_menu_row(
    ui: &mut Ui,
    icon: Icon,
    icon_color: Color32,
    label: &str,
    text_color: Color32,
    hover_bg: Color32,
) -> bool {
    let font = FontId::new(13.0, family_regular());
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, text_color);
    let w = ui.available_width().max(galley.size().x + 36.0);
    let h = 28.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), hover_bg);
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(6),
            Stroke::new(1.5, icon_color),
            StrokeKind::Inside,
        );
    }
    let icon_rect = Rect::from_center_size(
        Pos2::new(rect.left() + 14.0, rect.center().y),
        Vec2::splat(16.0),
    );
    paint_style_tinted(ui.painter(), icon_rect, icon, icon_color);
    let text_pos = Pos2::new(rect.left() + 30.0, rect.center().y - galley.size().y * 0.5);
    ui.painter().galley(text_pos, galley, text_color);
    resp.clicked()
}

/// Paints an inline horizontal icon + text label without adding vertical gaps.
pub fn inline_icon_label(
    ui: &mut Ui,
    icon_size: f32,
    paint_fn: impl FnOnce(&Painter, Pos2, f32),
    text: egui::RichText,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(icon_size), Sense::hover());
        paint_fn(ui.painter(), icon_rect.center(), icon_size);
        ui.label(text);
    });
}

/// Callback type for painting an optional leading icon inside [`selectable_icon_chip`].
pub type ChipIconPainter<'a> = &'a dyn Fn(&Painter, Pos2, f32, Color32);

/// Paints a small selectable chip with an optional leading icon and text label.
#[allow(clippy::too_many_arguments)]
pub fn selectable_icon_chip(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash,
    selected: bool,
    label: &str,
    font_size: f32,
    text_color: Color32,
    selected_bg: Color32,
    hover_bg: Color32,
    accent: Color32,
    paint_icon: Option<ChipIconPainter<'_>>,
) -> Response {
    let font = FontId::new(font_size, family_regular());
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, text_color);
    let has_icon = paint_icon.is_some();
    let icon_w = if has_icon { 12.0 } else { 0.0 };
    let gap = if has_icon { 4.0 } else { 0.0 };
    let pad_x = 6.0;
    let pad_y = 2.5;
    let w = pad_x * 2.0 + icon_w + gap + galley.size().x;
    let h = (galley.size().y + pad_y * 2.0).max(20.0);
    let _ = id_salt;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), selected_bg);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), hover_bg);
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.5, accent),
            StrokeKind::Inside,
        );
    }
    if let Some(paint_fn) = paint_icon {
        let icon_c = Pos2::new(rect.left() + pad_x + icon_w * 0.5, rect.center().y);
        paint_fn(ui.painter(), icon_c, icon_w, accent);
    }
    let text_pos = Pos2::new(
        rect.left() + pad_x + icon_w + gap,
        rect.center().y - galley.size().y * 0.5,
    );
    ui.painter().galley(text_pos, galley, text_color);
    resp
}
