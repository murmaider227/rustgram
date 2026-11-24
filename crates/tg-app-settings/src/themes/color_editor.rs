//! Interactive HSV/RGB/Hex + Alpha color picker widget.
//!
//! Port of `Telegram/SourceFiles/ui/widgets/color_editor.h` and `color_editor.cpp`.
//!
//! Supports both [`ColorEditorMode::Rgba`] (2D Saturation/Value picker + vertical Hue bar +
//! horizontal Opacity bar + H/S/B, R/G/B, A, `#rrggbb[aa]` text inputs) and
//! [`ColorEditorMode::Hsl`] (2D Hue/Saturation picker + horizontal Lightness bar with optional
//! lightness limits).

use egui::{
    Color32, ColorImage, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, StrokeKind,
    TextureHandle, TextureOptions, Ui, Vec2,
};
use tg_theme::Color;
use tg_ui::theme::{Colors, theme_color_to_egui};

/// Color picker layout mode matching `ColorEditor::Mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorEditorMode {
    /// Full RGBA picker (2D SV field, vertical Hue bar, horizontal Alpha bar).
    #[default]
    Rgba,
    /// Accent HSL picker (2D HS field, horizontal Lightness bar with limits).
    Hsl,
}

/// Pure view-model and egui widget state for `ui/widgets/color_editor.cpp`.
pub struct ColorEditorState {
    mode: ColorEditorMode,
    original: Color,
    current: Color,
    /// Hue in `0..=359` (retained even when saturation/value is 0 so dragging stays smooth).
    hue: i32,
    /// Saturation in `0..=255`.
    sat: i32,
    /// Brightness (Value in RGBA mode, Lightness in HSL mode) in `0..=255`.
    bright: i32,
    /// Lightness bounds for [`ColorEditorMode::Hsl`].
    lightness_min: i32,
    lightness_max: i32,
    // Draft text fields kept synchronized with `current`.
    hex_text: String,
    r_text: String,
    g_text: String,
    b_text: String,
    a_text: String,
    h_text: String,
    s_text: String,
    v_text: String,
    // Cached GPU textures for the 2D picker, Hue bar, and Alpha/Lightness bar.
    picker_tex: Option<(i32, ColorEditorMode, TextureHandle)>,
    hue_tex: Option<TextureHandle>,
}

impl ColorEditorState {
    /// Creates a new `ColorEditorState` initialized with `initial` as both before & live color.
    #[must_use]
    pub fn new(mode: ColorEditorMode, initial: Color) -> Self {
        let mut state = Self {
            mode,
            original: initial,
            current: initial,
            hue: 0,
            sat: 0,
            bright: 0,
            lightness_min: 0,
            lightness_max: 255,
            hex_text: String::new(),
            r_text: String::new(),
            g_text: String::new(),
            b_text: String::new(),
            a_text: String::new(),
            h_text: String::new(),
            s_text: String::new(),
            v_text: String::new(),
            picker_tex: None,
            hue_tex: None,
        };
        state.sync_from_color(initial, true);
        state
    }

    /// Returns the active [`ColorEditorMode`].
    #[must_use]
    pub const fn mode(&self) -> ColorEditorMode {
        self.mode
    }

    /// Returns the original ("before") color snapshot.
    #[must_use]
    pub const fn original_color(&self) -> Color {
        self.original
    }

    /// Returns the live edited color (`ColorEditor::color()`).
    #[must_use]
    pub const fn color(&self) -> Color {
        self.current
    }

    /// Returns `(hue_deg 0..=359, sat 0..=255, value_or_lightness 0..=255)`.
    #[must_use]
    pub const fn hsb(&self) -> (i32, i32, i32) {
        (self.hue, self.sat, self.bright)
    }

    /// Returns the normalized `(x, y)` cursor position in `0.0..=1.0` inside the 2D picker.
    #[must_use]
    pub fn picker_xy(&self) -> (f32, f32) {
        match self.mode {
            ColorEditorMode::Rgba => (
                (self.sat as f32 / 255.0).clamp(0.0, 1.0),
                (1.0 - self.bright as f32 / 255.0).clamp(0.0, 1.0),
            ),
            ColorEditorMode::Hsl => (
                (self.hue as f32 / 360.0).clamp(0.0, 1.0),
                (1.0 - self.sat as f32 / 255.0).clamp(0.0, 1.0),
            ),
        }
    }

