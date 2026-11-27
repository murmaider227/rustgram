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

pub use tg_theme::PaletteColor;

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

    /// Look up a typed palette colour (`colors.get_at(PaletteColor::WINDOW_BG)`).
    ///
    /// Palettes read the slot directly; adapters that remap names keep this default, which
    /// asks [`Self::get`] with the colour's name, so their overrides still apply.
    fn get_at(&self, color: PaletteColor) -> Option<Color32> {
        self.get(color.name())
    }

    /// Typed counterpart of [`Self::color`]: a misspelt colour is a compile error.
    fn at(&self, color: PaletteColor) -> Color32 {
        self.get_at(color).unwrap_or(Color32::TRANSPARENT)
    }
}

impl<T: Colors + ?Sized> Colors for &T {
    fn get(&self, name: &str) -> Option<Color32> {
        (**self).get(name)
    }

    fn get_at(&self, color: PaletteColor) -> Option<Color32> {
        (**self).get_at(color)
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

    fn get_at(&self, color: PaletteColor) -> Option<Color32> {
        if color == PaletteColor::TRANSPARENT {
            return Some(Color32::TRANSPARENT);
        }
        Some(theme_color_to_egui(Palette::at(self, color)))
    }
}

impl Colors for ThemeInstance {
    fn get(&self, name: &str) -> Option<Color32> {
        self.palette.get(name)
    }

