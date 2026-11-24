//! Buttons and crisp vector UI icons.
//!
//! Port of `Telegram/lib_ui/ui/widgets/buttons.h` and
//! `Telegram/lib_ui/ui/widgets/buttons.cpp` (`FlatButton`, `RoundButton`,
//! `IconButton`, `LinkButton`, `CrossButton`).

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, Response, Role, Sense, Shape, Stroke,
    Ui, Vec2, WidgetInfo,
};

use crate::anim::{Transition, color_lerp};
use crate::effects::{RippleMask, update_and_paint_ripple};
use crate::icons::{paint_style_icon, style_icon_for_kind};
use crate::painter::{HoverAnimState, RoundRect};
use crate::style::{
    FlatButtonStyle, IconButtonStyle, LinkButtonStyle, RoundButtonStyle, constants,
};
use crate::theme::Colors;

/// Built-in crisp vector glyphs for `IconButton`, menus, and title bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Close,
    Back,
    Search,
    MenuDots,
    Hamburger,
    Settings,
    Send,
    Attach,
    Check,
    Plus,
    Trash,
    Edit,
    Info,
    Star,
    Bookmark,
    ChevronRight,
    Lock,
    Minimize,
    Maximize,
    Restore,
}

impl IconKind {
    /// Default accessible label for screen readers when no explicit tooltip/name is set.
    #[must_use]
    pub const fn accessible_name(self) -> &'static str {
        match self {
            Self::Close => "Close",
            Self::Back => "Back",
            Self::Search => "Search",
            Self::MenuDots => "More options",
            Self::Hamburger => "Main menu",
            Self::Settings => "Settings",
            Self::Send => "Send",
            Self::Attach => "Attach",
            Self::Check => "Confirm",
            Self::Plus => "Add",
            Self::Trash => "Delete",
            Self::Edit => "Edit",
            Self::Info => "Info",
            Self::Star => "Star",
            Self::Bookmark => "Bookmark",
            Self::ChevronRight => "Next",
            Self::Lock => "Lock",
            Self::Minimize => "Minimize",
            Self::Maximize => "Maximize",
            Self::Restore => "Restore",
        }
    }
}

