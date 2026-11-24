//! Font descriptor types and style flags.
//!
//! Port of `Telegram/lib_ui/ui/style/style_core_font.h` and `style_core_font.cpp`.

use crate::scale::{Scale, scale};
use crate::types::Pixels;

/// Default UI font family in tdesktop (`"Open Sans"`).
pub const DEFAULT_FONT_FAMILY: &str = "Open Sans";

/// Special sentinel indicating the OS system UI font (`style::SystemFontTag()`).
pub const SYSTEM_FONT_TAG: &str = "(system)\0";

/// Subscript/superscript font size multiplier (`kSubSuperMultiplier` in `style_core_font.cpp`).
pub const SUB_SUPER_MULTIPLIER: f64 = 0.75;

/// Bitset of font style flags matching `style::FontFlag` in `style_core_font.h`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FontFlags(u8);

impl FontFlags {
    /// No flags (regular weight, normal style).
    pub const NONE: Self = Self(0);
    /// Bold / semibold (`FontFlag::Bold = 0x01`; tdesktop maps both `bold` and `semibold` to this flag).
    pub const BOLD: Self = Self(0x01);
    /// Alias for [`FontFlags::BOLD`] (`semibold` in `.style` files).
    pub const SEMIBOLD: Self = Self::BOLD;
    /// Italic (`FontFlag::Italic = 0x02`).
    pub const ITALIC: Self = Self(0x02);
    /// Underline (`FontFlag::Underline = 0x04`).
    pub const UNDERLINE: Self = Self(0x04);
    /// Strike-out (`FontFlag::StrikeOut = 0x08`).
    pub const STRIKE_OUT: Self = Self(0x08);
    /// Subscript or superscript (`FontFlag::SubOrSuper = 0x10`).
    pub const SUB_OR_SUPER: Self = Self(0x10);
    /// Monospace (`FontFlag::Monospace = 0x20`).
    pub const MONOSPACE: Self = Self(0x20);

    /// Construct from raw bit representation.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 0x3F)
    }

    /// Raw bit representation.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns `true` if all bits in `other` are set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Union of two flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Remove `other` bits from `self`.
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Whether the bold/semibold flag is set.
    #[must_use]
    pub const fn is_bold(self) -> bool {
        self.contains(Self::BOLD)
    }

    /// Whether the semibold flag is set (equivalent to [`Self::is_bold`] in tdesktop).
    #[must_use]
    pub const fn is_semibold(self) -> bool {
        self.contains(Self::SEMIBOLD)
    }

    /// Whether the italic flag is set.
    #[must_use]
    pub const fn is_italic(self) -> bool {
        self.contains(Self::ITALIC)
    }

    /// Whether the underline flag is set.
    #[must_use]
    pub const fn is_underline(self) -> bool {
        self.contains(Self::UNDERLINE)
    }

    /// Whether the strike-out flag is set.
    #[must_use]
    pub const fn is_strike_out(self) -> bool {
        self.contains(Self::STRIKE_OUT)
    }

    /// Whether the subscript/superscript flag is set.
    #[must_use]
    pub const fn is_sub_or_super(self) -> bool {
        self.contains(Self::SUB_OR_SUPER)
    }

    /// Whether the monospace flag is set.
    #[must_use]
    pub const fn is_monospace(self) -> bool {
        self.contains(Self::MONOSPACE)
    }
}

impl std::ops::BitOr for FontFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl std::ops::BitAnd for FontFlags {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

/// Font descriptor (`style::font`).
///
/// Stores the family override (empty string means default UI font `"Open Sans"`), unscaled
/// design-time pixel size, and [`FontFlags`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Font {
    /// Explicit font family name, or `""` to use the default UI font (`"Open Sans"`).
    pub family: &'static str,
    /// Unscaled design-time pixel size (e.g. `13px`).
    pub size: Pixels,
    /// Style flags (bold/semibold, italic, underline, strike-out, sub/super, monospace).
    pub flags: FontFlags,
}

impl Font {
    /// Construct a new font descriptor.
    #[must_use]
    pub const fn new(family: &'static str, size: Pixels, flags: FontFlags) -> Self {
        Self {
            family,
            size,
            flags,
        }
    }

    /// Effective family name (`DEFAULT_FONT_FAMILY` if `family` is empty).
    #[must_use]
    pub const fn effective_family(self) -> &'static str {
        if self.family.is_empty() {
            DEFAULT_FONT_FAMILY
        } else {
            self.family
        }
    }

    /// Scaled font size in pixels for the current global [`scale()`].
    #[must_use]
    pub fn scaled_size(self) -> i32 {
        self.scaled_size_at(scale())
    }

    /// Scaled font size in pixels for an explicit [`Scale`].
    ///
    /// Applies [`SUB_SUPER_MULTIPLIER`] when [`FontFlags::SUB_OR_SUPER`] is set (and not monospace),
    /// matching `ResolveFont` in `style_core_font.cpp`.
    #[must_use]
    pub fn scaled_size_at(self, target_scale: Scale) -> i32 {
        let base = self.size.scaled_at(target_scale);
        if self.flags.is_sub_or_super() && !self.flags.is_monospace() {
            (f64::from(base) * SUB_SUPER_MULTIPLIER).round() as i32
        } else {
            base
        }
    }

    /// Return a copy with a flag enabled or disabled, matching `FontData::otherFlagsFont`.
    #[must_use]
    pub const fn with_flag(self, flag: FontFlags, set: bool) -> Self {
        let new_flags = if !set {
            self.flags.difference(flag)
        } else if self.flags.union(flag).is_monospace() {
            FontFlags::MONOSPACE
        } else {
            self.flags.union(flag)
        };
        Self {
            family: self.family,
            size: self.size,
            flags: new_flags,
        }
    }

    /// Return a copy with bold/semibold toggled.
    #[must_use]
    pub const fn bold(self, set: bool) -> Self {
        self.with_flag(FontFlags::BOLD, set)
    }

    /// Return a copy with semibold toggled (alias for [`Self::bold`]).
    #[must_use]
    pub const fn semibold(self, set: bool) -> Self {
        self.bold(set)
    }

    /// Return a copy with italic toggled.
    #[must_use]
    pub const fn italic(self, set: bool) -> Self {
        self.with_flag(FontFlags::ITALIC, set)
    }

    /// Return a copy with underline toggled.
    #[must_use]
    pub const fn underline(self, set: bool) -> Self {
        self.with_flag(FontFlags::UNDERLINE, set)
    }

    /// Return a copy with strike-out toggled.
    #[must_use]
    pub const fn strikeout(self, set: bool) -> Self {
        self.with_flag(FontFlags::STRIKE_OUT, set)
    }

    /// Return a copy with subscript/superscript toggled.
    #[must_use]
    pub const fn sub_or_super(self, set: bool) -> Self {
        self.with_flag(FontFlags::SUB_OR_SUPER, set)
    }

    /// Return a copy with monospace toggled.
    #[must_use]
    pub const fn monospace(self, set: bool) -> Self {
        self.with_flag(FontFlags::MONOSPACE, set)
    }
}
