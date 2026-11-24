//! Live theme manager, `tg-theme` palette integration, and `egui` visuals synchronization.
//!
//! Replaces the old hardcoded `stand_in::DayPalette` table with real `tg-theme` palettes
//! (Classic/Default, DayBlue, Night, NightGreen, accent colorizer, custom `.tdesktop-theme`,
//! and wallpapers) while implementing [`Colors`] and [`tg_style::ColorResolver`].

use std::collections::HashMap;

use egui::{Color32, Visuals};
use tg_style::{Color as StyleColor, ColorResolver, Scale, set_scale};
use tg_theme::{
    Color as ThemeColor, Colorizer, EmbeddedAccentRecipe, EmbeddedScheme, EmbeddedType,
    NightModeState, Palette, ThemeError, ThemeInstance, WallPaper, default_accent_colors,
    embedded_scheme, embedded_themes, load_theme_from_bytes, prepare_embedded_palette,
};

use crate::anim::with_alpha;
use crate::fonts::ensure_tdesktop_fonts;
use crate::icons::{CachedIconTexture, IconTextureCache, invalidate_context_icon_cache};

/// Palette color lookup trait keyed by tdesktop palette variable name
/// (for example `"windowBg"`, `"activeButtonBg"`, `"historyPeer1UserpicBg"`).
pub trait Colors {
    /// Look up a color by palette name, returning `None` if unknown.
    fn get(&self, name: &str) -> Option<Color32>;

    /// Look up a color by palette name, falling back to transparent black if unknown.
    fn color(&self, name: &str) -> Color32 {
        self.get(name).unwrap_or(Color32::TRANSPARENT)
    }
}

impl<T: Colors + ?Sized> Colors for &T {
    fn get(&self, name: &str) -> Option<Color32> {
        (**self).get(name)
    }
}

/// Convert a `tg_theme::Color` into an `egui::Color32`.
#[inline]
#[must_use]
pub fn theme_color_to_egui(c: ThemeColor) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

/// Convert an `egui::Color32` into a `tg_theme::Color`.
#[inline]
#[must_use]
pub fn egui_color_to_theme(c: Color32) -> ThemeColor {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    ThemeColor::rgba(r, g, b, a)
}

/// Convert a `tg_style::Color` into an `egui::Color32`.
#[inline]
#[must_use]
pub fn style_color_to_egui(c: StyleColor) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

/// Convert an `egui::Color32` into a `tg_style::Color`.
#[inline]
#[must_use]
pub fn egui_color_to_style(c: Color32) -> StyleColor {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    StyleColor::rgba(r, g, b, a)
}

/// Parse a tdesktop hex color (`#RRGGBB` or `#RRGGBBAA`, with or without `#`).
#[must_use]
pub fn parse_hex_color(hex: &str) -> Option<Color32> {
    ThemeColor::from_hex(hex.trim()).map(theme_color_to_egui)
}

impl Colors for Palette {
    fn get(&self, name: &str) -> Option<Color32> {
        match name {
            "transparent" => Some(Color32::TRANSPARENT),
            "white" => Some(theme_color_to_egui(self.white)),
            _ => self.color_by_name(name).map(theme_color_to_egui),
        }
    }
}

impl Colors for ThemeInstance {
    fn get(&self, name: &str) -> Option<Color32> {
        self.palette.get(name)
    }
}

/// Adapter letting any [`Colors`] implementation act as a [`tg_style::ColorResolver`].
pub struct ColorsResolver<'a>(pub &'a dyn Colors);

impl ColorResolver for ColorsResolver<'_> {
    fn resolve(&self, name: &str) -> Option<StyleColor> {
        self.0.get(name).map(egui_color_to_style)
    }
}

/// Resolve a `tg_style::ColorRef` against any [`Colors`] palette into an `egui::Color32`.
#[inline]
#[must_use]
pub fn resolve_color_ref(color_ref: tg_style::ColorRef, colors: &dyn Colors) -> Color32 {
    let resolver = ColorsResolver(colors);
    style_color_to_egui(color_ref.resolve(&resolver))
}

