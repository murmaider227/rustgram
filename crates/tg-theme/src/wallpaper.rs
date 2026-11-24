//! Wallpaper domain model, binary serialization, and `t.me/bg` URL parsing.
//!
//! Port of `SourceFiles/data/data_wall_paper.h`, `SourceFiles/data/data_wall_paper.cpp`,
//! and `bg/` link handling in `SourceFiles/boxes/background_preview_box.cpp` and
//! `SourceFiles/core/local_url_handlers.cpp`.

use std::collections::BTreeMap;

use image::RgbaImage;

use crate::color::Color;
use crate::gradient::{DEFAULT_BACKGROUND_COLOR, dither_image, generate_gradient};
use crate::svg_pattern::{
    PatternTileMode, SvgPatternError, composite_pattern_on_gradient_opaque,
    render_tiled_pattern_wallpaper, should_invert_pattern_for_intensity, tile_pattern_mask,
    tint_pattern_pixel,
};

/// 64-bit wallpaper identifier (`WallPaperId` in tdesktop).
pub type WallPaperId = u64;

/// Converts a signed 32-bit legacy background ID to its 64-bit `WallPaperId`.
/// Exact port of `FromLegacyBackgroundId` in `data_wall_paper.cpp`.
#[must_use]
pub const fn from_legacy_background_id(legacy_id: i32) -> WallPaperId {
    0xffff_ffff_0000_0000u64 | (legacy_id as u32 as u64)
}

/// Sentinel ID for an uninitialized wallpaper.
pub const UNINITIALIZED_BACKGROUND_ID: WallPaperId = from_legacy_background_id(-999);
/// Sentinel ID for testing a theme wallpaper.
pub const TESTING_THEME_BACKGROUND_ID: WallPaperId = from_legacy_background_id(-666);
/// Sentinel ID for testing the default wallpaper.
pub const TESTING_DEFAULT_BACKGROUND_ID: WallPaperId = from_legacy_background_id(-665);
/// Sentinel ID for testing a theme editor wallpaper.
pub const TESTING_EDITOR_BACKGROUND_ID: WallPaperId = from_legacy_background_id(-664);
/// Sentinel ID indicating the active theme's bundled background image.
pub const THEME_BACKGROUND_ID: WallPaperId = from_legacy_background_id(-2);
/// Sentinel ID for a user-picked custom or color-only wallpaper.
pub const CUSTOM_BACKGROUND_ID: WallPaperId = from_legacy_background_id(-1);
/// Legacy default wallpaper v1.
pub const LEGACY1_DEFAULT_BACKGROUND_ID: WallPaperId = from_legacy_background_id(0);
/// Legacy default wallpaper v2.
pub const LEGACY2_DEFAULT_BACKGROUND_ID: WallPaperId = 5_947_530_738_516_623_361;
/// Legacy default wallpaper v3.
pub const LEGACY3_DEFAULT_BACKGROUND_ID: WallPaperId = 5_778_236_420_632_084_488;
/// Legacy default wallpaper v4.
pub const LEGACY4_DEFAULT_BACKGROUND_ID: WallPaperId = 5_945_087_215_657_811_969;
/// Current default Telegram wallpaper ID.
pub const DEFAULT_BACKGROUND_ID: WallPaperId = 5_933_856_211_186_221_059;
/// Legacy incorrect default background ID treated as legacy v2.
pub const INCORRECT_DEFAULT_BACKGROUND_ID: WallPaperId = from_legacy_background_id(105);

const VERSION_TAG: i32 = 0x7fff_ffff;
const SERIALIZE_VERSION: i32 = 1;
const DEFAULT_INTENSITY: i32 = 50;
const MAX_WALLPAPER_SLUG_LEN: usize = 255;

/// Bitflags describing a [`WallPaper`] (`Data::WallPaperFlags`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct WallPaperFlags(u32);

impl WallPaperFlags {
    /// Empty flags.
    pub const EMPTY: Self = Self(0);
    /// Pattern wallpaper (composited on top of background colors).
    pub const PATTERN: Self = Self(1 << 0);
    /// Built-in/default wallpaper.
    pub const DEFAULT: Self = Self(1 << 1);
    /// Current user is the creator.
    pub const CREATOR: Self = Self(1 << 2);
    /// Dark-mode wallpaper.
    pub const DARK: Self = Self(1 << 3);

