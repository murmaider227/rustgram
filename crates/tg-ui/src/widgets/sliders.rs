//! Continuous media sliders, filled sliders, and discrete settings/tab sliders.
//!
//! Port of `Telegram/SourceFiles/ui/widgets/continuous_sliders.cpp`
//! (`ContinuousSlider`, `MediaSlider`, `FilledSlider`) and
//! `Telegram/lib_ui/ui/widgets/discrete_sliders.cpp` (`SettingsSlider`).

use egui::{
    Align2, CornerRadius, CursorIcon, Key, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui,
    Vec2,
};

use crate::anim::{SimpleAnimation, Transition, color_lerp, interpolate_f32, with_alpha};
use crate::effects::{RippleMask, update_and_paint_ripple};
use crate::painter::HoverAnimState;
use crate::style::{FilledSliderStyle, MediaSliderStyle, SettingsSliderStyle};

/// Media playback / volume continuous slider mirroring `Ui::MediaSlider`.
pub struct MediaSlider<'a> {
    value: &'a mut f64,
    received_till: Option<f64>,
    style: MediaSliderStyle,
    always_display_marker: bool,
    disabled: bool,
    width: Option<f32>,
    height: f32,
}

impl<'a> MediaSlider<'a> {
    /// Create a new `MediaSlider` bound to `value` in `[0.0, 1.0]`.
    pub fn new(value: &'a mut f64, style: MediaSliderStyle) -> Self {
        Self {
            value,
            received_till: None,
            style,
            always_display_marker: false,
            disabled: false,
            width: None,
            height: 20.0,
        }
    }

    /// Show a buffered / downloaded progress segment up to `received_till` in `[0.0, 1.0]`.
    pub fn received_till(mut self, received: f64) -> Self {
        self.received_till = Some(received.clamp(0.0, 1.0));
        self
    }

    /// Always keep the circular seek thumb visible (instead of growing on hover).
    pub fn always_display_marker(mut self, always: bool) -> Self {
        self.always_display_marker = always;
        self
    }

