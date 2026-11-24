//! Chat background renderer, procedural GPL-safe pattern masks, and `history/` integration API.
//!
//! Port of `Telegram/SourceFiles/window/themes/window_theme.cpp` (`ChatBackground`)
//! and `Telegram/SourceFiles/ui/chat/chat_theme.cpp`.
//!
//! # Integration contract for `history/`
//!
//! `history/` does not need to know how wallpapers, gradients, patterns, blur, or motion
//! are rendered. Obtain a mutable reference to [`ChatBackground`] (or use the context-local
//! shared handle [`active_chat_background`]) and call [`ChatBackground::paint`]:
//!
//! ```ignore
//! use crate::themes::ChatBackground;
//!
//! // Inside history canvas painting:
//! chat_background.paint(ui, canvas_rect, Some(peer_id));
//! ```
//!
//! When a message is sent or received in a chat with motion enabled, call
//! [`ChatBackground::trigger_motion_step`] to advance the complex 4-color gradient phase
//! smoothly just like Telegram Desktop.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, Mutex};

use egui::{
    Color32, ColorImage, Context, CornerRadius, Id, Pos2, Rect, TextureHandle, TextureOptions, Ui,
};
use image::{Rgba, RgbaImage};
use tg_theme::svg_pattern::{
    PatternTileMode, SvgPatternError, TGV_MAX_DECOMPRESSED_BYTES,
    composite_pattern_on_gradient_opaque, decode_svg_or_tgv_bytes, is_gzip_bytes, is_svg_bytes,
    rasterize_svg_pattern_mask, rasterize_tgv_pattern_mask, render_tiled_pattern_wallpaper,
    tile_pattern_mask,
};
use tg_theme::{
    ADJUSTABLE_COLOR_KEYS, BACKGROUND_PIXEL_LIMIT, Color, DEFAULT_BACKGROUND_COLOR, Palette,
    ThemeError, WallPaper, WallPaperFlags, compose_pattern_on_gradient,
    compute_chat_background_rects, compute_real_progress, compute_real_rotation,
    count_average_color_image, count_average_color_slice, dither_image, generate_background_image,
    generate_gradient, prepare_blurred_background, prepare_image_for_tiled,
    preprocess_background_image, theme_adjusted_color,
};
use tg_ui::theme::theme_color_to_egui;

/// Default render dimension for cached gradient/pattern chat background textures.
const DEFAULT_RENDER_SIZE: u32 = 384;
/// Dimension of procedurally generated tileable pattern masks.
const PATTERN_MASK_SIZE: u32 = 256;

/// Procedural GPL-safe pattern mask styles composited over chat gradients.
///
/// Every pattern is generated algorithmically as a black-on-transparent `RgbaImage` mask
/// compatible with [`tg_theme::compose_pattern_on_gradient`] (both positive soft-light
/// intensity and negative dark-mode inverted mask intensity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PatternKind {
    /// No pattern overlay.
    None,
    /// Telegram-inspired geometric doodle motifs (stars, paper planes, moons, diamonds, bubbles).
    #[default]
    Doodles,
    /// Flowing diagonal wave ribbons.
    Waves,
    /// Subtle honeycomb hexagonal lattice.
    Hexagons,
    /// Botanical leaf & petal silhouettes.
    Botanical,
    /// Minimalist constellation dots and fine rings.
    Constellation,
}

impl PatternKind {
    /// All selectable non-empty pattern kinds.
    pub const ALL: &'static [Self] = &[
        Self::Doodles,
        Self::Waves,
        Self::Hexagons,
        Self::Botanical,
        Self::Constellation,
    ];

    /// Human-readable label for UI selectors.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Doodles => "Doodles",
            Self::Waves => "Waves",
            Self::Hexagons => "Hexagons",
            Self::Botanical => "Botanical",
            Self::Constellation => "Constellation",
        }
    }

    /// Stable slug identifier used in wallpaper metadata and caching.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Doodles => "doodles",
            Self::Waves => "waves",
            Self::Hexagons => "hexagons",
            Self::Botanical => "botanical",
            Self::Constellation => "constellation",
        }
    }

    /// Resolves a pattern kind from a wallpaper slug or emoji hint.
    #[must_use]
    pub fn from_slug(slug: &str) -> Self {
        let lower = slug.to_ascii_lowercase();
        if lower.contains("wave") || lower.contains("ocean") || lower.contains("aqua") {
            Self::Waves
        } else if lower.contains("hex") || lower.contains("tech") || lower.contains("grid") {
            Self::Hexagons
        } else if lower.contains("botan") || lower.contains("leaf") || lower.contains("meadow") {
            Self::Botanical
        } else if lower.contains("star")
            || lower.contains("night")
            || lower.contains("constel")
            || lower.contains("astro")
        {
            Self::Constellation
        } else if lower == "none" {
            Self::None
        } else {
            Self::Doodles
        }
    }
}

/// Complete configuration for one chat background (global or per-peer override).
#[derive(Debug, Clone)]
pub struct BackgroundConfig {
    /// Core `tg-theme` wallpaper descriptor (colors, rotation, intensity, blurred, flags, slug).
    pub wallpaper: WallPaper,
    /// Procedural pattern mask fallback to overlay when `wallpaper.is_pattern()` is true and
    /// [`Self::pattern_bytes`] is `None`.
    pub pattern: PatternKind,
    /// Optional raw SVG or gzipped `.tgv` vector pattern wallpaper bytes (WP-45 rasterizer).
    ///
    /// When present and `wallpaper.is_pattern()` is `true`, [`ChatBackground`] rasterizes and
    /// composites this vector pattern via `tg_theme::svg_pattern` instead of procedural
    /// [`PatternKind`].
    pub pattern_bytes: Option<Arc<Vec<u8>>>,
    /// Optional custom image loaded from disk or bundled inside a `.tdesktop-theme`.
    pub custom_image: Option<Arc<RgbaImage>>,
    /// Whether image/pattern rendering tiles across the viewport.
    pub tiled: bool,
    /// Whether smooth complex-gradient motion animation is enabled.
    pub motion: bool,
    /// Dark-mode dimming percentage (`0..=100`, applied when rendering image wallpapers in dark mode).
    pub dark_dimming: u8,
}

impl Default for BackgroundConfig {
    fn default() -> Self {
        Self::classic_meadow()
    }
}