/// Default Day theme palette backed by `tg_theme::Palette::default()`, with optional per-key
/// overrides for tests and previews.
///
/// Retained for source compatibility with callers previously using `stand_in::DayPalette`.
/// Prefer [`ThemeManager`] or `tg_theme::Palette` directly in new code.
#[derive(Debug, Clone)]
pub struct DayPalette {
    palette: Palette,
    overrides: HashMap<String, Color32>,
}

impl Default for DayPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl DayPalette {
    /// Create a default Day theme palette backed by `tg_theme::Palette::default()`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            palette: Palette::default(),
            overrides: HashMap::new(),
        }
    }

    /// Construct from an existing `tg_theme::Palette`.
    #[must_use]
    pub fn from_palette(palette: Palette) -> Self {
        Self {
            palette,
            overrides: HashMap::new(),
        }
    }

    /// Access the underlying `tg_theme::Palette`.
    #[must_use]
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Override a single palette entry (useful for previews and tests).
    #[must_use]
    pub fn with_override(mut self, name: impl Into<String>, color: Color32) -> Self {
        self.overrides.insert(name.into(), color);
        self
    }
}

impl Colors for DayPalette {
    fn get(&self, name: &str) -> Option<Color32> {
        if let Some(&c) = self.overrides.get(name) {
            return Some(c);
        }
        self.palette.get(name)
    }
}

impl ColorResolver for DayPalette {
    fn resolve(&self, name: &str) -> Option<StyleColor> {
        if let Some(&c) = self.overrides.get(name) {
            return Some(egui_color_to_style(c));
        }
        self.palette.resolve(name)
    }
}

/// Runtime theme manager for switching embedded themes, night mode, accent colors,
/// custom `.tdesktop-theme` palettes, wallpapers, and DPI scale, while invalidating
/// cached icon textures whenever visual state changes.
pub struct ThemeManager {
    embedded: EmbeddedType,
    accent: Option<ThemeColor>,
    palette: Palette,
    recipe: Option<EmbeddedAccentRecipe>,
    night_mode: NightModeState,
    wallpaper: Option<WallPaper>,
    scale: Scale,
    generation: u64,
    icon_cache: IconTextureCache,
}

impl Default for ThemeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemeManager {
    /// Create a theme manager initialized to Telegram Desktop's Classic (Default Day) theme.
    #[must_use]
    pub fn new() -> Self {
        Self::with_embedded(EmbeddedType::Default)
    }

    /// Create a theme manager initialized to a specific built-in embedded theme.
    #[must_use]
    pub fn with_embedded(kind: EmbeddedType) -> Self {
        let palette = prepare_embedded_palette(kind, None).unwrap_or_default();
        let recipe = EmbeddedAccentRecipe::new(kind).ok();
        let mut night_mode = NightModeState::default();
        night_mode.set_night_mode(kind.is_dark());
        Self {
            embedded: kind,
            accent: None,
            palette,
            recipe,
            night_mode,
            wallpaper: None,
            scale: tg_style::scale(),
            generation: 1,
            icon_cache: IconTextureCache::new(),
        }
    }

    /// Currently selected built-in theme identifier.
    #[must_use]
    pub const fn embedded_type(&self) -> EmbeddedType {
        self.embedded
    }

    /// Metadata scheme for the currently selected built-in theme.
    #[must_use]
    pub fn embedded_scheme(&self) -> EmbeddedScheme {
        embedded_scheme(self.embedded)
    }

    /// List all 4 built-in Telegram Desktop themes (Classic, Day, Tinted, Night).
    #[must_use]
    pub fn available_embedded_themes() -> Vec<EmbeddedScheme> {
        embedded_themes()
    }

    /// Return the 8 preset accent colors for the currently selected embedded theme.
    #[must_use]
    pub fn preset_accent_colors(&self) -> Vec<ThemeColor> {
        default_accent_colors(self.embedded)
    }

