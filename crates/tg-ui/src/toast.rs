//! Transient toast notifications with fade and edge-slide transitions.
//!
//! Port of `Telegram/lib_ui/ui/toast/toast.h`,
//! `Telegram/lib_ui/ui/toast/toast.cpp`, and
//! `Telegram/lib_ui/ui/toast/toast_widget.cpp`.

use egui::{Area, CornerRadius, Id, Order, Pos2, Rect, Vec2};

use crate::anim::{Transition, interpolate_f32, with_alpha};
use crate::style::ToastStyle;
use crate::text::{RichText, build_layout_job};
use crate::widgets::buttons::{IconKind, paint_icon};

/// Screen edge where a toast attaches (or center fade when `None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToastAttach {
    /// Centered fade-in / fade-out toast.
    #[default]
    None,
    Top,
    Bottom,
    Left,
    Right,
}

/// Configuration for a toast notification (`Ui::Toast::Config`).
#[derive(Debug, Clone)]
pub struct ToastConfig {
    pub title: Option<String>,
    pub text: RichText,
    pub icon: Option<IconKind>,
    pub attach: ToastAttach,
    /// Hold duration in ms before fade/slide-out begins (tdesktop default: 2500ms).
    pub duration_ms: u32,
}

impl ToastConfig {
    /// Simple text toast with default hold duration (2500ms).
    pub fn simple(text: impl Into<String>) -> Self {
        Self {
            title: None,
            text: RichText::plain(text),
            icon: None,
            attach: ToastAttach::Bottom,
            duration_ms: 2500,
        }
    }

    /// Optional bold title above body text.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Optional leading icon.
    pub fn with_icon(mut self, icon: IconKind) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Set attachment edge (or `ToastAttach::None` for centered fade).
    pub fn with_attach(mut self, attach: ToastAttach) -> Self {
        self.attach = attach;
        self
    }

    /// Set hold duration in milliseconds.
    pub fn with_duration_ms(mut self, duration_ms: u32) -> Self {
        self.duration_ms = duration_ms;
        self
    }
}

#[derive(Debug, Clone)]
struct ActiveToast {
    id: u64,
    config: ToastConfig,
    created_sec: f64,
}

/// Manages active toast notifications and paints them above window content.
#[derive(Debug, Default)]
pub struct ToastManager {
    toasts: Vec<ActiveToast>,
    next_id: u64,
}

impl ToastManager {
    /// Create an empty toast manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueue a new toast notification (replaces previous toast on the same attach side
    /// like tdesktop's single-slot manager).
    pub fn show_toast(&mut self, ctx: &egui::Context, config: ToastConfig) {
        let now = ctx.input(|i| i.time);
        self.toasts.retain(|t| t.config.attach != config.attach);
        self.next_id += 1;
        self.toasts.push(ActiveToast {
            id: self.next_id,
            config,
            created_sec: now,
        });
        ctx.request_repaint();
    }

    /// Clear all active toasts.
    pub fn clear(&mut self) {
        self.toasts.clear();
    }

    /// Number of currently live toasts.
    pub fn active_count(&self) -> usize {
        self.toasts.len()
    }

    /// Advance and render all active toasts within `bounds` (normally `ctx.viewport_rect()`).
    pub fn paint(&mut self, ctx: &egui::Context, bounds: Rect, style: &ToastStyle) {
        if self.toasts.is_empty() {
            return;
        }
        let now = ctx.input(|i| i.time);
        let mut keep = Vec::with_capacity(self.toasts.len());

        for toast in self.toasts.drain(..) {
            let elapsed_ms = ((now - toast.created_sec).max(0.0)) * 1000.0;
            let hold = f64::from(toast.config.duration_ms);
            let (in_ms, out_ms) = if toast.config.attach == ToastAttach::None {
                (
                    f64::from(style.duration_fade_in_ms.max(1)),
                    f64::from(style.duration_fade_out_ms.max(1)),
                )
            } else {
                let s = f64::from(style.duration_slide_ms.max(1));
                (s, s)
            };

            let total_ms = in_ms + hold + out_ms;
            if elapsed_ms >= total_ms {
                continue;
            }

            let level = if elapsed_ms < in_ms {
                let dt = (elapsed_ms / in_ms).clamp(0.0, 1.0);
                Transition::EaseOutCirc.eval(1.0, dt)
            } else if elapsed_ms < in_ms + hold {
                1.0
            } else {
                let dt = ((elapsed_ms - in_ms - hold) / out_ms).clamp(0.0, 1.0);
                1.0 - Transition::SineInOut.eval(1.0, dt)
            };

            paint_single_toast(ctx, bounds, &toast, style, level);

            if elapsed_ms < in_ms || elapsed_ms >= in_ms + hold {
                ctx.request_repaint();
            } else {
                let rem = (in_ms + hold - elapsed_ms) / 1000.0;
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(rem.max(0.02)));
            }
            keep.push(toast);
        }