impl BackgroundConfig {
    /// Telegram's signature Classic Meadow 4-color patterned wallpaper.
    #[must_use]
    pub fn classic_meadow() -> Self {
        Self {
            wallpaper: WallPaper::construct_default(),
            pattern: PatternKind::Doodles,
            pattern_bytes: None,
            custom_image: None,
            tiled: true,
            motion: false,
            dark_dimming: 0,
        }
    }

    /// Telegram-style dark starry gradient wallpaper suited for Night mode.
    #[must_use]
    pub fn night_aurora() -> Self {
        let mut wp = WallPaper::custom()
            .with_background_colors(vec![
                Color::rgb(0x13, 0x20, 0x33),
                Color::rgb(0x1E, 0x3B, 0x52),
                Color::rgb(0x18, 0x26, 0x3A),
                Color::rgb(0x28, 0x37, 0x54),
            ])
            .with_pattern_intensity(-42);
        wp.flags = WallPaperFlags::PATTERN.union(WallPaperFlags::DARK);
        wp.slug = "night_aurora".to_owned();
        Self {
            wallpaper: wp,
            pattern: PatternKind::Constellation,
            pattern_bytes: None,
            custom_image: None,
            tiled: true,
            motion: true,
            dark_dimming: 0,
        }
    }

    /// Creates a solid-color background configuration.
    #[must_use]
    pub fn solid(color: Color) -> Self {
        let mut wp = WallPaper::custom().with_background_colors(vec![color]);
        wp.slug = color.to_rgb_hex();
        Self {
            wallpaper: wp,
            pattern: PatternKind::None,
            pattern_bytes: None,
            custom_image: None,
            tiled: false,
            motion: false,
            dark_dimming: 0,
        }
    }

    /// Creates a 2-to-4-color gradient background configuration with optional pattern.
    #[must_use]
    pub fn gradient(
        colors: Vec<Color>,
        rotation: i32,
        pattern: PatternKind,
        intensity: i32,
        motion: bool,
    ) -> Self {
        let mut wp = WallPaper::custom()
            .with_background_colors(colors)
            .with_pattern_intensity(intensity.clamp(-100, 100));
        wp.rotation = (rotation.rem_euclid(360) / 45) * 45;
        if pattern != PatternKind::None {
            wp.flags = wp.flags.union(WallPaperFlags::PATTERN);
        }
        Self {
            wallpaper: wp,
            pattern,
            pattern_bytes: None,
            custom_image: None,
            tiled: pattern != PatternKind::None,
            motion,
            dark_dimming: 0,
        }
    }

    /// Creates a vector SVG or gzipped `.tgv` pattern wallpaper configuration backed by
    /// `tg_theme::svg_pattern` (no procedural [`PatternKind`] fallback).
    pub fn from_svg_pattern(
        mut wallpaper: WallPaper,
        svg_or_tgv_bytes: Vec<u8>,
        motion: bool,
    ) -> Result<Self, SvgPatternError> {
        let _ = decode_svg_or_tgv_bytes(&svg_or_tgv_bytes, TGV_MAX_DECOMPRESSED_BYTES)?;
        wallpaper.flags = wallpaper.flags.union(WallPaperFlags::PATTERN);
        wallpaper.intensity = wallpaper.intensity.clamp(-100, 100);
        if wallpaper.background_colors.is_empty() {
            wallpaper.background_colors = WallPaper::construct_default().background_colors;
        }
        Ok(Self {
            wallpaper,
            pattern: PatternKind::None,
            pattern_bytes: Some(Arc::new(svg_or_tgv_bytes)),
            custom_image: None,
            tiled: true,
            motion,
            dark_dimming: 0,
        })
    }

    /// Attaches raw SVG or gzipped `.tgv` pattern wallpaper bytes to this configuration,
    /// setting `WallPaperFlags::PATTERN` and disabling procedural fallback.
    #[must_use]
    pub fn with_pattern_bytes(mut self, svg_or_tgv_bytes: Vec<u8>) -> Self {
        self.wallpaper.flags = self.wallpaper.flags.union(WallPaperFlags::PATTERN);
        self.pattern = PatternKind::None;
        self.pattern_bytes = Some(Arc::new(svg_or_tgv_bytes));
        self.tiled = true;
        self
    }

    /// Returns `true` when this configuration carries real SVG or `.tgv` pattern bytes.
    #[must_use]
    pub fn has_svg_pattern(&self) -> bool {
        self.pattern_bytes
            .as_ref()
            .is_some_and(|bytes| !bytes.is_empty())
    }

    /// Creates a custom image background configuration after preprocessing dimensions.
    #[must_use]
    pub fn from_image(image: RgbaImage, tiled: bool, blurred: bool) -> Self {
        let processed = preprocess_background_image(&image);
        let wp = WallPaper::custom().with_blurred(blurred);
        Self {
            wallpaper: wp,
            pattern: PatternKind::None,
            pattern_bytes: None,
            custom_image: Some(Arc::new(processed)),
            tiled,
            motion: false,
            dark_dimming: 25,
        }
    }

    /// Returns `true` when this background supports complex-gradient motion animation
    /// (3 or 4 gradient colors and no opaque full photo overriding it).
    #[must_use]
    pub fn supports_motion(&self) -> bool {
        self.custom_image.is_none() && self.wallpaper.background_colors.len() >= 3
    }

    /// Returns `true` when blur toggle is meaningful (non-pattern photo/image background).
    #[must_use]
    pub fn supports_blur(&self) -> bool {
        self.custom_image.is_some() && !self.wallpaper.is_pattern()
    }

