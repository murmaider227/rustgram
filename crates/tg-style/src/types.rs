//! Core geometric and layout style types (`Pixels`, `Point`, `Size`, `Margins`, `Rect`, `Align`, `Cursor`).
//!
//! Port of `Telegram/lib_ui/ui/style/style_core_types.h`.

use crate::scale::{Scale, convert_scale, scale};

/// Unscaled design-time pixel value (at 100% scale).
///
/// Call [`Pixels::scaled`] to obtain the DPI-scaled value for the current global [`Scale`],
/// or [`Pixels::scaled_at`] for an explicit [`Scale`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pixels(pub i32);

impl Pixels {
    /// Zero pixels.
    pub const ZERO: Self = Self(0);

    /// Create a new unscaled pixel measurement.
    #[must_use]
    pub const fn new(raw: i32) -> Self {
        Self(raw)
    }

    /// Return the raw unscaled (100%) pixel value.
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }

    /// Scale this pixel value using the current global [`scale()`].
    #[must_use]
    pub fn scaled(self) -> i32 {
        convert_scale(self.0, scale())
    }

    /// Scale this pixel value using an explicit [`Scale`].
    #[must_use]
    pub fn scaled_at(self, target_scale: Scale) -> i32 {
        convert_scale(self.0, target_scale)
    }
}

impl std::ops::Add for Pixels {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl std::ops::Sub for Pixels {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

impl std::ops::Neg for Pixels {
    type Output = Self;
    fn neg(self) -> Self::Output {
        Self(-self.0)
    }
}

/// Unscaled 2D point (`style::point`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Point {
    /// Horizontal offset.
    pub x: Pixels,
    /// Vertical offset.
    pub y: Pixels,
}

/// DPI-scaled 2D point with concrete `i32` coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ScaledPoint {
    /// Scaled horizontal coordinate.
    pub x: i32,
    /// Scaled vertical coordinate.
    pub y: i32,
}

impl Point {
    /// Origin `(0px, 0px)`.
    pub const ZERO: Self = Self::new(0, 0);

    /// Construct a point from raw 100% pixel values.
    #[must_use]
    pub const fn new(x: i32, y: i32) -> Self {
        Self {
            x: Pixels::new(x),
            y: Pixels::new(y),
        }
    }

    /// Scale both coordinates using the current global [`scale()`].
    #[must_use]
    pub fn scaled(self) -> ScaledPoint {
        self.scaled_at(scale())
    }

    /// Scale both coordinates using an explicit [`Scale`].
    #[must_use]
    pub fn scaled_at(self, target_scale: Scale) -> ScaledPoint {
        ScaledPoint {
            x: self.x.scaled_at(target_scale),
            y: self.y.scaled_at(target_scale),
        }
    }
}

/// Unscaled 2D size (`style::size`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Size {
    /// Width in unscaled pixels.
    pub width: Pixels,
    /// Height in unscaled pixels.
    pub height: Pixels,
}

/// DPI-scaled 2D size with concrete `i32` dimensions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ScaledSize {
    /// Scaled width.
    pub width: i32,
    /// Scaled height.
    pub height: i32,
}

impl Size {
    /// Empty `(0px, 0px)` size.
    pub const ZERO: Self = Self::new(0, 0);

    /// Construct a size from raw 100% pixel dimensions.
    #[must_use]
    pub const fn new(width: i32, height: i32) -> Self {
        Self {
            width: Pixels::new(width),
            height: Pixels::new(height),
        }
    }

    /// Scale both dimensions using the current global [`scale()`].
    #[must_use]
    pub fn scaled(self) -> ScaledSize {
        self.scaled_at(scale())
    }

    /// Scale both dimensions using an explicit [`Scale`].
    #[must_use]
    pub fn scaled_at(self, target_scale: Scale) -> ScaledSize {
        ScaledSize {
            width: self.width.scaled_at(target_scale),
            height: self.height.scaled_at(target_scale),
        }
    }
}

/// Unscaled 4-sided margins (`style::margins`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Margins {
    /// Left margin.
    pub left: Pixels,
    /// Top margin.
    pub top: Pixels,
    /// Right margin.
    pub right: Pixels,
    /// Bottom margin.
    pub bottom: Pixels,
}

/// DPI-scaled 4-sided margins with concrete `i32` values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ScaledMargins {
    /// Scaled left margin.
    pub left: i32,
    /// Scaled top margin.
    pub top: i32,
    /// Scaled right margin.
    pub right: i32,
    /// Scaled bottom margin.
    pub bottom: i32,
}

impl Margins {
    /// Zero margins on all sides.
    pub const ZERO: Self = Self::new(0, 0, 0, 0);