        self.toasts = keep;
    }
}

fn paint_single_toast(
    ctx: &egui::Context,
    bounds: Rect,
    toast: &ActiveToast,
    style: &ToastStyle,
    level: f64,
) {
    let mut combined = RichText::new();
    if let Some(title) = &toast.config.title {
        combined = combined.push_bold(title).push_plain("\n");
    }
    let offset = combined.text.len();
    combined.text.push_str(&toast.config.text.text);
    for sp in &toast.config.text.spans {
        combined.spans.push(crate::text::TextSpan {
            start: sp.start + offset,
            end: sp.end + offset,
            kind: sp.kind.clone(),
        });
    }

    let icon_pad = if toast.config.icon.is_some() {
        28.0
    } else {
        0.0
    };
    let max_text_w = (style.max_width - style.padding.horizontal() - icon_pad)
        .min(
            (bounds.width() - style.margin.horizontal() - style.padding.horizontal() - icon_pad)
                .max(60.0),
        )
        .max(60.0);

    let opacity = if toast.config.attach == ToastAttach::None {
        level
    } else {
        level.clamp(0.0, 1.0)
    };
    let fg = with_alpha(style.fg, opacity);
    let bg = with_alpha(style.bg, opacity);

    let job = build_layout_job(
        &combined,
        &style.style,
        &style.palette,
        fg,
        max_text_w,
        None,
        true,
    );
    let galley = ctx.fonts_mut(|f| f.layout_job(job));

    let width = (galley.size().x + style.padding.horizontal() + icon_pad)
        .clamp(style.min_width, style.max_width);
    let height = (galley.size().y + style.padding.vertical()).max(36.0);

    let middle_x = bounds.center().x - width * 0.5;
    let middle_y = bounds.center().y - height * 0.5;
    let t = level as f32;

    let pos = match toast.config.attach {
        ToastAttach::None => Pos2::new(middle_x, middle_y),
        ToastAttach::Top => Pos2::new(
            middle_x,
            interpolate_f32(bounds.min.y - height, bounds.min.y + style.margin.top, t),
        ),
        ToastAttach::Bottom => Pos2::new(
            middle_x,
            interpolate_f32(bounds.max.y, bounds.max.y - height - style.margin.bottom, t),
        ),
        ToastAttach::Left => Pos2::new(
            interpolate_f32(bounds.min.x - width, bounds.min.x + style.margin.left, t),
            middle_y,
        ),
        ToastAttach::Right => Pos2::new(
            interpolate_f32(bounds.max.x, bounds.max.x - width - style.margin.right, t),
            middle_y,
        ),
    };

    Area::new(Id::unique(("tg_toast", toast.id)))
        .order(Order::Foreground)
        .interactable(false)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
            let r = style.radius.round().clamp(0.0, 255.0) as u8;
            ui.painter().rect_filled(rect, CornerRadius::same(r), bg);

            if let Some(icon) = toast.config.icon {
                let icon_rect = Rect::from_center_size(
                    Pos2::new(rect.min.x + style.padding.left + 8.0, rect.center().y),
                    Vec2::splat(16.0),
                );
                paint_icon(ui.painter(), icon_rect, icon, fg);
            }

            let text_pos = Pos2::new(
                rect.min.x + style.padding.left + icon_pad,
                rect.center().y - galley.size().y * 0.5,
            );
            ui.painter().galley(text_pos, galley, fg);
        });
}
