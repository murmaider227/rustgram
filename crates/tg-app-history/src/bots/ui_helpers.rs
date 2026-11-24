//! Shared painting primitives, color tokens, vector icons, and procedural preview art
//! for `tg-app::bots` widgets.
//!
//! Port origin: `Telegram/SourceFiles/ui/chat/chat.style` (`msgBotKbButton`, `botKbButton`,
//! `bubbleRadiusSmall`, `bubbleRadiusLarge`), `Telegram/SourceFiles/history/view/history_view_element.cpp`
//! (`KeyboardStyle`), and `Telegram/SourceFiles/chat_helpers/bot_keyboard.cpp`.

use std::f32::consts::TAU;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Painter, Pos2, Rect, Response, Sense,
    Stroke, Ui, Vec2,
};
use tg_bots::keyboard_layout::{ButtonRounding, CornerRounding};
use tg_bots::markup::{ButtonColor, ButtonType, ButtonTypeIcon};

use tg_app_core::stand_in::theme::Palette;

/// Small corner radius matching `st::bubbleRadiusSmall` (`6px`).
pub const BUBBLE_RADIUS_SMALL: u8 = 6;
/// Large outer corner radius matching `st::bubbleRadiusLarge` (`14px` in egui logical px).
pub const BUBBLE_RADIUS_LARGE: u8 = 14;

/// Semantic bot button colors derived from the active [`Palette`].
#[derive(Debug, Clone, Copy)]
pub struct BotButtonColors {
    /// Normal inline button translucent fill over chat wallpaper (`msgBotKbBg`).
    pub inline_normal_bg: Color32,
    /// Normal inline button hover overlay (`msgBotKbOverBgAdd`).
    pub inline_normal_hover: Color32,
    /// Normal inline button pressed overlay.
    pub inline_normal_pressed: Color32,
    /// Normal inline button text (`msgServiceFg`).
    pub inline_normal_fg: Color32,
    /// Normal reply keyboard button fill (`botKbBg`).
    pub reply_normal_bg: Color32,
    /// Normal reply keyboard button hover fill.
    pub reply_normal_hover: Color32,
    /// Normal reply keyboard button pressed fill (`botKbDownBg`).
    pub reply_normal_pressed: Color32,
    /// Normal reply keyboard button text (`botKbColor`).
    pub reply_normal_fg: Color32,
    /// Primary accent button fill (`botKbPrimaryBg`).
    pub primary_bg: Color32,
    /// Danger / destructive button fill (`botKbDangerBg`).
    pub danger_bg: Color32,
    /// Success / positive button fill (`botKbSuccessBg`).
    pub success_bg: Color32,
    /// Foreground text on colored (`Primary` / `Danger` / `Success`) buttons.
    pub colored_fg: Color32,
}

impl BotButtonColors {
    /// Derives semantic bot button colors from `palette` (supporting both Day and Night modes).
    #[must_use]
    pub fn from_palette(palette: &Palette) -> Self {
        let dark = is_dark_palette(palette);
        if dark {
            Self {
                inline_normal_bg: Color32::from_rgba_unmultiplied(0x1E, 0x2C, 0x3A, 215),
                inline_normal_hover: Color32::from_rgba_unmultiplied(0x2B, 0x3E, 0x52, 235),
                inline_normal_pressed: Color32::from_rgba_unmultiplied(0x36, 0x4E, 0x66, 245),
                inline_normal_fg: Color32::WHITE,
                reply_normal_bg: Color32::from_rgb(0x23, 0x2E, 0x3C),
                reply_normal_hover: Color32::from_rgb(0x2B, 0x39, 0x4A),
                reply_normal_pressed: Color32::from_rgb(0x34, 0x45, 0x59),
                reply_normal_fg: Color32::from_rgb(0xF5, 0xF5, 0xF5),
                primary_bg: Color32::from_rgb(0x3B, 0x82, 0xD6),
                danger_bg: Color32::from_rgb(0xD9, 0x4B, 0x4B),
                success_bg: Color32::from_rgb(0x2F, 0x9E, 0x56),
                colored_fg: Color32::WHITE,
            }
        } else {
            Self {
                inline_normal_bg: Color32::from_rgba_unmultiplied(0x4C, 0x67, 0x7E, 178),
                inline_normal_hover: Color32::from_rgba_unmultiplied(0x3E, 0x57, 0x6D, 205),
                inline_normal_pressed: Color32::from_rgba_unmultiplied(0x31, 0x48, 0x5C, 225),
                inline_normal_fg: Color32::WHITE,
                reply_normal_bg: Color32::from_rgb(0xEB, 0xEF, 0xF3),
                reply_normal_hover: Color32::from_rgb(0xDF, 0xE6, 0xEC),
                reply_normal_pressed: Color32::from_rgb(0xD1, 0xDC, 0xE5),
                reply_normal_fg: Color32::from_rgb(0x22, 0x22, 0x22),
                primary_bg: Color32::from_rgb(0x33, 0x90, 0xEC),
                danger_bg: Color32::from_rgb(0xE0, 0x4A, 0x4A),
                success_bg: Color32::from_rgb(0x2E, 0xA6, 0x52),
                colored_fg: Color32::WHITE,
            }
        }
    }