    /// Sets HSL lightness limits (used for theme accent color selection).
    pub fn set_lightness_limits(&mut self, min: i32, max: i32) {
        self.lightness_min = min.clamp(0, 254);
        self.lightness_max = max.clamp(self.lightness_min + 1, 255);
        if self.mode == ColorEditorMode::Hsl {
            let clamped = self.bright.clamp(self.lightness_min, self.lightness_max);
            if clamped != self.bright {
                self.bright = clamped;
                let adjusted = Color::from_hsl(self.hue, self.sat, self.bright, 255);
                self.sync_from_color(adjusted, false);
            }
        }
    }

    /// Displays an external color in the editor while keeping [`Self::original_color`] intact
    /// (`ColorEditor::showColor`, used when picking another key as a "same as" reference).
    pub fn show_color(&mut self, color: Color) {
        self.sync_from_color(color, true);
    }

    /// Reverts the live color back to [`Self::original_color`].
    pub fn revert_to_original(&mut self) {
        let orig = self.original;
        self.sync_from_color(orig, true);
    }

    /// Applies an HSV/HSL triple `(h: 0..=360, s_pct: 0..=100, v_pct: 0..=100)`.
    pub fn set_hsv_percent(&mut self, h_deg: i32, s_pct: i32, v_pct: i32) {
        self.hue = h_deg.rem_euclid(360);
        self.sat = ((s_pct.clamp(0, 100) * 255 + 50) / 100).clamp(0, 255);
        self.bright = ((v_pct.clamp(0, 100) * 255 + 50) / 100).clamp(0, 255);
        if self.mode == ColorEditorMode::Hsl {
            self.bright = self.bright.clamp(self.lightness_min, self.lightness_max);
        }
        let next = self.color_from_hsb();
        self.current = next;
        self.sync_text_fields();
    }

    /// Applies explicit RGBA channels (`0..=255`).
    pub fn set_rgba(&mut self, r: u8, g: u8, b: u8, a: u8) {
        let alpha = if self.mode == ColorEditorMode::Hsl {
            255
        } else {
            a
        };
        self.sync_from_color(Color::rgba(r, g, b, alpha), true);
    }

    /// Parses and applies a hex string (`#rrggbb`, `rrggbb`, `#rrggbbaa`, or `rrggbbaa`).
    /// Returns `true` if the hex string was valid and applied.
    pub fn apply_hex(&mut self, raw: &str) -> bool {
        let sanitized = sanitize_hex_input(raw, self.mode == ColorEditorMode::Rgba);
        if sanitized.len() != 6 && sanitized.len() != 8 {
            return false;
        }
        let Some(mut parsed) = Color::from_hex(&sanitized) else {
            return false;
        };
        if self.mode == ColorEditorMode::Hsl {
            parsed.a = 255;
        } else if sanitized.len() == 6 {
            parsed.a = self.current.a;
        }
        self.sync_from_color(parsed, true);
        true
    }

    /// Updates `(x, y)` in `0.0..=1.0` from dragging inside the 2D picker.
    pub fn set_picker_xy(&mut self, x: f32, y: f32) {
        let x = x.clamp(0.0, 1.0);
        let y = y.clamp(0.0, 1.0);
        match self.mode {
            ColorEditorMode::Rgba => {
                self.sat = (x * 255.0).round() as i32;
                self.bright = ((1.0 - y) * 255.0).round() as i32;
            }
            ColorEditorMode::Hsl => {
                self.hue = ((x * 359.0).round() as i32).clamp(0, 359);
                self.sat = ((1.0 - y) * 255.0).round() as i32;
            }
        }
        self.current = self.color_from_hsb();
        self.sync_text_fields();
    }

    /// Updates hue from the vertical Hue slider (`t = 0.0` at top = 360°, `t = 1.0` at bottom = 0°).
    pub fn set_hue_slider(&mut self, t: f32) {
        let deg = ((1.0 - t.clamp(0.0, 1.0)) * 359.0).round() as i32;
        self.hue = deg.clamp(0, 359);
        self.current = self.color_from_hsb();
        self.sync_text_fields();
    }

