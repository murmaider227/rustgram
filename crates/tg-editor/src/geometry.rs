//! 2D geometry primitives, colors, and Qt-compatible transform helpers.
//!
//! Port of geometry and color utilities used across `editor/*.cpp`.

use serde::{Deserialize, Serialize};

/// 2D floating-point coordinate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct PointF {
    /// Horizontal coordinate.
    pub x: f64,
    /// Vertical coordinate.
    pub y: f64,
}

impl PointF {
    /// Creates a new point.
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Returns true if both coordinates are finite.
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }

    /// Returns true if both coordinates are zero.
    #[must_use]
    pub fn is_null(self) -> bool {
        self.x == 0.0 && self.y == 0.0
    }

    /// Euclidean distance to another point.
    #[must_use]
    pub fn distance_to(self, other: Self) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

/// 2D integer size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Size {
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
}

impl Size {
    /// Creates a new integer size.
    #[must_use]
    pub const fn new(width: i32, height: i32) -> Self {
        Self { width, height }
    }

    /// Returns true if either dimension is non-positive.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    /// Swaps width and height.
    #[must_use]
    pub const fn transposed(self) -> Self {
        Self {
            width: self.height,
            height: self.width,
        }
    }

    /// Converts to a floating-point size.
    #[must_use]
    pub const fn to_f64(self) -> SizeF {
        SizeF {
            width: self.width as f64,
            height: self.height as f64,
        }
    }

    /// Scales this size to fit inside `bounds` while keeping aspect ratio,
    /// matching `QSize::scaled(bounds, Qt::KeepAspectRatio)`.
    #[must_use]
    pub fn scaled_keep_aspect(self, bounds: Self) -> Self {
        if self.is_empty() || bounds.is_empty() {
            return Self::default();
        }
        let rw = bounds.width as f64 / self.width as f64;
        let rh = bounds.height as f64 / self.height as f64;
        let scale = rw.min(rh);
        Self {
            width: (self.width as f64 * scale).round().max(1.0) as i32,
            height: (self.height as f64 * scale).round().max(1.0) as i32,
        }
    }

    /// Scales this size to cover `bounds` while keeping aspect ratio,
    /// matching `QSize::scaled(bounds, Qt::KeepAspectRatioByExpanding)`.
    #[must_use]
    pub fn scaled_keep_aspect_expanding(self, bounds: Self) -> Self {
        if self.is_empty() || bounds.is_empty() {
            return Self::default();
        }
        let rw = bounds.width as f64 / self.width as f64;
        let rh = bounds.height as f64 / self.height as f64;
        let scale = rw.max(rh);
        Self {
            width: (self.width as f64 * scale).round().max(1.0) as i32,
            height: (self.height as f64 * scale).round().max(1.0) as i32,
        }
    }
}

/// 2D floating-point size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SizeF {
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl SizeF {
    /// Creates a new floating-point size.
    #[must_use]
    pub const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }

    /// Returns true if either dimension is non-positive.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// Swaps width and height.
    #[must_use]
    pub const fn transposed(self) -> Self {
        Self {
            width: self.height,
            height: self.width,
        }
    }

    /// Scales this size to fit inside `bounds` while keeping aspect ratio.
    #[must_use]
    pub fn scaled_keep_aspect(self, bounds: Self) -> Self {
        if self.is_empty() || bounds.is_empty() {
            return Self::default();
        }
        let scale = (bounds.width / self.width).min(bounds.height / self.height);
        Self {
            width: self.width * scale,
            height: self.height * scale,
        }
    }
}

/// Four-sided margins.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Margins {
    /// Left margin.
    pub left: i32,
    /// Top margin.
    pub top: i32,
    /// Right margin.
    pub right: i32,
    /// Bottom margin.
    pub bottom: i32,
}

impl Margins {
    /// Creates margins from individual edges.
    #[must_use]
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Creates uniform margins on all four sides.
    #[must_use]
    pub const fn uniform(value: i32) -> Self {
        Self {
            left: value,
            top: value,
            right: value,
            bottom: value,
        }
    }