    /// Resolves `(background, foreground)` for a button given its semantic [`ButtonColor`],
    /// whether it is an inline keyboard button, hover state, and pressed state.
    #[must_use]
    pub fn resolve(
        self,
        color: ButtonColor,
        is_inline: bool,
        hovered: bool,
        pressed: bool,
    ) -> (Color32, Color32) {
        match color {
            ButtonColor::Normal => {
                if is_inline {
                    let bg = if pressed {
                        self.inline_normal_pressed
                    } else if hovered {
                        self.inline_normal_hover
                    } else {
                        self.inline_normal_bg
                    };
                    (bg, self.inline_normal_fg)
                } else {
                    let bg = if pressed {
                        self.reply_normal_pressed
                    } else if hovered {
                        self.reply_normal_hover
                    } else {
                        self.reply_normal_bg
                    };
                    (bg, self.reply_normal_fg)
                }
            }
            ButtonColor::Primary => (
                adjust_state(self.primary_bg, hovered, pressed),
                self.colored_fg,
            ),
            ButtonColor::Danger => (
                adjust_state(self.danger_bg, hovered, pressed),
                self.colored_fg,
            ),
            ButtonColor::Success => (
                adjust_state(self.success_bg, hovered, pressed),
                self.colored_fg,
            ),
        }
    }
}

/// Returns `true` when `palette` represents a dark theme.
#[must_use]
pub fn is_dark_palette(palette: &Palette) -> bool {
    let luma = (u32::from(palette.window_bg.r()) * 299
        + u32::from(palette.window_bg.g()) * 587
        + u32::from(palette.window_bg.b()) * 114)
        / 1000;
    luma < 128
}