    /// Computes a deterministic cache key capturing every visual parameter.
    #[must_use]
    fn cache_key(
        &self,
        rotation_add: i32,
        width: u32,
        height: u32,
        dark_mode: bool,
    ) -> BackgroundCacheKey {
        let image_ptr = self
            .custom_image
            .as_ref()
            .map_or(0usize, |img| Arc::as_ptr(img) as usize);
        let pattern_bytes_hash = self
            .pattern_bytes
            .as_deref()
            .map_or(0u64, |bytes| hash_pattern_bytes(bytes));
        BackgroundCacheKey {
            colors: self.wallpaper.background_colors.clone(),
            rotation: self.wallpaper.rotation,
            rotation_add,
            intensity: self.wallpaper.intensity,
            is_pattern: self.wallpaper.is_pattern(),
            pattern: self.pattern,
            pattern_bytes_hash,
            blurred: self.wallpaper.blurred,
            tiled: self.tiled,
            image_ptr,
            width,
            height,
            dark_mode,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BackgroundCacheKey {
    colors: Vec<Color>,
    rotation: i32,
    rotation_add: i32,
    intensity: i32,
    is_pattern: bool,
    pattern: PatternKind,
    pattern_bytes_hash: u64,
    blurred: bool,
    tiled: bool,
    image_ptr: usize,
    width: u32,
    height: u32,
    dark_mode: bool,
}

fn hash_pattern_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.len().hash(&mut hasher);
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Live chat background manager exposing a clean rendering API for `history/` and previews.
///
/// Manages:
/// - Global wallpaper + per-chat (`peer_id: i64`) wallpaper overrides
/// - Procedural GPL-safe pattern mask caching
/// - GPU `egui::TextureHandle` caching (rebuilt only when visual state or motion phase changes)
/// - Adjustable palette color derivation (`msgServiceBg`, `historyScrollBg`, etc.)
/// - Motion animation phase stepping for 3- and 4-color Telegram freeform gradients
pub struct ChatBackground {
    global: BackgroundConfig,
    per_peer: HashMap<i64, BackgroundConfig>,
    pattern_masks: HashMap<PatternKind, Arc<RgbaImage>>,
    svg_pattern_masks: HashMap<(u64, u32), Arc<RgbaImage>>,
    texture_cache: HashMap<BackgroundCacheKey, TextureHandle>,
    motion_rotation_add: i32,
    motion_anim_progress: f32,
    motion_animating: bool,
    dark_mode: bool,
    generation: u64,
}

impl Default for ChatBackground {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatBackground {
    /// Creates a new `ChatBackground` initialized to Telegram's Classic Meadow wallpaper.
    #[must_use]
    pub fn new() -> Self {
        Self {
            global: BackgroundConfig::classic_meadow(),
            per_peer: HashMap::new(),
            pattern_masks: HashMap::new(),
            svg_pattern_masks: HashMap::new(),
            texture_cache: HashMap::new(),
            motion_rotation_add: 0,
            motion_anim_progress: 1.0,
            motion_animating: false,
            dark_mode: false,
            generation: 1,
        }
    }

    /// Returns the monotonically increasing generation counter (bumped on every change).
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns whether dark mode rendering hints (dimming, negative pattern fallback) are active.
    #[must_use]
    pub const fn is_dark_mode(&self) -> bool {
        self.dark_mode
    }

    /// Updates dark mode state (does not overwrite a user-chosen wallpaper, only dimming hints).
    pub fn set_dark_mode(&mut self, dark: bool) {
        if self.dark_mode != dark {
            self.dark_mode = dark;
            self.generation = self.generation.wrapping_add(1);
        }
    }

    /// Returns the global default chat background configuration.
    #[must_use]
    pub const fn global_config(&self) -> &BackgroundConfig {
        &self.global
    }

    /// Replaces the global background applied to all chats without a per-chat override.
    pub fn set_global(&mut self, config: BackgroundConfig) {
        self.global = config;
        self.generation = self.generation.wrapping_add(1);
        self.trim_cache();
    }

    /// Sets or replaces a per-chat background override for `peer_id`.
    pub fn set_peer(&mut self, peer_id: i64, config: BackgroundConfig) {
        self.per_peer.insert(peer_id, config);
        self.generation = self.generation.wrapping_add(1);
        self.trim_cache();
    }

    /// Removes any per-chat background override for `peer_id`, falling back to global.
    pub fn clear_peer(&mut self, peer_id: i64) -> bool {
        let removed = self.per_peer.remove(&peer_id).is_some();
        if removed {
            self.generation = self.generation.wrapping_add(1);
        }
        removed
    }

    /// Returns whether `peer_id` has a custom per-chat wallpaper override.
    #[must_use]
    pub fn has_peer_override(&self, peer_id: i64) -> bool {
        self.per_peer.contains_key(&peer_id)
    }

    /// Resolves the effective [`BackgroundConfig`] for `peer_id` (falling back to global).
    #[must_use]
    pub fn resolve_for_peer(&self, peer_id: Option<i64>) -> &BackgroundConfig {
        peer_id
            .and_then(|id| self.per_peer.get(&id))
            .unwrap_or(&self.global)
    }

    /// Loads an image file from disk (PNG, JPEG, WebP, GIF) and returns a preprocessed
    /// [`BackgroundConfig`] ready for previewing or applying.
    pub fn load_config_from_file(path: &Path) -> Result<BackgroundConfig, ThemeError> {
        let bytes = std::fs::read(path)?;
        Self::load_config_from_bytes(&bytes)
    }

    /// Decodes an image from raw bytes with pixel-limit validation matching `tdesktop`.
    pub fn load_config_from_bytes(bytes: &[u8]) -> Result<BackgroundConfig, ThemeError> {
        let trimmed = bytes
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .map_or(bytes, |i| &bytes[i..]);
        if trimmed.starts_with(b"<svg") || trimmed.starts_with(b"<?xml") {
            return Err(ThemeError::BackgroundImage(
                "SVG files cannot be used as raster photo backgrounds".to_owned(),
            ));
        }
        let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| ThemeError::BackgroundImage(e.to_string()))?;
        let (w, h) = reader
            .into_dimensions()
            .map_err(|e| ThemeError::BackgroundImage(e.to_string()))?;
        if w == 0 || h == 0 || u64::from(w) * u64::from(h) > BACKGROUND_PIXEL_LIMIT {
            return Err(ThemeError::BackgroundImage(format!(
                "invalid background image dimensions {w}x{h}"
            )));
        }
        let decoded = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| ThemeError::BackgroundImage(e.to_string()))?
            .decode()
            .map_err(|e| ThemeError::BackgroundImage(e.to_string()))?;
        let mut rgba = decoded.to_rgba8();
        for px in rgba.pixels_mut() {
            let a = u16::from(px.0[3]);
            if a < 255 {
                let inv = 255 - a;
                px.0[0] = ((u16::from(px.0[0]) * a + 255 * inv + 127) / 255) as u8;
                px.0[1] = ((u16::from(px.0[1]) * a + 255 * inv + 127) / 255) as u8;
                px.0[2] = ((u16::from(px.0[2]) * a + 255 * inv + 127) / 255) as u8;
                px.0[3] = 255;
            }
        }
        Ok(BackgroundConfig::from_image(rgba, false, false))
    }

    /// Advances the complex freeform gradient animation by one 45-degree motion step
    /// (called when sending/receiving messages or pressing the Motion preview trigger).
    pub fn trigger_motion_step(&mut self) {
        self.motion_rotation_add = (self.motion_rotation_add + 45).rem_euclid(720);
        self.motion_anim_progress = 0.0;
        self.motion_animating = true;
    }

    /// Advances ongoing motion interpolation by `dt_seconds`. Returns `true` if still animating.
    pub fn tick_motion(&mut self, dt_seconds: f32) -> bool {
        if !self.motion_animating {
            return false;
        }
        self.motion_anim_progress = (self.motion_anim_progress + dt_seconds * 2.5).min(1.0);
        if self.motion_anim_progress >= 1.0 {
            self.motion_animating = false;
        }
        self.motion_animating
    }

    /// Current motion rotation offset in `0..720` (for testing and diagnostics).
    #[must_use]
    pub const fn motion_rotation_add(&self) -> i32 {
        self.motion_rotation_add
    }

    /// Loads a raw SVG or gzipped `.tgv` vector pattern wallpaper from disk.
    pub fn load_pattern_from_file(
        path: &Path,
        wallpaper: WallPaper,
    ) -> Result<BackgroundConfig, ThemeError> {
        let bytes = std::fs::read(path)?;
        Self::load_pattern_from_bytes(&bytes, wallpaper)
            .map_err(|e| ThemeError::BackgroundImage(e.to_string()))
    }

    /// Validates and wraps raw SVG or gzipped `.tgv` vector pattern wallpaper bytes into a
    /// [`BackgroundConfig`] rendered via `tg_theme::svg_pattern`.
    pub fn load_pattern_from_bytes(
        bytes: &[u8],
        wallpaper: WallPaper,
    ) -> Result<BackgroundConfig, SvgPatternError> {
        let motion = wallpaper.background_colors.len() >= 3;
        BackgroundConfig::from_svg_pattern(wallpaper, bytes.to_vec(), motion)
    }

    /// Returns `true` if `bytes` looks like a raw SVG or gzipped `.tgv` pattern wallpaper.
    #[must_use]
    pub fn is_svg_or_tgv_pattern_bytes(bytes: &[u8]) -> bool {
        is_gzip_bytes(bytes) || is_svg_bytes(bytes)
    }

    /// Rasterizes (and caches) raw SVG or gzipped `.tgv` pattern bytes into a canonical
    /// black-on-transparent alpha mask using `tg_theme::svg_pattern`.
    pub fn rasterize_pattern_bytes(
        &mut self,
        bytes: &[u8],
        tile_size: u32,
    ) -> Result<Arc<RgbaImage>, SvgPatternError> {
        let dim = tile_size.clamp(16, 1024);
        let key = (hash_pattern_bytes(bytes), dim);
        if let Some(existing) = self.svg_pattern_masks.get(&key) {
            return Ok(Arc::clone(existing));
        }
        let mask = if is_gzip_bytes(bytes) {
            rasterize_tgv_pattern_mask(bytes, Some((dim, dim)), TGV_MAX_DECOMPRESSED_BYTES)?
        } else {
            rasterize_svg_pattern_mask(bytes, Some((dim, dim)))?
        };
        let arc = Arc::new(mask);
        if self.svg_pattern_masks.len() >= 32 {
            self.svg_pattern_masks.clear();
        }
        self.svg_pattern_masks.insert(key, Arc::clone(&arc));
        Ok(arc)
    }

    /// Returns (and lazily caches) the procedural pattern mask image for `kind`.
    pub fn pattern_mask(&mut self, kind: PatternKind) -> Option<Arc<RgbaImage>> {
        if kind == PatternKind::None {
            return None;
        }
        let mask = self
            .pattern_masks
            .entry(kind)
            .or_insert_with(|| Arc::new(generate_pattern_mask(kind, PATTERN_MASK_SIZE)));
        Some(Arc::clone(mask))
    }

    /// Renders `config` into an `RgbaImage` of `(width, height)` using `tg-theme`'s
    /// SVG/`.tgv` rasterizer (`tg_theme::svg_pattern`), gradient, dithering, pattern
    /// compositing, and StackBlur renderers.
    pub fn render_image(
        &mut self,
        config: &BackgroundConfig,
        width: u32,
        height: u32,
        rotation_add: i32,
    ) -> RgbaImage {
        let w = width.max(1);
        let h = height.max(1);

        // 1. Custom photo/image background
        if let Some(ref img) = config.custom_image {
            let base = if config.wallpaper.blurred {
                prepare_blurred_background(img)
            } else {
                (**img).clone()
            };
            return if config.tiled {
                prepare_image_for_tiled(&base)
            } else {
                base
            };
        }

        // 2. Gradient / solid + SVG/`.tgv` vector pattern OR procedural pattern overlay
        let colors = if config.wallpaper.background_colors.is_empty() {
            vec![DEFAULT_BACKGROUND_COLOR]
        } else {
            config.wallpaper.background_colors.clone()
        };
        let eff_rot = compute_real_rotation(colors.len(), config.wallpaper.rotation, rotation_add);
        let eff_progress = if config.motion && colors.len() >= 3 {
            compute_real_progress(colors.len(), config.wallpaper.rotation, rotation_add)
        } else {
            1.0
        };

        // 2a. Real SVG / gzipped `.tgv` server pattern wallpaper via WP-45 `tg_theme::svg_pattern`
        if config.wallpaper.is_pattern()
            && let Some(ref raw_pattern) = config.pattern_bytes
            && !raw_pattern.is_empty()
        {
            let tile_dim = PATTERN_MASK_SIZE.min(w.max(h)).max(32);
            let tile_mode = if config.tiled {
                PatternTileMode::Centered
            } else {
                PatternTileMode::Repeat
            };

            if rotation_add == 0 || !(config.motion && colors.len() >= 3) {
                if let Ok(rendered) = render_tiled_pattern_wallpaper(
                    raw_pattern,
                    w,
                    h,
                    &colors,
                    eff_rot,
                    config.wallpaper.intensity,
                    Some((tile_dim, tile_dim)),
                    tile_mode,
                ) {
                    return rendered;
                }
            } else if let Ok(mask_tile) = self.rasterize_pattern_bytes(raw_pattern, tile_dim) {
                let tiled_mask = tile_pattern_mask(&mask_tile, w, h, tile_mode);
                let mut grad = generate_gradient(w, h, &colors, eff_rot, eff_progress);
                if colors.len() > 1 && config.wallpaper.intensity >= 0 {
                    grad = dither_image(&grad);
                }
                return composite_pattern_on_gradient_opaque(
                    &grad,
                    &tiled_mask,
                    &colors,
                    config.wallpaper.intensity,
                );
            }
        }

        // 2b. Procedural pattern fallback when no SVG/`.tgv` bytes are supplied
        let pattern_mask = if config.wallpaper.is_pattern() && config.pattern != PatternKind::None {
            self.pattern_mask(config.pattern)
        } else {
            None
        };

        if let Some(mask) = pattern_mask {
            let mut grad = generate_gradient(w, h, &colors, eff_rot, eff_progress);
            if colors.len() > 1 && config.wallpaper.pattern_opacity() >= 0.0 {
                grad = dither_image(&grad);
            }
            let composited = compose_pattern_on_gradient(
                &grad,
                &mask,
                &colors,
                config.wallpaper.pattern_opacity(),
            );
            // For negative intensity (dark mode masked pattern), composite over black so opaque.
            if config.wallpaper.pattern_opacity() < 0.0 {
                composite_over_black(&composited)
            } else {
                composited
            }
        } else {
            generate_background_image(w, h, &colors, eff_rot, 0.0, None)
        }
    }

    /// Renders the active background for `peer_id` at `(width, height)` (used for theme export).
    pub fn create_current_image(
        &mut self,
        peer_id: Option<i64>,
        width: u32,
        height: u32,
    ) -> RgbaImage {
        let cfg = self.resolve_for_peer(peer_id).clone();
        self.render_image(&cfg, width, height, 0)
    }

    /// Obtains (or lazily renders and uploads) the `egui::TextureHandle` for `config`.
    pub fn texture_for_config(
        &mut self,
        ctx: &Context,
        config: &BackgroundConfig,
        render_size: u32,
    ) -> TextureHandle {
        let rot_add = if config.motion && config.supports_motion() {
            self.motion_rotation_add
        } else {
            0
        };
        let key = config.cache_key(rot_add, render_size, render_size, self.dark_mode);
        if let Some(existing) = self.texture_cache.get(&key) {
            return existing.clone();
        }
        let rgba = self.render_image(config, render_size, render_size, rot_add);
        let color_image = rgba_to_color_image(&rgba);
        let options = if config.tiled {
            TextureOptions {
                wrap_mode: egui::TextureWrapMode::Repeat,
                ..TextureOptions::LINEAR
            }
        } else {
            TextureOptions::LINEAR
        };
        let name = format!(
            "tg_chat_bg_{}_{}",
            self.generation,
            self.texture_cache.len()
        );
        let handle = ctx.load_texture(name, color_image, options);
        self.texture_cache.insert(key, handle.clone());
        self.trim_cache();
        handle
    }

    /// Paints the active chat background for `peer_id` into `rect`.
    ///
    /// This is the primary entry point intended for `crates/tg-app/src/history/`.
    /// Handles aspect-fill cropping (`compute_chat_background_rects`), seamless tiling,
    /// and dark-mode dimming automatically.
    pub fn paint(&mut self, ui: &mut Ui, rect: Rect, peer_id: Option<i64>) {
        let cfg = self.resolve_for_peer(peer_id).clone();
        self.paint_config(ui, rect, &cfg, self.dark_mode);
    }

    /// Paints an explicit [`BackgroundConfig`] into `rect` (used by `BackgroundPreviewBox`
    /// and chat theme previews with independent Day/Night preview mode).
    pub fn paint_config(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        config: &BackgroundConfig,
        dark_mode: bool,
    ) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        // Fast path: solid opaque color with no pattern and no custom image.
        if config.custom_image.is_none()
            && !config.wallpaper.is_pattern()
            && !config.has_svg_pattern()
            && config.wallpaper.background_colors.len() == 1
        {
            let fill = theme_color_to_egui(config.wallpaper.background_colors[0]);
            ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
            return;
        }

        let tex = self.texture_for_config(ui.ctx(), config, DEFAULT_RENDER_SIZE);
        let [tw, th] = tex.size();
        let uv = if config.tiled && config.custom_image.is_none() && config.wallpaper.is_pattern() {
            // Patterned gradient covers the rect cleanly while keeping aspect ratio.
            aspect_fill_uv(rect.width(), rect.height(), tw as f32, th as f32)
        } else if config.tiled && config.custom_image.is_some() {
            Rect::from_min_max(
                Pos2::ZERO,
                Pos2::new(
                    rect.width() / (tw.max(1) as f32),
                    rect.height() / (th.max(1) as f32),
                ),
            )
        } else {
            aspect_fill_uv(rect.width(), rect.height(), tw as f32, th as f32)
        };

        ui.painter().image(tex.id(), rect, uv, Color32::WHITE);

        // Dark-mode dimming overlay for photo wallpapers.
        if dark_mode && config.custom_image.is_some() && config.dark_dimming > 0 {
            let alpha = ((u16::from(config.dark_dimming.min(100)) * 255) / 100) as u8;
            ui.painter().rect_filled(
                rect,
                CornerRadius::ZERO,
                Color32::from_rgba_unmultiplied(0, 0, 0, alpha),
            );
        }
    }

    /// Computes the average background color used to harmonize service pills and scrollbars.
    #[must_use]
    pub fn average_color(&self, peer_id: Option<i64>) -> Color {
        let cfg = self.resolve_for_peer(peer_id);
        if let Some(ref img) = cfg.custom_image {
            count_average_color_image(img)
        } else if !cfg.wallpaper.background_colors.is_empty() {
            count_average_color_slice(&cfg.wallpaper.background_colors)
        } else {
            DEFAULT_BACKGROUND_COLOR
        }
    }

    /// Computes the 6 Telegram adjustable palette colors (`msgServiceBg`, `msgServiceBgSelected`,
    /// `historyScrollBg`, `historyScrollBgOver`, `historyScrollBarBg`, `historyScrollBarBgOver`)
    /// shifted to harmonize with the active background (`ChatBackground::adjustPaletteUsingColor`).
    #[must_use]
    pub fn adjustable_colors(
        &self,
        base: &Palette,
        peer_id: Option<i64>,
    ) -> Vec<(&'static str, Color)> {
        let avg = self.average_color(peer_id);
        ADJUSTABLE_COLOR_KEYS
            .iter()
            .filter_map(|&key| {
                let orig = base.color_by_name(key)?;
                Some((key, theme_adjusted_color(orig, avg)))
            })
            .collect()
    }

    /// Applies [`Self::adjustable_colors`] directly onto `palette`.
    pub fn apply_adjustable_colors(&self, palette: &mut Palette, peer_id: Option<i64>) {
        for (key, adjusted) in self.adjustable_colors(palette, peer_id) {
            palette.set_color(key, adjusted);
        }
    }

    fn trim_cache(&mut self) {
        if self.texture_cache.len() > 24 {
            self.texture_cache.clear();
        }
    }
}

/// Converts an `image::RgbaImage` into an `egui::ColorImage`.
#[must_use]
pub fn rgba_to_color_image(img: &RgbaImage) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw())
}