    /// Sum of left and right margins.
    #[must_use]
    pub const fn horizontal(self) -> i32 {
        self.left + self.right
    }

    /// Sum of top and bottom margins.
    #[must_use]
    pub const fn vertical(self) -> i32 {
        self.top + self.bottom
    }
}

/// Integer axis-aligned rectangle (`x`, `y`, `width`, `height`).
///
/// Mirrors `QRect` semantics where `is_valid()` means `width > 0 && height > 0`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rect {
    /// Left coordinate (may be negative for expanded canvases).
    pub x: i32,
    /// Top coordinate (may be negative for expanded canvases).
    pub y: i32,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
}

impl Rect {
    /// Creates a new rectangle.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Creates a rectangle at `(0, 0)` with the given size.
    #[must_use]
    pub const fn from_size(size: Size) -> Self {
        Self {
            x: 0,
            y: 0,
            width: size.width,
            height: size.height,
        }
    }

    /// Returns true if width and height are both strictly positive.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.width > 0 && self.height > 0
    }

    /// Returns true if width or height is non-positive.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.is_valid()
    }

    /// Exclusive right edge (`x + width`).
    #[must_use]
    pub const fn right_edge(self) -> i32 {
        self.x + self.width
    }

    /// Exclusive bottom edge (`y + height`).
    #[must_use]
    pub const fn bottom_edge(self) -> i32 {
        self.y + self.height
    }

    /// Size of this rectangle.
    #[must_use]
    pub const fn size(self) -> Size {
        Size::new(self.width, self.height)
    }

    /// Returns true if `other` is completely contained within `self`.
    #[must_use]
    pub const fn contains_rect(self, other: Self) -> bool {
        if !self.is_valid() || !other.is_valid() {
            return false;
        }
        other.x >= self.x
            && other.y >= self.y
            && other.right_edge() <= self.right_edge()
            && other.bottom_edge() <= self.bottom_edge()
    }

    /// Bounding union of two rectangles (empty rect acts as identity).
    #[must_use]
    pub fn united(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = self.right_edge().max(other.right_edge());
        let bottom = self.bottom_edge().max(other.bottom_edge());
        Self::new(left, top, right - left, bottom - top)
    }

    /// Intersection of two rectangles (empty if disjoint).
    #[must_use]
    pub fn intersected(self, other: Self) -> Self {
        if self.is_empty() || other.is_empty() {
            return Self::default();
        }
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = self.right_edge().min(other.right_edge());
        let bottom = self.bottom_edge().min(other.bottom_edge());
        if right <= left || bottom <= top {
            Self::default()
        } else {
            Self::new(left, top, right - left, bottom - top)
        }
    }

    /// Subtracts margins from each edge (`rect - margins`).
    #[must_use]
    pub const fn shrink(self, m: Margins) -> Self {
        Self::new(
            self.x + m.left,
            self.y + m.top,
            self.width - m.left - m.right,
            self.height - m.top - m.bottom,
        )
    }

    /// Translates the rectangle by `(dx, dy)`.
    #[must_use]
    pub const fn translated(self, dx: i32, dy: i32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.width, self.height)
    }

    /// Converts to a floating-point rectangle.
    #[must_use]
    pub const fn to_f64(self) -> RectF {
        RectF {
            x: self.x as f64,
            y: self.y as f64,
            width: self.width as f64,
            height: self.height as f64,
        }
    }
}