    /// Active custom accent override, if any.
    #[must_use]
    pub const fn accent(&self) -> Option<ThemeColor> {
        self.accent
    }

    /// Live `tg_theme::Palette`.
    #[must_use]
    pub const fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Night-mode state machine.
    #[must_use]
    pub const fn night_mode_state(&self) -> &NightModeState {
        &self.night_mode
    }

    /// Mutable access to the night-mode state machine.
    pub fn night_mode_state_mut(&mut self) -> &mut NightModeState {
        &mut self.night_mode
    }

    /// Active chat wallpaper metadata, if set.
    #[must_use]
    pub const fn wallpaper(&self) -> Option<&WallPaper> {
        self.wallpaper.as_ref()
    }

    /// Set the active chat wallpaper.
    pub fn set_wallpaper(&mut self, wallpaper: Option<WallPaper>) {
        self.wallpaper = wallpaper;
    }

    /// Whether the active palette is dark (`Palette::is_dark_palette`).
    #[must_use]
    pub fn is_dark(&self) -> bool {
        self.palette.is_dark_palette()
    }

    /// Current UI DPI scale.
    #[must_use]
    pub const fn scale(&self) -> Scale {
        self.scale
    }

    /// Monotonically increasing generation counter incremented on every theme/accent/scale change.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Shared read access to the managed icon texture cache.
    #[must_use]
    pub const fn icon_cache(&self) -> &IconTextureCache {
        &self.icon_cache
    }

    /// Mutable access to the managed icon texture cache.
    pub fn icon_cache_mut(&mut self) -> &mut IconTextureCache {
        &mut self.icon_cache
    }

    /// Switch to a built-in embedded theme (`Default`, `DayBlue`, `Night`, `NightGreen`),
    /// optionally keeping or replacing the accent color, and invalidate cached textures.
    pub fn set_embedded(&mut self, kind: EmbeddedType, accent: Option<ThemeColor>) {
        let palette = prepare_embedded_palette(kind, accent).unwrap_or_default();
        self.embedded = kind;
        self.accent = accent;
        self.palette = palette;
        self.recipe = EmbeddedAccentRecipe::new(kind).ok();
        self.night_mode.set_night_mode(kind.is_dark());
        self.invalidate_textures();
    }

    /// Switch accent color on the current embedded theme (`None` resets to base accent)
    /// and invalidate cached textures.
    pub fn set_accent(&mut self, accent: Option<ThemeColor>) {
        self.accent = accent;
        match accent {
            Some(color) => {
                if let Some(recipe) = &self.recipe {
                    recipe.apply(&mut self.palette, color);
                } else if let Ok(p) = prepare_embedded_palette(self.embedded, Some(color)) {
                    self.palette = p;
                }
            }
            None => {
                if let Ok(p) = prepare_embedded_palette(self.embedded, None) {
                    self.palette = p;
                }
            }
        }
        self.invalidate_textures();
    }

    /// Switch between day (`EmbeddedType::Default`) and night (`EmbeddedType::Night`) themes.
    pub fn set_night_mode(&mut self, night: bool) {
        let target = if night {
            if self.embedded.is_dark() {
                self.embedded
            } else {
                EmbeddedType::Night
            }
        } else if !self.embedded.is_dark() {
            self.embedded
        } else {
            EmbeddedType::Default
        };
        if target != self.embedded || self.night_mode.is_night_mode() != night {
            self.set_embedded(target, None);
        }
    }

    /// Toggle between day and night mode.
    pub fn toggle_night_mode(&mut self) {
        let next = !self.is_dark();
        self.set_night_mode(next);
    }

    /// Load a custom `.tdesktop-theme` or `.tdesktop-palette` from raw bytes and invalidate textures.
    pub fn load_custom_theme_bytes(&mut self, bytes: &[u8]) -> Result<(), ThemeError> {
        let instance = load_theme_from_bytes(bytes, &Colorizer::default(), None, false)?;
        self.palette = instance.palette;
        self.recipe = None;
        self.accent = None;
        self.night_mode
            .set_night_mode(self.palette.is_dark_palette());
        self.invalidate_textures();
        Ok(())
    }