/// Computes centered aspect-fill UV coordinates using `tg_theme::compute_chat_background_rects`.
#[must_use]
pub fn aspect_fill_uv(fill_w: f32, fill_h: f32, img_w: f32, img_h: f32) -> Rect {
    if fill_w <= 0.0 || fill_h <= 0.0 || img_w <= 0.0 || img_h <= 0.0 {
        return Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    }
    let rects = compute_chat_background_rects(
        fill_w.round().max(1.0) as u32,
        fill_h.round().max(1.0) as u32,
        img_w.round().max(1.0) as u32,
        img_h.round().max(1.0) as u32,
    );
    let (fx, fy, fw, fh) = rects.from;
    let u0 = (fx.max(0) as f32 / img_w).clamp(0.0, 1.0);
    let v0 = (fy.max(0) as f32 / img_h).clamp(0.0, 1.0);
    let u1 = ((fx.max(0) as f32 + fw as f32) / img_w).clamp(0.0, 1.0);
    let v1 = ((fy.max(0) as f32 + fh as f32) / img_h).clamp(0.0, 1.0);
    Rect::from_min_max(Pos2::new(u0, v0), Pos2::new(u1, v1))
}

fn composite_over_black(img: &RgbaImage) -> RgbaImage {
    let mut out = RgbaImage::new(img.width(), img.height());
    for (x, y, p) in img.enumerate_pixels() {
        let a = u16::from(p.0[3]);
        let r = ((u16::from(p.0[0]) * a + 127) / 255) as u8;
        let g = ((u16::from(p.0[1]) * a + 127) / 255) as u8;
        let b = ((u16::from(p.0[2]) * a + 127) / 255) as u8;
        out.put_pixel(x, y, Rgba([r, g, b, 255]));
    }
    out
}