/// Floating-point axis-aligned rectangle (`x`, `y`, `width`, `height`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct RectF {
    /// Left coordinate.
    pub x: f64,
    /// Top coordinate.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl RectF {
    /// Creates a new floating-point rectangle.
    #[must_use]
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Creates a rectangle from two corner points (`top_left`, `bottom_right`).
    #[must_use]
    pub fn from_edges(left: f64, top: f64, right: f64, bottom: f64) -> Self {
        Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
    }

    /// Left edge (`x`).
    #[must_use]
    pub const fn left(self) -> f64 {
        self.x
    }

    /// Top edge (`y`).
    #[must_use]
    pub const fn top(self) -> f64 {
        self.y
    }

    /// Right edge (`x + width`).
    #[must_use]
    pub fn right(self) -> f64 {
        self.x + self.width
    }

    /// Bottom edge (`y + height`).
    #[must_use]
    pub fn bottom(self) -> f64 {
        self.y + self.height
    }

    /// Top-left point.
    #[must_use]
    pub const fn top_left(self) -> PointF {
        PointF::new(self.x, self.y)
    }

    /// Center point.
    #[must_use]
    pub fn center(self) -> PointF {
        PointF::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    /// Size of this rectangle.
    #[must_use]
    pub const fn size(self) -> SizeF {
        SizeF::new(self.width, self.height)
    }

    /// Returns true if width and height are strictly positive.
    #[must_use]
    pub fn is_valid(self) -> bool {
        self.width > 0.0 && self.height > 0.0
    }

    /// Returns true if width or height is non-positive.
    #[must_use]
    pub fn is_empty(self) -> bool {
        !self.is_valid()
    }

    /// Returns true if `point` lies within `[left, right] x [top, bottom]`.
    #[must_use]
    pub fn contains_point(self, point: PointF) -> bool {
        if self.is_empty() {
            return false;
        }
        point.x >= self.left()
            && point.x <= self.right()
            && point.y >= self.top()
            && point.y <= self.bottom()
    }

    /// Translates the rectangle by `(dx, dy)`.
    #[must_use]
    pub fn translated(self, dx: f64, dy: f64) -> Self {
        Self::new(self.x + dx, self.y + dy, self.width, self.height)
    }

    /// Expands outward by `margin` on all four sides.
    #[must_use]
    pub fn expanded(self, margin: f64) -> Self {
        Self::new(
            self.x - margin,
            self.y - margin,
            self.width + margin * 2.0,
            self.height + margin * 2.0,
        )
    }

    /// Bounding union of two rectangles.
    #[must_use]
    pub fn united(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let left = self.left().min(other.left());
        let top = self.top().min(other.top());
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self::from_edges(left, top, right, bottom)
    }

    /// Intersection of two rectangles.
    #[must_use]
    pub fn intersected(self, other: Self) -> Self {
        if self.is_empty() || other.is_empty() {
            return Self::default();
        }
        let left = self.left().max(other.left());
        let top = self.top().max(other.top());
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= left || bottom <= top {
            Self::default()
        } else {
            Self::from_edges(left, top, right, bottom)
        }
    }

    /// Returns true if the two rectangles intersect with positive area.
    #[must_use]
    pub fn intersects(self, other: Self) -> bool {
        !self.intersected(other).is_empty()
    }

    /// Converts to integer `Rect` using Qt's `QRectF::toRect()` rounding rules.
    #[must_use]
    pub fn to_rect(self) -> Rect {
        let left = self.x.round() as i32;
        let top = self.y.round() as i32;
        let right = (self.x + self.width).round() as i32;
        let bottom = (self.y + self.height).round() as i32;
        Rect::new(left, top, right - left, bottom - top)
    }

    /// Converts to aligned enclosing integer `Rect` (`QRectF::toAlignedRect()`).
    #[must_use]
    pub fn to_aligned_rect(self) -> Rect {
        let left = self.x.floor() as i32;
        let top = self.y.floor() as i32;
        let right = (self.x + self.width).ceil() as i32;
        let bottom = (self.y + self.height).ceil() as i32;
        Rect::new(left, top, right - left, bottom - top)
    }
}

/// 8-bit sRGBA color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Color {
    /// Red channel (`0..=255`).
    pub r: u8,
    /// Green channel (`0..=255`).
    pub g: u8,
    /// Blue channel (`0..=255`).
    pub b: u8,
    /// Alpha channel (`0..=255`, 255 = opaque).
    pub a: u8,
}

impl Default for Color {
    fn default() -> Self {
        Self::BLACK
    }
}