    /// Creates flags from raw bits.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Returns raw flag bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Returns `true` if all bits in `other` are set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns the bitwise OR of two flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Converts legacy MTP/serialized flag bits to `WallPaperFlags` (`RawFromLegacyFlags`).
    #[must_use]
    pub const fn from_legacy_raw(legacy: u32) -> Self {
        let mut bits = 0u32;
        if (legacy & (1 << 0)) != 0 {
            bits |= Self::CREATOR.0;
        }
        if (legacy & (1 << 1)) != 0 {
            bits |= Self::DEFAULT.0;
        }
        if (legacy & (1 << 3)) != 0 {
            bits |= Self::PATTERN.0;
        }
        if (legacy & (1 << 4)) != 0 {
            bits |= Self::DARK.0;
        }
        Self(bits)
    }
}

/// Telegram wallpaper metadata (`Data::WallPaper`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WallPaper {
    /// Wallpaper ID (or sentinel ID).
    pub id: WallPaperId,
    /// Server access hash for cloud wallpapers.
    pub access_hash: u64,
    /// Owner user ID (bare 64-bit).
    pub owner_id: u64,
    /// Wallpaper flags (pattern / default / creator / dark).
    pub flags: WallPaperFlags,
    /// URL slug (either server slug or color/gradient slug).
    pub slug: String,
    /// Optional chat-theme emoticon ID.
    pub emoji_id: String,
    /// Up to 4 background colors (1 = solid, 2 = linear gradient, 3-4 = freeform gradient).
    pub background_colors: Vec<Color>,
    /// Linear gradient rotation in degrees (multiple of 45 in `0..=315`).
    pub rotation: i32,
    /// Pattern intensity in `-100..=100` (negative = dark mode inverted/masked pattern).
    pub intensity: i32,
    /// Whether the wallpaper photo is blurred.
    pub blurred: bool,
    /// Optional cloud document ID (replaces `DocumentData*` pointer in the C++).
    pub document_id: Option<u64>,
}

impl WallPaper {
    /// Creates a new wallpaper with the given ID and default intensity (50).
    #[must_use]
    pub fn new(id: WallPaperId) -> Self {
        Self {
            id,
            access_hash: 0,
            owner_id: 0,
            flags: WallPaperFlags::EMPTY,
            slug: String::new(),
            emoji_id: String::new(),
            background_colors: Vec::new(),
            rotation: 0,
            intensity: DEFAULT_INTENSITY,
            blurred: false,
            document_id: None,
        }
    }

    /// Constructs the default Telegram green 4-color patterned wallpaper
    /// (`WallPaper::ConstructDefault`).
    #[must_use]
    pub fn construct_default() -> Self {
        let mut paper = Self::new(DEFAULT_BACKGROUND_ID)
            .with_pattern_intensity(50)
            .with_background_colors(vec![
                Color::rgb(219, 221, 187),
                Color::rgb(107, 165, 135),
                Color::rgb(213, 216, 141),
                Color::rgb(136, 184, 132),
            ]);
        paper.flags = paper
            .flags
            .union(WallPaperFlags::DEFAULT)
            .union(WallPaperFlags::PATTERN);
        paper
    }

    /// Creates a theme-bundled background wallpaper sentinel.
    #[must_use]
    pub fn theme() -> Self {
        Self::new(THEME_BACKGROUND_ID)
    }

    /// Creates a custom wallpaper sentinel.
    #[must_use]
    pub fn custom() -> Self {
        Self::new(CUSTOM_BACKGROUND_ID)
    }

    /// Creates the legacy v1 default wallpaper sentinel.
    #[must_use]
    pub fn legacy1_default() -> Self {
        Self::new(LEGACY1_DEFAULT_BACKGROUND_ID)
    }

    /// Creates an uninitialized wallpaper sentinel.
    #[must_use]
    pub fn uninitialized() -> Self {
        Self::new(UNINITIALIZED_BACKGROUND_ID)
    }

    /// Creates a testing-theme wallpaper sentinel.
    #[must_use]
    pub fn testing_theme() -> Self {
        Self::new(TESTING_THEME_BACKGROUND_ID)
    }

    /// Creates a testing-default wallpaper sentinel.
    #[must_use]
    pub fn testing_default() -> Self {
        Self::new(TESTING_DEFAULT_BACKGROUND_ID).with_params_from(&Self::construct_default())
    }

    /// Creates a testing-editor wallpaper sentinel.
    #[must_use]
    pub fn testing_editor() -> Self {
        Self::new(TESTING_EDITOR_BACKGROUND_ID)
    }

