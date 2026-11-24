//! Hover tooltips and directional important-callout tooltips.
//!
//! Port of `Telegram/lib_ui/ui/widgets/tooltip.h` and
//! `Telegram/lib_ui/ui/widgets/tooltip.cpp` (`Tooltip`, `ImportantTooltip`).

use egui::{
    Align2, Area, CornerRadius, Id, Order, Pos2, Rect, Response, Shape, Stroke, StrokeKind, Vec2,
};

use crate::anim::{SimpleAnimation, Transition, with_alpha};
use crate::style::{ImportantTooltipStyle, TooltipStyle, constants};

/// Show a classic Telegram Desktop hover tooltip (`defaultTooltip`) when `response`
/// is hovered.
pub fn show_tooltip_on_hover(
    ctx: &egui::Context,
    response: &Response,
    text: &str,
    style: &TooltipStyle,
) {
    if !response.hovered() || text.is_empty() {
        return;
    }
    let anchor = ctx
        .pointer_hover_pos()
        .unwrap_or_else(|| Pos2::new(response.rect.center().x, response.rect.max.y));
    paint_tooltip_at(ctx, response.id.with("_tg_tooltip"), anchor, text, style);
}

/// Render a tdesktop [`TooltipStyle`] box anchored near `anchor` with viewport clamping.
pub fn paint_tooltip_at(
    ctx: &egui::Context,
    id: Id,
    anchor: Pos2,
    text: &str,
    style: &TooltipStyle,
) {
    let font_id = style.text_style.font.font_id();
    let add_w = 2.0 * constants::LINE_WIDTH + style.text_padding.horizontal();
    let add_h = 2.0 * constants::LINE_WIDTH + style.text_padding.vertical();
    let wrap_w = (style.width_max - add_w).max(40.0);

    let galley = ctx.fonts_mut(|f| f.layout(text.to_owned(), font_id, style.text_fg, wrap_w));
    let size = Vec2::new(galley.size().x + add_w, galley.size().y + add_h);

    let screen = ctx.viewport_rect();
    let mut pos = Pos2::new(anchor.x + style.shift.x, anchor.y + style.shift.y);
    if pos.x + size.x > screen.max.x - style.skip {
        pos.x = (screen.max.x - style.skip - size.x).max(screen.min.x + style.skip);
    }
    if pos.x < screen.min.x + style.skip {
        pos.x = screen.min.x + style.skip;
    }
    if pos.y + size.y > screen.max.y - style.skip {
        pos.y = (anchor.y - size.y - style.skip).max(screen.min.y + style.skip);
    }

    Area::new(id)
        .order(Order::Tooltip)
        .interactable(false)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            let r = constants::ROUND_RADIUS_SMALL.round() as u8;
            ui.painter()
                .rect_filled(rect, CornerRadius::same(r), style.text_bg);
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(r),
                Stroke::new(constants::LINE_WIDTH, style.text_border),
                StrokeKind::Inside,
            );
            let text_pos = Pos2::new(
                rect.min.x + constants::LINE_WIDTH + style.text_padding.left,
                rect.min.y + constants::LINE_WIDTH + style.text_padding.top,
            );
            ui.painter().galley(text_pos, galley, style.text_fg);
        });
}

/// Direction the [`ImportantTooltip`] arrow points toward its target rect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TooltipSide {
    /// Tooltip sits above target; arrow points down.
    #[default]
    Top,
    /// Tooltip sits below target; arrow points up.
    Bottom,
}

/// Animated dark callout tooltip with directional arrow (`Ui::ImportantTooltip`).
#[derive(Debug, Clone, Default)]
pub struct ImportantTooltipState {
    shown: bool,
    opacity_anim: SimpleAnimation,
    hide_at_sec: Option<f64>,
}

impl ImportantTooltipState {
    /// Create a hidden important tooltip state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Show the tooltip, optionally auto-hiding after `auto_hide_ms`.
    pub fn toggle(
        &mut self,
        ctx: &egui::Context,
        visible: bool,
        style: &ImportantTooltipStyle,
        auto_hide_ms: Option<u32>,
    ) {
        let now = ctx.input(|i| i.time);
        if self.shown != visible {
            self.shown = visible;
            self.opacity_anim.change(
                now,
                if visible { 1.0 } else { 0.0 },
                style.duration_ms,
                Transition::SineInOut,
            );
            ctx.request_repaint();
        }
        self.hide_at_sec = if visible {
            auto_hide_ms.map(|ms| now + f64::from(ms) / 1000.0)
        } else {
            None
        };
    }

    /// Whether the tooltip is logically shown.
    pub const fn is_shown(&self) -> bool {
        self.shown
    }

    /// Paint the important tooltip pointing at `target` rect.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        id: Id,
        target: Rect,
        side: TooltipSide,
        text: &str,
        style: &ImportantTooltipStyle,
    ) {
        let now = ctx.input(|i| i.time);
        if let Some(deadline) = self.hide_at_sec {
            if now >= deadline && self.shown {
                self.toggle(ctx, false, style, None);
            } else if self.shown {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                    (deadline - now).max(0.02),
                ));
            }
        }

        let opacity = self
            .opacity_anim
            .sample(now, if self.shown { 1.0 } else { 0.0 });
        if self.opacity_anim.animating() {
            ctx.request_repaint();
        }
        if opacity <= 0.01 {
            return;
        }

        let font_id = egui::FontId::proportional(11.5);
        let galley = ctx.fonts_mut(|f| f.layout(text.to_owned(), font_id, style.fg, 240.0));
        let box_size = Vec2::new(
            galley.size().x + style.padding.horizontal(),
            galley.size().y + style.padding.vertical(),
        );
        let arrow = style.arrow;
        let screen = ctx.viewport_rect();
        let x = (target.center().x - box_size.x * 0.5).clamp(
            screen.min.x + 8.0,
            (screen.max.x - box_size.x - 8.0).max(8.0),
        );
        let y = match side {
            TooltipSide::Top => target.min.y - box_size.y - arrow - 4.0,
            TooltipSide::Bottom => target.max.y + arrow + 4.0,
        };

        let bg = with_alpha(style.bg, opacity);
        let fg = with_alpha(style.fg, opacity);
        let r = style.radius.round().clamp(0.0, 255.0) as u8;
        let arrow_x = target.center().x.clamp(x + 12.0, x + box_size.x - 12.0);

        Area::new(id.with("_tg_imp_tooltip"))
            .order(Order::Tooltip)
            .interactable(false)
            .fixed_pos(Pos2::new(x, y))
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(box_size, egui::Sense::hover());
                ui.painter().rect_filled(rect, CornerRadius::same(r), bg);

                let tri = match side {
                    TooltipSide::Top => vec![
                        Pos2::new(arrow_x - arrow, rect.max.y),
                        Pos2::new(arrow_x + arrow, rect.max.y),
                        Pos2::new(arrow_x, rect.max.y + arrow),
                    ],
                    TooltipSide::Bottom => vec![
                        Pos2::new(arrow_x - arrow, rect.min.y),
                        Pos2::new(arrow_x + arrow, rect.min.y),
                        Pos2::new(arrow_x, rect.min.y - arrow),
                    ],
                };
                ui.painter()
                    .add(Shape::convex_polygon(tri, bg, Stroke::NONE));

                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    text,
                    egui::FontId::proportional(11.5),
                    fg,
                );
            });
    }
}
