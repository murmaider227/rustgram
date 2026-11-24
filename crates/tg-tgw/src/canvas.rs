//! Budgeted 2D canvas executor and paint allowance tracking for sandboxed HTML widgets.
//!
//! Port origin: `Telegram/SourceFiles/tgw/tgw_canvas.h/.cpp` and `tgw_painter.h/.cpp`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tiny_skia::{
    BlendMode, Color as SkColor, FillRule, LineCap as SkLineCap, LineJoin as SkLineJoin, Paint,
    PathBuilder, Pixmap, Rect as SkRect, Stroke, StrokeDash, Transform,
};

use crate::error::{Result, TgwError};
use crate::filters::{ClipStack, FilterOp, apply_filters, measure_filters};

/// Bytes per RGBA8 pixel (`kPixelBytes = 4` in `tgw_canvas.cpp:16`).
pub const CANVAS_PIXEL_BYTES: usize = 4;

/// Maximum canvas width or height in pixels (`kMaxSide = 8192` in `tgw_canvas.cpp:17`).
pub const MAX_CANVAS_SIDE: u32 = 8192;

/// Maximum bytes of canvas backing stores per widget backend (`kMaxBackendBytes = 64 MiB`).
pub const MAX_CANVAS_BACKEND_BYTES: usize = 64 * 1024 * 1024;

/// Maximum bytes of canvas backing stores across all widgets (`kMaxTotalBytes = 128 MiB`).
pub const MAX_CANVAS_TOTAL_BYTES: usize = 128 * 1024 * 1024;

/// Maximum bytes read back in one `canvas_read` (`kMaxReadBytes = 64 MiB`).
pub const MAX_CANVAS_READ_BYTES: usize = 64 * 1024 * 1024;

/// Maximum pixel-work units per frame (`kMaxPixelWork = 1 << 25` = 33,554,432).
pub const MAX_CANVAS_PIXEL_WORK: u64 = 1 << 25;

/// Maximum simultaneous canvases per widget (`kMaxCanvases = 128`).
pub const MAX_CANVASES: usize = 128;

/// Maximum UTF-8 bytes in a single canvas text op (`kMaxOpText = 4096`).
pub const MAX_OP_TEXT: usize = 4096;

/// Maximum gradient stops per op (`kMaxOpStops = 256`).
pub const MAX_OP_STOPS: usize = 256;

/// Maximum dash pattern segments per op (`kMaxOpDashes = 128`).
pub const MAX_OP_DASHES: usize = 128;

/// Maximum clip stack depth per op (`kMaxOpClips = 1025`).
pub const MAX_OP_CLIPS: usize = 1025;

/// Maximum shadow blur radius (`kMaxShadowBlur = 100.0`).
pub const MAX_SHADOW_BLUR: f64 = 100.0;

/// Default stroke miter limit (`kMiterLimit = 10.0`).
pub const DEFAULT_MITER_LIMIT: f32 = 10.0;

/// Diagnostic kind for canvas resource limit exceeded (`kLimitKind` in `tgw_canvas.cpp:53`).
pub const CANVAS_LIMIT_DIAG_KIND: &str = "canvas-limit";