    /// Constructs a color/gradient wallpaper from a slug like `"aabbcc"`, `"aabbcc-112233"`,
    /// or `"aabbcc~112233~445566~778899"` (`WallPaper::FromColorsSlug`).
    #[must_use]
    pub fn from_colors_slug(slug: &str) -> Option<Self> {
        let colors = colors_from_string(slug);
        if colors.is_empty() {
            return None;
        }
        let mut result = Self::custom();
        result.slug = slug.to_string();
        result.background_colors = colors;
        Some(result)
    }

    /// Constructs an emoji-keyed wallpaper (`WallPaper::FromEmojiId`).
    #[must_use]
    pub fn from_emoji_id(emoji_id: impl Into<String>) -> Self {
        let mut result = Self::new(0);
        result.emoji_id = emoji_id.into();
        result
    }

    /// Constructs a wallpaper from a legacy 32-bit ID (`WallPaper::FromLegacyId`).
    #[must_use]
    pub fn from_legacy_id(legacy_id: i32) -> Self {
        let mut result = Self::new(from_legacy_background_id(legacy_id));
        if !result.is_custom() {
            result.flags = WallPaperFlags::DEFAULT;
        }
        result
    }

    /// Constructs a wallpaper from legacy serialized fields (`WallPaper::FromLegacySerialized`).
    #[must_use]
    pub fn from_legacy_serialized(
        id: u64,
        access_hash: u64,
        legacy_flags: u32,
        slug: String,
    ) -> Self {
        let mut result = Self::new(id);
        result.access_hash = access_hash;
        result.flags = WallPaperFlags::from_legacy_raw(legacy_flags);
        if let Some(color) = color_from_rgb6(&slug) {
            result.background_colors.push(color);
        }
        result.slug = slug;
        result
    }

    /// Checks logical visual equality (`WallPaper::equals`, ignoring `id`/`access_hash`/`owner_id`).
    #[must_use]
    pub fn visual_equals(&self, other: &Self) -> bool {
        self.flags == other.flags
            && self.slug == other.slug
            && self.emoji_id == other.emoji_id
            && self.background_colors == other.background_colors
            && self.rotation == other.rotation
            && self.intensity == other.intensity
            && self.blurred == other.blurred
            && self.document_id == other.document_id
    }