    /// Updates alpha from the horizontal Opacity slider (`t` in `0.0..=1.0`).
    pub fn set_alpha_slider(&mut self, t: f32) {
        if self.mode != ColorEditorMode::Rgba {
            return;
        }
        self.current.a = (t.clamp(0.0, 1.0) * 255.0).round() as u8;
        self.sync_text_fields();
    }

    /// Updates lightness from the horizontal Lightness slider (`t` in `0.0..=1.0`).
    pub fn set_lightness_slider(&mut self, t: f32) {
        if self.mode != ColorEditorMode::Hsl {
            return;
        }
        let span = (self.lightness_max - self.lightness_min) as f32;
        self.bright = (self.lightness_min as f32 + t.clamp(0.0, 1.0) * span).round() as i32;
        self.current = self.color_from_hsb();
        self.sync_text_fields();
    }

    fn color_from_hsb(&self) -> Color {
        match self.mode {
            ColorEditorMode::Rgba => {
                Color::from_hsv(self.hue, self.sat, self.bright, self.current.a)
            }
            ColorEditorMode::Hsl => Color::from_hsl(
                self.hue,
                self.sat,
                self.bright.clamp(self.lightness_min, self.lightness_max),
                255,
            ),
        }
    }

    fn sync_from_color(&mut self, mut color: Color, update_hue: bool) {
        match self.mode {
            ColorEditorMode::Rgba => {
                let hsv = color.to_hsv();
                if update_hue && hsv.h >= 0 {
                    self.hue = hsv.h;
                }
                self.sat = hsv.s;
                self.bright = hsv.v;
            }
            ColorEditorMode::Hsl => {
                color.a = 255;
                let hsl = color.to_hsl();
                if update_hue && hsl.h >= 0 {
                    self.hue = hsl.h;
                }
                self.sat = hsl.s;
                self.bright = hsl.l.clamp(self.lightness_min, self.lightness_max);
                color = Color::from_hsl(self.hue, self.sat, self.bright, 255);
            }
        }
        self.current = color;
        self.sync_text_fields();
    }

    fn sync_text_fields(&mut self) {
        self.hex_text = self
            .current
            .to_hex_string()
            .trim_start_matches('#')
            .to_owned();
        self.r_text = self.current.r.to_string();
        self.g_text = self.current.g.to_string();
        self.b_text = self.current.b.to_string();
        self.a_text = self.current.a.to_string();
        self.h_text = self.hue.to_string();
        self.s_text = ((self.sat * 100 + 127) / 255).to_string();
        self.v_text = ((self.bright * 100 + 127) / 255).to_string();
    }

