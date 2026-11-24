//! Color picker palette, HSL/HSV slider math, brush size slider math,
//! and brush state serialization.
//!
//! Port of `editor/color_picker.h`, `editor/color_picker.cpp`,
//! `ui/widgets/color_editor.cpp`, and brush serialization from `editor/photo_editor.cpp`.

use serde::{Deserialize, Serialize};

use crate::common::{
    Brush, BrushTool, MAX_BRUSH_WIDTH, MIN_BRUSH_WIDTH, TextAlignment, TextPrefs, TextStyle,
    TextTypeface,
};
use crate::geometry::Color;

/// Minimum normalized brush size ratio (`kMinBrushSize` in `color_picker.cpp`).
pub const MIN_BRUSH_SIZE_RATIO: f64 = 0.1;

/// Default brush size control height in logical pixels (`photoEditorBrushSizeControlHeight`).
pub const SIZE_CONTROL_HEIGHT: i32 = 280;

/// Hit padding around the vertical brush size slider (`photoEditorBrushSizeControlHitPadding`).
pub const SIZE_CONTROL_HIT_PADDING: i32 = 24;

/// Top width of the expanded brush size control (`photoEditorBrushSizeControlExpandedTopWidth`).
pub const SIZE_CONTROL_EXPANDED_TOP_WIDTH: i32 = 25;

/// Bottom width of the expanded brush size control (`photoEditorBrushSizeControlExpandedBottomWidth`).
pub const SIZE_CONTROL_EXPANDED_BOTTOM_WIDTH: i32 = 4;

/// Collapsed width of the brush size control (`photoEditorBrushSizeControlCollapsedWidth`).
pub const SIZE_CONTROL_COLLAPSED_WIDTH: i32 = 6;

/// Horizontal expand shift (`photoEditorBrushSizeControlExpandShift`).
pub const SIZE_CONTROL_EXPAND_SHIFT: i32 = 14;

/// Default 10-color editor swatch palette (`PaletteColors` in `color_picker.cpp`).
pub const DEFAULT_PALETTE: [Color; 10] = [
    Color::rgb(0, 0, 0),
    Color::rgb(255, 255, 255),
    Color::rgb(234, 39, 57),
    Color::rgb(252, 150, 77),
    Color::rgb(252, 222, 101),
    Color::rgb(128, 200, 100),
    Color::rgb(73, 197, 237),
    Color::rgb(48, 81, 227),
    Color::rgb(219, 58, 210),
    Color::rgb(255, 114, 169),
];

const BRUSHES_VERSION: i32 = -2;
const PRECISION: f64 = 100_000.0;

/// Integer HSL representation matching Qt's `QColor` HSL fields (`H: 0..=360`, `S: 0..=255`, `L: 0..=255`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HslColor {
    /// Hue in degrees (`0..=360`).
    pub hue: u16,
    /// Saturation (`0..=255`).
    pub saturation: u8,
    /// Lightness (`0..=255`).
    pub lightness: u8,
}

/// Integer HSV representation matching Qt's `QColor` HSV fields (`H: 0..=360`, `S: 0..=255`, `V: 0..=255`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HsvColor {
    /// Hue in degrees (`0..=360`).
    pub hue: u16,
    /// Saturation (`0..=255`).
    pub saturation: u8,
    /// Value / brightness (`0..=255`).
    pub value: u8,
}

/// Color editor slider mode (`ColorEditor::Mode`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorSliderMode {
    /// Hue + saturation on 2D picker, lightness on slider (used by photo editor color picker).
    #[default]
    Hsl,
    /// Saturation + value on 2D picker, hue on vertical slider, opacity on horizontal slider.
    Rgba,
}

/// Converts a `0..=255` byte to a `0..=100` integer percentage (`ColorEditor::percentFromByte`).
#[must_use]
pub fn percent_from_byte(byte: u8) -> u8 {
    ((byte as f64 * 100.0 / 255.0).round() as i32).clamp(0, 100) as u8
}

/// Converts a `0..=100` integer percentage to a `0..=255` byte (`ColorEditor::percentToByte`).
#[must_use]
pub fn percent_to_byte(percent: u8) -> u8 {
    ((percent as f64 * 255.0 / 100.0).round() as i32).clamp(0, 255) as u8
}