/// Paint an [`IconKind`] centered inside `rect` with `color`.
///
/// Uses cached `tg_style::Icon` rasterization when a matching tdesktop asset exists,
/// falling back to crisp vector paths for icons without a direct single-layer asset.
pub fn paint_icon(painter: &egui::Painter, rect: Rect, icon: IconKind, color: Color32) {
    if let Some(tg_icon) = style_icon_for_kind(icon)
        && paint_style_icon(
            painter,
            rect,
            tg_icon,
            &tg_style::DefaultPaletteResolver,
            Some(color),
        )
    {
        return;
    }

    let c = rect.center();
    let s = rect.width().min(rect.height());
    let r = s * 0.5;
    let stroke = Stroke::new((s * 0.11).clamp(1.3, 2.2), color);

    match icon {
        IconKind::Close => {
            let d = r * 0.62;
            painter.line_segment([c - Vec2::new(d, d), c + Vec2::new(d, d)], stroke);
            painter.line_segment([c + Vec2::new(-d, d), c + Vec2::new(d, -d)], stroke);
        }
        IconKind::Back => {
            let d = r * 0.58;
            painter.line_segment([c + Vec2::new(d, 0.0), c - Vec2::new(d, 0.0)], stroke);
            painter.add(Shape::line(
                vec![
                    c + Vec2::new(0.0, -d * 0.85),
                    c - Vec2::new(d, 0.0),
                    c + Vec2::new(0.0, d * 0.85),
                ],
                stroke,
            ));
        }
        IconKind::Search => {
            let cr = r * 0.45;
            let cc = c - Vec2::splat(r * 0.16);
            painter.circle_stroke(cc, cr, stroke);
            let h0 = cc + Vec2::splat(cr * 0.72);
            let h1 = c + Vec2::splat(r * 0.68);
            painter.line_segment([h0, h1], stroke);
        }
        IconKind::MenuDots => {
            let dot_r = (s * 0.095).max(1.4);
            let step = r * 0.52;
            painter.circle_filled(c - Vec2::new(0.0, step), dot_r, color);
            painter.circle_filled(c, dot_r, color);
            painter.circle_filled(c + Vec2::new(0.0, step), dot_r, color);
        }
        IconKind::Hamburger => {
            let w = r * 0.72;
            let step = r * 0.48;
            for dy in [-step, 0.0, step] {
                painter.line_segment([c + Vec2::new(-w, dy), c + Vec2::new(w, dy)], stroke);
            }
        }
        IconKind::Settings => {
            painter.circle_stroke(c, r * 0.42, stroke);
            for i in 0..6 {
                let a = (i as f32 / 6.0) * std::f32::consts::TAU;
                let p0 = c + Vec2::new(a.cos(), a.sin()) * (r * 0.52);
                let p1 = c + Vec2::new(a.cos(), a.sin()) * (r * 0.8);
                painter.line_segment([p0, p1], stroke);
            }
        }
        IconKind::Send => {
            let pts = vec![
                c + Vec2::new(-r * 0.65, -r * 0.55),
                c + Vec2::new(r * 0.78, 0.0),
                c + Vec2::new(-r * 0.65, r * 0.55),
                c + Vec2::new(-r * 0.35, 0.0),
            ];
            painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        IconKind::Attach => {
            let w = r * 0.36;
            let h = r * 0.68;
            painter.add(Shape::line(
                vec![
                    c + Vec2::new(-w, -h * 0.3),
                    c + Vec2::new(-w, h * 0.65),
                    c + Vec2::new(0.0, h),
                    c + Vec2::new(w, h * 0.65),
                    c + Vec2::new(w, -h * 0.65),
                    c + Vec2::new(0.0, -h),
                    c + Vec2::new(-w * 0.5, -h * 0.65),
                    c + Vec2::new(-w * 0.5, h * 0.35),
                ],
                stroke,
            ));
        }
        IconKind::Check => {
            painter.add(Shape::line(
                vec![
                    c + Vec2::new(-r * 0.62, r * 0.02),
                    c + Vec2::new(-r * 0.14, r * 0.52),
                    c + Vec2::new(r * 0.68, -r * 0.45),
                ],
                stroke,
            ));
        }
        IconKind::Plus => {
            let d = r * 0.65;
            painter.line_segment([c - Vec2::new(d, 0.0), c + Vec2::new(d, 0.0)], stroke);
            painter.line_segment([c - Vec2::new(0.0, d), c + Vec2::new(0.0, d)], stroke);
        }
        IconKind::Trash => {
            let w = r * 0.55;
            let h = r * 0.65;
            painter.line_segment(
                [
                    c + Vec2::new(-w * 1.15, -h * 0.55),
                    c + Vec2::new(w * 1.15, -h * 0.55),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    c + Vec2::new(-w * 0.45, -h * 0.85),
                    c + Vec2::new(w * 0.45, -h * 0.85),
                ],
                stroke,
            );
            painter.add(Shape::line(
                vec![
                    c + Vec2::new(-w * 0.8, -h * 0.5),
                    c + Vec2::new(-w * 0.65, h * 0.78),
                    c + Vec2::new(w * 0.65, h * 0.78),
                    c + Vec2::new(w * 0.8, -h * 0.5),
                ],
                stroke,
            ));
        }
        IconKind::Edit => {
            let d = r * 0.62;
            painter.line_segment(
                [c + Vec2::new(-d, d), c + Vec2::new(d * 0.8, -d * 0.8)],
                stroke,
            );
            painter.line_segment(
                [c + Vec2::new(-d, d), c + Vec2::new(-d * 0.35, d * 0.9)],
                stroke,
            );
        }
        IconKind::Info => {
            painter.circle_stroke(c, r * 0.72, stroke);
            painter.circle_filled(c + Vec2::new(0.0, -r * 0.32), (s * 0.08).max(1.2), color);
            painter.line_segment(
                [c + Vec2::new(0.0, -r * 0.06), c + Vec2::new(0.0, r * 0.4)],
                stroke,
            );
        }
        IconKind::Star => {
            let mut pts = Vec::with_capacity(10);
            for i in 0..10 {
                let rad = if i % 2 == 0 { r * 0.78 } else { r * 0.34 };
                let a = (i as f32 / 10.0) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                pts.push(c + Vec2::new(a.cos() * rad, a.sin() * rad));
            }
            pts.push(pts[0]);
            painter.add(Shape::line(pts, stroke));
        }
        IconKind::Bookmark => {
            let w = r * 0.5;
            let h = r * 0.68;
            painter.add(Shape::line(
                vec![
                    c + Vec2::new(-w, h),
                    c + Vec2::new(-w, -h),
                    c + Vec2::new(w, -h),
                    c + Vec2::new(w, h),
                    c + Vec2::new(0.0, h * 0.45),
                    c + Vec2::new(-w, h),
                ],
                stroke,
            ));
        }
        IconKind::ChevronRight => {
            let d = r * 0.42;
            painter.add(Shape::line(
                vec![
                    c + Vec2::new(-d * 0.5, -d),
                    c + Vec2::new(d * 0.6, 0.0),
                    c + Vec2::new(-d * 0.5, d),
                ],
                stroke,
            ));
        }
        IconKind::Lock => {
            let w = r * 0.56;
            let body = Rect::from_min_max(c + Vec2::new(-w, -r * 0.05), c + Vec2::new(w, r * 0.7));
            painter.rect_stroke(
                body,
                CornerRadius::same(2),
                stroke,
                egui::StrokeKind::Middle,
            );
            painter.add(Shape::line(
                vec![
                    c + Vec2::new(-w * 0.55, -r * 0.05),
                    c + Vec2::new(-w * 0.55, -r * 0.48),
                    c + Vec2::new(0.0, -r * 0.68),
                    c + Vec2::new(w * 0.55, -r * 0.48),
                    c + Vec2::new(w * 0.55, -r * 0.05),
                ],
                stroke,
            ));
        }
        IconKind::Minimize => {
            let w = r * 0.62;
            let thin = Stroke::new(1.1, color);
            painter.line_segment([c + Vec2::new(-w, 0.0), c + Vec2::new(w, 0.0)], thin);
        }
        IconKind::Maximize => {
            let w = r * 0.56;
            let thin = Stroke::new(1.1, color);
            painter.rect_stroke(
                Rect::from_center_size(c, Vec2::splat(w * 2.0)),
                CornerRadius::ZERO,
                thin,
                egui::StrokeKind::Middle,
            );
        }
        IconKind::Restore => {
            let w = r * 0.46;
            let thin = Stroke::new(1.1, color);
            let front = Rect::from_center_size(c + Vec2::new(-1.0, 1.0), Vec2::splat(w * 2.0));
            painter.rect_stroke(front, CornerRadius::ZERO, thin, egui::StrokeKind::Middle);
            painter.add(Shape::line(
                vec![
                    Pos2::new(front.min.x + 2.0, front.min.y),
                    Pos2::new(front.min.x + 2.0, front.min.y - 2.0),
                    Pos2::new(front.max.x + 2.0, front.min.y - 2.0),
                    Pos2::new(front.max.x + 2.0, front.max.y - 2.0),
                    Pos2::new(front.max.x, front.max.y - 2.0),
                ],
                thin,
            ));
        }
    }
}

/// Compute natural button width mirroring `RoundButton::resizeToText` in `buttons.cpp`.
pub fn compute_round_button_width(
    text_width: f32,
    style: &RoundButtonStyle,
    full_width: Option<f32>,
) -> f32 {
    if let Some(fw) = full_width {
        if fw > 0.0 {
            return fw
                + if style.full_radius {
                    style.padding.horizontal()
                } else {
                    0.0
                };
        }
        return text_width - fw;
    }
    if style.width <= 0.0 {
        text_width - style.width + style.padding.horizontal()
    } else {
        style.width + style.padding.horizontal()
    }
}

/// Rounded button widget mirroring `Ui::RoundButton`.
pub struct RoundButton<'a> {
    text: &'a str,
    style: RoundButtonStyle,
    numbers_text: Option<&'a str>,
    icon: Option<IconKind>,
    full_width: Option<f32>,
    uppercase: bool,
    enabled: bool,
}