    /// Renders the complete ColorEditor widget into `ui`.
    ///
    /// Returns `true` if [`Self::color`] changed during this frame.
    pub fn show(&mut self, ui: &mut Ui, colors: &dyn Colors) -> bool {
        let before = self.current;
        let picker_size = 216.0;
        let hue_bar_w = 24.0;
        let gap = 12.0;

        let text_fg = colors.color("windowFg");
        let sub_fg = colors.color("windowSubTextFg");
        let border_col = colors.color("shadowFg");

        ui.vertical(|ui| {
            // Top row: 2D picker + vertical Hue slider (RGBA mode) + before/after swatch & numeric fields
            ui.horizontal(|ui| {
                // 2D color square
                self.draw_2d_picker(ui, Vec2::splat(picker_size), border_col);

                if self.mode == ColorEditorMode::Rgba {
                    ui.add_space(gap);
                    self.draw_hue_slider(ui, Vec2::new(hue_bar_w, picker_size), border_col);
                }

                ui.add_space(gap);

                // Right column: Before/After swatch + HSV & RGB inputs
                ui.vertical(|ui| {
                    self.draw_before_after_swatch(ui, Vec2::new(124.0, 34.0), border_col);
                    ui.add_space(8.0);

                    // H / S / B (or L)
                    let bright_label = if self.mode == ColorEditorMode::Rgba {
                        "V"
                    } else {
                        "L"
                    };
                    let hsb_changed = self.draw_channel_row(
                        ui,
                        [("H", "°", 360), ("S", "%", 100), (bright_label, "%", 100)],
                        true,
                        sub_fg,
                        text_fg,
                    );
                    if hsb_changed {
                        let h = self.h_text.parse::<i32>().unwrap_or(self.hue);
                        let s = self.s_text.parse::<i32>().unwrap_or(0);
                        let v = self.v_text.parse::<i32>().unwrap_or(0);
                        self.set_hsv_percent(h, s, v);
                    }

                    ui.add_space(6.0);

                    // R / G / B
                    let rgb_changed = self.draw_channel_row(
                        ui,
                        [("R", "", 255), ("G", "", 255), ("B", "", 255)],
                        false,
                        sub_fg,
                        text_fg,
                    );
                    if rgb_changed {
                        let r = self
                            .r_text
                            .parse::<u16>()
                            .unwrap_or(u16::from(self.current.r))
                            .min(255) as u8;
                        let g = self
                            .g_text
                            .parse::<u16>()
                            .unwrap_or(u16::from(self.current.g))
                            .min(255) as u8;
                        let b = self
                            .b_text
                            .parse::<u16>()
                            .unwrap_or(u16::from(self.current.b))
                            .min(255) as u8;
                        self.set_rgba(r, g, b, self.current.a);
                    }

                    ui.add_space(6.0);

                    // Hex field + Alpha field
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("#").size(12.5).strong().color(sub_fg));
                        let hex_resp = ui.add_sized(
                            Vec2::new(76.0, 22.0),
                            egui::TextEdit::singleline(&mut self.hex_text)
                                .font(FontId::monospace(12.0)),
                        );
                        if hex_resp.changed() {
                            self.hex_text = sanitize_hex_input(
                                &self.hex_text,
                                self.mode == ColorEditorMode::Rgba,
                            );
                            if self.hex_text.len() == 6 || self.hex_text.len() == 8 {
                                let raw = self.hex_text.clone();
                                self.apply_hex(&raw);
                            }
                        }

                        if self.mode == ColorEditorMode::Rgba {
                            ui.label(egui::RichText::new("A").size(11.5).color(sub_fg));
                            let a_resp = ui.add_sized(
                                Vec2::new(34.0, 22.0),
                                egui::TextEdit::singleline(&mut self.a_text)
                                    .font(FontId::monospace(11.5)),
                            );
                            if a_resp.changed() {
                                self.a_text = sanitize_digits(&self.a_text, 255);
                                if let Ok(v) = self.a_text.parse::<u16>() {
                                    self.current.a = v.min(255) as u8;
                                    self.hex_text = self
                                        .current
                                        .to_hex_string()
                                        .trim_start_matches('#')
                                        .to_owned();
                                }
                            }
                        }
                    });
                });
            });

            ui.add_space(10.0);