    /// Disable interaction.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Set slider width (defaults to `ui.available_width()`).
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Render the slider in `ui`.
    pub fn show(self, ui: &mut Ui) -> Response {
        let w = self.width.unwrap_or_else(|| ui.available_width()).max(40.0);
        let sense = if self.disabled {
            Sense::hover()
        } else {
            Sense::click_and_drag()
        };
        let (rect, mut response) = ui.allocate_exact_size(Vec2::new(w, self.height), sense);

        let pad = if self.always_display_marker {
            self.style.seek_size.width * 0.5
        } else {
            0.0
        };
        let seek_rect = Rect::from_min_max(
            Pos2::new(rect.min.x + pad, rect.min.y),
            Pos2::new((rect.max.x - pad).max(rect.min.x + pad + 1.0), rect.max.y),
        );

        if !self.disabled {
            if response.hovered() || response.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if (response.dragged() || response.clicked())
                && let Some(pos) = ui.ctx().pointer_interact_pos()
            {
                let v = f64::from((pos.x - seek_rect.min.x) / seek_rect.width()).clamp(0.0, 1.0);
                if (v - *self.value).abs() > 1e-6 {
                    *self.value = v;
                    response.mark_changed();
                }
            }
            if response.has_focus() {
                ui.input(|i| {
                    if i.key_pressed(Key::ArrowRight) {
                        *self.value = (*self.value + 0.05).clamp(0.0, 1.0);
                        response.mark_changed();
                    } else if i.key_pressed(Key::ArrowLeft) {
                        *self.value = (*self.value - 0.05).clamp(0.0, 1.0);
                        response.mark_changed();
                    }
                });
            }
        }

        let over = HoverAnimState::update(
            ui.ctx(),
            response.id,
            !self.disabled && (response.hovered() || response.dragged()),
            self.style.duration_ms,
            Transition::SineInOut,
        );

        if ui.is_rect_visible(rect) {
            let val = self.value.clamp(0.0, 1.0);
            let received = self.received_till.unwrap_or(val).max(val).clamp(0.0, 1.0);
            let track_h = self.style.width;
            let track_y = rect.center().y - track_h * 0.5;
            let radius = (track_h * 0.5).ceil() as u8;

            let mid_x = seek_rect.min.x + (val as f32) * seek_rect.width();
            let till_x = seek_rect.min.x + (received as f32) * seek_rect.width();

            let active_fg = if self.disabled {
                self.style.active_fg_disabled
            } else {
                color_lerp(self.style.active_fg, self.style.active_fg_over, over)
            };
            let inactive_fg = if self.disabled {
                self.style.inactive_fg_disabled
            } else {
                color_lerp(self.style.inactive_fg, self.style.inactive_fg_over, over)
            };

            // Full inactive bar
            let full_track = Rect::from_min_size(
                Pos2::new(rect.min.x, track_y),
                Vec2::new(rect.width(), track_h),
            );
            ui.painter()
                .rect_filled(full_track, CornerRadius::same(radius), inactive_fg);

            // Received / buffered bar
            if till_x > mid_x {
                let buf_rect = Rect::from_min_max(
                    Pos2::new(rect.min.x, track_y),
                    Pos2::new(till_x.min(rect.max.x), track_y + track_h),
                );
                ui.painter().rect_filled(
                    buf_rect,
                    CornerRadius::same(radius),
                    self.style.received_till_fg,
                );
            }

            // Active played bar
            if mid_x > rect.min.x {
                let active_rect = Rect::from_min_max(
                    Pos2::new(rect.min.x, track_y),
                    Pos2::new(mid_x.min(rect.max.x), track_y + track_h),
                );
                ui.painter()
                    .rect_filled(active_rect, CornerRadius::same(radius), active_fg);
            }

            // Circular seek marker (grows smoothly with `over`, or always full when configured).
            let marker_ratio = if self.disabled {
                0.0
            } else if self.always_display_marker {
                1.0
            } else {
                over
            };
            if marker_ratio > 0.02 {
                let r = (self.style.seek_size.width * 0.5) * (marker_ratio as f32);
                let cx = mid_x.clamp(rect.min.x + r, rect.max.x - r);
                let center = Pos2::new(cx, rect.center().y);
                ui.painter().circle_filled(center, r, active_fg);
                if self.style.border_width > 0.0 {
                    ui.painter().circle_stroke(
                        center,
                        r,
                        Stroke::new(self.style.border_width, self.style.border_fg),
                    );
                }
            }
        }
        response
    }
}

/// Expanding-bar slider mirroring `Ui::FilledSlider`.
pub struct FilledSlider<'a> {
    value: &'a mut f64,
    style: FilledSliderStyle,
    disabled: bool,
    width: Option<f32>,
}

impl<'a> FilledSlider<'a> {
    /// Create a new `FilledSlider` bound to `value` in `[0.0, 1.0]`.
    pub fn new(value: &'a mut f64, style: FilledSliderStyle) -> Self {
        Self {
            value,
            style,
            disabled: false,
            width: None,
        }
    }

