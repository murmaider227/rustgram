//! Scroll area with tdesktop-style auto-hiding overlay scrollbars and edge shadows.
//!
//! Port of `Telegram/lib_ui/ui/widgets/scroll_area.h` and
//! `Telegram/lib_ui/ui/widgets/scroll_area.cpp`.

use egui::{CornerRadius, Pos2, Rect, ScrollArea, Sense, Ui, Vec2};

use crate::anim::{SimpleAnimation, Transition, color_lerp, with_alpha};
use crate::style::ScrollAreaStyle;

/// Exact port of `Ui::ComputeScrollTo` in `scroll_area.cpp` (lines 23-58).
///
/// Computes the minimal scroll offset so `[to_from, to_till)` is visible within
/// a viewport of height `size` clamped to `[to_min, to_max]`. If `to_till < 0`,
/// scrolls exactly to `to_from`.
pub fn compute_scroll_to(
    mut to_from: i32,
    mut to_till: i32,
    to_min: i32,
    to_max: i32,
    current: i32,
    size: i32,
) -> i32 {
    to_from = to_from.clamp(to_min, to_max);
    let exact = to_till < 0;
    let cur_bottom = current + size;
    if !exact && to_from >= current {
        if to_till < to_from {
            to_till = to_from;
        }
        if to_till <= cur_bottom {
            return current;
        }
        let mut sc_to_from = to_till - size;
        if sc_to_from > to_from {
            sc_to_from = to_from;
        }
        sc_to_from
    } else {
        to_from
    }
}

/// Compute scrollbar thumb geometry `(thumb_y, thumb_h)` matching `ScrollBar::updateBar`
/// in `scroll_area.cpp` (lines 146-183).
pub fn compute_scrollbar_thumb(
    viewport_h: f32,
    content_h: f32,
    scroll_y: f32,
    track_h: f32,
    min_thumb_h: f32,
) -> Option<(f32, f32)> {
    let max_scroll = (content_h - viewport_h).max(0.0);
    if content_h <= viewport_h + 0.5 || max_scroll <= 0.5 || track_h < min_thumb_h {
        return None;
    }
    let raw_h = (track_h * viewport_h) / content_h;
    if raw_h >= track_h {
        return None;
    }
    let h = raw_h.max(min_thumb_h).min(track_h);
    let y = ((track_h - h) * (scroll_y / max_scroll).clamp(0.0, 1.0)).clamp(0.0, track_h - h);
    Some((y, h))
}

#[derive(Debug, Clone, Copy, Default)]
struct TgScrollBarState {
    last_scroll_y: f32,
    pending_scroll_y: Option<f32>,
    last_activity_sec: f64,
    hiding: bool,
    over_track: bool,
    over_thumb: bool,
    dragging: bool,
    drag_grab_offset: f32,
    a_opacity: SimpleAnimation,
    a_over: SimpleAnimation,
    a_bar_over: SimpleAnimation,
}

/// Vertical scroll container with tdesktop-styled auto-hiding overlay scrollbar
/// and optional top/bottom edge shadows.
pub struct TgScrollArea<'a> {
    id_salt: &'a str,
    style: ScrollAreaStyle,
    max_height: Option<f32>,
    show_shadows: bool,
}

impl<'a> TgScrollArea<'a> {
    /// Create a new `TgScrollArea`.
    pub fn new(id_salt: &'a str, style: ScrollAreaStyle) -> Self {
        Self {
            id_salt,
            style,
            max_height: None,
            show_shadows: false,
        }
    }

    /// Constrain viewport maximum height.
    pub fn max_height(mut self, height: f32) -> Self {
        self.max_height = Some(height);
        self
    }

    /// Enable 1px top/bottom shadow lines when content is scrolled past an edge.
    pub fn show_shadows(mut self, show: bool) -> Self {
        self.show_shadows = show;
        self
    }