            // Bottom horizontal slider: Opacity (RGBA) or Lightness (HSL)
            let slider_w = picker_size
                + if self.mode == ColorEditorMode::Rgba {
                    gap + hue_bar_w
                } else {
                    0.0
                };
            match self.mode {
                ColorEditorMode::Rgba => {
                    self.draw_alpha_slider(ui, Vec2::new(slider_w, 22.0), border_col, sub_fg);
                }
                ColorEditorMode::Hsl => {
                    self.draw_lightness_slider(ui, Vec2::new(slider_w, 22.0), border_col, sub_fg);
                }
            }
        });

        self.current != before
    }

    fn draw_2d_picker(&mut self, ui: &mut Ui, size: Vec2, border_col: Color32) {
        let key_val = match self.mode {
            ColorEditorMode::Rgba => self.hue,
            ColorEditorMode::Hsl => self.bright,
        };
        let needs_rebuild = match &self.picker_tex {
            Some((cached_val, cached_mode, _)) => {
                *cached_val != key_val || *cached_mode != self.mode
            }
            None => true,
        };
        if needs_rebuild {
            let img = build_2d_picker_image(self.mode, key_val, 128);
            let tex = ui
                .ctx()
                .load_texture("tg_color_editor_2d", img, TextureOptions::LINEAR);
            self.picker_tex = Some((key_val, self.mode, tex));
        }

        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        }
        if (resp.clicked() || resp.dragged())
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let x = (pos.x - rect.left()) / rect.width().max(1.0);
            let y = (pos.y - rect.top()) / rect.height().max(1.0);
            self.set_picker_xy(x, y);
        }

        if let Some((_, _, ref tex)) = self.picker_tex {
            ui.painter().image(
                tex.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(2),
            Stroke::new(1.0, border_col),
            StrokeKind::Inside,
        );

        // Contrast-aware circular marker matching `ColorEditor::Picker::paintEvent`
        let (nx, ny) = self.picker_xy();
        let marker_pos = Pos2::new(
            rect.left() + nx * rect.width(),
            rect.top() + ny * rect.height(),
        );
        let lightness = 0.2989 * self.current.red_f()
            + 0.5870 * self.current.green_f()
            + 0.1140 * self.current.blue_f();
        let ring_col = if lightness > 0.6 {
            Color32::BLACK
        } else {
            Color32::WHITE
        };
        ui.painter()
            .circle_stroke(marker_pos, 6.5, Stroke::new(2.0, ring_col));
    }

    fn draw_hue_slider(&mut self, ui: &mut Ui, size: Vec2, border_col: Color32) {
        let tex_id = self
            .hue_tex
            .get_or_insert_with(|| {
                let img = build_hue_bar_image(1, 180);
                ui.ctx()
                    .load_texture("tg_color_editor_hue", img, TextureOptions::LINEAR)
            })
            .id();
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
        }
        if (resp.clicked() || resp.dragged())
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let t = (pos.y - rect.top()) / rect.height().max(1.0);
            self.set_hue_slider(t);
        }

        ui.painter().image(
            tex_id,
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(2),
            Stroke::new(1.0, border_col),
            StrokeKind::Inside,
        );

        let y = rect.top() + (1.0 - (self.hue as f32 / 360.0).clamp(0.0, 1.0)) * rect.height();
        let indicator = Rect::from_center_size(
            Pos2::new(rect.center().x, y),
            Vec2::new(rect.width() + 4.0, 5.0),
        );
        ui.painter().rect_stroke(
            indicator,
            CornerRadius::same(2),
            Stroke::new(2.0, Color32::WHITE),
            StrokeKind::Outside,
        );
        ui.painter().rect_stroke(
            indicator,
            CornerRadius::same(2),
            Stroke::new(1.0, Color32::BLACK),
            StrokeKind::Inside,
        );
    }

    fn draw_alpha_slider(&mut self, ui: &mut Ui, size: Vec2, border_col: Color32, sub_fg: Color32) {
        ui.horizontal(|ui| {
            let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if (resp.clicked() || resp.dragged())
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let t = (pos.x - rect.left()) / rect.width().max(1.0);
                self.set_alpha_slider(t);
            }
            draw_checkerboard(ui, rect, 6.0);

            // Smooth horizontal alpha ramp in 32 steps
            let steps = 32;
            let step_w = rect.width() / (steps as f32);
            for i in 0..steps {
                let a = ((i as f32 / (steps - 1) as f32) * 255.0).round() as u8;
                let seg = Rect::from_min_size(
                    Pos2::new(rect.left() + i as f32 * step_w, rect.top()),
                    Vec2::new(step_w + 0.5, rect.height()),
                );
                ui.painter().rect_filled(
                    seg,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(
                        self.current.r,
                        self.current.g,
                        self.current.b,
                        a,
                    ),
                );
            }
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(2),
                Stroke::new(1.0, border_col),
                StrokeKind::Inside,
            );

            let x = rect.left() + (f32::from(self.current.a) / 255.0) * rect.width();
            let handle = Rect::from_center_size(
                Pos2::new(x, rect.center().y),
                Vec2::new(6.0, rect.height() + 4.0),
            );
            ui.painter().rect_stroke(
                handle,
                CornerRadius::same(2),
                Stroke::new(2.0, Color32::WHITE),
                StrokeKind::Outside,
            );
            ui.painter().rect_stroke(
                handle,
                CornerRadius::same(2),
                Stroke::new(1.0, Color32::BLACK),
                StrokeKind::Inside,
            );

            ui.add_space(6.0);
            let pct = ((u16::from(self.current.a) * 100 + 127) / 255) as u8;
            ui.label(
                egui::RichText::new(format!("Alpha {pct}%"))
                    .size(11.5)
                    .color(sub_fg),
            );
        });
    }

    fn draw_lightness_slider(
        &mut self,
        ui: &mut Ui,
        size: Vec2,
        border_col: Color32,
        sub_fg: Color32,
    ) {
        ui.horizontal(|ui| {
            let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if (resp.clicked() || resp.dragged())
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let t = (pos.x - rect.left()) / rect.width().max(1.0);
                self.set_lightness_slider(t);
            }
            let steps = 32;
            let step_w = rect.width() / (steps as f32);
            let span = (self.lightness_max - self.lightness_min) as f32;
            for i in 0..steps {
                let l = (self.lightness_min as f32 + (i as f32 / (steps - 1) as f32) * span).round()
                    as i32;
                let col = theme_color_to_egui(Color::from_hsl(self.hue, self.sat, l, 255));
                let seg = Rect::from_min_size(
                    Pos2::new(rect.left() + i as f32 * step_w, rect.top()),
                    Vec2::new(step_w + 0.5, rect.height()),
                );
                ui.painter().rect_filled(seg, CornerRadius::ZERO, col);
            }
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(2),
                Stroke::new(1.0, border_col),
                StrokeKind::Inside,
            );
            let t = ((self.bright - self.lightness_min) as f32 / span.max(1.0)).clamp(0.0, 1.0);
            let x = rect.left() + t * rect.width();
            let handle = Rect::from_center_size(
                Pos2::new(x, rect.center().y),
                Vec2::new(6.0, rect.height() + 4.0),
            );
            ui.painter().rect_stroke(
                handle,
                CornerRadius::same(2),
                Stroke::new(2.0, Color32::WHITE),
                StrokeKind::Outside,
            );
            ui.painter().rect_stroke(
                handle,
                CornerRadius::same(2),
                Stroke::new(1.0, Color32::BLACK),
                StrokeKind::Inside,
            );

            ui.add_space(6.0);
            let pct = ((self.bright * 100 + 127) / 255).clamp(0, 100);
            ui.label(
                egui::RichText::new(format!("Lightness {pct}%"))
                    .size(11.5)
                    .color(sub_fg),
            );
        });
    }

    fn draw_before_after_swatch(&mut self, ui: &mut Ui, size: Vec2, border_col: Color32) {
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        draw_checkerboard(ui, rect, 6.0);
        let mid_x = rect.center().x;
        let left_rect = Rect::from_min_max(rect.min, Pos2::new(mid_x, rect.bottom()));
        let right_rect = Rect::from_min_max(Pos2::new(mid_x, rect.top()), rect.max);
        ui.painter().rect_filled(
            left_rect,
            CornerRadius::ZERO,
            theme_color_to_egui(self.original),
        );
        ui.painter().rect_filled(
            right_rect,
            CornerRadius::ZERO,
            theme_color_to_egui(self.current),
        );
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, border_col),
            StrokeKind::Inside,
        );
        // Clicking the left ("before") half reverts to the original color, matching tdesktop.
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
            && pos.x < mid_x
        {
            self.revert_to_original();
        }
        if resp.hovered() {
            resp.on_hover_text("Left: original (click to revert) • Right: new color");
        }
    }

    fn draw_channel_row(
        &mut self,
        ui: &mut Ui,
        specs: [(&str, &str, u16); 3],
        is_hsb: bool,
        sub_fg: Color32,
        _text_fg: Color32,
    ) -> bool {
        let mut changed = false;
        let (f0, f1, f2) = if is_hsb {
            (&mut self.h_text, &mut self.s_text, &mut self.v_text)
        } else {
            (&mut self.r_text, &mut self.g_text, &mut self.b_text)
        };
        let fields = [f0, f1, f2];
        for (idx, field) in fields.into_iter().enumerate() {
            let (label, unit, limit) = specs[idx];
            ui.horizontal(|ui| {
                ui.add_sized(
                    Vec2::new(14.0, 20.0),
                    egui::Label::new(egui::RichText::new(label).size(11.5).color(sub_fg)),
                );
                let resp = ui.add_sized(
                    Vec2::new(48.0, 20.0),
                    egui::TextEdit::singleline(field).font(FontId::monospace(11.5)),
                );
                if resp.changed() {
                    *field = sanitize_digits(field, limit);
                    changed = true;
                }
                if !unit.is_empty() {
                    ui.label(egui::RichText::new(unit).size(11.0).color(sub_fg));
                }
            });
        }
        changed
    }
}