    /// Returns `true` if the wallpaper has no ID, slug, or colors.
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.id == 0 && self.slug.is_empty() && self.background_colors.is_empty()
    }

    /// Returns `true` if the pattern flag is set.
    #[must_use]
    pub const fn is_pattern(&self) -> bool {
        self.flags.contains(WallPaperFlags::PATTERN)
    }

    /// Returns `true` if the default flag is set.
    #[must_use]
    pub const fn is_default_flag(&self) -> bool {
        self.flags.contains(WallPaperFlags::DEFAULT)
    }

    /// Returns `true` if the creator flag is set.
    #[must_use]
    pub const fn is_creator(&self) -> bool {
        self.flags.contains(WallPaperFlags::CREATOR)
    }

    /// Returns `true` if the dark flag is set.
    #[must_use]
    pub const fn is_dark(&self) -> bool {
        self.flags.contains(WallPaperFlags::DARK)
    }

    /// Returns `true` if this is the theme-bundled wallpaper sentinel.
    #[must_use]
    pub const fn is_theme(&self) -> bool {
        self.id == THEME_BACKGROUND_ID
    }

    /// Returns `true` if this is the custom wallpaper sentinel.
    #[must_use]
    pub const fn is_custom(&self) -> bool {
        self.id == CUSTOM_BACKGROUND_ID
    }

    /// Returns `true` if this is the current default wallpaper ID.
    #[must_use]
    pub const fn is_default_wallpaper(&self) -> bool {
        self.id == DEFAULT_BACKGROUND_ID
    }

    /// Returns `true` if this is legacy default v1.
    #[must_use]
    pub const fn is_legacy1_default(&self) -> bool {
        self.id == LEGACY1_DEFAULT_BACKGROUND_ID
    }

    /// Returns `true` if this is legacy default v2 (or the old incorrect ID 105).
    #[must_use]
    pub const fn is_legacy2_default(&self) -> bool {
        self.id == LEGACY2_DEFAULT_BACKGROUND_ID || self.id == INCORRECT_DEFAULT_BACKGROUND_ID
    }

    /// Returns `true` if this is legacy default v3.
    #[must_use]
    pub const fn is_legacy3_default(&self) -> bool {
        self.id == LEGACY3_DEFAULT_BACKGROUND_ID
    }

    /// Returns `true` if this is legacy default v4.
    #[must_use]
    pub const fn is_legacy4_default(&self) -> bool {
        self.id == LEGACY4_DEFAULT_BACKGROUND_ID
    }

    /// Returns `true` if this is a server/cloud wallpaper (`Data::IsCloudWallPaper`).
    #[must_use]
    pub const fn is_cloud(&self) -> bool {
        self.id != INCORRECT_DEFAULT_BACKGROUND_ID
            && !self.is_theme()
            && !self.is_custom()
            && !self.is_legacy1_default()
            && self.id != UNINITIALIZED_BACKGROUND_ID
            && self.id != TESTING_THEME_BACKGROUND_ID
            && self.id != TESTING_DEFAULT_BACKGROUND_ID
            && self.id != TESTING_EDITOR_BACKGROUND_ID
    }

    /// Pattern opacity in `-1.0..=1.0` (`intensity / 100.0`).
    #[must_use]
    pub fn pattern_opacity(&self) -> f64 {
        f64::from(self.intensity) / 100.0
    }

    /// Pattern intensity clamped to the valid Telegram range `-100..=100`.
    #[must_use]
    pub const fn clamped_pattern_intensity(&self) -> i32 {
        if self.intensity < -100 {
            -100
        } else if self.intensity > 100 {
            100
        } else {
            self.intensity
        }
    }

    /// Returns `true` when this wallpaper is a dark-mode negative-intensity pattern.
    #[must_use]
    pub const fn is_negative_pattern(&self) -> bool {
        self.is_pattern() && self.intensity < 0
    }

    /// Returns `true` when positive-intensity pattern strokes should invert to white
    /// over a dark background (`Ui::IsPatternInverted`).
    #[must_use]
    pub fn should_invert_pattern(&self) -> bool {
        should_invert_pattern_for_intensity(&self.background_colors, self.intensity)
    }

    /// Computes the tinted opaque pixel color for a pattern coverage value `pattern_alpha`
    /// (`0..=255`) over `bg`, applying this wallpaper's intensity in `-100..=100`.
    ///
    /// - Positive intensity (`0..=100`): SoftLight overlay (inverted to white on dark backgrounds).
    /// - Negative intensity (`-100..=-1`): dark-mode black background with gradient-tinted pattern.
    #[must_use]
    pub fn tint_pattern_color(&self, bg: Color, pattern_alpha: u8) -> Color {
        tint_pattern_pixel(
            bg,
            pattern_alpha,
            self.clamped_pattern_intensity(),
            self.should_invert_pattern(),
        )
    }

    /// Tiles a rasterized pattern alpha mask seamlessly across `(width, height)` and
    /// composites it with this wallpaper's background colors, rotation, and intensity
    /// (`-100..=100`) using origin-aligned tiling.
    #[must_use]
    pub fn tint_pattern_image(
        &self,
        pattern_mask: &RgbaImage,
        width: u32,
        height: u32,
    ) -> RgbaImage {
        self.tint_pattern_image_with_mode(pattern_mask, width, height, PatternTileMode::Repeat)
    }

    /// Tiles a rasterized pattern alpha mask across `(width, height)` with an explicit
    /// [`PatternTileMode`] and composites it using this wallpaper's colors, rotation,
    /// and intensity (`-100..=100`).
    #[must_use]
    pub fn tint_pattern_image_with_mode(
        &self,
        pattern_mask: &RgbaImage,
        width: u32,
        height: u32,
        mode: PatternTileMode,
    ) -> RgbaImage {
        let tiled = tile_pattern_mask(pattern_mask, width, height, mode);
        let colors: &[Color] = if self.background_colors.is_empty() {
            &[DEFAULT_BACKGROUND_COLOR]
        } else {
            &self.background_colors
        };
        let intensity = self.clamped_pattern_intensity();
        let mut gradient = generate_gradient(width, height, colors, self.gradient_rotation(), 1.0);
        if colors.len() > 1 && intensity >= 0 {
            gradient = dither_image(&gradient);
        }
        composite_pattern_on_gradient_opaque(&gradient, &tiled, colors, intensity)
    }

    /// Decompresses/parses an SVG or gzipped `.tgv` pattern, tiles it seamlessly over
    /// this wallpaper's multi-stop gradient, and applies this wallpaper's intensity
    /// (`-100..=100`).
    pub fn render_svg_pattern(
        &self,
        svg_or_tgv: &[u8],
        width: u32,
        height: u32,
        tile_size: Option<(u32, u32)>,
    ) -> Result<RgbaImage, SvgPatternError> {
        render_tiled_pattern_wallpaper(
            svg_or_tgv,
            width,
            height,
            &self.background_colors,
            self.gradient_rotation(),
            self.clamped_pattern_intensity(),
            tile_size,
            PatternTileMode::Repeat,
        )
    }

    /// Effective static gradient rotation (`0` for 3- or 4-color complex gradients).
    #[must_use]
    pub fn gradient_rotation(&self) -> i32 {
        if self.background_colors.len() < 3 {
            self.rotation
        } else {
            0
        }
    }

    /// Returns `true` if this wallpaper has a shareable slug.
    #[must_use]
    pub fn has_share_url(&self) -> bool {
        !self.slug.is_empty()
    }

    fn collect_share_params(&self) -> Vec<String> {
        let mut params = Vec::new();
        if self.is_pattern() {
            if !self.background_colors.is_empty() {
                params.push(format!(
                    "bg_color={}",
                    string_from_colors(&self.background_colors)
                ));
            }
            if self.intensity != 0 {
                params.push(format!("intensity={}", self.intensity));
            }
        }
        if self.rotation != 0 && self.background_colors.len() == 2 {
            params.push(format!("rotation={}", self.rotation));
        }
        if self.blurred {
            params.push("mode=blur".to_string());
        }
        params
    }

    /// Computes the cache/identity key string (`WallPaper::key`).
    #[must_use]
    pub fn key(&self) -> String {
        if self.is_null() {
            return String::new();
        }
        let base = if !self.slug.is_empty() {
            format!("bg/{}", self.slug)
        } else if self.id != 0 {
            self.id.to_string()
        } else {
            string_from_colors(&self.background_colors)
        };
        let mut params = self.collect_share_params();
        if self.document_id.is_some() && !self.is_pattern() {
            params.push(format!("&intensity={}", self.intensity));
        }
        if params.is_empty() {
            base
        } else {
            format!("{base}?{}", params.join("&"))
        }
    }

    /// Formats a full `https://t.me/bg/...` share URL (`WallPaper::shareUrl`).
    #[must_use]
    pub fn share_url(&self, base_domain: &str) -> Option<String> {
        if !self.has_share_url() {
            return None;
        }
        let domain = base_domain.trim_end_matches('/');
        let base = format!("{domain}/bg/{}", self.slug);
        let params = self.collect_share_params();
        Some(if params.is_empty() {
            base
        } else {
            format!("{base}?{}", params.join("&"))
        })
    }

    /// Applies URL query parameters (`mode`, `bg_color`/`gradient`/`color`/`slug`,
    /// `intensity`, `rotation`) matching `WallPaper::withUrlParams`.
    #[must_use]
    pub fn with_url_params(&self, params: &BTreeMap<String, String>) -> Self {
        let mut result = self.clone();
        result.blurred = false;
        result.background_colors = colors_from_string(&result.slug);
        result.intensity = DEFAULT_INTENSITY;

        if let Some(mode) = params.get("mode")
            && !mode.is_empty()
        {
            for part in mode.replace('+', " ").split(' ') {
                if part == "blur" {
                    result.blurred = true;
                }
            }
        }
        for key in ["bg_color", "gradient", "color", "slug"] {
            if result.background_colors.is_empty()
                && let Some(val) = params.get(key)
            {
                result.background_colors = colors_from_string(val);
            }
        }
        if let Some(val) = params.get("intensity")
            && let Ok(intensity) = val.parse::<i32>()
            && (-100..=100).contains(&intensity)
        {
            result.intensity = intensity;
        }
        let raw_rot = params
            .get("rotation")
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(0);
        result.rotation = (raw_rot.clamp(0, 315) / 45) * 45;
        result
    }

    /// Returns a copy with `blurred` updated.
    #[must_use]
    pub fn with_blurred(&self, blurred: bool) -> Self {
        let mut result = self.clone();
        result.blurred = blurred;
        result
    }

    /// Returns a copy with `intensity` updated.
    #[must_use]
    pub fn with_pattern_intensity(&self, intensity: i32) -> Self {
        let mut result = self.clone();
        result.intensity = intensity;
        result
    }

    /// Returns a copy with `background_colors` updated (updating `slug` if it was a color slug).
    #[must_use]
    pub fn with_background_colors(&self, colors: Vec<Color>) -> Self {
        let mut result = self.clone();
        result.background_colors = colors;
        if !colors_from_string(&self.slug).is_empty() && !result.background_colors.is_empty() {
            result.slug = string_from_colors(&result.background_colors);
        }
        result
    }

    /// Returns a copy inheriting blur, colors, intensity, and pattern flag from `other`.
    #[must_use]
    pub fn with_params_from(&self, other: &Self) -> Self {
        let mut result = self.clone();
        result.blurred = other.blurred;
        if !other.background_colors.is_empty() {
            result.background_colors = other.background_colors.clone();
            if !colors_from_string(&self.slug).is_empty() {
                result.slug = string_from_colors(&result.background_colors);
            }
        }
        result.intensity = other.intensity;
        if other.is_pattern() {
            result.flags = result.flags.union(WallPaperFlags::PATTERN);
        }
        result
    }

    /// Serializes to the exact Qt `QDataStream` (`Qt_5_1`, big-endian) format
    /// used by `WallPaper::serialize()`.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.slug.len() * 2);
        out.extend_from_slice(&self.id.to_be_bytes());
        out.extend_from_slice(&self.access_hash.to_be_bytes());
        out.extend_from_slice(&VERSION_TAG.to_be_bytes());
        out.extend_from_slice(&SERIALIZE_VERSION.to_be_bytes());
        out.extend_from_slice(&(self.flags.bits() as i32).to_be_bytes());
        write_qstring(&mut out, &self.slug);
        out.extend_from_slice(&(if self.blurred { 1i32 } else { 0i32 }).to_be_bytes());
        out.extend_from_slice(&(self.background_colors.len() as i32).to_be_bytes());
        for &c in &self.background_colors {
            out.extend_from_slice(&c.to_serialized_rgb().to_be_bytes());
        }
        out.extend_from_slice(&self.intensity.to_be_bytes());
        out.extend_from_slice(&self.rotation.to_be_bytes());
        out.extend_from_slice(&self.owner_id.to_be_bytes());
        out
    }

    /// Deserializes from bytes produced by `WallPaper::serialize()` (both v1 and legacy formats).
    #[must_use]
    pub fn from_serialized(serialized: &[u8]) -> Option<Self> {
        if serialized.is_empty() {
            return None;
        }
        let mut cur = QDataReader::new(serialized);
        let id = cur.read_u64()?;
        let access_hash = cur.read_u64()?;
        let version_tag = cur.read_i32()?;

        let flags: u32;
        let owner_id: u64;
        let slug: String;
        let blurred: i32;
        let mut background_colors: Vec<Color> = Vec::new();
        let intensity: i32;
        let rotation: i32;

        if version_tag == VERSION_TAG {
            let _version = cur.read_i32()?;
            flags = cur.read_i32()? as u32;
            slug = cur.read_qstring()?;
            blurred = cur.read_i32()?;
            let count = cur.read_i32()?;
            if !(0..=4).contains(&count) {
                return None;
            }
            background_colors.reserve(count as usize);
            for _ in 0..count {
                let raw = cur.read_u32()?;
                let color = Color::maybe_from_serialized_rgb(raw)?;
                background_colors.push(color);
            }
            intensity = cur.read_i32()?;
            rotation = cur.read_i32()?;
            owner_id = cur.read_u64()?;
        } else {
            slug = cur.read_qstring()?;
            let settings = cur.read_i32()?;
            let bg_color = cur.read_u32()?;
            intensity = cur.read_i32()?;
            let mut legacy_owner = 0u64;
            if !cur.at_end() {
                let f1 = cur.read_i32()? as u32 as u64;
                let f2 = if !cur.at_end() {
                    cur.read_i32()? as u32 as u64
                } else {
                    0
                };
                legacy_owner = f1 | (f2 << 32);
            }
            owner_id = legacy_owner;
            flags = WallPaperFlags::from_legacy_raw(version_tag as u32).bits();
            blurred = if (settings & (1 << 1)) != 0 { 1 } else { 0 };
            if let Some(color) = Color::maybe_from_serialized_rgb(bg_color) {
                background_colors.push(color);
            }
            rotation = 0;
        }

        if !(-100..=100).contains(&intensity) {
            return None;
        }

        Some(Self {
            id,
            access_hash,
            owner_id,
            flags: WallPaperFlags::from_bits(flags),
            slug,
            emoji_id: String::new(),
            background_colors,
            rotation,
            intensity,
            blurred: blurred == 1,
            document_id: None,
        })
    }
}