/// Returns a context-scoped shared [`ChatBackground`] handle stored in `egui::Context` temp data.
///
/// This lets `history/` paint the live chat background chosen in the Background Picker or
/// Theme Editor without needing extra constructor wiring:
///
/// ```ignore
/// if let Ok(mut bg) = crate::themes::active_chat_background(ui.ctx()).lock() {
///     bg.paint(ui, canvas_rect, Some(peer_id));
/// }
/// ```
#[must_use]
pub fn active_chat_background(ctx: &Context) -> Arc<Mutex<ChatBackground>> {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_insert_with(Id::unique("tg_app_active_chat_background"), || {
            Arc::new(Mutex::new(ChatBackground::new()))
        })
        .clone()
    })
}

// --- Procedural GPL-safe pattern mask generator ---

/// Generates a seamless tileable black-on-transparent pattern mask of `size x size` pixels.
///
/// Output format matches `tg_theme::compose_pattern_on_gradient`: RGB = `(0, 0, 0)` with
/// smooth anti-aliased alpha in `0..=255`.
#[must_use]
pub fn generate_pattern_mask(kind: PatternKind, size: u32) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(size, size, Rgba([0, 0, 0, 0]));
    if size == 0 || kind == PatternKind::None {
        return img;
    }

    match kind {
        PatternKind::None => {}
        PatternKind::Doodles => render_doodles_pattern(&mut img),
        PatternKind::Waves => render_waves_pattern(&mut img),
        PatternKind::Hexagons => render_hexagons_pattern(&mut img),
        PatternKind::Botanical => render_botanical_pattern(&mut img),
        PatternKind::Constellation => render_constellation_pattern(&mut img),
    }

    img
}