impl<'a> RoundButton<'a> {
    /// Create a new `RoundButton` with `text` and `style`.
    pub fn new(text: &'a str, style: RoundButtonStyle) -> Self {
        Self {
            text,
            style,
            numbers_text: None,
            icon: None,
            full_width: None,
            uppercase: false,
            enabled: true,
        }
    }

    /// Attach a secondary counter string (e.g. `"3"` in Forward/Delete buttons).
    pub fn numbers_text(mut self, numbers: &'a str) -> Self {
        if !numbers.is_empty() {
            self.numbers_text = Some(numbers);
        }
        self
    }

    /// Attach a leading icon.
    pub fn icon(mut self, icon: IconKind) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Override width (`> 0` fixed width, `< 0` text width + `-w`).
    pub fn full_width(mut self, width: f32) -> Self {
        self.full_width = Some(width);
        self
    }

    /// Use pill shape (`radius = height / 2`).
    pub fn full_radius(mut self, enabled: bool) -> Self {
        self.style.full_radius = enabled;
        self
    }

    /// Transform label text to uppercase (`RoundButtonTextTransform::ToUpper`).
    pub fn uppercase(mut self, uppercase: bool) -> Self {
        self.uppercase = uppercase;
        self
    }

    /// Enable or disable interaction.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Show the button in `ui`.
    pub fn show(self, ui: &mut Ui) -> Response {
        let label = if self.uppercase {
            self.text.to_uppercase()
        } else {
            self.text.to_owned()
        };
        let font_id = self.style.style.font.font_id();
        let text_galley =
            ui.painter()
                .layout_no_wrap(label.clone(), font_id.clone(), self.style.text_fg);
        let numbers_galley = self.numbers_text.map(|n| {
            ui.painter()
                .layout_no_wrap(n.to_owned(), font_id.clone(), self.style.numbers_text_fg)
        });
        let icon_extra = if self.icon.is_some() { 20.0 } else { 0.0 };
        let numbers_extra = numbers_galley
            .as_ref()
            .map_or(0.0, |g| self.style.numbers_skip + g.size().x);
        let content_w = text_galley.size().x + icon_extra + numbers_extra;

        let total_w = compute_round_button_width(content_w, &self.style, self.full_width);
        let total_h = self.style.height + self.style.padding.vertical();

        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(Vec2::new(total_w, total_h), sense);
        let a11y_label = match self.numbers_text {
            Some(numbers) => format!("{label} {numbers}"),
            None => label.clone(),
        };
        response
            .widget_info(|| WidgetInfo::labeled(Role::Button, self.enabled, a11y_label.clone()));
        if self.enabled && response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }

