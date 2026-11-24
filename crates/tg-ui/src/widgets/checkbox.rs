//! Animated Checkbox, Radio button, and Toggle switch widgets.
//!
//! Port of `Telegram/lib_ui/ui/widgets/checkbox.h` and
//! `Telegram/lib_ui/ui/widgets/checkbox.cpp` (`CheckView`, `RadioView`,
//! `ToggleView`, and `Checkbox`).

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Id, Painter, Pos2, Rect, Response, Role, Sense,
    Shape, Stroke, StrokeKind, Ui, Vec2, WidgetInfo,
};

use crate::anim::{SimpleAnimation, Transition, color_lerp, interpolate_f32, with_alpha};
use crate::effects::{RippleMask, update_and_paint_ripple};
use crate::style::{CheckStyle, CheckboxStyle, FontSpec, RadioStyle, ToggleStyle, constants};

#[derive(Debug, Clone, Copy, Default)]
struct CheckAnimState {
    checked: bool,
    initialized: bool,
    anim: SimpleAnimation,
}

fn update_check_anim(ctx: &egui::Context, id: Id, checked: bool, duration_ms: u32) -> f64 {
    let now = ctx.input(|i| i.time);
    let key = id.with("_tg_check_anim");
    let mut state = ctx
        .data_mut(|d| d.get_temp::<CheckAnimState>(key))
        .unwrap_or_default();

    if !state.initialized {
        state.initialized = true;
        state.checked = checked;
    } else if state.checked != checked {
        state.checked = checked;
        state.anim.change(
            now,
            if checked { 1.0 } else { 0.0 },
            duration_ms,
            Transition::SineInOut,
        );
    }
    let val = state
        .anim
        .sample(now, if state.checked { 1.0 } else { 0.0 });
    if state.anim.animating() {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(key, state));
    val
}

/// Animated checkbox widget mirroring `Ui::Checkbox` + `Ui::CheckView`.
pub struct Checkbox<'a> {
    checked: &'a mut bool,
    label: &'a str,
    style: CheckboxStyle,
    check_style: CheckStyle,
    enabled: bool,
}

impl<'a> Checkbox<'a> {
    /// Create a new `Checkbox`.
    pub fn new(
        checked: &'a mut bool,
        label: &'a str,
        style: CheckboxStyle,
        check_style: CheckStyle,
    ) -> Self {
        Self {
            checked,
            label,
            style,
            check_style,
            enabled: true,
        }
    }

    /// Set enabled/disabled state (`disabledOpacity = 0.5`).
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Render the checkbox and toggle `*checked` on click.
    pub fn show(self, ui: &mut Ui) -> Response {
        let font_id = self.style.style.font.font_id();
        let text_galley =
            ui.painter()
                .layout_no_wrap(self.label.to_owned(), font_id.clone(), self.style.text_fg);

        let box_size = self.check_style.diameter;
        let height =
            box_size.max(text_galley.size().y) + self.style.margin.top + self.style.margin.bottom;
        let width = self.style.check_position.x
            + box_size
            + if self.label.is_empty() {
                self.style.margin.right
            } else {
                self.style.text_position.x + text_galley.size().x + self.style.margin.right
            };

        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, mut response) = ui.allocate_exact_size(Vec2::new(width, height), sense);
        if self.enabled && response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if self.enabled && response.clicked() {
            *self.checked = !*self.checked;
            response.mark_changed();
        }
        response.widget_info(|| {
            WidgetInfo::selected(Role::CheckBox, self.enabled, *self.checked, self.label)
        });

        let toggled = update_check_anim(
            ui.ctx(),
            response.id,
            *self.checked,
            self.check_style.duration_ms,
        );