fn stamp_alpha(img: &mut RgbaImage, x: i32, y: i32, alpha: u8) {
    if alpha == 0 {
        return;
    }
    let w = img.width() as i32;
    let h = img.height() as i32;
    let wx = x.rem_euclid(w) as u32;
    let wy = y.rem_euclid(h) as u32;
    let px = img.get_pixel_mut(wx, wy);
    px.0 = [0, 0, 0, px.0[3].max(alpha)];
}

fn draw_ring_aa(
    img: &mut RgbaImage,
    cx: f32,
    cy: f32,
    radius: f32,
    thickness: f32,
    max_alpha: f32,
) {
    let bound = (radius + thickness + 2.0).ceil() as i32;
    let half_t = thickness * 0.5;
    for dy in -bound..=bound {
        for dx in -bound..=bound {
            let d = ((dx as f32).hypot(dy as f32) - radius).abs();
            if d < half_t + 1.0 {
                let cov = (1.0 - (d - half_t).max(0.0)).clamp(0.0, 1.0);
                let a = (cov * max_alpha).round().clamp(0.0, 255.0) as u8;
                stamp_alpha(img, cx.round() as i32 + dx, cy.round() as i32 + dy, a);
            }
        }
    }
}

fn draw_disc_aa(img: &mut RgbaImage, cx: f32, cy: f32, radius: f32, max_alpha: f32) {
    let bound = (radius + 2.0).ceil() as i32;
    for dy in -bound..=bound {
        for dx in -bound..=bound {
            let d = (dx as f32).hypot(dy as f32);
            if d < radius + 1.0 {
                let cov = (radius + 0.5 - d).clamp(0.0, 1.0);
                let a = (cov * max_alpha).round().clamp(0.0, 255.0) as u8;
                stamp_alpha(img, cx.round() as i32 + dx, cy.round() as i32 + dy, a);
            }
        }
    }
}