        if ui.is_rect_visible(rect) {
            let inner = Rect::from_min_max(
                Pos2::new(
                    rect.min.x + self.style.padding.left,
                    rect.min.y + self.style.padding.top,
                ),
                Pos2::new(
                    rect.max.x - self.style.padding.right,
                    rect.max.y - self.style.padding.bottom,
                ),
            );
            let radius = if self.style.full_radius {
                inner.height() * 0.5
            } else if self.style.radius > 0.0 {
                self.style.radius
            } else {
                constants::BUTTON_RADIUS
            };

            let over_factor = HoverAnimState::update(
                ui.ctx(),
                response.id,
                self.enabled && (response.hovered() || response.is_pointer_button_down_on()),
                constants::UNIVERSAL_DURATION_MS,
                Transition::SineInOut,
            );
            let bg = color_lerp(self.style.text_bg, self.style.text_bg_over, over_factor);
            let fg = color_lerp(self.style.text_fg, self.style.text_fg_over, over_factor);
            let num_fg = color_lerp(
                self.style.numbers_text_fg,
                self.style.numbers_text_fg_over,
                over_factor,
            );

            let bg_draw = if self.enabled {
                bg
            } else {
                crate::anim::with_alpha(bg, 0.5)
            };
            RoundRect::new(radius, bg_draw).paint(ui.painter(), inner);

            if self.enabled {
                update_and_paint_ripple(
                    ui.painter(),
                    response.id,
                    inner,
                    RippleMask::RoundRect(radius),
                    &self.style.ripple,
                    response.is_pointer_button_down_on(),
                    None,
                );
            }

            let fg_draw = if self.enabled {
                fg
            } else {
                crate::anim::with_alpha(fg, 0.6)
            };

            let mut x = inner.center().x - content_w * 0.5;
            if let Some(icon) = self.icon {
                let icon_rect =
                    Rect::from_center_size(Pos2::new(x + 7.0, inner.center().y), Vec2::splat(14.0));
                paint_icon(ui.painter(), icon_rect, icon, fg_draw);
                x += icon_extra;
            }
            ui.painter().text(
                Pos2::new(x, inner.center().y),
                Align2::LEFT_CENTER,
                label,
                font_id.clone(),
                fg_draw,
            );

            if let Some(numbers) = self.numbers_text {
                let nx = x + text_galley.size().x + self.style.numbers_skip;
                ui.painter().text(
                    Pos2::new(nx, inner.center().y),
                    Align2::LEFT_CENTER,
                    numbers,
                    font_id,
                    num_fg,
                );
            }
        }