        if ui.is_rect_visible(rect) {
            let opacity = if self.enabled {
                1.0
            } else {
                f64::from(self.style.disabled_opacity)
            };
            let check_rect = Rect::from_min_size(
                Pos2::new(
                    rect.min.x + self.style.check_position.x,
                    rect.center().y - box_size * 0.5,
                ),
                Vec2::splat(box_size),
            );

            if self.enabled {
                let ripple_pad = self.check_style.ripple_area_padding;
                let ripple_rect = check_rect.expand(ripple_pad);
                let ripple_color =
                    color_lerp(self.style.ripple_bg, self.style.ripple_bg_active, toggled);
                update_and_paint_ripple(
                    ui.painter(),
                    response.id,
                    ripple_rect,
                    RippleMask::Ellipse,
                    &self.style.ripple,
                    response.is_pointer_button_down_on(),
                    Some(ripple_color),
                );
            }

            paint_check_view(
                ui.painter(),
                check_rect,
                &self.check_style,
                toggled,
                opacity,
            );

            if !self.label.is_empty() {
                let text_fg = with_alpha(
                    color_lerp(self.style.text_fg, self.style.text_fg_active, toggled),
                    opacity,
                );
                let tx = check_rect.max.x + self.style.text_position.x;
                ui.painter().text(
                    Pos2::new(tx, rect.center().y),
                    Align2::LEFT_CENTER,
                    self.label,
                    font_id,
                    text_fg,
                );
            }
        }
        response
    }
}

/// Paint `Ui::CheckView` (rounded square + animated fill + checkmark).
pub fn paint_check_view(
    painter: &Painter,
    rect: Rect,
    st: &CheckStyle,
    toggled: f64,
    opacity: f64,
) {
    let border_color = with_alpha(color_lerp(st.untoggled_fg, st.toggled_fg, toggled), opacity);
    let fill_color = with_alpha(color_lerp(st.bg, border_color, toggled), opacity);
    let inset = st.thickness * 0.5;
    let draw_rect = rect.shrink(inset);
    let radius = (constants::ROUND_RADIUS_SMALL).round() as u8;

    painter.rect_filled(draw_rect, CornerRadius::same(radius), fill_color);
    painter.rect_stroke(
        draw_rect,
        CornerRadius::same(radius),
        Stroke::new(st.thickness, border_color),
        StrokeKind::Middle,
    );

    if toggled > 0.05 {
        let check_fg = with_alpha(st.check_fg, toggled * opacity);
        let c = rect.center();
        let s = rect.width();
        let p0 = Pos2::new(c.x - s * 0.25, c.y + s * 0.02);
        let p1 = Pos2::new(c.x - s * 0.05, c.y + s * 0.22);
        let p2 = Pos2::new(c.x + s * 0.28, c.y - s * 0.19);
        painter.add(Shape::line(vec![p0, p1, p2], Stroke::new(2.0, check_fg)));
    }
}

/// Animated radio button mirroring `Ui::Radiobutton` + `Ui::RadioView`.
pub struct Radio<'a, T: PartialEq + Copy> {
    current: &'a mut T,
    value: T,
    label: &'a str,
    style: CheckboxStyle,
    radio_style: RadioStyle,
    enabled: bool,
}

impl<'a, T: PartialEq + Copy> Radio<'a, T> {
    /// Create a new radio option bound to `current`.
    pub fn new(
        current: &'a mut T,
        value: T,
        label: &'a str,
        style: CheckboxStyle,
        radio_style: RadioStyle,
    ) -> Self {
        Self {
            current,
            value,
            label,
            style,
            radio_style,
            enabled: true,
        }
    }

    /// Set enabled state.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Render the radio button.
    pub fn show(self, ui: &mut Ui) -> Response {
        let is_selected = *self.current == self.value;
        let font_id = self.style.style.font.font_id();
        let text_galley =
            ui.painter()
                .layout_no_wrap(self.label.to_owned(), font_id.clone(), self.style.text_fg);

        let diam = self.radio_style.diameter;
        let height =
            diam.max(text_galley.size().y) + self.style.margin.top + self.style.margin.bottom;
        let width = self.style.check_position.x
            + diam
            + if self.label.is_empty() {
                self.style.margin.right
            } else {
                self.style.text_position.x + text_galley.size().x + self.style.margin.right
            };

        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, mut response) = ui.allocate_exact_size(Vec2::new(width, height), sense);
        if self.enabled && response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if self.enabled && response.clicked() && *self.current != self.value {
            *self.current = self.value;
            response.mark_changed();
        }
        response.widget_info(|| {
            WidgetInfo::selected(
                Role::RadioButton,
                self.enabled,
                *self.current == self.value,
                self.label,
            )
        });

        let toggled = update_check_anim(
            ui.ctx(),
            response.id,
            *self.current == self.value,
            self.radio_style.duration_ms,
        );