/// Parsed representation of a `t.me/bg/...` or `tg://bg?...` wallpaper link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedWallPaperUrl {
    /// Raw slug (either a server slug or a color/gradient slug), if present.
    pub slug: Option<String>,
    /// Query parameters extracted from the URL.
    pub params: BTreeMap<String, String>,
    /// Immediately constructed wallpaper if the URL specifies colors/gradient directly
    /// (without needing a server `account.getWallPaper` call).
    pub local_wallpaper: Option<WallPaper>,
}

/// Validates a server wallpaper slug (`IsValidWallPaperSlug` in `background_preview_box.cpp`).
#[must_use]
pub fn is_valid_wallpaper_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_WALLPAPER_SLUG_LEN {
        return false;
    }
    slug.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Parses a 6-character RGB hex string without `#`.
#[must_use]
pub fn color_from_rgb6(s: &str) -> Option<Color> {
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Color::from_hex(s)
}

/// Parses 1 to 4 colors from a Telegram wallpaper color/gradient slug:
/// - 1 color: `rrggbb`
/// - 2 colors: `rrggbb-rrggbb` or `rrggbb~rrggbb`
/// - 3 or 4 colors: `rrggbb~rrggbb~rrggbb[~rrggbb]`
///
/// Exact port of `ColorsFromString` in `data_wall_paper.cpp`.
#[must_use]
pub fn colors_from_string(s: &str) -> Vec<Color> {
    let bytes = s.as_bytes();
    let count = bytes.len() / 6;
    if count == 0 || count > 4 || bytes.len() != count * 7 - 1 {
        return Vec::new();
    }
    let mut result = Vec::with_capacity(count);
    for i in 0..count {
        if i + 1 < count {
            let sep = bytes[i * 7 + 6];
            if sep != b'~' && (count > 2 || sep != b'-') {
                return Vec::new();
            }
        }
        let slice = &s[i * 7..i * 7 + 6];
        let Some(parsed) = color_from_rgb6(slice) else {
            return Vec::new();
        };
        result.push(parsed);
    }
    result
}