    /// Replace the live palette directly and invalidate cached textures.
    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
        self.recipe = None;
        self.night_mode
            .set_night_mode(self.palette.is_dark_palette());
        self.invalidate_textures();
    }

    /// Update the active DPI scale (`tg_style::set_scale`) and invalidate cached textures.
    pub fn set_scale(&mut self, new_scale: Scale) {
        if self.scale != new_scale {
            self.scale = new_scale;
            set_scale(new_scale);
            self.invalidate_textures();
        }
    }

    /// Invalidate all cached icon textures and bump [`Self::generation`].
    pub fn invalidate_textures(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.icon_cache.set_palette_generation(self.generation);
    }

    /// Rasterize or fetch a cached `egui` texture for `icon` using the active palette and scale.
    pub fn icon_texture(
        &mut self,
        ctx: &egui::Context,
        icon: tg_style::Icon,
        color_override: Option<Color32>,
    ) -> Result<CachedIconTexture, tg_style::IconError> {
        let dpr = (ctx.pixels_per_point().round() as i32)
            .max(tg_style::device_pixel_ratio())
            .max(1);
        self.icon_cache.set_palette_generation(self.generation);
        self.icon_cache
            .get_or_rasterize(ctx, icon, &self.palette, self.scale, dpr, color_override)
    }

    /// Synchronize fonts, `egui::Visuals`, and context icon cache invalidation with `ctx`.
    pub fn apply_to_egui(&self, ctx: &egui::Context) {
        ensure_tdesktop_fonts(ctx);
        invalidate_context_icon_cache(ctx, self.generation);
        apply_palette_visuals(ctx, &self.palette);
    }
}

impl Colors for ThemeManager {
    fn get(&self, name: &str) -> Option<Color32> {
        self.palette.get(name)
    }
}

impl ColorResolver for ThemeManager {
    fn resolve(&self, name: &str) -> Option<StyleColor> {
        self.palette.resolve(name)
    }
}

/// Apply visuals derived from any live [`Colors`] palette (auto-detecting light vs dark mode
/// from `windowBg` luminance) and register tdesktop's bundled fonts.
pub fn apply_palette_visuals(ctx: &egui::Context, colors: &dyn Colors) {
    ensure_tdesktop_fonts(ctx);

    let window_bg = colors.color("windowBg");
    let window_bg_over = colors.color("windowBgOver");
    let window_fg = colors.color("windowFg");
    let active_bg = colors.color("windowBgActive");
    let input_bg = colors.color("filterInputInactiveBg");
    let divider = colors.color("shadowFg");
    let border_active = colors.color("filterInputBorderFg");

    let bg_theme = egui_color_to_theme(window_bg);
    let is_dark = tg_theme::count_contrast(bg_theme, ThemeColor::BLACK)
        < tg_theme::count_contrast(bg_theme, ThemeColor::WHITE);

    let mut visuals = if is_dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    visuals.panel_fill = window_bg;
    visuals.window_fill = window_bg;
    visuals.extreme_bg_color = input_bg;
    visuals.faint_bg_color = window_bg_over;
    visuals.override_text_color = Some(window_fg);
    visuals.selection.bg_fill = with_alpha(active_bg, 0.25);
    visuals.selection.stroke = egui::Stroke::new(1.5, active_bg);
    visuals.widgets.noninteractive.bg_fill = window_bg;
    visuals.widgets.noninteractive.bg_stroke.color = divider;
    visuals.widgets.inactive.bg_fill = input_bg;
    visuals.widgets.inactive.bg_stroke.color = divider;
    visuals.widgets.hovered.bg_fill = window_bg_over;
    visuals.widgets.hovered.bg_stroke.color = border_active;
    visuals.widgets.active.bg_fill = active_bg;
    visuals.widgets.active.bg_stroke.color = active_bg;
    ctx.set_visuals(visuals);
}