fn draw_segment_aa(
    img: &mut RgbaImage,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    thickness: f32,
    max_alpha: f32,
) {
    let min_x = x0.min(x1) - thickness - 2.0;
    let max_x = x0.max(x1) + thickness + 2.0;
    let min_y = y0.min(y1) - thickness - 2.0;
    let max_y = y0.max(y1) + thickness + 2.0;
    let vx = x1 - x0;
    let vy = y1 - y0;
    let len_sq = (vx * vx + vy * vy).max(1e-4);
    let half_t = thickness * 0.5;

    for y in (min_y.floor() as i32)..=(max_y.ceil() as i32) {
        for x in (min_x.floor() as i32)..=(max_x.ceil() as i32) {
            let px = x as f32 - x0;
            let py = y as f32 - y0;
            let t = ((px * vx + py * vy) / len_sq).clamp(0.0, 1.0);
            let d = (px - vx * t).hypot(py - vy * t);
            if d < half_t + 1.0 {
                let cov = (1.0 - (d - half_t).max(0.0)).clamp(0.0, 1.0);
                let a = (cov * max_alpha).round().clamp(0.0, 255.0) as u8;
                stamp_alpha(img, x, y, a);
            }
        }
    }
}

fn draw_star_outline(
    img: &mut RgbaImage,
    cx: f32,
    cy: f32,
    outer_r: f32,
    inner_r: f32,
    angle_offset: f32,
) {
    let mut pts = [(0.0f32, 0.0f32); 10];
    for (i, slot) in pts.iter_mut().enumerate() {
        let a = angle_offset + (i as f32) * std::f32::consts::TAU / 10.0;
        let r = if i % 2 == 0 { outer_r } else { inner_r };
        *slot = (cx + a.cos() * r, cy + a.sin() * r);
    }
    for i in 0..10 {
        let (x0, y0) = pts[i];
        let (x1, y1) = pts[(i + 1) % 10];
        draw_segment_aa(img, x0, y0, x1, y1, 2.0, 195.0);
    }
}

fn draw_diamond_outline(img: &mut RgbaImage, cx: f32, cy: f32, rx: f32, ry: f32) {
    let pts = [(cx, cy - ry), (cx + rx, cy), (cx, cy + ry), (cx - rx, cy)];
    for i in 0..4 {
        let (x0, y0) = pts[i];
        let (x1, y1) = pts[(i + 1) % 4];
        draw_segment_aa(img, x0, y0, x1, y1, 2.0, 185.0);
    }
}

fn draw_paper_plane(img: &mut RgbaImage, cx: f32, cy: f32, scale: f32) {
    let p_tip = (cx + 12.0 * scale, cy - 8.0 * scale);
    let p_left = (cx - 12.0 * scale, cy - 1.0 * scale);
    let p_mid = (cx - 2.0 * scale, cy + 3.0 * scale);
    let p_bot = (cx + 4.0 * scale, cy + 10.0 * scale);
    draw_segment_aa(img, p_left.0, p_left.1, p_tip.0, p_tip.1, 2.1, 200.0);
    draw_segment_aa(img, p_tip.0, p_tip.1, p_bot.0, p_bot.1, 2.1, 200.0);
    draw_segment_aa(img, p_left.0, p_left.1, p_mid.0, p_mid.1, 1.9, 190.0);
    draw_segment_aa(img, p_mid.0, p_mid.1, p_tip.0, p_tip.1, 1.7, 175.0);
    draw_segment_aa(img, p_mid.0, p_mid.1, p_bot.0, p_bot.1, 1.9, 190.0);
}

fn draw_chat_bubble(img: &mut RgbaImage, cx: f32, cy: f32, w: f32, h: f32) {
    let hw = w * 0.5;
    let hh = h * 0.5;
    draw_segment_aa(
        img,
        cx - hw + 3.0,
        cy - hh,
        cx + hw - 3.0,
        cy - hh,
        2.0,
        190.0,
    );
    draw_segment_aa(
        img,
        cx + hw,
        cy - hh + 3.0,
        cx + hw,
        cy + hh - 3.0,
        2.0,
        190.0,
    );
    draw_segment_aa(
        img,
        cx - hw + 4.0,
        cy + hh,
        cx + hw - 3.0,
        cy + hh,
        2.0,
        190.0,
    );
    draw_segment_aa(
        img,
        cx - hw,
        cy - hh + 3.0,
        cx - hw,
        cy + hh - 3.0,
        2.0,
        190.0,
    );
    // Small tail
    draw_segment_aa(
        img,
        cx - hw + 2.0,
        cy + hh,
        cx - hw - 3.0,
        cy + hh + 4.0,
        1.8,
        185.0,
    );
    // Two message text lines inside
    draw_segment_aa(
        img,
        cx - hw + 5.0,
        cy - 2.0,
        cx + hw - 5.0,
        cy - 2.0,
        1.5,
        160.0,
    );
    draw_segment_aa(img, cx - hw + 5.0, cy + 3.0, cx + 2.0, cy + 3.0, 1.5, 160.0);
}