    /// Show scrollable contents and paint tdesktop's custom overlay scrollbar.
    pub fn show<R>(self, ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
        let id = ui.make_persistent_id(self.id_salt);
        let state_key = id.with("_tg_scrollbar");
        let now = ui.ctx().input(|i| i.time);

        let mut bar_state = ui
            .ctx()
            .data_mut(|d| d.get_temp::<TgScrollBarState>(state_key))
            .unwrap_or_else(|| TgScrollBarState {
                hiding: self.style.hiding_ms > 0,
                ..Default::default()
            });

        // Reserve inner scroll area with native egui bar hidden so we paint tdesktop's.
        let mut sa = ScrollArea::vertical()
            .id_salt(id.with("_inner"))
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .auto_shrink([false, false]);
        if let Some(mh) = self.max_height {
            sa = sa.max_height(mh);
        }
        if let Some(pending_y) = bar_state.pending_scroll_y.take() {
            sa = sa.vertical_scroll_offset(pending_y);
        }

        let output = sa.show(ui, add_contents);
        let viewport = output.inner_rect;
        let content_h = output.content_size.y;
        let scroll_y = output.state.clamped_offset().y;
        let max_scroll = (content_h - viewport.height()).max(0.0);

        if (scroll_y - bar_state.last_scroll_y).abs() > 0.25 {
            bar_state.last_scroll_y = scroll_y;
            bar_state.last_activity_sec = now;
            if bar_state.hiding {
                bar_state.hiding = false;
                bar_state
                    .a_opacity
                    .change(now, 1.0, self.style.duration_ms, Transition::SineInOut);
            }
        }

        // Paint optional top / bottom scroll shadows.
        if self.show_shadows && max_scroll > 0.5 {
            if scroll_y > self.style.topsh {
                ui.painter().rect_filled(
                    Rect::from_min_max(
                        viewport.min,
                        Pos2::new(viewport.max.x, viewport.min.y + 1.0),
                    ),
                    CornerRadius::ZERO,
                    self.style.sh_color,
                );
            }
            if scroll_y < max_scroll - self.style.bottomsh {
                ui.painter().rect_filled(
                    Rect::from_min_max(
                        Pos2::new(viewport.min.x, viewport.max.y - 1.0),
                        viewport.max,
                    ),
                    CornerRadius::ZERO,
                    self.style.sh_color,
                );
            }
        }

        let track_rect = Rect::from_min_max(
            Pos2::new(
                viewport.max.x - self.style.width,
                viewport.min.y + self.style.deltat,
            ),
            Pos2::new(viewport.max.x, viewport.max.y - self.style.deltab),
        );

        if !self.style.bar_hidden
            && let Some((thumb_y, thumb_h)) = compute_scrollbar_thumb(
                viewport.height(),
                content_h,
                scroll_y,
                track_rect.height(),
                self.style.min_height,
            )
        {
            let bar_w = (self.style.width - 2.0 * self.style.deltax).max(2.0);
            let outer_track = Rect::from_min_size(
                Pos2::new(track_rect.min.x + self.style.deltax, track_rect.min.y),
                Vec2::new(bar_w, track_rect.height()),
            );
            let thumb_rect = Rect::from_min_size(
                Pos2::new(
                    track_rect.min.x + self.style.deltax,
                    track_rect.min.y + thumb_y,
                ),
                Vec2::new(bar_w, thumb_h),
            );

            // Handle hover & drag on the overlay scrollbar.
            let bar_resp = ui.interact(
                track_rect,
                id.with("_bar_interact"),
                Sense::click_and_drag(),
            );
            let pointer = ui.ctx().pointer_hover_pos();
            let now_over_track = bar_resp.hovered() || bar_state.dragging;
            let now_over_thumb =
                pointer.is_some_and(|p| thumb_rect.expand(3.0).contains(p)) || bar_state.dragging;

            if bar_resp.drag_started()
                && let Some(pos) = ui.ctx().pointer_interact_pos()
            {
                bar_state.dragging = true;
                if thumb_rect.contains(pos) {
                    bar_state.drag_grab_offset = pos.y - thumb_rect.min.y;
                } else {
                    bar_state.drag_grab_offset = thumb_h * 0.5;
                }
            }
            if bar_resp.drag_stopped() {
                bar_state.dragging = false;
                bar_state.last_activity_sec = now;
            }
            if (bar_state.dragging || bar_resp.clicked())
                && let Some(pos) = ui.ctx().pointer_interact_pos()
            {
                let denom = (track_rect.height() - thumb_h).max(1.0);
                let new_thumb_y =
                    (pos.y - track_rect.min.y - bar_state.drag_grab_offset).clamp(0.0, denom);
                let target_y = (new_thumb_y / denom) * max_scroll;
                bar_state.pending_scroll_y = Some(target_y);
                bar_state.last_scroll_y = target_y;
                bar_state.last_activity_sec = now;
                ui.ctx().request_repaint();
            }

            // Hover transitions
            if bar_state.over_track != now_over_track {
                bar_state.over_track = now_over_track;
                bar_state.a_over.change(
                    now,
                    if now_over_track { 1.0 } else { 0.0 },
                    self.style.duration_ms,
                    Transition::SineInOut,
                );
                if now_over_track && bar_state.hiding {
                    bar_state.hiding = false;
                    bar_state.a_opacity.change(
                        now,
                        1.0,
                        self.style.duration_ms,
                        Transition::SineInOut,
                    );
                }
            }
            if bar_state.over_thumb != now_over_thumb {
                bar_state.over_thumb = now_over_thumb;
                bar_state.a_bar_over.change(
                    now,
                    if now_over_thumb { 1.0 } else { 0.0 },
                    self.style.duration_ms,
                    Transition::SineInOut,
                );
            }

            // Auto-hide check after `hiding_ms`.
            if self.style.hiding_ms > 0 && !bar_state.hiding && !now_over_track {
                let idle_ms = (now - bar_state.last_activity_sec) * 1000.0;
                if idle_ms >= f64::from(self.style.hiding_ms) {
                    bar_state.hiding = true;
                    bar_state.a_opacity.change(
                        now,
                        0.0,
                        self.style.duration_ms,
                        Transition::SineInOut,
                    );
                } else {
                    let remaining = (f64::from(self.style.hiding_ms) - idle_ms) / 1000.0;
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_secs_f64(
                            remaining.max(0.02),
                        ));
                }
            } else if self.style.hiding_ms == 0 {
                bar_state.hiding = false;
            }

            let opacity = bar_state
                .a_opacity
                .sample(now, if bar_state.hiding { 0.0 } else { 1.0 });
            let over_t = bar_state
                .a_over
                .sample(now, if bar_state.over_track { 1.0 } else { 0.0 });
            let bar_over_t = bar_state
                .a_bar_over
                .sample(now, if bar_state.over_thumb { 1.0 } else { 0.0 });

            if bar_state.a_opacity.animating()
                || bar_state.a_over.animating()
                || bar_state.a_bar_over.animating()
            {
                ui.ctx().request_repaint();
            }

            if opacity > 0.01 {
                let bg = with_alpha(
                    color_lerp(self.style.bg, self.style.bg_over, over_t),
                    opacity,
                );
                let bar = with_alpha(
                    color_lerp(self.style.bar_bg, self.style.bar_bg_over, bar_over_t),
                    opacity,
                );
                let r = if self.style.round < 0.0 {
                    (bar_w * 0.5).ceil() as u8
                } else {
                    self.style.round.round().clamp(0.0, 255.0) as u8
                };
                let fg_painter = ui.painter();
                fg_painter.rect_filled(outer_track, CornerRadius::same(r), bg);
                fg_painter.rect_filled(thumb_rect, CornerRadius::same(r), bar);
            }
        }

        ui.ctx().data_mut(|d| d.insert_temp(state_key, bar_state));
        output.inner
    }
}
