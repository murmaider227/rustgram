//! Flat and rounded single-line / multi-line input fields with animated floating
//! placeholder and expanding underline.
//!
//! Port of `Telegram/lib_ui/ui/widgets/fields/input_field.h` and
//! `Telegram/lib_ui/ui/widgets/fields/input_field.cpp`.

use egui::{
    Align2, CornerRadius, FontId, Id, Pos2, Rect, Response, ScrollArea, Stroke, StrokeKind,
    TextEdit, Ui, Vec2,
};

use crate::anim::{SimpleAnimation, Transition, color_lerp, interpolate_f32, with_alpha};
use crate::style::{FontSpec, InputFieldStyle, constants};

/// Input field line mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputFieldMode {
    #[default]
    SingleLine,
    MultiLine,
}

#[derive(Debug, Clone, Copy, Default)]
struct InputFieldAnimState {
    initialized: bool,
    focused: bool,
    placeholder_shifted: bool,
    error: bool,
    border_visible: bool,
    border_start_x: f32,
    a_focused: SimpleAnimation,
    a_placeholder_shifted: SimpleAnimation,
    a_error: SimpleAnimation,
    a_border_shown: SimpleAnimation,
    a_border_opacity: SimpleAnimation,
    a_shake: SimpleAnimation,
}

/// Telegram Desktop-style input field with:
/// - animated floating placeholder (`placeholderScale` + `placeholderShift`),
/// - bottom underline expanding outward from the click X position (`borderAnimationStart`),
/// - rounded search field mode (`borderRadius > 0`),
/// - multi-line auto-growth between `height_min` and `height_max` with scrolling,
/// - error state tinting and horizontal shake feedback.
pub struct InputField<'a> {
    id_salt: &'a str,
    text: &'a mut String,
    placeholder: &'a str,
    style: InputFieldStyle,
    mode: InputFieldMode,
    password: bool,
    error: bool,
    max_length: Option<usize>,
    width: Option<f32>,
}

impl<'a> InputField<'a> {
    /// Create a single-line `InputField`.
    pub fn single_line(
        id_salt: &'a str,
        text: &'a mut String,
        placeholder: &'a str,
        style: InputFieldStyle,
    ) -> Self {
        Self {
            id_salt,
            text,
            placeholder,
            style,
            mode: InputFieldMode::SingleLine,
            password: false,
            error: false,
            max_length: None,
            width: None,
        }
    }

    /// Create a multi-line `InputField` that grows up to `style.height_max` and scrolls.
    pub fn multi_line(
        id_salt: &'a str,
        text: &'a mut String,
        placeholder: &'a str,
        style: InputFieldStyle,
    ) -> Self {
        Self {
            mode: InputFieldMode::MultiLine,
            ..Self::single_line(id_salt, text, placeholder, style)
        }
    }

    /// Mask characters as a password input.
    pub fn password(mut self, password: bool) -> Self {
        self.password = password;
        self
    }

    /// Display error border and placeholder tint.
    pub fn error(mut self, error: bool) -> Self {
        self.error = error;
        self
    }

    /// Enforce and display an optional maximum character count.
    pub fn max_length(mut self, limit: usize) -> Self {
        self.max_length = Some(limit);
        self
    }

    /// Override field width (defaults to `ui.available_width()`).
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Trigger a horizontal shake animation on the field with `id`.
    pub fn trigger_shake(ctx: &egui::Context, id: Id) {
        let now = ctx.input(|i| i.time);
        let key = id.with("_tg_input_anim");
        let mut st = ctx
            .data_mut(|d| d.get_temp::<InputFieldAnimState>(key))
            .unwrap_or_default();
        st.a_shake.start(
            now,
            0.0,
            1.0,
            constants::SHAKE_DURATION_MS,
            Transition::Linear,
        );
        ctx.data_mut(|d| d.insert_temp(key, st));
        ctx.request_repaint();
    }