/// Paints a classic light/dark transparency checkerboard inside `rect`.
pub fn draw_checkerboard(ui: &Ui, rect: Rect, cell: f32) {
    let light = Color32::from_rgb(0xFF, 0xFF, 0xFF);
    let dark = Color32::from_rgb(0xCC, 0xCC, 0xCC);
    ui.painter().rect_filled(rect, CornerRadius::ZERO, light);
    let cols = (rect.width() / cell).ceil() as i32;
    let rows = (rect.height() / cell).ceil() as i32;
    for r in 0..rows {
        for c in 0..cols {
            if (r + c) % 2 == 1 {
                let min = Pos2::new(rect.left() + c as f32 * cell, rect.top() + r as f32 * cell);
                let max = Pos2::new(
                    (min.x + cell).min(rect.right()),
                    (min.y + cell).min(rect.bottom()),
                );
                ui.painter()
                    .rect_filled(Rect::from_min_max(min, max), CornerRadius::ZERO, dark);
            }
        }
    }
}

/// Sanitizes a numeric channel field string (`ColorEditor::Field::correctValue`).
#[must_use]
pub fn sanitize_digits(raw: &str, limit: u16) -> String {
    let digits: String = raw.chars().filter(char::is_ascii_digit).take(3).collect();
    if digits.is_empty() {
        return String::new();
    }
    match digits.parse::<u16>() {
        Ok(v) if v > limit => limit.to_string(),
        Ok(_) => digits,
        Err(_) => limit.to_string(),
    }
}