    fn get_at(&self, color: PaletteColor) -> Option<Color32> {
        self.palette.get_at(color)
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

/// A built-in theme with its accent colour, as remembered for day mode or for night mode
/// (tdesktop keeps one saved theme per mode: `Local::` `themeKeyDay` / `themeKeyNight`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedEmbedded {
    /// Built-in theme.
    pub kind: EmbeddedType,
    /// Accent colour override chosen for it, if any.
    pub accent: Option<ThemeColor>,
}

impl SavedEmbedded {
    /// tdesktop's default for a mode with no saved theme (`ChatBackground::reapplyWithNightMode`):
    /// `NightThemePath()` for night, the empty path (Classic) for day.
    #[must_use]
    pub const fn default_for(night: bool) -> Self {
        Self {
            kind: if night {
                EmbeddedType::Night
            } else {
                EmbeddedType::Default
            },
            accent: None,
        }
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
    /// Theme remembered for day mode; the night-mode toggle returns to it.
    saved_day: SavedEmbedded,
    /// Theme remembered for night mode.
    saved_night: SavedEmbedded,
    wallpaper: Option<WallPaper>,
    scale: Scale,
    generation: u64,
    /// Process-unique id of this manager, so values derived from its palette can be cached
    /// by [`Self::palette_cache_key`].
    instance_id: u64,
    icon_cache: IconTextureCache,
}

static NEXT_THEME_MANAGER_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

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
        let mut saved_day = SavedEmbedded::default_for(false);
        let mut saved_night = SavedEmbedded::default_for(true);
        if kind.is_dark() {
            saved_night.kind = kind;
        } else {
            saved_day.kind = kind;
        }
        Self {
            embedded: kind,
            accent: None,
            palette,
            recipe,
            night_mode,
            saved_day,
            saved_night,
            wallpaper: None,
            scale: tg_style::scale(),
            generation: 1,
            instance_id: NEXT_THEME_MANAGER_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
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

    /// Key that changes whenever this manager's palette may have changed: `(instance id,
    /// generation)`. Every palette change goes through [`Self::invalidate_textures`], which
    /// bumps the generation, so a value derived from the palette stays valid while its key does.
    #[must_use]
    pub const fn palette_cache_key(&self) -> (u64, u64) {
        (self.instance_id, self.generation)
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
        self.remember(SavedEmbedded { kind, accent });
        self.invalidate_textures();
    }

    /// The built-in theme remembered for night mode (`night == true`) or for day mode.
    #[must_use]
    pub const fn saved_embedded(&self, night: bool) -> SavedEmbedded {
        if night {
            self.saved_night
        } else {
            self.saved_day
        }
    }

    /// Remembers `saved` for its own mode without applying it (restoring the saved day and
    /// night themes at start-up). The applied theme does not change.
    pub fn remember(&mut self, saved: SavedEmbedded) {
        if saved.kind.is_dark() {
            self.saved_night = saved;
        } else {
            self.saved_day = saved;
        }
    }

    /// Switch accent color on the current embedded theme (`None` resets to base accent)
    /// and invalidate cached textures.
    pub fn set_accent(&mut self, accent: Option<ThemeColor>) {
        self.accent = accent;
        self.remember(SavedEmbedded {
            kind: self.embedded,
            accent,
        });
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

    /// Switch between day and night mode, applying the theme remembered for the target mode
    /// (`ChatBackground::reapplyWithNightMode`: the saved theme of that mode, else Classic for
    /// day and `NightThemePath()` for night). Does nothing when that mode is already active.
    pub fn set_night_mode(&mut self, night: bool) {
        if self.embedded.is_dark() != night || self.night_mode.is_night_mode() != night {
            let saved = self.saved_embedded(night);
            self.set_embedded(saved.kind, saved.accent);
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

    fn get_at(&self, color: PaletteColor) -> Option<Color32> {
        self.palette.get_at(color)
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
    let visuals = palette_visuals(colors);
    // tdesktop's look depends only on its palette, never on the OS light/dark setting. Pin the
    // egui theme to the palette so a later OS theme report (eframe's default preference follows
    // the system) cannot swap in egui's stock style for the other mode.
    let theme = if visuals.dark_mode {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_theme(theme);
    ctx.set_visuals_of(theme, visuals);
}

/// The `egui::Visuals` of a palette (light or dark by the contrast of `windowBg`), without
/// applying them anywhere. [`apply_palette_visuals`] sets them for the whole window; a panel
/// that previews another theme sets them on its own `Ui` only.
#[must_use]
pub fn palette_visuals(colors: &dyn Colors) -> Visuals {
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
    crate::fonts::apply_tdesktop_text_rendering(&mut visuals);
    visuals.panel_fill = window_bg;
    visuals.window_fill = window_bg;
    visuals.extreme_bg_color = input_bg;
    visuals.faint_bg_color = window_bg_over;
    visuals.override_text_color = Some(window_fg);
    // Search / composer / emoji-search placeholders use tdesktop's opaque `placeholderFg`
    // (`st::defaultInputField.placeholderFg`), not a translucent fade of `windowFg`.
    visuals.weak_text_color = colors
        .get("placeholderFg")
        .filter(|c| c.a() > 0)
        .or(Some(with_alpha(window_fg, 0.55)));
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
    visuals
}

#[cfg(test)]
mod palette_theme_tests {
    use super::*;

    fn run_frame(ctx: &egui::Context, system_theme: Option<egui::Theme>) {
        let input = egui::RawInput {
            system_theme,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |_| {});
        output.textures_delta.clear();
    }

    #[test]
    fn os_theme_reported_after_start_up_does_not_replace_the_palette_visuals() {
        for (embedded, dark, os) in [
            (EmbeddedType::Default, false, egui::Theme::Dark),
            (EmbeddedType::Night, true, egui::Theme::Light),
        ] {
            let ctx = egui::Context::default();
            ctx.set_theme(egui::ThemePreference::System);
            // eframe creates the app (which applies the palette) before the first frame
            // reports the OS theme; the OS theme arrives afterwards.
            run_frame(&ctx, None);
            ThemeManager::with_embedded(embedded).apply_to_egui(&ctx);
            run_frame(&ctx, Some(os));
            run_frame(&ctx, Some(os));
            let visuals = ctx.global_style().visuals.clone();
            assert_eq!(visuals.dark_mode, dark, "{embedded:?} with OS {os:?}");
            let window_bg = ThemeManager::with_embedded(embedded).color("windowBg");
            assert_eq!(visuals.panel_fill, window_bg, "{embedded:?} with OS {os:?}");
        }
    }

    /// TGB-2955: the night toggle returns to the theme (and accent) each mode had, as
    /// tdesktop's saved day / night themes do, instead of always Classic / Tinted.
    #[test]
    fn night_toggle_returns_to_the_theme_remembered_for_each_mode() {
        let accent = ThemeColor::rgb(0xd0, 0x40, 0x40);
        let mut tm = ThemeManager::new();
        tm.set_embedded(EmbeddedType::DayBlue, Some(accent));
        tm.set_night_mode(true);
        assert_eq!(tm.embedded_type(), EmbeddedType::Night);
        assert!(tm.is_dark());
        tm.set_embedded(EmbeddedType::NightGreen, None);
        tm.set_night_mode(false);
        assert_eq!(tm.embedded_type(), EmbeddedType::DayBlue);
        assert_eq!(tm.accent(), Some(accent));
        assert!(!tm.is_dark());
        tm.toggle_night_mode();
        assert_eq!(tm.embedded_type(), EmbeddedType::NightGreen);

        // Asking for the mode that is already active changes nothing.
        let generation = tm.generation();
        tm.set_night_mode(true);
        assert_eq!(tm.generation(), generation);
    }

    /// TGB-2955: the dark palette reaches egui on the frame after the switch.
    #[test]
    fn dark_theme_gives_dark_egui_visuals_on_the_next_frame() {
        let ctx = egui::Context::default();
        let mut tm = ThemeManager::new();
        tm.apply_to_egui(&ctx);
        run_frame(&ctx, None);
        assert!(!ctx.global_style().visuals.dark_mode);

        tm.set_night_mode(true);
        tm.apply_to_egui(&ctx);
        run_frame(&ctx, None);
        let visuals = ctx.global_style().visuals.clone();
        assert!(visuals.dark_mode);
        assert_eq!(visuals.panel_fill, tm.color("windowBg"));
    }
}