/// Formats 1 to 4 colors into a wallpaper slug (`StringFromColors`).
/// Uses `-` for 2 colors and `~` for 3 or 4 colors.
#[must_use]
pub fn string_from_colors(colors: &[Color]) -> String {
    let sep = if colors.len() > 2 { "~" } else { "-" };
    colors
        .iter()
        .map(|c| c.to_rgb_hex())
        .collect::<Vec<_>>()
        .join(sep)
}

fn parse_query_params(query: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        if let Some((k, v)) = pair.split_once('=') {
            map.insert(percent_decode(k), percent_decode(v));
        } else {
            map.insert(percent_decode(pair), String::new());
        }
    }
    map
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parses a `t.me/bg/...`, `telegram.me/bg/...`, `bg/...`, or `tg://bg?...` wallpaper URL.
///
/// Mirrors `local_url_handlers.cpp` (`^bg/([a-zA-Z0-9._-~]+)(\?(.+)?)?$`) and
/// `BackgroundPreviewBox::Start`.
#[must_use]
pub fn parse_wallpaper_url(url: &str) -> Option<ParsedWallPaperUrl> {
    let trimmed = url.trim().split('#').next().unwrap_or("").trim();

    // Case 1: tg://bg?...
    if let Some(rest) = trimmed
        .strip_prefix("tg://bg?")
        .or_else(|| trimmed.strip_prefix("tg://bg/?"))
    {
        let params = parse_query_params(rest);
        let slug = params
            .get("slug")
            .or_else(|| params.get("gradient"))
            .or_else(|| params.get("color"))
            .cloned();
        let local_wallpaper = slug
            .as_deref()
            .and_then(WallPaper::from_colors_slug)
            .map(|p| p.with_url_params(&params));
        return Some(ParsedWallPaperUrl {
            slug,
            params,
            local_wallpaper,
        });
    }

    // Case 2: https://t.me/bg/<slug>[?<params>], t.me/bg/<slug>, bg/<slug>
    let after_bg = if let Some(rest) = trimmed.strip_prefix("bg/") {
        rest
    } else {
        let pos = trimmed.find("/bg/")?;
        &trimmed[pos + 4..]
    };

    let (slug_part, query_part) = match after_bg.split_once('?') {
        Some((s, q)) => (s, Some(q)),
        None => (after_bg, None),
    };
    if slug_part.is_empty()
        || !slug_part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'~'))
    {
        return None;
    }

    let params = query_part.map(parse_query_params).unwrap_or_default();
    let local_wallpaper =
        WallPaper::from_colors_slug(slug_part).map(|p| p.with_url_params(&params));
    if local_wallpaper.is_none() && !is_valid_wallpaper_slug(slug_part) {
        return None;
    }

    Some(ParsedWallPaperUrl {
        slug: Some(slug_part.to_string()),
        params,
        local_wallpaper,
    })
}