impl Color {
    /// Opaque black `(0, 0, 0, 255)`.
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    /// Opaque white `(255, 255, 255, 255)`.
    pub const WHITE: Self = Self::rgb(255, 255, 255);
    /// Fully transparent `(0, 0, 0, 0)`.
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);

    /// Creates an opaque RGB color.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Creates an RGBA color.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Returns a copy with alpha multiplied by `factor` (clamped to `[0, 1]`).
    #[must_use]
    pub fn with_alpha_factor(self, factor: f64) -> Self {
        let a = ((self.a as f64) * factor.clamp(0.0, 1.0)).round() as u8;
        Self { a, ..self }
    }

    /// Converts to `tiny_skia::Color`.
    #[must_use]
    pub fn to_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.r, self.g, self.b, self.a)
    }

    /// Formats as `#aarrggbb` matching Qt `QColor::name(QColor::HexArgb)`.
    #[must_use]
    pub fn to_hex_argb(self) -> String {
        format!("#{:02x}{:02x}{:02x}{:02x}", self.a, self.r, self.g, self.b)
    }

    /// Parses `#rrggbb` or `#aarrggbb` (with or without leading `#`).
    #[must_use]
    pub fn from_hex_argb(s: &str) -> Option<Self> {
        let hex = s.strip_prefix('#').unwrap_or(s);
        match hex.len() {
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some(Self::rgb(r, g, b))
            }
            8 => {
                let a = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let r = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let g = u8::from_str_radix(&hex[4..6], 16).ok()?;
                let b = u8::from_str_radix(&hex[6..8], 16).ok()?;
                Some(Self::rgba(r, g, b, a))
            }
            _ => None,
        }
    }

    /// Packs as `0xAARRGGBB` (matching `QRgb`).
    #[must_use]
    pub const fn to_argb_u32(self) -> u32 {
        ((self.a as u32) << 24) | ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }
}

/// Normalizes an angle in degrees to `{0, 90, 180, 270}` (clockwise).
#[must_use]
pub const fn normalize_angle_90(angle: i32) -> i32 {
    let rem = angle.rem_euclid(360);
    ((rem + 45) / 90 * 90) % 360
}

/// Returns `size.transposed()` when `angle` is 90° or 270°, matching `FlipSizeByRotation`.
#[must_use]
pub const fn flip_size_by_rotation(size: Size, angle: i32) -> Size {
    if (normalize_angle_90(angle) / 90) % 2 == 1 {
        size.transposed()
    } else {
        size
    }
}

/// Floating-point variant of [`flip_size_by_rotation`].
#[must_use]
pub const fn flip_size_f_by_rotation(size: SizeF, angle: i32) -> SizeF {
    if (normalize_angle_90(angle) / 90) % 2 == 1 {
        size.transposed()
    } else {
        size
    }
}

/// 2D affine matrix using Qt row-vector convention:
/// `x' = m11 * x + m21 * y + dx`, `y' = m12 * x + m22 * y + dy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine2D {
    /// Scale/rotation element `(1, 1)`.
    pub m11: f64,
    /// Rotation/shear element `(1, 2)`.
    pub m12: f64,
    /// Rotation/shear element `(2, 1)`.
    pub m21: f64,
    /// Scale/rotation element `(2, 2)`.
    pub m22: f64,
    /// X translation.
    pub dx: f64,
    /// Y translation.
    pub dy: f64,
}

impl Default for Affine2D {
    fn default() -> Self {
        Self::identity()
    }
}