    /// Set width (defaults to `ui.available_width()`).
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Render the filled slider in `ui`.
    pub fn show(self, ui: &mut Ui) -> Response {
        let w = self.width.unwrap_or_else(|| ui.available_width()).max(40.0);
        let h = (self.style.full_width + 8.0).max(16.0);
        let sense = if self.disabled {
            Sense::hover()
        } else {
            Sense::click_and_drag()
        };
        let (rect, mut response) = ui.allocate_exact_size(Vec2::new(w, h), sense);
        if !self.disabled {
            if response.hovered() || response.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if (response.dragged() || response.clicked())
                && let Some(pos) = ui.ctx().pointer_interact_pos()
            {
                let v = f64::from((pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
                if (v - *self.value).abs() > 1e-6 {
                    *self.value = v;
                    response.mark_changed();
                }
            }
        }

        let over = HoverAnimState::update(
            ui.ctx(),
            response.id,
            !self.disabled && (response.hovered() || response.dragged()),
            self.style.duration_ms,
            Transition::SineInOut,
        );

        if ui.is_rect_visible(rect) {
            let line_w = interpolate_f32(self.style.line_width, self.style.full_width, over as f32);
            let y0 = rect.max.y - line_w;
            let mid_x = rect.min.x + (self.value.clamp(0.0, 1.0) as f32) * rect.width();

            let active_col = if self.disabled {
                self.style.disabled_fg
            } else {
                self.style.active_fg
            };
            if mid_x > rect.min.x {
                ui.painter().rect_filled(
                    Rect::from_min_max(Pos2::new(rect.min.x, y0), Pos2::new(mid_x, rect.max.y)),
                    CornerRadius::ZERO,
                    active_col,
                );
            }
            if rect.max.x > mid_x {
                let inactive_alpha = (0.35 + 0.65 * over).clamp(0.0, 1.0);
                ui.painter().rect_filled(
                    Rect::from_min_max(Pos2::new(mid_x, y0), rect.max),
                    CornerRadius::ZERO,
                    with_alpha(self.style.inactive_fg, inactive_alpha),
                );
            }
        }

        response
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct DiscreteSliderAnim {
    active_index: usize,
    initialized: bool,
    bar_x: SimpleAnimation,
    bar_w: SimpleAnimation,
}

/// Discrete labeled slider / animated underline tab bar mirroring `Ui::SettingsSlider`
/// and `defaultTabsSlider`.
pub struct SettingsSlider<'a> {
    id_salt: &'a str,
    selected: &'a mut usize,
    sections: &'a [&'a str],
    style: SettingsSliderStyle,
    width: Option<f32>,
}

impl<'a> SettingsSlider<'a> {
    /// Create a new discrete slider / tab strip.
    pub fn new(
        id_salt: &'a str,
        selected: &'a mut usize,
        sections: &'a [&'a str],
        style: SettingsSliderStyle,
    ) -> Self {
        Self {
            id_salt,
            selected,
            sections,
            style,
            width: None,
        }
    }

    /// Override total width.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Render the discrete slider / tab bar.
    pub fn show(self, ui: &mut Ui) -> Response {
        let count = self.sections.len().max(1);
        *self.selected = (*self.selected).min(count - 1);

        let w = self.width.unwrap_or_else(|| ui.available_width()).max(60.0);
        let id = ui.make_persistent_id(self.id_salt);
        let (rect, mut response) =
            ui.allocate_exact_size(Vec2::new(w, self.style.height), Sense::click());

        let inner_w = (w - 2.0 * self.style.padding).max(1.0);
        let section_w = inner_w / count as f32;
        let font_id = self.style.label_style.font.font_id();

        let label_widths: Vec<f32> = self
            .sections
            .iter()
            .map(|s| {
                ui.painter()
                    .layout_no_wrap((*s).to_owned(), font_id.clone(), self.style.label_fg)
                    .size()
                    .x
            })
            .collect();

        let target_bar = |idx: usize| -> (f32, f32) {
            let sx = rect.min.x + self.style.padding + idx as f32 * section_w;
            if self.style.bar_snap_to_label {
                let lw = label_widths
                    .get(idx)
                    .copied()
                    .unwrap_or(section_w)
                    .min(section_w);
                (sx + (section_w - lw) * 0.5, lw)
            } else {
                (
                    sx + self.style.bar_skip,
                    (section_w - 2.0 * self.style.bar_skip).max(4.0),
                )
            }
        };

        // Handle clicks on each section.
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let mut down_section: Option<usize> = None;
        if let Some(pos) = ui.ctx().pointer_interact_pos()
            && rect.contains(pos)
        {
            let idx = (((pos.x - rect.min.x - self.style.padding) / section_w).floor() as isize)
                .clamp(0, count as isize - 1) as usize;
            if response.is_pointer_button_down_on() {
                down_section = Some(idx);
            }
            if response.clicked() && *self.selected != idx {
                *self.selected = idx;
                response.mark_changed();
            }
        }

        let now = ui.ctx().input(|i| i.time);
        let anim_key = id.with("_tg_discrete_anim");
        let mut anim = ui
            .ctx()
            .data_mut(|d| d.get_temp::<DiscreteSliderAnim>(anim_key))
            .unwrap_or_default();
        let (tx, tw) = target_bar(*self.selected);
        if !anim.initialized {
            anim.initialized = true;
            anim.active_index = *self.selected;
            anim.bar_x
                .start(now, f64::from(tx), f64::from(tx), 0, Transition::SineInOut);
            anim.bar_w
                .start(now, f64::from(tw), f64::from(tw), 0, Transition::SineInOut);
        } else if anim.active_index != *self.selected {
            anim.active_index = *self.selected;
            anim.bar_x.change(
                now,
                f64::from(tx),
                self.style.duration_ms,
                Transition::SineInOut,
            );
            anim.bar_w.change(
                now,
                f64::from(tw),
                self.style.duration_ms,
                Transition::SineInOut,
            );
            ui.ctx().request_repaint();
        }
        let cur_x = anim.bar_x.sample(now, f64::from(tx)) as f32;
        let cur_w = anim.bar_w.sample(now, f64::from(tw)) as f32;
        if anim.bar_x.animating() || anim.bar_w.animating() {
            ui.ctx().request_repaint();
        }
        ui.ctx().data_mut(|d| d.insert_temp(anim_key, anim));

        if ui.is_rect_visible(rect) {
            let r = self.style.bar_radius.round().clamp(0.0, 255.0) as u8;

            // Section backgrounds/ripples + labels
            for (i, label) in self.sections.iter().enumerate() {
                let srect = Rect::from_min_size(
                    Pos2::new(
                        rect.min.x + self.style.padding + i as f32 * section_w,
                        rect.min.y,
                    ),
                    Vec2::new(section_w, self.style.height),
                );
                let is_active = i == *self.selected;
                if self.style.ripple.show_duration_ms > 0 {
                    let ripple_col = if is_active {
                        self.style.ripple_bg_active
                    } else {
                        self.style.ripple_bg
                    };
                    update_and_paint_ripple(
                        ui.painter(),
                        id.with(i),
                        srect,
                        RippleMask::Rect,
                        &self.style.ripple,
                        down_section == Some(i),
                        Some(ripple_col),
                    );
                }

                // Inactive bar track segment (if non-transparent)
                if self.style.bar_fg.a() > 0 {
                    let bar_rect = Rect::from_min_size(
                        Pos2::new(
                            srect.min.x + self.style.bar_skip,
                            rect.min.y + self.style.bar_top,
                        ),
                        Vec2::new(
                            (section_w - 2.0 * self.style.bar_skip).max(2.0),
                            self.style.bar_stroke,
                        ),
                    );
                    ui.painter()
                        .rect_filled(bar_rect, CornerRadius::same(r), self.style.bar_fg);
                }

                let fg = if is_active {
                    self.style.label_fg_active
                } else {
                    self.style.label_fg
                };
                ui.painter().text(
                    Pos2::new(srect.center().x, rect.min.y + self.style.label_top),
                    Align2::CENTER_TOP,
                    *label,
                    font_id.clone(),
                    fg,
                );
            }

            // Sliding active bar
            let active_bar = Rect::from_min_size(
                Pos2::new(cur_x, rect.min.y + self.style.bar_top),
                Vec2::new(cur_w, self.style.bar_stroke),
            );
            ui.painter()
                .rect_filled(active_bar, CornerRadius::same(r), self.style.bar_fg_active);

            // Subtle bottom hairline for tab bars
            if self.style.bar_snap_to_label {
                ui.painter().rect_stroke(
                    Rect::from_min_max(Pos2::new(rect.min.x, rect.max.y - 1.0), rect.max),
                    CornerRadius::ZERO,
                    Stroke::new(1.0, with_alpha(self.style.label_fg, 0.12)),
                    StrokeKind::Inside,
                );
            }
        }

        response
    }
}