        if ui.is_rect_visible(rect) {
            let opacity = if self.enabled {
                1.0
            } else {
                f64::from(self.style.disabled_opacity)
            };
            let radio_rect = Rect::from_min_size(
                Pos2::new(
                    rect.min.x + self.style.check_position.x,
                    rect.center().y - diam * 0.5,
                ),
                Vec2::splat(diam),
            );

            if self.enabled {
                let ripple_rect = radio_rect.expand(self.radio_style.ripple_area_padding);
                let ripple_color =
                    color_lerp(self.style.ripple_bg, self.style.ripple_bg_active, toggled);
                update_and_paint_ripple(
                    ui.painter(),
                    response.id,
                    ripple_rect,
                    RippleMask::Ellipse,
                    &self.style.ripple,
                    response.is_pointer_button_down_on(),
                    Some(ripple_color),
                );
            }

            paint_radio_view(
                ui.painter(),
                radio_rect,
                &self.radio_style,
                toggled,
                opacity,
            );

            if !self.label.is_empty() {
                let text_fg = with_alpha(
                    color_lerp(
                        self.style.text_fg,
                        self.style.text_fg_active,
                        if is_selected { 1.0 } else { toggled },
                    ),
                    opacity,
                );
                let tx = radio_rect.max.x + self.style.text_position.x;
                ui.painter().text(
                    Pos2::new(tx, rect.center().y),
                    Align2::LEFT_CENTER,
                    self.label,
                    font_id,
                    text_fg,
                );
            }
        }
        response
    }
}

/// Paint `Ui::RadioView` matching the exact outer ring + animated inner dot geometry
/// from `RadioView::paint` in `checkbox.cpp` (lines 413-439).
pub fn paint_radio_view(
    painter: &Painter,
    rect: Rect,
    st: &RadioStyle,
    toggled: f64,
    opacity: f64,
) {
    let fg = with_alpha(color_lerp(st.untoggled_fg, st.toggled_fg, toggled), opacity);
    let outer_skip = (st.outer_skip_tenths / 10.0) + (st.thickness * 0.5);
    let outer_radius = (st.diameter * 0.5 - outer_skip).max(1.0);
    if st.bg.a() > 0 {
        painter.circle_filled(rect.center(), outer_radius, with_alpha(st.bg, opacity));
    }
    painter.circle_stroke(rect.center(), outer_radius, Stroke::new(st.thickness, fg));

    if toggled > 0.01 {
        let skip0 = st.diameter * 0.5;
        let skip1 = st.skip_tenths / 10.0;
        let check_skip = skip0 * (1.0 - toggled as f32) + skip1 * (toggled as f32);
        let inner_r = (st.diameter * 0.5 - check_skip).max(0.0);
        if inner_r > 0.2 {
            painter.circle_filled(rect.center(), inner_r, fg);
        }
    }
}

/// Animated toggle switch mirroring `Ui::ToggleView` (optionally paired with a label).
pub struct Toggle<'a> {
    checked: &'a mut bool,
    label: Option<&'a str>,
    style: ToggleStyle,
    label_color: Color32,
    font: FontSpec,
    enabled: bool,
}

impl<'a> Toggle<'a> {
    /// Create a standalone toggle switch.
    pub fn new(checked: &'a mut bool, style: ToggleStyle) -> Self {
        Self {
            checked,
            label: None,
            style,
            label_color: Color32::BLACK,
            font: FontSpec::from_tg(tg_style::st::normal_font),
            enabled: true,
        }
    }

    /// Attach a label beside the toggle.
    pub fn with_label(mut self, label: &'a str, color: Color32) -> Self {
        self.label = Some(label);
        self.label_color = color;
        self
    }

    /// Set enabled state.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Natural pixel size of the toggle track + border (`ToggleView::getSize`).
    pub fn toggle_size(st: &ToggleStyle) -> Vec2 {
        Vec2::new(
            2.0 * st.border + st.diameter + st.width,
            2.0 * st.border + st.diameter,
        )
    }