    /// Construct margins from raw 100% pixel values `(left, top, right, bottom)`.
    #[must_use]
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left: Pixels::new(left),
            top: Pixels::new(top),
            right: Pixels::new(right),
            bottom: Pixels::new(bottom),
        }
    }

    /// Returns `true` if all four margins are zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.left.0 == 0 && self.top.0 == 0 && self.right.0 == 0 && self.bottom.0 == 0
    }

    /// Sum of unscaled left and right margins (`leftRight` in `.style`).
    #[must_use]
    pub const fn left_right(self) -> Pixels {
        Pixels::new(self.left.0 + self.right.0)
    }

    /// Sum of unscaled top and bottom margins (`topBottom` in `.style`).
    #[must_use]
    pub const fn top_bottom(self) -> Pixels {
        Pixels::new(self.top.0 + self.bottom.0)
    }

    /// Scale all four margins using the current global [`scale()`].
    #[must_use]
    pub fn scaled(self) -> ScaledMargins {
        self.scaled_at(scale())
    }

    /// Scale all four margins using an explicit [`Scale`].
    #[must_use]
    pub fn scaled_at(self, target_scale: Scale) -> ScaledMargins {
        ScaledMargins {
            left: self.left.scaled_at(target_scale),
            top: self.top.scaled_at(target_scale),
            right: self.right.scaled_at(target_scale),
            bottom: self.bottom.scaled_at(target_scale),
        }
    }
}

impl ScaledMargins {
    /// Sum of scaled left and right margins.
    #[must_use]
    pub const fn horizontal(self) -> i32 {
        self.left + self.right
    }

    /// Sum of scaled top and bottom margins.
    #[must_use]
    pub const fn vertical(self) -> i32 {
        self.top + self.bottom
    }
}

/// Unscaled rectangle (`style::rect`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    /// X origin.
    pub x: Pixels,
    /// Y origin.
    pub y: Pixels,
    /// Width.
    pub width: Pixels,
    /// Height.
    pub height: Pixels,
}

/// DPI-scaled rectangle with concrete `i32` coordinates and dimensions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ScaledRect {
    /// Scaled X origin.
    pub x: i32,
    /// Scaled Y origin.
    pub y: i32,
    /// Scaled width.
    pub width: i32,
    /// Scaled height.
    pub height: i32,
}

impl Rect {
    /// Empty rectangle at origin.
    pub const ZERO: Self = Self::new(0, 0, 0, 0);

    /// Construct a rectangle from raw 100% pixel values.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x: Pixels::new(x),
            y: Pixels::new(y),
            width: Pixels::new(width),
            height: Pixels::new(height),
        }
    }

    /// Scale the rectangle using the current global [`scale()`].
    #[must_use]
    pub fn scaled(self) -> ScaledRect {
        self.scaled_at(scale())
    }

    /// Scale the rectangle using an explicit [`Scale`].
    #[must_use]
    pub fn scaled_at(self, target_scale: Scale) -> ScaledRect {
        ScaledRect {
            x: self.x.scaled_at(target_scale),
            y: self.y.scaled_at(target_scale),
            width: self.width.scaled_at(target_scale),
            height: self.height.scaled_at(target_scale),
        }
    }
}

/// Alignment flags matching tdesktop's `style::al_*` constants (`style_core_types.h`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Align {
    /// `style::al_topleft`
    #[default]
    TopLeft,
    /// `style::al_top` (top + horizontal center)
    Top,
    /// `style::al_topright`
    TopRight,
    /// `style::al_right` (vertical center + right)
    Right,
    /// `style::al_bottomright`
    BottomRight,
    /// `style::al_bottom` (bottom + horizontal center)
    Bottom,
    /// `style::al_bottomleft`
    BottomLeft,
    /// `style::al_left` (vertical center + left)
    Left,
    /// `style::al_center` (vertical center + horizontal center)
    Center,
    /// `style::al_justify`
    Justify,
}

/// Cursor shapes matching tdesktop's `style::cur_*` constants (`style_core_types.h`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cursor {
    /// `style::cur_default` (arrow)
    #[default]
    Default,
    /// `style::cur_pointer` (pointing hand)
    Pointer,
    /// `style::cur_text` (I-beam)
    Text,
    /// `style::cur_cross` (crosshair)
    Cross,
    /// `style::cur_sizever` (vertical resize)
    SizeVer,
    /// `style::cur_sizehor` (horizontal resize)
    SizeHor,
    /// `style::cur_sizebdiag` (diagonal `/` resize)
    SizeBDiag,
    /// `style::cur_sizefdiag` (diagonal `\` resize)
    SizeFDiag,
    /// `style::cur_sizeall` (4-way move/resize)
    SizeAll,
}