/// W3C Canvas 2D composite / blend modes (`tgw_canvas.cpp:120-152`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompositeMode {
    #[default]
    SourceOver,
    SourceIn,
    SourceOut,
    SourceAtop,
    DestinationOver,
    DestinationIn,
    DestinationOut,
    DestinationAtop,
    Lighter,
    Copy,
    Xor,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    // W3C Compositing Level 1 non-separable HSL blend modes.
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl CompositeMode {
    /// Returns `true` if this mode is one of the four W3C non-separable HSL blend modes
    /// (`Hue`, `Saturation`, `Color`, `Luminosity`).
    #[must_use]
    pub const fn is_non_separable(self) -> bool {
        matches!(
            self,
            Self::Hue | Self::Saturation | Self::Color | Self::Luminosity
        )
    }

    /// Returns `Some(name)` if this mode is unsupported.
    ///
    /// All 26 W3C Canvas 2D composite modes (including `Hue`, `Saturation`, `Color`, and
    /// `Luminosity`) are implemented, so this always returns `None`.
    #[must_use]
    pub const fn unsupported_name(self) -> Option<&'static str> {
        None
    }

    fn to_skia(self) -> BlendMode {
        match self {
            Self::SourceOver | Self::Hue | Self::Saturation | Self::Color | Self::Luminosity => {
                BlendMode::SourceOver
            }
            Self::SourceIn => BlendMode::SourceIn,
            Self::SourceOut => BlendMode::SourceOut,
            Self::SourceAtop => BlendMode::SourceAtop,
            Self::DestinationOver => BlendMode::DestinationOver,
            Self::DestinationIn => BlendMode::DestinationIn,
            Self::DestinationOut => BlendMode::DestinationOut,
            Self::DestinationAtop => BlendMode::DestinationAtop,
            Self::Lighter => BlendMode::Plus,
            Self::Copy => BlendMode::Source,
            Self::Xor => BlendMode::Xor,
            Self::Multiply => BlendMode::Multiply,
            Self::Screen => BlendMode::Screen,
            Self::Overlay => BlendMode::Overlay,
            Self::Darken => BlendMode::Darken,
            Self::Lighten => BlendMode::Lighten,
            Self::ColorDodge => BlendMode::ColorDodge,
            Self::ColorBurn => BlendMode::ColorBurn,
            Self::HardLight => BlendMode::HardLight,
            Self::SoftLight => BlendMode::SoftLight,
            Self::Difference => BlendMode::Difference,
            Self::Exclusion => BlendMode::Exclusion,
        }
    }
}

/// Stroke line cap style (`PathCap` in `tgw_painter.h`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

/// Stroke line join style (`PathJoin` in `tgw_painter.h`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

/// RGBA8 color value for canvas operations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgba {
    /// Red channel (`0..=255`).
    pub r: u8,
    /// Green channel (`0..=255`).
    pub g: u8,
    /// Blue channel (`0..=255`).
    pub b: u8,
    /// Alpha channel (`0..=255`).
    pub a: u8,
}

impl Rgba {
    /// Constructs an RGBA color.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

/// 2D affine matrix `[a, b, c, d, tx, ty]` (`Matrix` in `tgw_canvas.cpp:196-199`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Matrix2D(pub [f32; 6]);

impl Default for Matrix2D {
    fn default() -> Self {
        Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
    }
}

impl Matrix2D {
    /// Returns `true` if all 6 matrix components are finite.
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.0.iter().all(|v| v.is_finite())
    }

    fn to_skia(self) -> Transform {
        let [sx, ky, kx, sy, tx, ty] = self.0;
        Transform::from_row(sx, ky, kx, sy, tx, ty)
    }
}

/// Vector path command (`twlite::Path::Op`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PathOp {
    /// Move cursor to `(x, y)`.
    MoveTo(f32, f32),
    /// Line segment to `(x, y)`.
    LineTo(f32, f32),
    /// Quadratic Bezier curve through `(cx, cy)` to `(x, y)`.
    QuadTo(f32, f32, f32, f32),
    /// Cubic Bezier curve through `(c1x, c1y)` and `(c2x, c2y)` to `(x, y)`.
    CubicTo(f32, f32, f32, f32, f32, f32),
    /// Close current subpath.
    Close,
}

/// Single canvas drawing command (`twlite::CanvasOp`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CanvasOp {
    /// Clears a rectangle to transparent (`clearRect`).
    ClearRect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        matrix: Matrix2D,
    },
    /// Fills a rectangle with solid color (`fillRect`).
    FillRect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: Rgba,
        alpha: f32,
        composite: CompositeMode,
        matrix: Matrix2D,
    },
    /// Fills a vector path (`fill`).
    FillPath {
        ops: Vec<PathOp>,
        even_odd: bool,
        color: Rgba,
        alpha: f32,
        composite: CompositeMode,
        matrix: Matrix2D,
    },
    /// Strokes a vector path (`stroke`).
    StrokePath {
        ops: Vec<PathOp>,
        color: Rgba,
        alpha: f32,
        line_width: f32,
        line_cap: LineCap,
        line_join: LineJoin,
        miter_limit: f32,
        dashes: Vec<f32>,
        dash_offset: f32,
        composite: CompositeMode,
        matrix: Matrix2D,
    },
    /// Writes raw unpremultiplied RGBA pixels into `(x, y, width, height)` (`putImageData`).
    PutImageData {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}