    /// Show the toggle in `ui`.
    pub fn show(self, ui: &mut Ui) -> Response {
        let tsize = Self::toggle_size(&self.style);
        let font_id = self.font.font_id();
        let label_galley = self.label.map(|l| {
            ui.painter()
                .layout_no_wrap(l.to_owned(), font_id.clone(), self.label_color)
        });
        let gap = 12.0;
        let total_w = tsize.x + label_galley.as_ref().map_or(0.0, |g| gap + g.size().x);
        let total_h = tsize
            .y
            .max(label_galley.as_ref().map_or(0.0, |g| g.size().y))
            + 8.0;

        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, mut response) = ui.allocate_exact_size(Vec2::new(total_w, total_h), sense);
        if self.enabled && response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if self.enabled && response.clicked() {
            *self.checked = !*self.checked;
            response.mark_changed();
        }
        let a11y_label = self.label.unwrap_or("Toggle");
        response.widget_info(|| {
            WidgetInfo::selected(Role::CheckBox, self.enabled, *self.checked, a11y_label)
        });

        let toggled =
            update_check_anim(ui.ctx(), response.id, *self.checked, self.style.duration_ms);

        if ui.is_rect_visible(rect) {
            let opacity = if self.enabled { 1.0 } else { 0.5 };
            let toggle_rect = Rect::from_min_size(
                Pos2::new(rect.min.x, rect.center().y - tsize.y * 0.5),
                tsize,
            );
            paint_toggle_view(ui.painter(), toggle_rect, &self.style, toggled, opacity);

            if let Some(label) = self.label {
                ui.painter().text(
                    Pos2::new(toggle_rect.max.x + gap, rect.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    font_id,
                    with_alpha(self.label_color, opacity),
                );
            }
        }
        response
    }
}

/// Paint `Ui::ToggleView` matching `ToggleView::paint` in `checkbox.cpp` (lines 86-120).
pub fn paint_toggle_view(
    painter: &Painter,
    rect: Rect,
    st: &ToggleStyle,
    toggled: f64,
    opacity: f64,
) {
    let left = rect.min.x + st.border;
    let top = rect.min.y + st.border;
    let full_width = st.diameter + st.width;
    let inner_diameter = (st.diameter - 2.0 * st.shift).max(2.0);
    let inner_radius = (inner_diameter * 0.5).ceil() as u8;

    let toggle_left = left + interpolate_f32(0.0, full_width - st.diameter, toggled as f32);
    let bg_rect = Rect::from_min_size(
        Pos2::new(left + st.shift, top + st.shift),
        Vec2::new((full_width - 2.0 * st.shift).max(2.0), inner_diameter),
    );
    let fg_color = with_alpha(color_lerp(st.untoggled_fg, st.toggled_fg, toggled), opacity);
    let thumb_bg = with_alpha(color_lerp(st.untoggled_bg, st.toggled_bg, toggled), opacity);

    painter.rect_filled(bg_rect, CornerRadius::same(inner_radius), fg_color);

    let thumb_center = Pos2::new(toggle_left + st.diameter * 0.5, top + st.diameter * 0.5);
    let thumb_r = st.diameter * 0.5;
    painter.circle_filled(thumb_center, thumb_r, thumb_bg);
    painter.circle_stroke(thumb_center, thumb_r, Stroke::new(st.border, fg_color));

    // Optional X -> V morph inside the circular thumb (`paintXV`).
    if st.xsize > 0.0 && st.stroke > 0.0 {
        let t = toggled as f32;
        let half = st.xsize * 0.45;
        let stroke = Stroke::new(st.stroke, fg_color);
        // Morph the two strokes of an 'X' smoothly into a checkmark 'V'.
        let a0 = Pos2::new(
            interpolate_f32(thumb_center.x - half, thumb_center.x - half * 0.9, t),
            interpolate_f32(thumb_center.y - half, thumb_center.y + half * 0.05, t),
        );
        let a1 = Pos2::new(
            interpolate_f32(thumb_center.x + half, thumb_center.x - half * 0.15, t),
            interpolate_f32(thumb_center.y + half, thumb_center.y + half * 0.8, t),
        );
        let b0 = Pos2::new(
            interpolate_f32(thumb_center.x - half, thumb_center.x - half * 0.15, t),
            interpolate_f32(thumb_center.y + half, thumb_center.y + half * 0.8, t),
        );
        let b1 = Pos2::new(
            interpolate_f32(thumb_center.x + half, thumb_center.x + half * 1.05, t),
            interpolate_f32(thumb_center.y - half, thumb_center.y - half * 0.65, t),
        );
        painter.line_segment([a0, a1], stroke);
        painter.line_segment([b0, b1], stroke);
    }
}
