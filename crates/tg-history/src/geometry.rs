//! Plain integer geometry primitives, side/corner flags, and bubble rounding types.
//!
//! Port of Qt `QPoint`/`QSize`/`QRect`/`QMargins` helpers used in
//! `Telegram/SourceFiles/ui/rect_part.h`, `Telegram/SourceFiles/ui/chat/message_bubble.h`,
//! `Telegram/SourceFiles/ui/grouped_layout.cpp`, and `Telegram/SourceFiles/history/view/media/history_view_media_common.h`.

use std::ops::{Add, BitAnd, BitAndAssign, BitOr, BitOrAssign, Sub};

/// A 2D point in logical pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: i32,
    /// Vertical coordinate.
    pub y: i32,
}

impl Point {
    /// Zero point `(0, 0)`.
    pub const ZERO: Self = Self { x: 0, y: 0 };

    /// Creates a new point.
    #[must_use]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

impl Add for Point {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl Sub for Point {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self::new(self.x - rhs.x, self.y - rhs.y)
    }
}

/// A 2D size in logical pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Size {
    /// Width in logical pixels.
    pub width: i32,
    /// Height in logical pixels.
    pub height: i32,
}

impl Size {
    /// Zero size `(0, 0)`.
    pub const ZERO: Self = Self {
        width: 0,
        height: 0,
    };

    /// Creates a new size.
    #[must_use]
    pub const fn new(width: i32, height: i32) -> Self {
        Self { width, height }
    }

    /// Returns `true` if either dimension is non-positive (matching `QSize::isEmpty()`).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    /// Scales this size to fit inside `bounds` while keeping aspect ratio,
    /// matching `QSize::scaled(bounds, Qt::KeepAspectRatio)`.
    #[must_use]
    pub fn scaled_keep_aspect_ratio(self, bounds: Self) -> Self {
        if self.width <= 0 || self.height <= 0 || bounds.width <= 0 || bounds.height <= 0 {
            return Self::ZERO;
        }
        let rw = (i64::from(bounds.height) * i64::from(self.width)) / i64::from(self.height);
        if rw <= i64::from(bounds.width) {
            Self::new(rw as i32, bounds.height)
        } else {
            let rh = (i64::from(bounds.width) * i64::from(self.height)) / i64::from(self.width);
            Self::new(bounds.width, rh as i32)
        }
    }
}

/// Four-sided margins in logical pixels (`left`, `top`, `right`, `bottom`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
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
    /// All-zero margins.
    pub const ZERO: Self = Self {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };

    /// Creates new margins `(left, top, right, bottom)` matching tdesktop `.style` `margins(l, t, r, b)`.
    #[must_use]
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Sum of left and right margins (`rect::m::sum::h`).
    #[must_use]
    pub const fn horizontal(self) -> i32 {
        self.left + self.right
    }

    /// Sum of top and bottom margins (`rect::m::sum::v`).
    #[must_use]
    pub const fn vertical(self) -> i32 {
        self.top + self.bottom
    }
}

impl Add for Margins {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            left: self.left + rhs.left,
            top: self.top + rhs.top,
            right: self.right + rhs.right,
            bottom: self.bottom + rhs.bottom,
        }
    }
}

/// An axis-aligned integer rectangle in logical pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    /// Left edge X coordinate.
    pub x: i32,
    /// Top edge Y coordinate.
    pub y: i32,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
}

impl Rect {
    /// Empty rectangle at `(0, 0)` with size `(0, 0)`.
    pub const ZERO: Self = Self {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };

    /// Creates a new rectangle from position and dimensions.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Creates a rectangle from a top-left point and a size.
    #[must_use]
    pub const fn from_point_size(origin: Point, size: Size) -> Self {
        Self {
            x: origin.x,
            y: origin.y,
            width: size.width,
            height: size.height,
        }
    }

    /// Left X coordinate.
    #[must_use]
    pub const fn left(self) -> i32 {
        self.x
    }

    /// Top Y coordinate.
    #[must_use]
    pub const fn top(self) -> i32 {
        self.y
    }

    /// Exclusive right edge (`x + width`, matching tdesktop `rect::right(r)`).
    #[must_use]
    pub const fn right(self) -> i32 {
        self.x + self.width
    }