/// Converts sRGB to normalized HSV `(hue in [0, 1), sat in [0, 1], val in [0, 1])`.
#[must_use]
pub fn rgb_to_hsv_f(color: Color) -> (f64, f64, f64) {
    let r = color.r as f64 / 255.0;
    let g = color.g as f64 / 255.0;
    let b = color.b as f64 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let val = max;
    let sat = if max > 0.0 { delta / max } else { 0.0 };
    let hue = if delta == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / delta + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if max == g {
        ((b - r) / delta + 2.0) / 6.0
    } else {
        ((r - g) / delta + 4.0) / 6.0
    };
    (
        hue.rem_euclid(1.0),
        sat.clamp(0.0, 1.0),
        val.clamp(0.0, 1.0),
    )
}

/// Converts normalized HSV (`hue in [0, 1]`, `sat in [0, 1]`, `val in [0, 1]`) to opaque sRGB.
#[must_use]
pub fn hsv_f_to_rgb(hue: f64, sat: f64, val: f64) -> Color {
    let h = hue.rem_euclid(1.0) * 6.0;
    let s = sat.clamp(0.0, 1.0);
    let v = val.clamp(0.0, 1.0);
    let sector = h.floor() as i32;
    let f = h - h.floor();
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match sector % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    Color::rgb(
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

/// Converts sRGB to [`HsvColor`].
#[must_use]
pub fn rgb_to_hsv(color: Color) -> HsvColor {
    let (h, s, v) = rgb_to_hsv_f(color);
    HsvColor {
        hue: ((h * 360.0).round() as i32).rem_euclid(360) as u16,
        saturation: (s * 255.0).round().clamp(0.0, 255.0) as u8,
        value: (v * 255.0).round().clamp(0.0, 255.0) as u8,
    }
}

/// Converts [`HsvColor`] + alpha to sRGBA.
#[must_use]
pub fn hsv_to_rgba(hsv: HsvColor, alpha: u8) -> Color {
    let hue = (hsv.hue % 360) as f64 / 360.0;
    let rgb = hsv_f_to_rgb(hue, hsv.saturation as f64 / 255.0, hsv.value as f64 / 255.0);
    Color::rgba(rgb.r, rgb.g, rgb.b, alpha)
}

/// Converts sRGB to [`HslColor`].
#[must_use]
pub fn rgb_to_hsl(color: Color) -> HslColor {
    let r = color.r as f64 / 255.0;
    let g = color.g as f64 / 255.0;
    let b = color.b as f64 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let l = (max + min) / 2.0;
    let s = if delta == 0.0 {
        0.0
    } else {
        delta / (1.0 - (2.0 * l - 1.0).abs())
    };
    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / delta + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if max == g {
        ((b - r) / delta + 2.0) / 6.0
    } else {
        ((r - g) / delta + 4.0) / 6.0
    };
    HslColor {
        hue: ((h * 360.0).round() as i32).rem_euclid(360) as u16,
        saturation: (s * 255.0).round().clamp(0.0, 255.0) as u8,
        lightness: (l * 255.0).round().clamp(0.0, 255.0) as u8,
    }
}

/// Converts [`HslColor`] + alpha to sRGBA.
#[must_use]
pub fn hsl_to_rgba(hsl: HslColor, alpha: u8) -> Color {
    let h = (hsl.hue % 360) as f64 / 360.0;
    let s = hsl.saturation as f64 / 255.0;
    let l = hsl.lightness as f64 / 255.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_prime = h * 6.0;
    let x = c * (1.0 - ((h_prime % 2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = match h_prime.floor() as i32 % 6 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Color::rgba(
        ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        alpha,
    )
}

/// Headless model of `ColorEditor`'s 2D picker + sliders (`ui/widgets/color_editor.cpp`).
#[derive(Debug, Clone, PartialEq)]
pub struct ColorSliderState {
    mode: ColorSliderMode,
    picker_x: f64,
    picker_y: f64,
    hue_slider: f64,
    lightness_slider: f64,
    opacity_slider: f64,
    lightness_min: u8,
    lightness_max: u8,
    color: Color,
}

impl ColorSliderState {
    /// Creates a new color slider state initialized to `color`.
    #[must_use]
    pub fn new(mode: ColorSliderMode, color: Color) -> Self {
        let mut state = Self {
            mode,
            picker_x: 0.0,
            picker_y: 0.0,
            hue_slider: 0.0,
            lightness_slider: 0.5,
            opacity_slider: 1.0,
            lightness_min: 0,
            lightness_max: 255,
            color,
        };
        state.set_color(color);
        state
    }

    /// Returns the active color slider mode.
    #[must_use]
    pub const fn mode(&self) -> ColorSliderMode {
        self.mode
    }

    /// Returns the currently selected RGBA color.
    #[must_use]
    pub const fn color(&self) -> Color {
        self.color
    }

    /// Returns the 2D picker coordinates `(x, y)` in `[0.0, 1.0]`.
    #[must_use]
    pub const fn picker_position(&self) -> (f64, f64) {
        (self.picker_x, self.picker_y)
    }

    /// Returns the lightness slider position in `[0.0, 1.0]` (HSL mode).
    #[must_use]
    pub const fn lightness_slider(&self) -> f64 {
        self.lightness_slider
    }

    /// Returns the hue slider position in `[0.0, 1.0]` (RGBA mode, inverted: 0=360°, 1=0°).
    #[must_use]
    pub const fn hue_slider(&self) -> f64 {
        self.hue_slider
    }

    /// Returns the opacity slider position in `[0.0, 1.0]` (RGBA mode).
    #[must_use]
    pub const fn opacity_slider(&self) -> f64 {
        self.opacity_slider
    }

    /// Sets lightness clamping bounds in HSL mode (`ColorEditor::setLightnessLimits`).
    pub fn set_lightness_limits(&mut self, min: u8, max: u8) {
        if max <= min {
            return;
        }
        self.lightness_min = min;
        self.lightness_max = max;
        let adjusted = self.apply_limits(self.color);
        self.set_color(adjusted);
    }

    /// Updates state from an RGB/RGBA color.
    pub fn set_color(&mut self, color: Color) {
        let clamped = self.apply_limits(color);
        self.color = clamped;
        match self.mode {
            ColorSliderMode::Hsl => {
                let hsl = rgb_to_hsl(clamped);
                self.picker_x = (hsl.hue as f64 / 360.0).clamp(0.0, 1.0);
                self.picker_y = 1.0 - (hsl.saturation as f64 / 255.0).clamp(0.0, 1.0);
                let range = (self.lightness_max - self.lightness_min) as f64;
                self.lightness_slider = if range > 0.0 {
                    ((hsl.lightness as f64 - self.lightness_min as f64) / range).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                self.opacity_slider = 1.0;
            }
            ColorSliderMode::Rgba => {
                let hsv = rgb_to_hsv(clamped);
                self.picker_x = (hsv.saturation as f64 / 255.0).clamp(0.0, 1.0);
                self.picker_y = 1.0 - (hsv.value as f64 / 255.0).clamp(0.0, 1.0);
                self.hue_slider = 1.0 - (hsv.hue as f64 / 360.0).clamp(0.0, 1.0);
                self.opacity_slider = (clamped.a as f64 / 255.0).clamp(0.0, 1.0);
            }
        }
    }

    /// Updates the 2D picker normalized coordinates `(x, y)` in `[0.0, 1.0]`.
    pub fn set_picker_position(&mut self, x: f64, y: f64) {
        self.picker_x = x.clamp(0.0, 1.0);
        self.picker_y = y.clamp(0.0, 1.0);
        self.recompute_color_from_controls();
    }

    /// Updates the lightness slider normalized value in `[0.0, 1.0]` (HSL mode).
    pub fn set_lightness_slider(&mut self, value: f64) {
        self.lightness_slider = value.clamp(0.0, 1.0);
        self.recompute_color_from_controls();
    }

    /// Updates the hue slider normalized value in `[0.0, 1.0]` (RGBA mode).
    pub fn set_hue_slider(&mut self, value: f64) {
        self.hue_slider = value.clamp(0.0, 1.0);
        self.recompute_color_from_controls();
    }

    /// Updates the opacity slider normalized value in `[0.0, 1.0]` (RGBA mode).
    pub fn set_opacity_slider(&mut self, value: f64) {
        self.opacity_slider = value.clamp(0.0, 1.0);
        self.recompute_color_from_controls();
    }

    /// Computes perceived lightness (`0.2989 R + 0.5870 G + 0.1140 B`) for cursor contrast.
    #[must_use]
    pub fn perceived_lightness(&self) -> f64 {
        0.2989 * (self.color.r as f64 / 255.0)
            + 0.5870 * (self.color.g as f64 / 255.0)
            + 0.1140 * (self.color.b as f64 / 255.0)
    }

    fn apply_limits(&self, color: Color) -> Color {
        if self.mode != ColorSliderMode::Hsl {
            return color;
        }
        let hsl = rgb_to_hsl(color);
        let clamped = hsl.lightness.clamp(self.lightness_min, self.lightness_max);
        if clamped == hsl.lightness {
            color
        } else {
            hsl_to_rgba(
                HslColor {
                    lightness: clamped,
                    ..hsl
                },
                255,
            )
        }
    }

    fn recompute_color_from_controls(&mut self) {
        match self.mode {
            ColorSliderMode::Hsl => {
                let hue = (self.picker_x * 360.0).round().clamp(0.0, 360.0) as u16;
                let sat = ((1.0 - self.picker_y) * 255.0).round().clamp(0.0, 255.0) as u8;
                let range = (self.lightness_max - self.lightness_min) as f64;
                let lightness = (self.lightness_min as f64
                    + (self.lightness_slider * range).round())
                .clamp(self.lightness_min as f64, self.lightness_max as f64)
                    as u8;
                self.color = hsl_to_rgba(
                    HslColor {
                        hue,
                        saturation: sat,
                        lightness,
                    },
                    255,
                );
            }
            ColorSliderMode::Rgba => {
                let hue = ((1.0 - self.hue_slider) * 360.0).round().clamp(0.0, 360.0) as u16;
                let sat = (self.picker_x * 255.0).round().clamp(0.0, 255.0) as u8;
                let val = ((1.0 - self.picker_y) * 255.0).round().clamp(0.0, 255.0) as u8;
                let alpha = (self.opacity_slider * 255.0).round().clamp(0.0, 255.0) as u8;
                self.color = hsv_to_rgba(
                    HsvColor {
                        hue,
                        saturation: sat,
                        value: val,
                    },
                    alpha,
                );
            }
        }
    }
}

/// Vertical brush-size slider math (`ColorPicker::sizeControl*` in `color_picker.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushSizeSlider {
    /// Total height of the slider widget (default `280`).
    pub control_height: i32,
    /// Vertical hit padding at top and bottom (default `24`).
    pub hit_padding: i32,
    /// Diameter of the top (widest) end of the tapered slider (default `25`).
    pub expanded_top_width: i32,
    /// Diameter of the bottom (narrowest) end of the tapered slider (default `4`).
    pub expanded_bottom_width: i32,
}

impl Default for BrushSizeSlider {
    fn default() -> Self {
        Self {
            control_height: SIZE_CONTROL_HEIGHT,
            hit_padding: SIZE_CONTROL_HIT_PADDING,
            expanded_top_width: SIZE_CONTROL_EXPANDED_TOP_WIDTH,
            expanded_bottom_width: SIZE_CONTROL_EXPANDED_BOTTOM_WIDTH,
        }
    }
}

impl BrushSizeSlider {
    /// Top Y coordinate of the usable drag range (`sizeControlTop`).
    #[must_use]
    pub const fn top(self) -> i32 {
        self.hit_padding + (self.expanded_top_width / 2)
    }

    /// Bottom Y coordinate of the usable drag range (`sizeControlBottom`).
    #[must_use]
    pub const fn bottom(self) -> i32 {
        (self.control_height - self.hit_padding) - (self.expanded_bottom_width / 2)
    }

    /// Converts a pointer Y coordinate to a brush size ratio in `[0.1, 1.0]`
    /// (`ColorPicker::sizeControlRatioFromY`).
    #[must_use]
    pub fn ratio_from_y(self, y: i32) -> f64 {
        let top = self.top();
        let bottom = self.bottom();
        if bottom <= top {
            return 1.0;
        }
        let clamped_y = y.clamp(top, bottom);
        let ratio = 1.0 - (clamped_y - top) as f64 / (bottom - top) as f64;
        ratio.clamp(MIN_BRUSH_SIZE_RATIO, 1.0)
    }

    /// Converts a brush size ratio in `[0.1, 1.0]` to a handle Y coordinate
    /// (`ColorPicker::sizeControlYFromRatio`).
    #[must_use]
    pub fn y_from_ratio(self, ratio: f64) -> i32 {
        let top = self.top();
        let bottom = self.bottom();
        if bottom <= top {
            return top;
        }
        let normalized =
            ((ratio - MIN_BRUSH_SIZE_RATIO) / (1.0 - MIN_BRUSH_SIZE_RATIO)).clamp(0.0, 1.0);
        let y = (bottom as f64 + (top - bottom) as f64 * normalized).round() as i32;
        y.clamp(top, bottom)
    }

    /// Computes the rendered handle diameter for a given brush size ratio
    /// (`ColorPicker::sizeControlHandleSize`).
    #[must_use]
    pub fn handle_size(self, ratio: f64) -> f64 {
        let width = MIN_BRUSH_WIDTH + (MAX_BRUSH_WIDTH - MIN_BRUSH_WIDTH) * ratio;
        width.clamp(
            self.expanded_bottom_width as f64,
            self.expanded_top_width as f64,
        )
    }
}

/// Persisted photo editor brush & tool preferences (`BrushState` in `photo_editor.cpp`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrushState {
    /// Per-tool saved brushes in UI order (`Pen, Arrow, Marker, Blur, Eraser`).
    pub brushes: [Brush; 5],
    /// Currently selected brush tool.
    pub tool: BrushTool,
    /// Whether vector shapes are drawn filled.
    pub fill_shapes: bool,
    /// Saved text overlay preferences.
    pub text_prefs: TextPrefs,
}

impl Default for BrushState {
    fn default() -> Self {
        Self {
            brushes: [
                Brush::default_for(BrushTool::Pen),
                Brush::default_for(BrushTool::Arrow),
                Brush::default_for(BrushTool::Marker),
                Brush::default_for(BrushTool::Blur),
                Brush::default_for(BrushTool::Eraser),
            ],
            tool: BrushTool::Pen,
            fill_shapes: false,
            text_prefs: TextPrefs::default(),
        }
    }
}

impl BrushState {
    /// Returns the currently active brush.
    #[must_use]
    pub fn active_brush(&self) -> Brush {
        let mut brush = self.brushes[self.tool.ui_index()];
        brush.tool = self.tool;
        brush.normalize_color();
        brush
    }

    /// Selects a brush tool and returns its saved brush settings.
    pub fn select_tool(&mut self, tool: BrushTool) -> Brush {
        self.tool = tool;
        self.active_brush()
    }

    /// Sets the active brush's color (no-op for fixed-color tools like Eraser/Blur).
    pub fn set_color(&mut self, color: Color) {
        let idx = self.tool.ui_index();
        self.brushes[idx].color = color;
        self.brushes[idx].normalize_color();
    }

    /// Sets the active brush's normalized size ratio (clamped to `[0.1, 1.0]`).
    pub fn set_size_ratio(&mut self, ratio: f64) {
        let idx = self.tool.ui_index();
        self.brushes[idx].size_ratio = ratio.clamp(MIN_BRUSH_SIZE_RATIO, 1.0);
    }

    /// Serializes to tdesktop's binary `QDataStream` (`Qt_5_3`) settings format
    /// (`Serialize` in `editor/photo_editor.cpp`).
    #[must_use]
    pub fn serialize_tdesktop(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(128);
        write_i32_be(&mut out, BRUSHES_VERSION);
        write_i32_be(&mut out, self.tool as i32);
        write_i32_be(&mut out, self.brushes.len() as i32);
        for (i, brush) in self.brushes.iter().enumerate() {
            let tool = BrushTool::from_ui_index(i);
            write_i32_be(&mut out, tool as i32);
            write_i32_be(&mut out, (brush.size_ratio * PRECISION) as i32);
            write_qcolor_be(&mut out, brush.color);
        }
        write_i32_be(&mut out, i32::from(self.fill_shapes));
        write_i32_be(&mut out, self.text_prefs.style as i32);
        write_i32_be(&mut out, self.text_prefs.typeface as i32);
        write_i32_be(&mut out, self.text_prefs.alignment as i32);
        write_i32_be(&mut out, (self.text_prefs.size_ratio * PRECISION) as i32);
        out
    }

    /// Deserializes from tdesktop's binary `QDataStream` settings format
    /// (`Deserialize` in `editor/photo_editor.cpp`).
    ///
    /// Invalid or truncated payloads gracefully fall back to defaults for any
    /// unread fields, matching tdesktop's behavior.
    #[must_use]
    pub fn deserialize_tdesktop(data: &[u8]) -> Self {
        let mut result = Self::default();
        let mut cur = ByteCursor::new(data);
        let Some(head) = cur.read_i32_be() else {
            return result;
        };
        if head < 0 {
            let version = head;
            let (Some(tool_val), Some(count)) = (cur.read_i32_be(), cur.read_i32_be()) else {
                return result;
            };
            result.tool = BrushTool::from_serialized(tool_val);
            let limit = count.clamp(0, result.brushes.len() as i32) as usize;
            for _ in 0..limit {
                let (Some(entry_tool), Some(size), Some(color)) =
                    (cur.read_i32_be(), cur.read_i32_be(), cur.read_qcolor_be())
                else {
                    return result;
                };
                let tool = BrushTool::from_serialized(entry_tool);
                let idx = tool.ui_index();
                if version == BRUSHES_VERSION && size > 0 {
                    result.brushes[idx].size_ratio = size as f64 / PRECISION;
                }
                if let Some(c) = color {
                    result.brushes[idx].color = c;
                }
                result.brushes[idx].tool = tool;
            }
            if let Some(fill_shapes) = cur.read_i32_be() {
                result.fill_shapes = fill_shapes == 1;
            }
            if let (Some(style), Some(typeface), Some(alignment), Some(size_ratio)) = (
                cur.read_i32_be(),
                cur.read_i32_be(),
                cur.read_i32_be(),
                cur.read_i32_be(),
            ) {
                result.text_prefs = TextPrefs {
                    style: TextStyle::from_serialized(style),
                    typeface: TextTypeface::from_serialized(typeface),
                    alignment: TextAlignment::from_serialized(alignment),
                    size_ratio: if size_ratio > 0 {
                        size_ratio as f64 / PRECISION
                    } else {
                        0.0
                    },
                };
            }
            return result;
        }

        // Legacy format: head was brush size, followed by QColor and optional tool id.
        let Some(color) = cur.read_qcolor_be() else {
            return result;
        };
        let tool_val = cur.read_i32_be().unwrap_or(BrushTool::Pen as i32);
        let tool = BrushTool::from_serialized(tool_val);
        let idx = tool.ui_index();
        if let Some(c) = color {
            result.brushes[idx].color = c;
        }
        result.brushes[idx].tool = tool;
        result.tool = tool;
        result
    }
}

fn write_i32_be(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_be_bytes());
}

/// Writes `QColor` in `QDataStream::Qt_5_3` format:
/// `i8 spec(1=Rgb)`, `u16 alpha`, `u16 red`, `u16 green`, `u16 blue`, `u16 pad(0)`.
fn write_qcolor_be(buf: &mut Vec<u8>, c: Color) {
    buf.push(1u8); // QColor::Rgb
    buf.extend_from_slice(&(u16::from(c.a) * 257).to_be_bytes());
    buf.extend_from_slice(&(u16::from(c.r) * 257).to_be_bytes());
    buf.extend_from_slice(&(u16::from(c.g) * 257).to_be_bytes());
    buf.extend_from_slice(&(u16::from(c.b) * 257).to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes());
}

struct ByteCursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteCursor<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        if end > self.data.len() {
            return None;
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Some(slice)
    }

    fn read_i32_be(&mut self) -> Option<i32> {
        let b = self.read_bytes(4)?;
        Some(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_u16_be(&mut self) -> Option<u16> {
        let b = self.read_bytes(2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }

    /// Reads `QColor` in `QDataStream::Qt_5_3` format.
    /// Returns `None` on stream EOF/error, `Some(None)` for `QColor::Invalid`,
    /// or `Some(Some(Color))` for a valid RGB color.
    fn read_qcolor_be(&mut self) -> Option<Option<Color>> {
        let spec = self.read_bytes(1)?[0];
        let a = self.read_u16_be()?;
        let r = self.read_u16_be()?;
        let g = self.read_u16_be()?;
        let b = self.read_u16_be()?;
        let _pad = self.read_u16_be()?;
        if spec == 0 {
            return Some(None);
        }
        Some(Some(Color::rgba(
            (r >> 8) as u8,
            (g >> 8) as u8,
            (b >> 8) as u8,
            (a >> 8) as u8,
        )))
    }
}