/// Applies an alpha multiplier `0.0..=1.0` to `color`.
#[must_use]
pub fn with_alpha(color: Color32, alpha: f32) -> Color32 {
    let a = (f32::from(color.a()) * alpha.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

fn adjust_state(base: Color32, hovered: bool, pressed: bool) -> Color32 {
    let factor = if pressed {
        0.82
    } else if hovered {
        0.91
    } else {
        1.0
    };
    Color32::from_rgba_unmultiplied(
        (f32::from(base.r()) * factor).round() as u8,
        (f32::from(base.g()) * factor).round() as u8,
        (f32::from(base.b()) * factor).round() as u8,
        base.a(),
    )
}

/// Converts `tg_bots::keyboard_layout::ButtonRounding` into an `egui::CornerRadius`.
#[must_use]
pub const fn to_corner_radius(rounding: ButtonRounding) -> CornerRadius {
    const fn r(c: CornerRounding) -> u8 {
        match c {
            CornerRounding::Small => BUBBLE_RADIUS_SMALL,
            CornerRounding::Large => BUBBLE_RADIUS_LARGE,
        }
    }
    CornerRadius {
        nw: r(rounding.top_left),
        ne: r(rounding.top_right),
        sw: r(rounding.bottom_left),
        se: r(rounding.bottom_right),
    }
}

/// Paints the top-right corner icon on an inline keyboard button.
///
/// Covers `Url`, `Auth`, `SwitchInline`/`SwitchInlineSame`, `WebView`/`SimpleWebView`,
/// `CopyText`, `Buy` (Payment), `UserProfile`, and `RequestPeer` buttons.
pub fn paint_inline_button_icon(
    painter: &Painter,
    button_rect: Rect,
    button_type: ButtonType,
    effective_icon: ButtonTypeIcon,
    color: Color32,
) {
    let icon_size = 10.0;
    let pad = 4.0;
    let r = Rect::from_min_size(
        Pos2::new(
            button_rect.right() - icon_size - pad,
            button_rect.top() + pad,
        ),
        Vec2::splat(icon_size),
    );
    let stroke = Stroke::new(1.35, color);

    // Explicit icons for button types that tdesktop maps to CornerIcon or distinct glyph.
    match button_type {
        ButtonType::UserProfile => {
            // Small person silhouette in the top-right corner.
            let head_c = Pos2::new(r.center().x, r.top() + 3.0);
            painter.circle_stroke(head_c, 2.0, stroke);
            painter.line_segment(
                [
                    Pos2::new(r.left() + 1.5, r.bottom() - 1.0),
                    Pos2::new(r.right() - 1.5, r.bottom() - 1.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(r.left() + 1.5, r.bottom() - 1.0),
                    Pos2::new(r.center().x, r.top() + 5.8),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(r.right() - 1.5, r.bottom() - 1.0),
                    Pos2::new(r.center().x, r.top() + 5.8),
                ],
                stroke,
            );
            return;
        }
        ButtonType::RequestPeer | ButtonType::CreateBot => {
            // Peer selector badge (small circle + plus).
            painter.circle_stroke(Pos2::new(r.left() + 3.5, r.center().y), 2.5, stroke);
            painter.line_segment(
                [
                    Pos2::new(r.right() - 3.0, r.center().y),
                    Pos2::new(r.right(), r.center().y),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(r.right() - 1.5, r.center().y - 1.5),
                    Pos2::new(r.right() - 1.5, r.center().y + 1.5),
                ],
                stroke,
            );
            return;
        }
        ButtonType::Auth => {
            // Login key + external arrow hint so Login URL buttons are immediately distinguishable.
            let ring = Pos2::new(r.left() + 3.0, r.bottom() - 3.0);
            painter.circle_stroke(ring, 2.2, stroke);
            painter.line_segment(
                [
                    Pos2::new(ring.x + 1.6, ring.y - 1.6),
                    Pos2::new(r.right() - 1.0, r.top() + 1.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(r.right() - 4.0, r.top() + 1.0),
                    Pos2::new(r.right() - 1.0, r.top() + 1.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(r.right() - 1.0, r.top() + 1.0),
                    Pos2::new(r.right() - 1.0, r.top() + 4.0),
                ],
                stroke,
            );
            return;
        }
        _ => {}
    }

    match effective_icon {
        ButtonTypeIcon::None => {}
        ButtonTypeIcon::Url => {
            // Diagonal top-right arrow (`inline_button_url`).
            painter.line_segment(
                [
                    Pos2::new(r.left() + 1.5, r.bottom() - 1.5),
                    Pos2::new(r.right() - 1.0, r.top() + 1.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(r.right() - 5.2, r.top() + 1.0),
                    Pos2::new(r.right() - 1.0, r.top() + 1.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(r.right() - 1.0, r.top() + 1.0),
                    Pos2::new(r.right() - 1.0, r.top() + 5.2),
                ],
                stroke,
            );
        }
        ButtonTypeIcon::SwitchPm => {
            // Curved/forward share arrow (`inline_button_switch`).
            painter.line_segment(
                [
                    Pos2::new(r.left() + 1.5, r.bottom() - 1.5),
                    Pos2::new(r.left() + 1.5, r.center().y),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(r.left() + 1.5, r.center().y),
                    Pos2::new(r.right() - 1.5, r.center().y),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(r.right() - 4.2, r.top() + 2.0),
                    Pos2::new(r.right() - 1.2, r.center().y),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(r.right() - 4.2, r.bottom() - 2.0),
                    Pos2::new(r.right() - 1.2, r.center().y),
                ],
                stroke,
            );
        }
        ButtonTypeIcon::Webview => {
            // Mini App window frame (`inline_button_web`).
            painter.rect_stroke(
                r.shrink(0.8),
                CornerRadius::same(2),
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment(
                [
                    Pos2::new(r.left() + 1.0, r.top() + 3.5),
                    Pos2::new(r.right() - 1.0, r.top() + 3.5),
                ],
                stroke,
            );
        }
        ButtonTypeIcon::Copy => {
            // Double overlapping squares (`inline_button_copy`).
            let back =
                Rect::from_min_size(Pos2::new(r.left() + 0.5, r.top() + 0.5), Vec2::splat(6.5));
            let front =
                Rect::from_min_size(Pos2::new(r.left() + 3.0, r.top() + 3.0), Vec2::splat(6.5));
            painter.rect_stroke(
                back,
                CornerRadius::same(1),
                Stroke::new(1.1, with_alpha(color, 0.75)),
                egui::StrokeKind::Inside,
            );
            painter.rect_stroke(
                front,
                CornerRadius::same(1),
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        ButtonTypeIcon::Payment => {
            // Payment card icon (`inline_button_card`).
            let card =
                Rect::from_min_size(Pos2::new(r.left(), r.top() + 1.5), Vec2::new(10.0, 7.0));
            painter.rect_stroke(
                card,
                CornerRadius::same(1),
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment(
                [
                    Pos2::new(card.left() + 1.0, card.top() + 2.8),
                    Pos2::new(card.right() - 1.0, card.top() + 2.8),
                ],
                stroke,
            );
        }
    }
}

/// Paints a small leading icon glyph inside a reply keyboard button for special request types.
pub fn paint_reply_button_leading_icon(
    painter: &Painter,
    center: Pos2,
    button_type: ButtonType,
    color: Color32,
) -> bool {
    let stroke = Stroke::new(1.4, color);
    match button_type {
        ButtonType::RequestPhone => {
            // Handset outline
            painter.rect_stroke(
                Rect::from_center_size(center, Vec2::new(7.0, 11.0)),
                CornerRadius::same(2),
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.circle_filled(Pos2::new(center.x, center.y + 3.2), 0.9, color);
            true
        }
        ButtonType::RequestLocation => {
            // Map pin
            painter.circle_stroke(Pos2::new(center.x, center.y - 1.8), 3.2, stroke);
            painter.line_segment(
                [
                    Pos2::new(center.x - 2.2, center.y + 0.6),
                    Pos2::new(center.x, center.y + 5.2),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x + 2.2, center.y + 0.6),
                    Pos2::new(center.x, center.y + 5.2),
                ],
                stroke,
            );
            true
        }
        ButtonType::RequestPoll => {
            // Mini bar chart
            painter.line_segment(
                [
                    Pos2::new(center.x - 3.5, center.y + 4.0),
                    Pos2::new(center.x - 3.5, center.y - 0.5),
                ],
                Stroke::new(1.8, color),
            );
            painter.line_segment(
                [
                    Pos2::new(center.x, center.y + 4.0),
                    Pos2::new(center.x, center.y - 4.0),
                ],
                Stroke::new(1.8, color),
            );
            painter.line_segment(
                [
                    Pos2::new(center.x + 3.5, center.y + 4.0),
                    Pos2::new(center.x + 3.5, center.y + 1.0),
                ],
                Stroke::new(1.8, color),
            );
            true
        }
        ButtonType::RequestPeer | ButtonType::CreateBot => {
            // Person + badge
            painter.circle_stroke(Pos2::new(center.x - 1.0, center.y - 2.2), 2.4, stroke);
            painter.line_segment(
                [
                    Pos2::new(center.x - 4.5, center.y + 4.0),
                    Pos2::new(center.x + 2.5, center.y + 4.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x + 2.5, center.y - 1.0),
                    Pos2::new(center.x + 5.5, center.y - 1.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x + 4.0, center.y - 2.5),
                    Pos2::new(center.x + 4.0, center.y + 0.5),
                ],
                stroke,
            );
            true
        }
        ButtonType::SimpleWebView | ButtonType::WebView => {
            painter.rect_stroke(
                Rect::from_center_size(center, Vec2::new(10.0, 9.0)),
                CornerRadius::same(2),
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x - 4.0, center.y - 1.5),
                    Pos2::new(center.x + 4.0, center.y - 1.5),
                ],
                stroke,
            );
            true
        }
        _ => false,
    }
}

/// Paints an animated circular loading spinner (used while an inline callback RPC is in flight).
pub fn paint_loading_spinner(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    color: Color32,
    time_secs: f64,
) {
    let start_angle = ((time_secs * 3.8) % 1.0) as f32 * TAU;
    let sweep = TAU * 0.68;
    let segments = 14usize;
    let mut pts = Vec::with_capacity(segments + 1);
    for i in 0..=segments {
        let a = start_angle + sweep * (i as f32 / segments as f32);
        pts.push(Pos2::new(
            center.x + radius * a.cos(),
            center.y + radius * a.sin(),
        ));
    }
    painter.add(egui::Shape::line(pts, Stroke::new(1.8, color)));
}

/// Draws a Telegram-styled rounded pill/tab button.
pub fn pill_button(
    ui: &mut Ui,
    label: &str,
    bg: Color32,
    fg: Color32,
    border: Option<Color32>,
) -> Response {
    let galley =
        ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), FontId::proportional(12.5), fg));
    let size = Vec2::new(galley.size().x + 20.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let fill = if resp.is_pointer_button_down_on() {
        with_alpha(bg, 0.80)
    } else if resp.hovered() {
        with_alpha(bg, 0.90)
    } else {
        bg
    };
    ui.painter().rect_filled(rect, CornerRadius::same(14), fill);
    if let Some(stroke_col) = border {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(14),
            Stroke::new(1.0, stroke_col),
            egui::StrokeKind::Inside,
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

/// Draws a full-width primary action button inside modal boxes.
pub fn box_action_button(
    ui: &mut Ui,
    label: &str,
    bg: Color32,
    fg: Color32,
    width: f32,
    height: f32,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let fill = if resp.is_pointer_button_down_on() {
        with_alpha(bg, 0.80)
    } else if resp.hovered() {
        with_alpha(bg, 0.90)
    } else {
        bg
    };
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.5),
        fg,
    );
    resp
}

/// Draws a crisp procedural scenic tile for inline photos, GIFs, game banners, or bot info headers.
pub fn paint_procedural_art(
    painter: &Painter,
    rect: Rect,
    seed: u64,
    rounding: CornerRadius,
    badge: Option<&str>,
) {
    let palettes: [(Color32, Color32, Color32); 6] = [
        (
            Color32::from_rgb(0x28, 0x67, 0xB2),
            Color32::from_rgb(0x4E, 0xB2, 0xEC),
            Color32::from_rgb(0xFF, 0xD2, 0x7A),
        ),
        (
            Color32::from_rgb(0x2B, 0x7A, 0x5B),
            Color32::from_rgb(0x61, 0xC2, 0x93),
            Color32::from_rgb(0xE8, 0xF5, 0xB8),
        ),
        (
            Color32::from_rgb(0x6B, 0x45, 0xA8),
            Color32::from_rgb(0xB9, 0x6F, 0xD8),
            Color32::from_rgb(0xFF, 0xB7, 0xD5),
        ),
        (
            Color32::from_rgb(0xC6, 0x5A, 0x3A),
            Color32::from_rgb(0xF0, 0x98, 0x5A),
            Color32::from_rgb(0xFF, 0xE5, 0x9E),
        ),
        (
            Color32::from_rgb(0x24, 0x48, 0x6B),
            Color32::from_rgb(0x3F, 0x7C, 0xA8),
            Color32::from_rgb(0x96, 0xE2, 0xFF),
        ),
        (
            Color32::from_rgb(0x8E, 0x3D, 0x67),
            Color32::from_rgb(0xD9, 0x6D, 0x8C),
            Color32::from_rgb(0xFF, 0xD6, 0xA5),
        ),
    ];
    let (base, mid, accent) = palettes[(seed as usize) % palettes.len()];

    painter.rect_filled(rect, rounding, base);
    // Horizontal layered atmospheric band
    let band = Rect::from_min_max(
        Pos2::new(rect.left(), rect.top() + rect.height() * 0.42),
        rect.max,
    );
    painter.rect_filled(band, CornerRadius::ZERO, with_alpha(mid, 0.72));

    // Sun/orb accent
    let orb_x = rect.left() + rect.width() * (0.28 + ((seed % 5) as f32) * 0.11);
    let orb_y = rect.top() + rect.height() * 0.30;
    let orb_r = (rect.height() * 0.18).clamp(8.0, 22.0);
    painter.circle_filled(Pos2::new(orb_x, orb_y), orb_r, with_alpha(accent, 0.90));

    // Stylized silhouette hill ridge
    let ridge_y = rect.bottom() - rect.height() * 0.24;
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(rect.left(), rect.bottom()),
            Pos2::new(rect.left() + rect.width() * 0.32, ridge_y),
            Pos2::new(
                rect.left() + rect.width() * 0.66,
                ridge_y + rect.height() * 0.12,
            ),
            Pos2::new(rect.right(), rect.bottom()),
        ],
        with_alpha(base, 0.85),
        Stroke::NONE,
    ));

    if let Some(tag) = badge {
        let tag_rect = Rect::from_min_size(
            Pos2::new(rect.left() + 6.0, rect.top() + 6.0),
            Vec2::new((tag.len() as f32 * 6.8 + 10.0).max(28.0), 17.0),
        );
        painter.rect_filled(
            tag_rect,
            CornerRadius::same(4),
            Color32::from_rgba_unmultiplied(0, 0, 0, 155),
        );
        painter.text(
            tag_rect.center(),
            Align2::CENTER_CENTER,
            tag,
            FontId::proportional(10.0),
            Color32::WHITE,
        );
    }
}

/// Paints a stylized mini map tile with roads and a red location pin (for Location/Venue results).
pub fn paint_map_thumbnail(painter: &Painter, rect: Rect, rounding: CornerRadius) {
    painter.rect_filled(rect, rounding, Color32::from_rgb(0xE8, 0xEF, 0xDC));
    // River stripe
    painter.line_segment(
        [
            Pos2::new(rect.left() + 4.0, rect.bottom() - 8.0),
            Pos2::new(rect.right() - 4.0, rect.top() + 10.0),
        ],
        Stroke::new(6.0, Color32::from_rgb(0xA7, 0xD3, 0xF5)),
    );
    // Road grid
    let road = Stroke::new(2.0, Color32::WHITE);
    painter.line_segment(
        [
            Pos2::new(rect.left(), rect.center().y),
            Pos2::new(rect.right(), rect.center().y),
        ],
        road,
    );
    painter.line_segment(
        [
            Pos2::new(rect.center().x - 6.0, rect.top()),
            Pos2::new(rect.center().x - 6.0, rect.bottom()),
        ],
        road,
    );
    // Red location pin
    let c = rect.center();
    painter.circle_filled(
        Pos2::new(c.x, c.y - 3.0),
        6.5,
        Color32::from_rgb(0xE0, 0x4A, 0x4A),
    );
    painter.circle_filled(Pos2::new(c.x, c.y - 3.0), 2.5, Color32::WHITE);
}

/// Formats seconds into `MM:SS`.
#[must_use]
pub fn format_duration(secs: i32) -> String {
    let s = secs.max(0);
    format!("{:02}:{:02}", s / 60, s % 60)
}