    /// Exclusive bottom edge (`y + height`, matching tdesktop `rect::bottom(r)`).
    #[must_use]
    pub const fn bottom(self) -> i32 {
        self.y + self.height
    }

    /// Top-left corner point.
    #[must_use]
    pub const fn top_left(self) -> Point {
        Point::new(self.x, self.y)
    }

    /// Size of the rectangle.
    #[must_use]
    pub const fn size(self) -> Size {
        Size::new(self.width, self.height)
    }

    /// Returns `true` if width or height is non-positive.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    /// Returns `true` if `point` lies inside `[x, x + width) x [y, y + height)`.
    #[must_use]
    pub const fn contains(self, point: Point) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }

    /// Translates the rectangle by `(dx, dy)`.
    #[must_use]
    pub const fn translated(self, dx: i32, dy: i32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            width: self.width,
            height: self.height,
        }
    }

    /// Shrinks the rectangle by `margins` (`QRect::marginsRemoved`).
    #[must_use]
    pub const fn margins_removed(self, margins: Margins) -> Self {
        Self {
            x: self.x + margins.left,
            y: self.y + margins.top,
            width: self.width - margins.left - margins.right,
            height: self.height - margins.top - margins.bottom,
        }
    }

    /// Expands the rectangle by `margins` (`QRect::marginsAdded`).
    #[must_use]
    pub const fn margins_added(self, margins: Margins) -> Self {
        Self {
            x: self.x - margins.left,
            y: self.y - margins.top,
            width: self.width + margins.left + margins.right,
            height: self.height + margins.top + margins.bottom,
        }
    }

    /// Returns the bounding union of two rectangles.
    #[must_use]
    pub fn united(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x1 = self.x.min(other.x);
        let y1 = self.y.min(other.y);
        let x2 = self.right().max(other.right());
        let y2 = self.bottom().max(other.bottom());
        Self::new(x1, y1, x2 - x1, y2 - y1)
    }
}

/// Bitmask of rectangle sides and corners (`RectPart` / `RectParts` in `ui/rect_part.h`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct RectParts(u16);

impl RectParts {
    /// No sides or corners.
    pub const NONE: Self = Self(0);
    /// Top side.
    pub const TOP: Self = Self(1 << 0);
    /// Right side.
    pub const RIGHT: Self = Self(1 << 1);
    /// Bottom side.
    pub const BOTTOM: Self = Self(1 << 2);
    /// Left side.
    pub const LEFT: Self = Self(1 << 3);
    /// Top-left corner.
    pub const TOP_LEFT: Self = Self(1 << 4);
    /// Top-right corner.
    pub const TOP_RIGHT: Self = Self(1 << 5);
    /// Bottom-left corner.
    pub const BOTTOM_LEFT: Self = Self(1 << 6);
    /// Bottom-right corner.
    pub const BOTTOM_RIGHT: Self = Self(1 << 7);

    /// All four sides.
    pub const ALL_SIDES: Self = Self(Self::TOP.0 | Self::RIGHT.0 | Self::BOTTOM.0 | Self::LEFT.0);

    /// Returns `true` if all bits in `other` are set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns `true` if any bit in `other` is set in `self`.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    /// Returns `true` if no bits are set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Combines two bitmasks in a `const` context.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns the raw bit representation.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }
}

impl BitOr for RectParts {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for RectParts {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for RectParts {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for RectParts {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

/// Rounding style of a single message bubble corner (`Ui::BubbleCornerRounding`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum BubbleCornerRounding {
    /// Sharp (unrounded) corner.
    #[default]
    None,
    /// Speech-bubble tail corner.
    Tail,
    /// Small radius corner (attached edge).
    Small,
    /// Large radius corner (unattached edge).
    Large,
}

/// Corner rounding configuration for all four corners of a message bubble (`Ui::BubbleRounding`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct BubbleRounding {
    /// Top-left corner rounding.
    pub top_left: BubbleCornerRounding,
    /// Top-right corner rounding.
    pub top_right: BubbleCornerRounding,
    /// Bottom-left corner rounding.
    pub bottom_left: BubbleCornerRounding,
    /// Bottom-right corner rounding.
    pub bottom_right: BubbleCornerRounding,
}

impl BubbleRounding {
    /// All four corners rounded with [`BubbleCornerRounding::Large`].
    pub const ALL_LARGE: Self = Self {
        top_left: BubbleCornerRounding::Large,
        top_right: BubbleCornerRounding::Large,
        bottom_left: BubbleCornerRounding::Large,
        bottom_right: BubbleCornerRounding::Large,
    };