/// Sanitizes a hex color input string (`ColorEditor::ResultField::correctValue`).
#[must_use]
pub fn sanitize_hex_input(raw: &str, allow_alpha: bool) -> String {
    let max_len = if allow_alpha { 8 } else { 6 };
    raw.chars()
        .filter(char::is_ascii_hexdigit)
        .take(max_len)
        .collect::<String>()
        .to_ascii_lowercase()
}

fn build_2d_picker_image(mode: ColorEditorMode, param: i32, size: usize) -> ColorImage {
    let mut pixels = Vec::with_capacity(size * size);
    let denom = (size.saturating_sub(1)).max(1) as f32;
    for y in 0..size {
        let yf = y as f32 / denom;
        for x in 0..size {
            let xf = x as f32 / denom;
            let c = match mode {
                ColorEditorMode::Rgba => {
                    let s = (xf * 255.0).round() as i32;
                    let v = ((1.0 - yf) * 255.0).round() as i32;
                    Color::from_hsv(param, s, v, 255)
                }
                ColorEditorMode::Hsl => {
                    let h = ((xf * 359.0).round() as i32).clamp(0, 359);
                    let s = ((1.0 - yf) * 255.0).round() as i32;
                    Color::from_hsl(h, s, param, 255)
                }
            };
            pixels.push(Color32::from_rgb(c.r, c.g, c.b));
        }
    }
    ColorImage::new([size, size], pixels)
}

fn build_hue_bar_image(width: usize, height: usize) -> ColorImage {
    let mut pixels = Vec::with_capacity(width * height);
    let denom = (height.saturating_sub(1)).max(1) as f32;
    for y in 0..height {
        let deg = ((1.0 - (y as f32 / denom)) * 359.0).round() as i32;
        let c = Color::from_hsv(deg.clamp(0, 359), 255, 255, 255);
        for _ in 0..width {
            pixels.push(Color32::from_rgb(c.r, c.g, c.b));
        }
    }
    ColorImage::new([width, height], pixels)
}