/// Computes stack-blur radius from Gaussian sigma (`StackRadius` in `tgw_painter.h:87`).
#[must_use]
pub fn stack_radius(sigma: f64) -> u32 {
    if !sigma.is_finite() || sigma <= 0.0 {
        return 0;
    }
    ((sigma * 1.5).round() as u32).min(MAX_SHADOW_BLUR as u32)
}

/// Computes stroke outer reach accounting for miter joins (`StrokeReach` in `tgw_painter.h:86`).
#[must_use]
pub fn stroke_reach(width: f64, miter_limit: f64) -> f64 {
    if !width.is_finite() || width <= 0.0 {
        return 0.0;
    }
    let miter = if miter_limit.is_finite() && miter_limit > 1.0 {
        miter_limit
    } else {
        1.0
    };
    (width * 0.5) * miter
}

/// Validates canvas dimensions against `MAX_CANVAS_SIDE` and `MAX_CANVAS_BACKEND_BYTES`
/// (`ValidSize` in `tgw_canvas.cpp:163-169`).
#[must_use]
pub fn valid_canvas_size(width: u32, height: u32) -> bool {
    if width == 0 || height == 0 || width > MAX_CANVAS_SIDE || height > MAX_CANVAS_SIDE {
        return false;
    }
    let bytes = (width as u64)
        .saturating_mul(height as u64)
        .saturating_mul(CANVAS_PIXEL_BYTES as u64);
    bytes <= MAX_CANVAS_BACKEND_BYTES as u64
}

/// Diagnostic emitted during canvas execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasDiagnostic {
    /// Diagnostic kind (e.g. `"canvas-limit"` or `"canvas-composite: hue"`).
    pub kind: String,
    /// Human-readable description.
    pub detail: String,
}

/// Per-widget budgeted 2D canvas store (`Tgw::Canvases` in `tgw_canvas.h/.cpp`).
#[derive(Debug, Default)]
pub struct CanvasStore {
    bitmaps: HashMap<i32, Pixmap>,
    bytes_used: usize,
    pixels_charged: u64,
    exhausted: bool,
    noted_limits: u8,
    diagnostics: Vec<CanvasDiagnostic>,
}

impl CanvasStore {
    /// Creates an empty canvas store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns total backing-store bytes currently allocated across all canvases in this store.
    #[must_use]
    pub const fn bytes_used(&self) -> usize {
        self.bytes_used
    }

    /// Returns total pixel-work units charged in the current frame.
    #[must_use]
    pub const fn pixels_charged(&self) -> u64 {
        self.pixels_charged
    }

    /// Returns `true` if the frame's pixel-work budget was exhausted.
    #[must_use]
    pub const fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Drains accumulated diagnostics.
    pub fn take_diagnostics(&mut self) -> Vec<CanvasDiagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    /// Refills per-frame pixel-work allowance (`Canvases::refill` in `tgw_canvas.h:35`).
    pub fn refill(&mut self) {
        self.pixels_charged = 0;
        self.exhausted = false;
    }

    /// Releases one canvas backing store (`Canvases::release`).
    pub fn release(&mut self, id: i32) {
        if let Some(old) = self.bitmaps.remove(&id) {
            self.bytes_used = self.bytes_used.saturating_sub(old.data().len());
        }
    }

    /// Releases all canvases (`Canvases::clear`).
    pub fn clear(&mut self) {
        self.bitmaps.clear();
        self.bytes_used = 0;
        self.pixels_charged = 0;
        self.exhausted = false;
    }

    /// Returns a reference to the backing [`Pixmap`] for `id`, if allocated.
    #[must_use]
    pub fn bitmap(&self, id: i32) -> Option<&Pixmap> {
        self.bitmaps.get(&id)
    }

    /// Returns a mutable reference to the backing [`Pixmap`] for `id`, if allocated.
    pub fn bitmap_mut(&mut self, id: i32) -> Option<&mut Pixmap> {
        self.bitmaps.get_mut(&id)
    }