impl Affine2D {
    /// Identity transform.
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            m11: 1.0,
            m12: 0.0,
            m21: 0.0,
            m22: 1.0,
            dx: 0.0,
            dy: 0.0,
        }
    }

    /// Translation matrix.
    #[must_use]
    pub const fn from_translate(tx: f64, ty: f64) -> Self {
        Self {
            m11: 1.0,
            m12: 0.0,
            m21: 0.0,
            m22: 1.0,
            dx: tx,
            dy: ty,
        }
    }

    /// Scale matrix.
    #[must_use]
    pub const fn from_scale(sx: f64, sy: f64) -> Self {
        Self {
            m11: sx,
            m12: 0.0,
            m21: 0.0,
            m22: sy,
            dx: 0.0,
            dy: 0.0,
        }
    }

    /// Clockwise rotation matrix in screen coordinates (+Y down).
    #[must_use]
    pub fn from_rotate_deg(degrees: f64) -> Self {
        // Exact trig for multiples of 90 degrees avoids floating-point drift.
        let rem = degrees.rem_euclid(360.0);
        let (sin, cos) = if (rem - 0.0).abs() < 1e-12 || (rem - 360.0).abs() < 1e-12 {
            (0.0, 1.0)
        } else if (rem - 90.0).abs() < 1e-12 {
            (1.0, 0.0)
        } else if (rem - 180.0).abs() < 1e-12 {
            (0.0, -1.0)
        } else if (rem - 270.0).abs() < 1e-12 {
            (-1.0, 0.0)
        } else {
            let rad = degrees.to_radians();
            (rad.sin(), rad.cos())
        };
        Self {
            m11: cos,
            m12: sin,
            m21: -sin,
            m22: cos,
            dx: 0.0,
            dy: 0.0,
        }
    }

    /// Matrix multiplication `self * rhs` (in row-vector form: apply `self`, then `rhs`).
    #[must_use]
    pub fn then(self, rhs: Self) -> Self {
        Self {
            m11: self.m11 * rhs.m11 + self.m12 * rhs.m21,
            m12: self.m11 * rhs.m12 + self.m12 * rhs.m22,
            m21: self.m21 * rhs.m11 + self.m22 * rhs.m21,
            m22: self.m21 * rhs.m12 + self.m22 * rhs.m22,
            dx: self.dx * rhs.m11 + self.dy * rhs.m21 + rhs.dx,
            dy: self.dx * rhs.m12 + self.dy * rhs.m22 + rhs.dy,
        }
    }

    /// Qt-style pre-multiply translate: `T(tx, ty) * self`.
    #[must_use]
    pub fn qt_translate(self, tx: f64, ty: f64) -> Self {
        Self::from_translate(tx, ty).then(self)
    }

    /// Qt-style pre-multiply scale: `S(sx, sy) * self`.
    #[must_use]
    pub fn qt_scale(self, sx: f64, sy: f64) -> Self {
        Self::from_scale(sx, sy).then(self)
    }

    /// Qt-style pre-multiply rotate: `R(deg) * self`.
    #[must_use]
    pub fn qt_rotate(self, degrees: f64) -> Self {
        Self::from_rotate_deg(degrees).then(self)
    }

    /// Maps a point through this transform.
    #[must_use]
    pub fn map_point(self, p: PointF) -> PointF {
        PointF::new(
            p.x * self.m11 + p.y * self.m21 + self.dx,
            p.x * self.m12 + p.y * self.m22 + self.dy,
        )
    }

    /// Maps a rectangle's four corners and returns their axis-aligned bounding box.
    #[must_use]
    pub fn map_rect(self, r: RectF) -> RectF {
        let p0 = self.map_point(PointF::new(r.left(), r.top()));
        let p1 = self.map_point(PointF::new(r.right(), r.top()));
        let p2 = self.map_point(PointF::new(r.left(), r.bottom()));
        let p3 = self.map_point(PointF::new(r.right(), r.bottom()));
        let min_x = p0.x.min(p1.x).min(p2.x).min(p3.x);
        let max_x = p0.x.max(p1.x).max(p2.x).max(p3.x);
        let min_y = p0.y.min(p1.y).min(p2.y).min(p3.y);
        let max_y = p0.y.max(p1.y).max(p2.y).max(p3.y);
        RectF::from_edges(min_x, min_y, max_x, max_y)
    }

    /// Computes Qt's `QImage::trueMatrix(transform, w, h)`: translates the
    /// transformed bounding box so its top-left corner sits at `(0, 0)`.
    #[must_use]
    pub fn true_matrix(self, width: f64, height: f64) -> Self {
        let mapped = self.map_rect(RectF::new(0.0, 0.0, width, height));
        self.then(Self::from_translate(-mapped.x, -mapped.y))
    }

    /// Converts to `tiny_skia::Transform`.
    #[must_use]
    pub fn to_skia(self) -> tiny_skia::Transform {
        tiny_skia::Transform::from_row(
            self.m11 as f32,
            self.m12 as f32,
            self.m21 as f32,
            self.m22 as f32,
            self.dx as f32,
            self.dy as f32,
        )
    }
}
