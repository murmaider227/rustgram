//! Palette color references, runtime `ColorResolver` trait, and contrast helpers.
//!
//! Port of `Telegram/lib_ui/ui/style/style_core_color.h` and contrast helpers from
//! `Telegram/lib_ui/ui/style/style_core.cpp`.

/// Plain RGBA8 color value (non-premultiplied sRGB channels).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Color {
    /// Red channel (`0..=255`).
    pub r: u8,
    /// Green channel (`0..=255`).
    pub g: u8,
    /// Blue channel (`0..=255`).
    pub b: u8,
    /// Alpha channel (`0..=255`).
    pub a: u8,
}

impl Color {
    /// Fully transparent white (`#ffffff00`), matching tdesktop `st::transparent`.
    pub const TRANSPARENT: Self = Self::rgba(255, 255, 255, 0);
    /// Opaque white (`#ffffffff`), matching tdesktop `st::white`.
    pub const WHITE: Self = Self::rgba(255, 255, 255, 255);
    /// Opaque black (`#000000ff`).
    pub const BLACK: Self = Self::rgba(0, 0, 0, 255);

    /// Construct an RGBA color.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Construct an opaque RGB color (`a = 255`).
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Construct a color from an `[r, g, b, a]` byte array.
    #[must_use]
    pub const fn from_array([r, g, b, a]: [u8; 4]) -> Self {
        Self { r, g, b, a }
    }

    /// Convert to an `[r, g, b, a]` byte array.
    #[must_use]
    pub const fn to_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// Return the same RGB color with alpha set to `0` (`ColorData::transparent()`).
    #[must_use]
    pub const fn transparent(self) -> Self {
        Self {
            r: self.r,
            g: self.g,
            b: self.b,
            a: 0,
        }
    }

    /// Convert to premultiplied RGBA bytes using tdesktop's exact `anim::shifted` formula.
    #[must_use]
    pub const fn to_premultiplied(self) -> [u8; 4] {
        let a = (self.a as u32) + 1;
        [
            (((self.r as u32) * a) >> 8) as u8,
            (((self.g as u32) * a) >> 8) as u8,
            (((self.b as u32) * a) >> 8) as u8,
            ((255 * a) >> 8) as u8,
        ]
    }
}

/// Symbolic reference to a palette color by its tdesktop palette key name
/// (e.g. `"windowBg"`, `"windowActiveTextFg"`, `"transparent"`).
///
/// Resolved dynamically at runtime via a [`ColorResolver`] (implemented by `tg-theme`'s palette).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColorRef(pub &'static str);

impl ColorRef {
    /// Special transparent color reference.
    pub const TRANSPARENT: Self = Self("transparent");
    /// Special white color reference.
    pub const WHITE: Self = Self("white");
    /// Default fallback color (`windowFg`).
    pub const WINDOW_FG: Self = Self("windowFg");

    /// Construct a palette color reference.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// Palette key name (e.g. `"windowActiveTextFg"`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.0
    }

    /// Resolve this color reference strictly through `resolver`.
    ///
    /// Special `"transparent"` always resolves to [`Color::TRANSPARENT`]; `"white"` falls back
    /// to [`Color::WHITE`] if not overridden by the resolver.
    #[must_use]
    pub fn try_resolve(self, resolver: &dyn ColorResolver) -> Option<Color> {
        if self.0 == "transparent" {
            return Some(Color::TRANSPARENT);
        }
        if let Some(c) = resolver.resolve(self.0) {
            return Some(c);
        }
        if self.0 == "white" {
            return Some(Color::WHITE);
        }
        None
    }

    /// Resolve this color reference through `resolver`, falling back to the built-in default
    /// `colors.palette` value (and finally [`Color::BLACK`]) if absent.
    #[must_use]
    pub fn resolve(self, resolver: &dyn ColorResolver) -> Color {
        self.try_resolve(resolver)
            .or_else(|| default_palette_color(self.0))
            .unwrap_or(Color::BLACK)
    }
}

/// Trait for resolving palette color names to concrete [`Color`] values at runtime.
///
/// `tg-theme` implements this trait on its active theme palette so `tg-ui` can resolve any
/// [`ColorRef`] or rasterize any [`crate::icon::Icon`] with live theme switching.
pub trait ColorResolver {
    /// Resolve a palette color key name (e.g. `"windowBg"`, `"msgInBg"`) to an RGBA color.
    fn resolve(&self, name: &str) -> Option<Color>;
}

impl<F> ColorResolver for F
where
    F: Fn(&str) -> Option<Color>,
{
    fn resolve(&self, name: &str) -> Option<Color> {
        self(name)
    }
}

/// Built-in default (day/classic) palette resolver backed by `assets/styles/ui/colors.palette`.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultPaletteResolver;

impl ColorResolver for DefaultPaletteResolver {
    fn resolve(&self, name: &str) -> Option<Color> {
        default_palette_color(name)
    }
}

/// Look up the default day-theme RGBA value for a palette color name from `ui/colors.palette`.
#[must_use]
pub fn default_palette_color(name: &str) -> Option<Color> {
    let idx = crate::DEFAULT_PALETTE_COLORS
        .binary_search_by_key(&name, |&(k, _)| k)
        .ok()?;
    Some(Color::from_array(crate::DEFAULT_PALETTE_COLORS[idx].1))
}

/// Perceptual color contrast check ported from `style::internal::GoodForContrast` (`style_core.cpp`).
#[must_use]
pub fn good_for_contrast(c1: Color, c2: Color) -> bool {
    const MIN_CONTRAST_DISTANCE: i32 = 64 * 64 * 4;
    let r_mean = (i32::from(c1.r) + i32::from(c2.r)) / 2;
    let r = i32::from(c1.r) - i32::from(c2.r);
    let g = i32::from(c1.g) - i32::from(c2.g);
    let b = i32::from(c1.b) - i32::from(c2.b);
    let distance = (((512 + r_mean) * r * r) >> 8) + (4 * g * g) + (((767 - r_mean) * b * b) >> 8);
    distance > MIN_CONTRAST_DISTANCE
}