    /// All four corners rounded with [`BubbleCornerRounding::Small`].
    pub const ALL_SMALL: Self = Self {
        top_left: BubbleCornerRounding::Small,
        top_right: BubbleCornerRounding::Small,
        bottom_left: BubbleCornerRounding::Small,
        bottom_right: BubbleCornerRounding::Small,
    };

    /// No rounding on any corner.
    pub const NONE: Self = Self {
        top_left: BubbleCornerRounding::None,
        top_right: BubbleCornerRounding::None,
        bottom_left: BubbleCornerRounding::None,
        bottom_right: BubbleCornerRounding::None,
    };
}

/// Vertical selection highlight interval within a bubble (`Ui::BubbleSelectionInterval`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct BubbleSelectionInterval {
    /// Top Y offset of the selected band.
    pub top: i32,
    /// Height of the selected band.
    pub height: i32,
}

/// Rounds a floating-point value to the nearest integer, half away from zero,
/// matching C++ `int(base::SafeRound(value))` (`std::round`).
#[must_use]
pub fn safe_round(value: f64) -> i32 {
    if value.is_nan() {
        0
    } else {
        value.round() as i32
    }
}

/// Clamps width and height to at least 1 pixel (`HistoryView::NonEmptySize`).
#[must_use]
pub fn non_empty_size(size: Size) -> Size {
    Size::new(size.width.max(1), size.height.max(1))
}

/// Downscales `size` into `bounds` preserving aspect ratio, without upscaling
/// (`HistoryView::DownscaledSize`).
#[must_use]
pub fn downscaled_size(size: Size, bounds: Size) -> Size {
    let scaled = if size.width > bounds.width || size.height > bounds.height {
        size.scaled_keep_aspect_ratio(bounds)
    } else {
        size
    };
    non_empty_size(scaled)
}

/// Derives corner flags from outer side flags (`Ui::GetCornersFromSides` in `ui/grouped_layout.cpp`).
#[must_use]
pub const fn get_corners_from_sides(sides: RectParts) -> RectParts {
    let mut result = RectParts::NONE;
    if sides.contains(RectParts::TOP) && sides.contains(RectParts::LEFT) {
        result = result.union(RectParts::TOP_LEFT);
    }
    if sides.contains(RectParts::TOP) && sides.contains(RectParts::RIGHT) {
        result = result.union(RectParts::TOP_RIGHT);
    }
    if sides.contains(RectParts::BOTTOM) && sides.contains(RectParts::LEFT) {
        result = result.union(RectParts::BOTTOM_LEFT);
    }
    if sides.contains(RectParts::BOTTOM) && sides.contains(RectParts::RIGHT) {
        result = result.union(RectParts::BOTTOM_RIGHT);
    }
    result
}

/// Computes the image scale size for covering a cell geometry (`Ui::GetImageScaleSizeForGeometry`
/// in `ui/grouped_layout.cpp`).
#[must_use]
pub fn get_image_scale_size_for_geometry(original: Size, geometry: Size) -> Size {
    let width = i64::from(geometry.width);
    let height = i64::from(geometry.height);
    let mut tw = i64::from(original.width);
    let mut th = i64::from(original.height);
    if tw * height > th * width {
        if th > height || tw * height < 2 * th * width {
            if th != 0 {
                tw = (height * tw) / th;
            }
            th = height;
        } else if tw < width {
            if tw != 0 {
                th = (width * th) / tw;
            }
            tw = width;
        }
    } else if tw > width || th * width < 2 * tw * height {
        if tw != 0 {
            th = (width * th) / tw;
        }
        tw = width;
    } else if tw > 0 && th < height {
        if th != 0 {
            tw = (height * tw) / th;
        }
        th = height;
    }
    if tw < 1 {
        tw = 1;
    }
    if th < 1 {
        th = 1;
    }
    Size::new(tw as i32, th as i32)
}