    /// Applies a chain of W3C [`FilterOp`]s to canvas `id`, charging estimated pixel work.
    pub fn apply_filters(&mut self, id: i32, filters: &[FilterOp], scale: f64) -> Result<()> {
        let Some(pixmap) = self.bitmaps.get(&id) else {
            return Ok(());
        };
        let (w, h) = (pixmap.width(), pixmap.height());
        let cost = measure_filters(filters, w, h, scale).ok_or_else(|| {
            TgwError::ResourceLimit("invalid or oversized canvas filter chain".to_owned())
        })?;
        if !self.charge_pixels(cost.work) {
            return Ok(());
        }
        if let Some(pixmap) = self.bitmaps.get_mut(&id)
            && !apply_filters(pixmap, filters, scale)
        {
            return Err(TgwError::ResourceLimit(
                "failed to apply canvas filter chain".to_owned(),
            ));
        }
        Ok(())
    }

    /// Masks canvas `id` in-place using a nested [`ClipStack`].
    pub fn apply_clip_mask(&mut self, id: i32, clips: &ClipStack) {
        if let Some(pixmap) = self.bitmaps.get_mut(&id) {
            clips.apply_mask_to_pixmap(pixmap);
        }
    }

    fn note_limit(&mut self, bit: u8, detail: &str) {
        if (self.noted_limits & bit) == 0 {
            self.noted_limits |= bit;
            self.diagnostics.push(CanvasDiagnostic {
                kind: CANVAS_LIMIT_DIAG_KIND.to_owned(),
                detail: detail.to_owned(),
            });
        }
    }

    fn charge_pixels(&mut self, pixels: u64) -> bool {
        if self.exhausted {
            return false;
        }
        let next = self.pixels_charged.saturating_add(pixels);
        if next > MAX_CANVAS_PIXEL_WORK {
            self.exhausted = true;
            self.note_limit(
                1,
                "canvas drawing exceeded the work allowance of a frame, the rest of its drawing was skipped",
            );
            return false;
        }
        self.pixels_charged = next;
        true
    }

    fn ensure_canvas(&mut self, id: i32, width: u32, height: u32) -> Result<()> {
        if !valid_canvas_size(width, height) {
            self.release(id);
            self.note_limit(
                2,
                "a canvas exceeded the canvas memory allowance and stays transparent",
            );
            return Err(TgwError::ResourceLimit(format!(
                "invalid canvas dimensions {width}x{height}"
            )));
        }

        if let Some(existing) = self.bitmaps.get(&id)
            && existing.width() == width
            && existing.height() == height
        {
            return Ok(());
        }

        if !self.bitmaps.contains_key(&id) && self.bitmaps.len() >= MAX_CANVASES {
            self.note_limit(
                2,
                "a canvas exceeded the canvas memory allowance and stays transparent",
            );
            return Err(TgwError::ResourceLimit(format!(
                "canvas count limit ({MAX_CANVASES}) reached"
            )));
        }

        let prev_bytes = self
            .bitmaps
            .get(&id)
            .map_or(0, |existing| existing.data().len());
        let needed = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(CANVAS_PIXEL_BYTES);
        let projected = self
            .bytes_used
            .saturating_sub(prev_bytes)
            .saturating_add(needed);
        if projected > MAX_CANVAS_BACKEND_BYTES {
            self.release(id);
            self.note_limit(
                2,
                "a canvas exceeded the canvas memory allowance and stays transparent",
            );
            return Err(TgwError::ResourceLimit(format!(
                "canvas memory {projected} exceeds backend limit {MAX_CANVAS_BACKEND_BYTES}"
            )));
        }

        let pixmap = Pixmap::new(width, height).ok_or_else(|| {
            TgwError::ResourceLimit(format!("failed to allocate {width}x{height} canvas"))
        })?;
        self.bytes_used = projected;
        self.bitmaps.insert(id, pixmap);
        Ok(())
    }