    /// Render the input field in `ui`.
    pub fn show(self, ui: &mut Ui) -> Response {
        let id = ui.make_persistent_id(self.id_salt);
        let now = ui.ctx().input(|i| i.time);
        let anim_key = id.with("_tg_input_anim");
        let mut anim = ui
            .ctx()
            .data_mut(|d| d.get_temp::<InputFieldAnimState>(anim_key))
            .unwrap_or_default();

        if !anim.initialized {
            anim.initialized = true;
            anim.placeholder_shifted = !self.text.is_empty();
            anim.error = self.error;
            anim.border_visible = self.error;
        }

        let width = self
            .width
            .unwrap_or_else(|| ui.available_width())
            .max(self.style.width_min);

        let line_h = self.style.style.font.line_height();
        let line_count = match self.mode {
            InputFieldMode::SingleLine => 1,
            InputFieldMode::MultiLine => {
                let inner_w = (width - self.style.text_margins.horizontal()).max(20.0);
                estimate_wrapped_lines(self.text, inner_w, self.style.style.font.size)
            }
        };
        let desired_h = (self.style.text_margins.vertical() + line_count as f32 * line_h)
            .clamp(self.style.height_min, self.style.height_max);

        let (outer_rect, _) =
            ui.allocate_exact_size(Vec2::new(width, desired_h), egui::Sense::hover());

        // Shake offset (`shakeShift = 4px`, 3 oscillations over `shakeDuration = 300ms`).
        let shake_t = anim.a_shake.sample(now, 0.0);
        let shake_dx = if anim.a_shake.animating() {
            ui.ctx().request_repaint();
            ((shake_t * std::f64::consts::TAU * 3.0).sin() * (1.0 - shake_t)) as f32
                * constants::SHAKE_SHIFT
        } else {
            0.0
        };
        let rect = outer_rect.translate(Vec2::new(shake_dx, 0.0));

        // Sample current animation factors before painting background/border.
        let focused_deg = anim
            .a_focused
            .sample(now, if anim.focused { 1.0 } else { 0.0 });
        let error_deg = anim.a_error.sample(now, if anim.error { 1.0 } else { 0.0 });
        let ph_shift_deg = anim
            .a_placeholder_shifted
            .sample(now, if anim.placeholder_shifted { 1.0 } else { 0.0 });
        let border_shown = anim.a_border_shown.sample(now, 1.0);
        let border_opacity = anim
            .a_border_opacity
            .sample(now, if anim.border_visible { 1.0 } else { 0.0 });

        if anim.a_focused.animating()
            || anim.a_error.animating()
            || anim.a_placeholder_shifted.animating()
            || anim.a_border_shown.animating()
            || anim.a_border_opacity.animating()
        {
            ui.ctx().request_repaint();
        }

        // Paint surrounding background & border (matches `paintFlatSurrounding` / rounded mode).
        if ui.is_rect_visible(rect) {
            paint_input_surrounding(
                ui.painter(),
                rect,
                &self.style,
                focused_deg,
                error_deg,
                border_shown,
                border_opacity,
                anim.border_start_x,
            );
        }

        // Inner text edit region.
        let text_rect = Rect::from_min_max(
            Pos2::new(
                rect.min.x + self.style.text_margins.left,
                rect.min.y + self.style.text_margins.top,
            ),
            Pos2::new(
                rect.max.x - self.style.text_margins.right,
                rect.max.y - self.style.text_margins.bottom,
            ),
        );

        let font_id = self.style.style.font.font_id();
        let edit_id = id.with("_edit");
        let text_fg = self.style.text_fg;

        let mut child_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(text_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child_ui.visuals_mut().override_text_color = Some(text_fg);
        child_ui.visuals_mut().selection.bg_fill = with_alpha(self.style.border_fg_active, 0.27);
        child_ui.visuals_mut().selection.stroke = Stroke::new(1.5, self.style.border_fg_active);

        let response = match self.mode {
            InputFieldMode::SingleLine => {
                let mut te = TextEdit::singleline(self.text)
                    .id(edit_id)
                    .frame(egui::Frame::NONE)
                    .font(font_id.clone())
                    .text_color(text_fg)
                    .password(self.password)
                    .desired_width(text_rect.width());
                if let Some(limit) = self.max_length {
                    te = te.char_limit(limit);
                }
                child_ui.add_sized(text_rect.size(), te)
            }
            InputFieldMode::MultiLine => {
                let needs_scroll = desired_h >= self.style.height_max - 0.5;
                let mut inner_resp: Option<Response> = None;
                ScrollArea::vertical()
                    .id_salt(id.with("_scroll"))
                    .max_height(text_rect.height())
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(if needs_scroll {
                        egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded
                    } else {
                        egui::scroll_area::ScrollBarVisibility::AlwaysHidden
                    })
                    .show(&mut child_ui, |ui| {
                        let mut te = TextEdit::multiline(self.text)
                            .id(edit_id)
                            .frame(egui::Frame::NONE)
                            .font(font_id.clone())
                            .text_color(text_fg)
                            .desired_width(text_rect.width())
                            .desired_rows(1);
                        if let Some(limit) = self.max_length {
                            te = te.char_limit(limit);
                        }
                        inner_resp = Some(ui.add(te));
                    });
                inner_resp
                    .unwrap_or_else(|| child_ui.allocate_response(Vec2::ZERO, egui::Sense::hover()))
            }
        };

        // Clicking anywhere on the outer container focuses the text edit.
        let click_rect = rect;
        if ui.input(|i| i.pointer.primary_pressed())
            && let Some(pos) = ui.ctx().pointer_interact_pos()
            && click_rect.contains(pos)
        {
            anim.border_start_x = (pos.x - rect.min.x).clamp(0.0, rect.width());
            if !response.has_focus() {
                response.request_focus();
            }
        }

        // Update animation targets from new focus/text/error state.
        let now_focused = response.has_focus();
        let now_shifted =
            !self.text.is_empty() || (now_focused && self.style.placeholder_scale > 0.0);
        let now_border_visible = self.error || now_focused;

        if anim.focused != now_focused {
            anim.focused = now_focused;
            if now_focused && anim.border_start_x <= 0.0 {
                anim.border_start_x = rect.width() * 0.5;
            }
            anim.a_focused.change(
                now,
                if now_focused { 1.0 } else { 0.0 },
                self.style.duration_ms,
                Transition::SineInOut,
            );
            ui.ctx().request_repaint();
        }
        if anim.placeholder_shifted != now_shifted {
            anim.placeholder_shifted = now_shifted;
            anim.a_placeholder_shifted.change(
                now,
                if now_shifted { 1.0 } else { 0.0 },
                self.style.duration_ms,
                Transition::SineInOut,
            );
            ui.ctx().request_repaint();
        }
        if anim.error != self.error {
            anim.error = self.error;
            anim.a_error.change(
                now,
                if self.error { 1.0 } else { 0.0 },
                self.style.duration_ms,
                Transition::SineInOut,
            );
            ui.ctx().request_repaint();
        }
        if anim.border_visible != now_border_visible {
            anim.border_visible = now_border_visible;
            if now_border_visible {
                if anim.a_border_opacity.animating() {
                    anim.a_border_opacity.start(
                        now,
                        0.0,
                        1.0,
                        self.style.duration_ms,
                        Transition::Linear,
                    );
                } else {
                    anim.a_border_shown.start(
                        now,
                        0.0,
                        1.0,
                        self.style.duration_ms,
                        Transition::EaseOutCirc,
                    );
                }
            } else {
                anim.a_border_opacity.start(
                    now,
                    1.0,
                    0.0,
                    self.style.duration_ms,
                    Transition::Linear,
                );
            }
            ui.ctx().request_repaint();
        }

        // Paint animated placeholder (floating scale/shift or fade/slide).
        if !self.placeholder.is_empty() && ui.is_rect_visible(rect) {
            paint_placeholder(
                ui.painter(),
                text_rect,
                self.placeholder,
                &self.style,
                ph_shift_deg,
                focused_deg,
                error_deg,
            );
        }

        // Paint optional character count indicator in top-right when near limit.
        if let Some(limit) = self.max_length
            && ui.is_rect_visible(rect)
        {
            let count = self.text.chars().count();
            let remaining = limit as isize - count as isize;
            let color = if remaining < 0 {
                self.style.border_fg_error
            } else {
                self.style.placeholder_fg
            };
            ui.painter().text(
                Pos2::new(rect.max.x, rect.min.y + 4.0),
                Align2::RIGHT_TOP,
                format!("{count}/{limit}"),
                FontId::proportional(11.0),
                color,
            );
        }

        ui.ctx().data_mut(|d| d.insert_temp(anim_key, anim));
        response
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_input_surrounding(
    painter: &egui::Painter,
    rect: Rect,
    st: &InputFieldStyle,
    focused_deg: f64,
    error_deg: f64,
    border_shown: f64,
    border_opacity: f64,
    border_start_x: f32,
) {
    if st.border_radius > 0.0 {
        // Rounded search/filter field (`filterInputInactiveBg` -> `filterInputActiveBg`).
        let bg = color_lerp(st.text_bg, st.text_bg_active, focused_deg);
        let cr = CornerRadius::same(st.border_radius.round().clamp(0.0, 255.0) as u8);
        painter.rect_filled(rect, cr, bg);

        let border_col = color_lerp(st.border_fg_active, st.border_fg_error, error_deg);
        if border_opacity > 0.01 && st.border_active > 0.0 {
            painter.rect_stroke(
                rect,
                cr,
                Stroke::new(st.border_active, with_alpha(border_col, border_opacity)),
                StrokeKind::Inside,
            );
        } else if st.border > 0.0 {
            painter.rect_stroke(
                rect,
                cr,
                Stroke::new(st.border, st.border_fg),
                StrokeKind::Inside,
            );
        }
        return;
    }

    // Flat field with animated bottom underline (`paintFlatSurrounding` in input_field.cpp).
    if st.text_bg.a() > 0 {
        painter.rect_filled(rect, CornerRadius::ZERO, st.text_bg);
    }
    if st.border > 0.0 {
        let base_line = Rect::from_min_max(Pos2::new(rect.min.x, rect.max.y - st.border), rect.max);
        painter.rect_filled(base_line, CornerRadius::ZERO, st.border_fg);
    }
    if st.border_active > 0.0 && border_opacity > 0.01 {
        let w = rect.width();
        let start = border_start_x.clamp(0.0, w);
        let from = start * (1.0 - border_shown as f32);
        let to = start + (w - start) * border_shown as f32;
        if to > from {
            let fg = with_alpha(
                color_lerp(st.border_fg_active, st.border_fg_error, error_deg),
                border_opacity,
            );
            let active_line = Rect::from_min_max(
                Pos2::new(rect.min.x + from, rect.max.y - st.border_active),
                Pos2::new(rect.min.x + to, rect.max.y),
            );
            painter.rect_filled(active_line, CornerRadius::ZERO, fg);
        }
    }
}

fn paint_placeholder(
    painter: &egui::Painter,
    text_rect: Rect,
    placeholder: &str,
    st: &InputFieldStyle,
    shift_deg: f64,
    focused_deg: f64,
    error_deg: f64,
) {
    let base_fg = color_lerp(st.placeholder_fg, st.placeholder_fg_active, focused_deg);
    let fg = color_lerp(base_fg, st.placeholder_fg_error, error_deg);

    if st.placeholder_scale > 0.0 {
        // Floating scaled placeholder (`placeholderScale` + `placeholderShift`).
        let scale = 1.0 - (1.0 - st.placeholder_scale) * (shift_deg as f32);
        let dy = interpolate_f32(0.0, st.placeholder_shift, shift_deg as f32);
        let dx = interpolate_f32(0.0, st.placeholder_shift_left, shift_deg as f32);
        let font_size = (st.placeholder_font.size * scale).max(9.0);
        let pos = Pos2::new(
            text_rect.min.x + st.placeholder_margins.left + dx,
            text_rect.min.y + st.placeholder_margins.top + dy,
        );
        let ph_font = FontSpec {
            size: font_size,
            ..st.placeholder_font
        }
        .font_id();
        painter.text(pos, Align2::LEFT_TOP, placeholder, ph_font, fg);
    } else if shift_deg < 0.99 {
        // Non-floating placeholder fades out & shifts horizontally when text is entered.
        let alpha = 1.0 - shift_deg;
        let dx = interpolate_f32(0.0, -st.placeholder_shift, shift_deg as f32);
        let pos = Pos2::new(
            text_rect.min.x + st.placeholder_margins.left + dx,
            text_rect.center().y,
        );
        painter.text(
            pos,
            Align2::LEFT_CENTER,
            placeholder,
            st.placeholder_font.font_id(),
            with_alpha(fg, alpha),
        );
    }
}

/// Estimate wrapped line count for multi-line auto-grow height calculation.
pub fn estimate_wrapped_lines(text: &str, available_width: f32, font_size: f32) -> usize {
    if text.is_empty() {
        return 1;
    }
    let avg_char_w = (font_size * 0.56).max(4.0);
    let chars_per_line = (available_width / avg_char_w).floor().max(1.0) as usize;
    let mut lines = 0usize;
    for raw_line in text.split('\n') {
        let len = raw_line.chars().count();
        lines += len.div_ceil(chars_per_line).max(1);
    }
    lines.max(1)
}