        response
    }
}

/// Rectangular flat button mirroring `Ui::FlatButton`.
pub struct FlatButton<'a> {
    text: &'a str,
    style: FlatButtonStyle,
    width_override: Option<f32>,
}

impl<'a> FlatButton<'a> {
    /// Create a new `FlatButton`.
    pub fn new(text: &'a str, style: FlatButtonStyle) -> Self {
        Self {
            text,
            style,
            width_override: None,
        }
    }

    /// Set an explicit width.
    pub fn width(mut self, width: f32) -> Self {
        self.width_override = Some(width);
        self
    }

    /// Show the flat button in `ui`.
    pub fn show(self, ui: &mut Ui) -> Response {
        let font_id = self.style.font.font_id();
        let galley =
            ui.painter()
                .layout_no_wrap(self.text.to_owned(), font_id.clone(), self.style.color);
        let width = self.width_override.unwrap_or_else(|| {
            if self.style.width < 0.0 {
                galley.size().x - self.style.width
            } else if self.style.width == 0.0 {
                galley.size().x + self.style.height - self.style.font.line_height()
            } else {
                self.style.width
            }
        });

        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(width, self.style.height), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), self.text));
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }

        if ui.is_rect_visible(rect) {
            let over = HoverAnimState::update(
                ui.ctx(),
                response.id,
                response.hovered() || response.is_pointer_button_down_on(),
                constants::UNIVERSAL_DURATION_MS,
                Transition::SineInOut,
            );
            let bg = color_lerp(self.style.bg_color, self.style.over_bg_color, over);
            let fg = color_lerp(self.style.color, self.style.over_color, over);

            ui.painter().rect_filled(rect, CornerRadius::ZERO, bg);
            update_and_paint_ripple(
                ui.painter(),
                response.id,
                rect,
                RippleMask::Rect,
                &self.style.ripple,
                response.is_pointer_button_down_on(),
                None,
            );
            ui.painter()
                .text(rect.center(), Align2::CENTER_CENTER, self.text, font_id, fg);
        }
        response
    }
}

/// Circular-ripple icon button mirroring `Ui::IconButton`.
pub struct IconButton {
    icon: IconKind,
    style_icon: Option<tg_style::Icon>,
    style_icon_over: Option<tg_style::Icon>,
    style: IconButtonStyle,
    tooltip: Option<String>,
    accessible_name: Option<String>,
}

impl IconButton {
    /// Create a new `IconButton`.
    pub fn new(icon: IconKind, style: IconButtonStyle) -> Self {
        Self {
            icon,
            style_icon: None,
            style_icon_over: None,
            style,
            tooltip: None,
            accessible_name: None,
        }
    }

    /// Create an `IconButton` directly from a generated `tg_style::structs::IconButton`.
    #[must_use]
    pub fn from_tg_style(tg_style: &tg_style::structs::IconButton, colors: &dyn Colors) -> Self {
        Self {
            icon: IconKind::MenuDots,
            style_icon: (!tg_style.icon.is_empty()).then_some(tg_style.icon),
            style_icon_over: (!tg_style.icon_over.is_empty()).then_some(tg_style.icon_over),
            style: IconButtonStyle::from_tg(tg_style, colors),
            tooltip: None,
            accessible_name: None,
        }
    }

    /// Override the rendered glyph with a real `tg_style::Icon` (and optional hover icon).
    #[must_use]
    pub fn with_style_icon(
        mut self,
        icon: tg_style::Icon,
        icon_over: Option<tg_style::Icon>,
    ) -> Self {
        self.style_icon = Some(icon);
        self.style_icon_over = icon_over;
        self
    }