// --- Qt QDataStream (Qt_5_1) helpers ---

fn write_qstring(out: &mut Vec<u8>, s: &str) {
    if s.is_empty() {
        out.extend_from_slice(&0xffff_ffffu32.to_be_bytes());
        return;
    }
    let units: Vec<u16> = s.encode_utf16().collect();
    let byte_len = (units.len() * 2) as u32;
    out.extend_from_slice(&byte_len.to_be_bytes());
    for u in units {
        out.extend_from_slice(&u.to_be_bytes());
    }
}

struct QDataReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> QDataReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.data.len()
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

    fn read_u32(&mut self) -> Option<u32> {
        let b = self.read_bytes(4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_i32(&mut self) -> Option<i32> {
        self.read_u32().map(|v| v as i32)
    }

    fn read_u64(&mut self) -> Option<u64> {
        let b = self.read_bytes(8)?;
        Some(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn read_qstring(&mut self) -> Option<String> {
        let byte_len = self.read_u32()?;
        if byte_len == 0xffff_ffff || byte_len == 0 {
            return Some(String::new());
        }
        if byte_len % 2 != 0 {
            return None;
        }
        let raw = self.read_bytes(byte_len as usize)?;
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_be_bytes(c))
            .collect();
        String::from_utf16(&units).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_wallpaper_and_serialization_round_trip() {
        let mut wp = WallPaper::construct_default();
        wp.access_hash = 0x1234_5678_9abc_def0;
        wp.owner_id = 42;
        wp.slug = "test_slug".to_string();
        wp.rotation = 90;
        wp.blurred = true;

        assert!(wp.is_default_wallpaper());
        assert!(wp.is_pattern());
        assert!(wp.is_cloud());
        assert_eq!(wp.background_colors.len(), 4);
        assert_eq!(wp.gradient_rotation(), 0); // 4 colors -> dynamic rotation = 0

        let bytes = wp.serialize();
        let decoded = WallPaper::from_serialized(&bytes).unwrap();
        assert_eq!(decoded, wp);
    }

    #[test]
    fn colors_slug_and_url_parsing() {
        // 1 color
        let p1 = parse_wallpaper_url("https://t.me/bg/40a7e3").unwrap();
        assert_eq!(
            p1.local_wallpaper.as_ref().unwrap().background_colors,
            vec![Color::rgb(0x40, 0xa7, 0xe3)]
        );

        // 2 colors with rotation and blur
        let p2 =
            parse_wallpaper_url("https://t.me/bg/ff0000-00ff00?rotation=135&mode=blur").unwrap();
        let wp2 = p2.local_wallpaper.unwrap();
        assert_eq!(
            wp2.background_colors,
            vec![Color::rgb(255, 0, 0), Color::rgb(0, 255, 0)]
        );
        assert_eq!(wp2.rotation, 135);
        assert!(wp2.blurred);
        assert_eq!(
            wp2.share_url("https://t.me").as_deref(),
            Some("https://t.me/bg/ff0000-00ff00?rotation=135&mode=blur")
        );

        // 4-color freeform gradient with negative intensity
        let p4 = parse_wallpaper_url("https://t.me/bg/dbddbb~6ba587~d5d88d~88b884?intensity=-45")
            .unwrap();
        let wp4 = p4.local_wallpaper.unwrap();
        assert_eq!(wp4.background_colors.len(), 4);
        assert_eq!(wp4.intensity, -45);
        assert!((wp4.pattern_opacity() - (-0.45)).abs() < 1e-9);

        // Server slug
        let ps =
            parse_wallpaper_url("https://t.me/bg/Br6n9k2s?bg_color=112233&intensity=30").unwrap();
        assert_eq!(ps.slug.as_deref(), Some("Br6n9k2s"));
        assert!(ps.local_wallpaper.is_none());
        assert_eq!(
            ps.params.get("bg_color").map(String::as_str),
            Some("112233")
        );

        // tg://bg? URL
        let ptg = parse_wallpaper_url("tg://bg?gradient=112233-445566&rotation=45").unwrap();
        let wptg = ptg.local_wallpaper.unwrap();
        assert_eq!(wptg.rotation, 45);
        assert_eq!(wptg.background_colors.len(), 2);
    }

    #[test]
    fn reject_invalid_serialized_wallpaper() {
        assert!(WallPaper::from_serialized(&[]).is_none());
        assert!(WallPaper::from_serialized(&[1, 2, 3]).is_none());
        let mut wp = WallPaper::construct_default();
        wp.intensity = 150;
        let bad = wp.serialize();
        assert!(WallPaper::from_serialized(&bad).is_none());
    }
}