fn render_doodles_pattern(img: &mut RgbaImage) {
    let s = img.width() as f32 / 256.0;
    // Balanced tiled arrangement of geometric chat & sky motifs
    draw_star_outline(img, 36.0 * s, 38.0 * s, 14.0 * s, 6.0 * s, -0.3);
    draw_paper_plane(img, 118.0 * s, 44.0 * s, 1.15 * s);
    draw_chat_bubble(img, 206.0 * s, 46.0 * s, 26.0 * s, 18.0 * s);

    draw_diamond_outline(img, 64.0 * s, 114.0 * s, 11.0 * s, 15.0 * s);
    draw_ring_aa(img, 156.0 * s, 116.0 * s, 12.0 * s, 2.2 * s, 190.0);
    draw_disc_aa(img, 156.0 * s, 116.0 * s, 3.0 * s, 175.0);
    draw_star_outline(img, 224.0 * s, 124.0 * s, 11.0 * s, 4.8 * s, 0.25);

    draw_chat_bubble(img, 42.0 * s, 194.0 * s, 24.0 * s, 16.0 * s);
    draw_star_outline(img, 128.0 * s, 196.0 * s, 13.0 * s, 5.5 * s, 0.15);
    draw_paper_plane(img, 204.0 * s, 206.0 * s, 1.0 * s);

    // Subtle sparkle dots filling whitespace
    for &(px, py, r) in &[
        (82.0, 26.0, 2.2),
        (168.0, 28.0, 1.8),
        (20.0, 122.0, 2.0),
        (106.0, 142.0, 2.3),
        (188.0, 154.0, 1.9),
        (92.0, 232.0, 2.2),
        (166.0, 240.0, 1.8),
        (244.0, 74.0, 2.0),
    ] {
        draw_disc_aa(img, px * s, py * s, r * s, 170.0);
    }
}

fn render_waves_pattern(img: &mut RgbaImage) {
    let w = img.width() as f32;
    let h = img.height() as f32;
    let bands = 6;
    for y in 0..img.height() {
        for x in 0..img.width() {
            let u = (x as f32) / w * std::f32::consts::TAU;
            let v = (y as f32) / h;
            let wave = (u * 2.0).sin() * 0.035 + (u + 1.2).cos() * 0.015;
            let shifted = (v + wave).rem_euclid(1.0);
            let band_pos = (shifted * (bands as f32)).fract();
            let dist = (band_pos - 0.5).abs();
            let line = (1.0 - (dist / 0.085)).clamp(0.0, 1.0);
            let a = (line * 185.0).round() as u8;
            if a > 0 {
                img.put_pixel(x, y, Rgba([0, 0, 0, a]));
            }
        }
    }
}

fn render_hexagons_pattern(img: &mut RgbaImage) {
    let size = img.width() as f32;
    let r = size / 8.0;
    let dx = r * 1.5;
    let dy = r * 3.0f32.sqrt();
    let cols = ((size / dx).ceil() as i32) + 2;
    let rows = ((size / dy).ceil() as i32) + 2;

    for col in -1..cols {
        for row in -1..rows {
            let cx = (col as f32) * dx;
            let cy = (row as f32) * dy
                + if col.rem_euclid(2) == 1 {
                    dy * 0.5
                } else {
                    0.0
                };
            let mut pts = [(0.0f32, 0.0f32); 6];
            for (i, slot) in pts.iter_mut().enumerate() {
                let ang = (i as f32) * std::f32::consts::TAU / 6.0;
                *slot = (cx + ang.cos() * (r * 0.88), cy + ang.sin() * (r * 0.88));
            }
            for i in 0..6 {
                let (x0, y0) = pts[i];
                let (x1, y1) = pts[(i + 1) % 6];
                draw_segment_aa(img, x0, y0, x1, y1, 1.8, 175.0);
            }
        }
    }
}

fn render_botanical_pattern(img: &mut RgbaImage) {
    let s = img.width() as f32 / 256.0;
    let leaves: [(f32, f32, f32, f32); 6] = [
        (48.0, 52.0, 0.55, 22.0),
        (168.0, 44.0, -0.65, 20.0),
        (114.0, 128.0, 0.25, 24.0),
        (222.0, 148.0, -0.45, 21.0),
        (58.0, 204.0, -0.35, 22.0),
        (174.0, 214.0, 0.65, 19.0),
    ];
    for (cx, cy, ang, len) in leaves {
        let cx = cx * s;
        let cy = cy * s;
        let len = len * s;
        let cos_a = ang.cos();
        let sin_a = ang.sin();
        // Central stem
        let x0 = cx - cos_a * len;
        let y0 = cy - sin_a * len;
        let x1 = cx + cos_a * len;
        let y1 = cy + sin_a * len;
        draw_segment_aa(img, x0, y0, x1, y1, 1.8 * s, 190.0);
        // Upper and lower curved leaf outlines sampled in 8 segments
        for side in [-1.0f32, 1.0f32] {
            let nx = -sin_a * side;
            let ny = cos_a * side;
            let steps = 8;
            for step in 0..steps {
                let t0 = (step as f32) / (steps as f32);
                let t1 = ((step + 1) as f32) / (steps as f32);
                let bulge0 = (t0 * std::f32::consts::PI).sin() * (len * 0.45);
                let bulge1 = (t1 * std::f32::consts::PI).sin() * (len * 0.45);
                let ax = x0 + (x1 - x0) * t0 + nx * bulge0;
                let ay = y0 + (y1 - y0) * t0 + ny * bulge0;
                let bx = x0 + (x1 - x0) * t1 + nx * bulge1;
                let by = y0 + (y1 - y0) * t1 + ny * bulge1;
                draw_segment_aa(img, ax, ay, bx, by, 1.9 * s, 185.0);
            }
        }
    }
}

fn render_constellation_pattern(img: &mut RgbaImage) {
    let s = img.width() as f32 / 256.0;
    let nodes: &[(f32, f32, f32)] = &[
        (32.0, 36.0, 3.0),
        (88.0, 24.0, 2.2),
        (134.0, 62.0, 3.4),
        (206.0, 34.0, 2.6),
        (238.0, 92.0, 2.4),
        (60.0, 114.0, 2.8),
        (114.0, 148.0, 2.3),
        (182.0, 132.0, 3.2),
        (40.0, 198.0, 2.5),
        (104.0, 224.0, 3.0),
        (172.0, 206.0, 2.4),
        (224.0, 234.0, 2.8),
    ];
    let edges: &[(usize, usize)] = &[
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 4),
        (0, 5),
        (5, 6),
        (2, 7),
        (6, 7),
        (5, 8),
        (8, 9),
        (6, 10),
        (9, 10),
        (10, 11),
    ];
    for &(a, b) in edges {
        let (x0, y0, _) = nodes[a];
        let (x1, y1, _) = nodes[b];
        draw_segment_aa(img, x0 * s, y0 * s, x1 * s, y1 * s, 1.3 * s, 125.0);
    }
    for &(x, y, r) in nodes {
        draw_disc_aa(img, x * s, y * s, r * s, 220.0);
        draw_ring_aa(img, x * s, y * s, (r + 3.5) * s, 1.2 * s, 130.0);
    }
}