    /// Optional hover tooltip label.
    pub fn tooltip(mut self, tip: impl Into<String>) -> Self {
        self.tooltip = Some(tip.into());
        self
    }

    /// Optional explicit accessible name for screen readers (overrides tooltip/icon fallback).
    pub fn accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    /// Show the icon button in `ui`.
    pub fn show(self, ui: &mut Ui) -> Response {
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(self.style.width, self.style.height),
            Sense::click(),
        );
        let a11y_label = self
            .accessible_name
            .as_deref()
            .or(self.tooltip.as_deref())
            .unwrap_or_else(|| self.icon.accessible_name());
        response.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), a11y_label));
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }

        if ui.is_rect_visible(rect) {
            let over = HoverAnimState::update(
                ui.ctx(),
                response.id,
                response.hovered() || response.is_pointer_button_down_on(),
                self.style.duration_ms,
                Transition::SineInOut,
            );

            let ripple_rect = Rect::from_min_size(
                Pos2::new(
                    rect.min.x + self.style.ripple_area_position.x,
                    rect.min.y + self.style.ripple_area_position.y,
                ),
                Vec2::splat(self.style.ripple_area_size),
            );
            // Subtle hover circle behind icon like tdesktop hover fade.
            if over > 0.01 {
                ui.painter().circle_filled(
                    ripple_rect.center(),
                    ripple_rect.width() * 0.5,
                    crate::anim::with_alpha(self.style.ripple.color, over * 0.55),
                );
            }

            update_and_paint_ripple(
                ui.painter(),
                response.id,
                ripple_rect,
                RippleMask::Ellipse,
                &self.style.ripple,
                response.is_pointer_button_down_on(),
                None,
            );

            let fg = color_lerp(self.style.icon_fg, self.style.icon_fg_over, over);
            let icon_rect =
                Rect::from_center_size(rect.center(), Vec2::splat(self.style.icon_size));
            if let Some(tg_ic) = self.style_icon {
                let chosen = if over > 0.5 {
                    self.style_icon_over.unwrap_or(tg_ic)
                } else {
                    tg_ic
                };
                if !paint_style_icon(
                    ui.painter(),
                    icon_rect,
                    chosen,
                    &tg_style::DefaultPaletteResolver,
                    Some(fg),
                ) {
                    paint_icon(ui.painter(), icon_rect, self.icon, fg);
                }
            } else {
                paint_icon(ui.painter(), icon_rect, self.icon, fg);
            }
        }

        if let Some(tip) = self.tooltip {
            response.on_hover_text(tip)
        } else {
            response
        }
    }
}

/// Inline text link button mirroring `Ui::LinkButton`.
pub struct LinkButton<'a> {
    text: &'a str,
    style: LinkButtonStyle,
}

impl<'a> LinkButton<'a> {
    /// Create a new `LinkButton`.
    pub fn new(text: &'a str, style: LinkButtonStyle) -> Self {
        Self { text, style }
    }

    /// Show the link button in `ui`.
    pub fn show(self, ui: &mut Ui) -> Response {
        let font_id = self.style.font.font_id();
        let galley =
            ui.painter()
                .layout_no_wrap(self.text.to_owned(), font_id.clone(), self.style.color);
        let size = Vec2::new(
            galley.size().x + self.style.padding.horizontal(),
            galley.size().y + self.style.padding.vertical(),
        );
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        response.widget_info(|| WidgetInfo::labeled(Role::Link, ui.is_enabled(), self.text));
        let over = response.hovered() || response.is_pointer_button_down_on();
        if over {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }

        if ui.is_rect_visible(rect) {
            let color = if over {
                self.style.over_color
            } else {
                self.style.color
            };
            let text_pos = Pos2::new(
                rect.min.x + self.style.padding.left,
                rect.min.y + self.style.padding.top,
            );
            ui.painter()
                .text(text_pos, Align2::LEFT_TOP, self.text, font_id, color);
            let draw_underline = if over {
                self.style.over_font.underline
            } else {
                self.style.font.underline
            };
            if draw_underline {
                let y = text_pos.y + galley.size().y - 1.0;
                ui.painter().line_segment(
                    [
                        Pos2::new(text_pos.x, y),
                        Pos2::new(text_pos.x + galley.size().x, y),
                    ],
                    Stroke::new(1.0, color),
                );
            }
        }
        response
    }
}