    /// Executes a batch of [`CanvasOp`]s on canvas `id` (`Canvases::run` in `tgw_canvas.cpp`).
    pub fn run(&mut self, id: i32, width: u32, height: u32, ops: &[CanvasOp]) -> Result<()> {
        self.ensure_canvas(id, width, height)?;
        let canvas_pixels = u64::from(width).saturating_mul(u64::from(height));

        for op in ops {
            if !self.charge_pixels(1024 + canvas_pixels.min(65_536)) {
                break;
            }
            match op {
                CanvasOp::ClearRect {
                    x,
                    y,
                    width: rw,
                    height: rh,
                    matrix,
                } => {
                    if !matrix.is_finite()
                        || !x.is_finite()
                        || !y.is_finite()
                        || *rw <= 0.0
                        || *rh <= 0.0
                    {
                        continue;
                    }
                    if let Some(rect) = SkRect::from_xywh(*x, *y, *rw, *rh)
                        && let Some(pixmap) = self.bitmaps.get_mut(&id)
                    {
                        let mut paint = Paint::default();
                        paint.set_color_rgba8(0, 0, 0, 0);
                        paint.blend_mode = BlendMode::Source;
                        pixmap.fill_rect(rect, &paint, matrix.to_skia(), None);
                    }
                }
                CanvasOp::FillRect {
                    x,
                    y,
                    width: rw,
                    height: rh,
                    color,
                    alpha,
                    composite,
                    matrix,
                } => {
                    if !matrix.is_finite()
                        || !x.is_finite()
                        || !y.is_finite()
                        || *rw <= 0.0
                        || *rh <= 0.0
                    {
                        continue;
                    }
                    let a = effective_alpha(color.a, *alpha);
                    if let Some(rect) = SkRect::from_xywh(*x, *y, *rw, *rh)
                        && let Some(pixmap) = self.bitmaps.get_mut(&id)
                    {
                        let mut paint = Paint::default();
                        paint.set_color(SkColor::from_rgba8(color.r, color.g, color.b, a));
                        paint.anti_alias = true;
                        if composite.is_non_separable() {
                            if let Some(mut scratch) = Pixmap::new(pixmap.width(), pixmap.height())
                            {
                                paint.blend_mode = BlendMode::Source;
                                scratch.fill_rect(rect, &paint, matrix.to_skia(), None);
                                composite_hsl_pixmap(pixmap, &scratch, *composite);
                            }
                        } else {
                            paint.blend_mode = composite.to_skia();
                            pixmap.fill_rect(rect, &paint, matrix.to_skia(), None);
                        }
                    }
                }
                CanvasOp::FillPath {
                    ops: path_ops,
                    even_odd,
                    color,
                    alpha,
                    composite,
                    matrix,
                } => {
                    if !matrix.is_finite() {
                        continue;
                    }
                    if let Some(path) = build_skia_path(path_ops)
                        && let Some(pixmap) = self.bitmaps.get_mut(&id)
                    {
                        let a = effective_alpha(color.a, *alpha);
                        let mut paint = Paint::default();
                        paint.set_color(SkColor::from_rgba8(color.r, color.g, color.b, a));
                        paint.anti_alias = true;
                        let rule = if *even_odd {
                            FillRule::EvenOdd
                        } else {
                            FillRule::Winding
                        };
                        if composite.is_non_separable() {
                            if let Some(mut scratch) = Pixmap::new(pixmap.width(), pixmap.height())
                            {
                                paint.blend_mode = BlendMode::Source;
                                scratch.fill_path(&path, &paint, rule, matrix.to_skia(), None);
                                composite_hsl_pixmap(pixmap, &scratch, *composite);
                            }
                        } else {
                            paint.blend_mode = composite.to_skia();
                            pixmap.fill_path(&path, &paint, rule, matrix.to_skia(), None);
                        }
                    }
                }
                CanvasOp::StrokePath {
                    ops: path_ops,
                    color,
                    alpha,
                    line_width,
                    line_cap,
                    line_join,
                    miter_limit,
                    dashes,
                    dash_offset,
                    composite,
                    matrix,
                } => {
                    if !matrix.is_finite()
                        || !line_width.is_finite()
                        || *line_width <= 0.0
                        || dashes.len() > MAX_OP_DASHES
                    {
                        continue;
                    }
                    if let Some(path) = build_skia_path(path_ops)
                        && let Some(pixmap) = self.bitmaps.get_mut(&id)
                    {
                        let a = effective_alpha(color.a, *alpha);
                        let mut paint = Paint::default();
                        paint.set_color(SkColor::from_rgba8(color.r, color.g, color.b, a));
                        paint.anti_alias = true;
                        let mut stroke = Stroke {
                            width: *line_width,
                            miter_limit: if miter_limit.is_finite() && *miter_limit > 0.0 {
                                *miter_limit
                            } else {
                                DEFAULT_MITER_LIMIT
                            },
                            line_cap: match line_cap {
                                LineCap::Butt => SkLineCap::Butt,
                                LineCap::Round => SkLineCap::Round,
                                LineCap::Square => SkLineCap::Square,
                            },
                            line_join: match line_join {
                                LineJoin::Miter => SkLineJoin::Miter,
                                LineJoin::Round => SkLineJoin::Round,
                                LineJoin::Bevel => SkLineJoin::Bevel,
                            },
                            dash: None,
                        };
                        if !dashes.is_empty() && dashes.iter().all(|d| d.is_finite() && *d >= 0.0) {
                            let offset = if dash_offset.is_finite() {
                                *dash_offset
                            } else {
                                0.0
                            };
                            stroke.dash = StrokeDash::new(dashes.clone(), offset);
                        }
                        if composite.is_non_separable() {
                            if let Some(mut scratch) = Pixmap::new(pixmap.width(), pixmap.height())
                            {
                                paint.blend_mode = BlendMode::Source;
                                scratch.stroke_path(&path, &paint, &stroke, matrix.to_skia(), None);
                                composite_hsl_pixmap(pixmap, &scratch, *composite);
                            }
                        } else {
                            paint.blend_mode = composite.to_skia();
                            pixmap.stroke_path(&path, &paint, &stroke, matrix.to_skia(), None);
                        }
                    }
                }
                CanvasOp::PutImageData {
                    x,
                    y,
                    width: rw,
                    height: rh,
                    rgba,
                } => {
                    let expected = (*rw as usize)
                        .saturating_mul(*rh as usize)
                        .saturating_mul(CANVAS_PIXEL_BYTES);
                    if rgba.len() != expected {
                        continue;
                    }
                    if let Some(pixmap) = self.bitmaps.get_mut(&id) {
                        let cw = pixmap.width() as i32;
                        let ch = pixmap.height() as i32;
                        let dst = pixmap.data_mut();
                        for row in 0..(*rh as i32) {
                            let dy = y.saturating_add(row);
                            if dy < 0 || dy >= ch {
                                continue;
                            }
                            for col in 0..(*rw as i32) {
                                let dx = x.saturating_add(col);
                                if dx < 0 || dx >= cw {
                                    continue;
                                }
                                let src_i = ((row as usize) * (*rw as usize) + (col as usize)) * 4;
                                let dst_i = ((dy as usize) * (cw as usize) + (dx as usize)) * 4;
                                let r = rgba[src_i];
                                let g = rgba[src_i + 1];
                                let b = rgba[src_i + 2];
                                let a = rgba[src_i + 3];
                                // tiny-skia stores premultiplied RGBA8.
                                let af = u16::from(a);
                                dst[dst_i] = ((u16::from(r) * af + 127) / 255) as u8;
                                dst[dst_i + 1] = ((u16::from(g) * af + 127) / 255) as u8;
                                dst[dst_i + 2] = ((u16::from(b) * af + 127) / 255) as u8;
                                dst[dst_i + 3] = a;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Reads back unpremultiplied RGBA8 pixels from `(x, y, width, height)` (`Canvases::read`).
    pub fn read(&self, id: i32, x: i32, y: i32, width: u32, height: u32) -> Result<Vec<u8>> {
        if width == 0 || height == 0 {
            return Ok(Vec::new());
        }
        let total = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(CANVAS_PIXEL_BYTES);
        if total > MAX_CANVAS_READ_BYTES {
            return Err(TgwError::ResourceLimit(format!(
                "canvas_read size {total} exceeds limit {MAX_CANVAS_READ_BYTES}"
            )));
        }
        let mut out = vec![0u8; total];
        let Some(pixmap) = self.bitmaps.get(&id) else {
            return Ok(out);
        };
        let cw = pixmap.width() as i32;
        let ch = pixmap.height() as i32;
        let src = pixmap.data();
        for row in 0..(height as i32) {
            let sy = y.saturating_add(row);
            if sy < 0 || sy >= ch {
                continue;
            }
            for col in 0..(width as i32) {
                let sx = x.saturating_add(col);
                if sx < 0 || sx >= cw {
                    continue;
                }
                let src_i = ((sy as usize) * (cw as usize) + (sx as usize)) * 4;
                let dst_i = ((row as usize) * (width as usize) + (col as usize)) * 4;
                let a = src[src_i + 3];
                if a == 0 {
                    continue;
                }
                if a == 255 {
                    out[dst_i..dst_i + 4].copy_from_slice(&src[src_i..src_i + 4]);
                } else {
                    let af = f32::from(a) / 255.0;
                    out[dst_i] = (f32::from(src[src_i]) / af).round().clamp(0.0, 255.0) as u8;
                    out[dst_i + 1] =
                        (f32::from(src[src_i + 1]) / af).round().clamp(0.0, 255.0) as u8;
                    out[dst_i + 2] =
                        (f32::from(src[src_i + 2]) / af).round().clamp(0.0, 255.0) as u8;
                    out[dst_i + 3] = a;
                }
            }
        }
        Ok(out)
    }
}

// =============================================================================
// W3C Compositing and Blending Level 1 — Non-Separable HSL Blend Modes
// =============================================================================

#[inline]
fn w3c_lum(c: [f64; 3]) -> f64 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

#[inline]
fn w3c_sat(c: [f64; 3]) -> f64 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

#[inline]
fn w3c_clip_color(mut c: [f64; 3]) -> [f64; 3] {
    let l = w3c_lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    if n < 0.0 {
        let denom = l - n;
        if denom > 0.0 {
            for ch in &mut c {
                *ch = l + (((*ch - l) * l) / denom);
            }
        } else {
            c = [0.0; 3];
        }
    }
    if x > 1.0 {
        let denom = x - l;
        if denom > 0.0 {
            for ch in &mut c {
                *ch = l + (((*ch - l) * (1.0 - l)) / denom);
            }
        } else {
            c = [1.0; 3];
        }
    }
    [
        c[0].clamp(0.0, 1.0),
        c[1].clamp(0.0, 1.0),
        c[2].clamp(0.0, 1.0),
    ]
}

#[inline]
fn w3c_set_lum(c: [f64; 3], l: f64) -> [f64; 3] {
    let d = l - w3c_lum(c);
    w3c_clip_color([c[0] + d, c[1] + d, c[2] + d])
}

#[inline]
fn w3c_set_sat(mut c: [f64; 3], s: f64) -> [f64; 3] {
    let mut idx = [0usize, 1, 2];
    if c[idx[0]] > c[idx[1]] {
        idx.swap(0, 1);
    }
    if c[idx[1]] > c[idx[2]] {
        idx.swap(1, 2);
    }
    if c[idx[0]] > c[idx[1]] {
        idx.swap(0, 1);
    }
    let min_i = idx[0];
    let mid_i = idx[1];
    let max_i = idx[2];

    let span = c[max_i] - c[min_i];
    if span > 0.0 {
        c[mid_i] = ((c[mid_i] - c[min_i]) * s) / span;
        c[max_i] = s;
    } else {
        c[mid_i] = 0.0;
        c[max_i] = 0.0;
    }
    c[min_i] = 0.0;
    c
}

/// Evaluates the W3C Compositing Level 1 non-separable blend function `B(Cb, Cs)` for straight
/// normalized RGB triples in `[0.0, 1.0]`.
#[must_use]
pub fn blend_hsl_rgb(cb: [f64; 3], cs: [f64; 3], mode: CompositeMode) -> [f64; 3] {
    match mode {
        CompositeMode::Hue => w3c_set_lum(w3c_set_sat(cs, w3c_sat(cb)), w3c_lum(cb)),
        CompositeMode::Saturation => w3c_set_lum(w3c_set_sat(cb, w3c_sat(cs)), w3c_lum(cb)),
        CompositeMode::Color => w3c_set_lum(cs, w3c_lum(cb)),
        CompositeMode::Luminosity => w3c_set_lum(cb, w3c_lum(cs)),
        _ => cs,
    }
}

/// Composites an unpremultiplied RGBA8 `source` pixel over `backdrop` using a W3C non-separable
/// HSL [`CompositeMode`] (`Hue`, `Saturation`, `Color`, `Luminosity`) with Porter-Duff
/// source-over alpha compositing.
#[must_use]
pub fn blend_hsl_pixel(backdrop: Rgba, source: Rgba, mode: CompositeMode) -> Rgba {
    if source.a == 0 {
        return backdrop;
    }
    let ab = f64::from(backdrop.a) / 255.0;
    let as_ = f64::from(source.a) / 255.0;
    let cb = [
        f64::from(backdrop.r) / 255.0,
        f64::from(backdrop.g) / 255.0,
        f64::from(backdrop.b) / 255.0,
    ];
    let cs = [
        f64::from(source.r) / 255.0,
        f64::from(source.g) / 255.0,
        f64::from(source.b) / 255.0,
    ];
    let blended = if backdrop.a == 0 {
        cs
    } else {
        blend_hsl_rgb(cb, cs, mode)
    };
    let ao = as_ + ab * (1.0 - as_);
    if ao <= 0.0 {
        return Rgba::new(0, 0, 0, 0);
    }
    let mut out = [0u8; 3];
    for c in 0..3 {
        let cr = (1.0 - ab) * cs[c] + ab * blended[c];
        let premul = as_ * cr + ab * (1.0 - as_) * cb[c];
        let straight = (premul / ao).clamp(0.0, 1.0);
        out[c] = (straight * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    let a = (ao * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgba::new(out[0], out[1], out[2], a)
}

/// Composites a premultiplied RGBA8 `src` [`Pixmap`] over `dst` using one of the W3C
/// non-separable HSL blend modes (`Hue`, `Saturation`, `Color`, `Luminosity`).
pub fn composite_hsl_pixmap(dst: &mut Pixmap, src: &Pixmap, mode: CompositeMode) {
    if dst.width() != src.width() || dst.height() != src.height() {
        return;
    }
    let ddata = dst.data_mut();
    let sdata = src.data();
    let (d_chunks, _) = ddata.as_chunks_mut::<4>();
    let (s_chunks, _) = sdata.as_chunks::<4>();
    for (d, s) in d_chunks.iter_mut().zip(s_chunks.iter()) {
        let sa = s[3];
        if sa == 0 {
            continue;
        }
        let as_ = f64::from(sa) / 255.0;
        let cs = [
            (f64::from(s[0]) / f64::from(sa)).clamp(0.0, 1.0),
            (f64::from(s[1]) / f64::from(sa)).clamp(0.0, 1.0),
            (f64::from(s[2]) / f64::from(sa)).clamp(0.0, 1.0),
        ];
        let da = d[3];
        if da == 0 {
            d.copy_from_slice(s);
            continue;
        }
        let ab = f64::from(da) / 255.0;
        let cb = [
            (f64::from(d[0]) / f64::from(da)).clamp(0.0, 1.0),
            (f64::from(d[1]) / f64::from(da)).clamp(0.0, 1.0),
            (f64::from(d[2]) / f64::from(da)).clamp(0.0, 1.0),
        ];
        let blended = blend_hsl_rgb(cb, cs, mode);
        let ao = as_ + ab * (1.0 - as_);
        let a_out = (ao * 255.0).round().clamp(0.0, 255.0) as u8;
        let max_c = f64::from(a_out);
        for c in 0..3 {
            let cr = (1.0 - ab) * cs[c] + ab * blended[c];
            let premul = as_ * cr + ab * (1.0 - as_) * cb[c];
            d[c] = (premul * 255.0).round().clamp(0.0, max_c) as u8;
        }
        d[3] = a_out;
    }
}

fn effective_alpha(color_a: u8, op_alpha: f32) -> u8 {
    let clamped = if op_alpha.is_finite() {
        op_alpha.clamp(0.0, 1.0)
    } else {
        1.0
    };
    (f32::from(color_a) * clamped).round().clamp(0.0, 255.0) as u8
}

fn build_skia_path(ops: &[PathOp]) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    for &op in ops {
        match op {
            PathOp::MoveTo(x, y) if x.is_finite() && y.is_finite() => pb.move_to(x, y),
            PathOp::LineTo(x, y) if x.is_finite() && y.is_finite() => pb.line_to(x, y),
            PathOp::QuadTo(cx, cy, x, y)
                if cx.is_finite() && cy.is_finite() && x.is_finite() && y.is_finite() =>
            {
                pb.quad_to(cx, cy, x, y);
            }
            PathOp::CubicTo(c1x, c1y, c2x, c2y, x, y)
                if c1x.is_finite()
                    && c1y.is_finite()
                    && c2x.is_finite()
                    && c2y.is_finite()
                    && x.is_finite()
                    && y.is_finite() =>
            {
                pb.cubic_to(c1x, c1y, c2x, c2y, x, y);
            }
            PathOp::Close => pb.close(),
            _ => {}
        }
    }
    pb.finish()
}
