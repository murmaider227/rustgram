//! Cached rasterization of `tg_style::Icon` masks into `egui::TextureHandle`s.
//!
//! Port of icon instance caching from `Telegram/lib_ui/ui/style/style_core_icon.cpp`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::{Color32, ColorImage, Id, Painter, Rect, TextureHandle, TextureOptions, Vec2};
use tg_style::{
    Color as StyleColor, ColorResolver, Icon, IconError, Scale, device_pixel_ratio, scale, st,
};

use crate::widgets::buttons::IconKind;

/// Cache key uniquely identifying a rasterized `tg_style::Icon` texture.
///
/// Keyed by `(icon, scale, dpr, color, palette_generation)`:
/// - When `color` is `Some(rgba)`, the icon was tinted with an explicit color override
///   and `palette_generation` is normalized to `0` so it does not depend on palette state.
/// - When `color` is `None`, layer colors were resolved through a live [`ColorResolver`],
///   so `palette_generation` ensures automatic invalidation on theme/accent changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IconCacheKey {
    /// Static multi-layer icon descriptor from `tg-style`.
    pub icon: Icon,
    /// Logical UI scale percentage (`100%..=300%`).
    pub scale: Scale,
    /// Device pixel ratio (`1`, `2`, `3`, ...).
    pub dpr: i32,
    /// Optional explicit RGBA color override (`None` = resolve layer colors via palette).
    pub color: Option<[u8; 4]>,
    /// Palette generation when `color` is `None` (`0` when `color` is `Some`).
    pub palette_generation: u64,
}

impl IconCacheKey {
    /// Create a cache key for a palette-resolved icon rasterization.
    #[must_use]
    pub const fn for_palette(
        icon: Icon,
        target_scale: Scale,
        dpr: i32,
        palette_generation: u64,
    ) -> Self {
        Self {
            icon,
            scale: target_scale,
            dpr,
            color: None,
            palette_generation,
        }
    }

    /// Create a cache key for an icon rasterized with an explicit color override.
    #[must_use]
    pub fn with_color(icon: Icon, target_scale: Scale, dpr: i32, color: Color32) -> Self {
        Self {
            icon,
            scale: target_scale,
            dpr,
            color: Some(color.to_srgba_unmultiplied()),
            palette_generation: 0,
        }
    }

    /// Create a cache key for optional color override.
    #[must_use]
    pub fn new(
        icon: Icon,
        target_scale: Scale,
        dpr: i32,
        color_override: Option<Color32>,
        palette_generation: u64,
    ) -> Self {
        match color_override {
            Some(c) => Self::with_color(icon, target_scale, dpr, c),
            None => Self::for_palette(icon, target_scale, dpr, palette_generation),
        }
    }
}

/// A cached `egui` GPU texture for a rasterized `tg_style::Icon`.
#[derive(Clone)]
pub struct CachedIconTexture {
    /// Uploaded `egui` texture handle.
    pub texture: TextureHandle,
    /// Logical size in points (`width / dpr`, `height / dpr`).
    pub logical_size: Vec2,
    /// Physical pixel dimensions `[width, height]`.
    pub physical_size: [u32; 2],
}

/// Rasterize a `tg_style::Icon` into an `egui::ColorImage` plus logical point size.
///
/// Works headless without an `egui::Context` (useful for unit tests and offscreen previews).
pub fn rasterize_icon_to_color_image(
    icon: Icon,
    resolver: &dyn ColorResolver,
    target_scale: Scale,
    dpr: i32,
    color_override: Option<Color32>,
) -> Result<(ColorImage, Vec2), IconError> {
    let dpr = dpr.max(1);
    let rgba = match color_override {
        Some(c) => {
            let [r, g, b, a] = c.to_srgba_unmultiplied();
            icon.rasterize_with_color(target_scale, dpr, StyleColor::rgba(r, g, b, a))?
        }
        None => icon.rasterize_at(target_scale, dpr, resolver)?,
    };

    let logical = Vec2::new(rgba.logical_width() as f32, rgba.logical_height() as f32);
    if rgba.width == 0 || rgba.height == 0 {
        return Ok((ColorImage::default(), Vec2::ZERO));
    }

    let img = ColorImage::from_rgba_premultiplied(
        [rgba.width as usize, rgba.height as usize],
        &rgba.pixels,
    );
    Ok((img, logical))
}

/// Texture cache mapping [`IconCacheKey`] -> [`CachedIconTexture`].
///
/// Invalidated automatically when [`Self::invalidate`] or [`Self::set_palette_generation`]
/// is called by [`crate::theme::ThemeManager`].
#[derive(Default)]
pub struct IconTextureCache {
    entries: HashMap<IconCacheKey, CachedIconTexture>,
    palette_generation: u64,
}

impl IconTextureCache {
    /// Create an empty texture cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current palette generation tracked by this cache.
    #[must_use]
    pub const fn palette_generation(&self) -> u64 {
        self.palette_generation
    }

    /// Number of currently cached icon textures.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether an entry for `key` is already cached.
    #[must_use]
    pub fn contains_key(&self, key: &IconCacheKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Drop all cached textures and advance the palette generation counter.
    pub fn invalidate(&mut self) {
        self.entries.clear();
        self.palette_generation = self.palette_generation.wrapping_add(1);
    }

    /// Synchronize with an external palette generation, clearing cached textures if it changed.
    pub fn set_palette_generation(&mut self, generation: u64) {
        if self.palette_generation != generation {
            self.entries.clear();
            self.palette_generation = generation;
        }
    }

    /// Fetch or rasterize and upload `icon` as an `egui::TextureHandle`.
    pub fn get_or_rasterize(
        &mut self,
        ctx: &egui::Context,
        icon: Icon,
        resolver: &dyn ColorResolver,
        target_scale: Scale,
        dpr: i32,
        color_override: Option<Color32>,
    ) -> Result<CachedIconTexture, IconError> {
        let dpr = dpr.max(1);
        let key = IconCacheKey::new(
            icon,
            target_scale,
            dpr,
            color_override,
            self.palette_generation,
        );
        if let Some(existing) = self.entries.get(&key) {
            return Ok(existing.clone());
        }

        let (image, logical_size) =
            rasterize_icon_to_color_image(icon, resolver, target_scale, dpr, color_override)?;
        let physical_size = [image.size[0] as u32, image.size[1] as u32];
        let tex_name = format!(
            "tg_icon_{:p}_{}_{}_{:?}_{}",
            icon.parts.as_ptr(),
            target_scale.percent(),
            dpr,
            key.color,
            key.palette_generation
        );
        let texture = ctx.load_texture(tex_name, image, TextureOptions::LINEAR);
        let cached = CachedIconTexture {
            texture,
            logical_size,
            physical_size,
        };
        self.entries.insert(key, cached.clone());
        Ok(cached)
    }
}

fn context_cache_id() -> Id {
    Id::unique("tg_ui_icon_texture_cache")
}

fn context_generation_id() -> Id {
    Id::unique("tg_ui_palette_generation")
}

/// Obtain or initialize the per-`egui::Context` shared [`IconTextureCache`].
#[must_use]
pub fn context_icon_cache(ctx: &egui::Context) -> Arc<Mutex<IconTextureCache>> {
    let key = context_cache_id();
    ctx.data_mut(|d| {
        if let Some(existing) = d.get_temp::<Arc<Mutex<IconTextureCache>>>(key) {
            existing
        } else {
            let cache = Arc::new(Mutex::new(IconTextureCache::new()));
            d.insert_temp(key, Arc::clone(&cache));
            cache
        }
    })
}

/// Invalidate the `egui::Context`-scoped icon texture cache (called on theme/accent/scale change).
pub fn invalidate_context_icon_cache(ctx: &egui::Context, new_generation: u64) {
    ctx.data_mut(|d| d.insert_temp(context_generation_id(), new_generation));
    let cache = context_icon_cache(ctx);
    if let Ok(mut guard) = cache.lock() {
        guard.set_palette_generation(new_generation);
    }
}

/// Paint a `tg_style::Icon` centered inside `rect` using the `egui::Context` texture cache.
///
/// Returns `true` if the icon was successfully painted, or `false` if empty or rasterization failed.
pub fn paint_style_icon(
    painter: &Painter,
    rect: Rect,
    icon: Icon,
    resolver: &dyn ColorResolver,
    color_override: Option<Color32>,
) -> bool {
    if icon.is_empty() {
        return false;
    }
    let ctx = painter.ctx();
    let target_scale = scale();
    let dpr = (ctx.pixels_per_point().round() as i32)
        .max(device_pixel_ratio())
        .max(1);
    let palette_gen = ctx
        .data_mut(|d| d.get_temp::<u64>(context_generation_id()))
        .unwrap_or(0);

    let cache = context_icon_cache(ctx);
    let cached = match cache.lock() {
        Ok(mut guard) => {
            guard.set_palette_generation(palette_gen);
            guard
                .get_or_rasterize(ctx, icon, resolver, target_scale, dpr, color_override)
                .ok()
        }
        Err(_) => None,
    };

    let Some(cached) = cached else {
        return false;
    };
    if cached.logical_size.x <= 0.0 || cached.logical_size.y <= 0.0 {
        return false;
    }

    // Fit inside `rect` while preserving aspect ratio (if `rect` is larger than logical size,
    // draw at crisp 1:1 logical size centered in `rect`; if smaller, scale down cleanly).
    let draw_size = if cached.logical_size.x <= rect.width() + 0.5
        && cached.logical_size.y <= rect.height() + 0.5
    {
        cached.logical_size
    } else {
        let sx = rect.width() / cached.logical_size.x;
        let sy = rect.height() / cached.logical_size.y;
        cached.logical_size * sx.min(sy)
    };

    let draw_rect = Rect::from_center_size(rect.center(), draw_size);
    let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    painter.image(cached.texture.id(), draw_rect, uv, Color32::WHITE);
    true
}

/// Paint a `tg_style::Icon` using an explicit [`IconTextureCache`].
pub fn paint_style_icon_with_cache(
    ctx: &egui::Context,
    painter: &Painter,
    rect: Rect,
    icon: Icon,
    resolver: &dyn ColorResolver,
    color_override: Option<Color32>,
    cache: &mut IconTextureCache,
) -> bool {
    if icon.is_empty() {
        return false;
    }
    let target_scale = scale();
    let dpr = (ctx.pixels_per_point().round() as i32)
        .max(device_pixel_ratio())
        .max(1);
    let Ok(cached) = cache.get_or_rasterize(ctx, icon, resolver, target_scale, dpr, color_override)
    else {
        return false;
    };
    if cached.logical_size.x <= 0.0 || cached.logical_size.y <= 0.0 {
        return false;
    }
    let draw_size = if cached.logical_size.x <= rect.width() + 0.5
        && cached.logical_size.y <= rect.height() + 0.5
    {
        cached.logical_size
    } else {
        let sx = rect.width() / cached.logical_size.x;
        let sy = rect.height() / cached.logical_size.y;
        cached.logical_size * sx.min(sy)
    };
    let draw_rect = Rect::from_center_size(rect.center(), draw_size);
    let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    painter.image(cached.texture.id(), draw_rect, uv, Color32::WHITE);
    true
}

/// Map a legacy [`IconKind`] to the corresponding real tdesktop [`tg_style::Icon`].
#[must_use]
pub fn style_icon_for_kind(kind: IconKind) -> Option<Icon> {
    match kind {
        IconKind::Close => Some(st::box_title_close_icon),
        IconKind::Back => Some(st::back_button_icon),
        IconKind::Search => Some(st::box_field_search_icon),
        IconKind::MenuDots => Some(st::menu_toggle_icon),
        IconKind::Hamburger => Some(st::dialogs_menu_toggle.icon),
        IconKind::Settings => Some(st::menu_icon_settings),
        IconKind::Send => Some(st::history_send_icon),
        IconKind::Attach => Some(st::history_attach.icon),
        IconKind::Check => Some(st::default_checkbox_icon),
        IconKind::Trash => Some(st::menu_icon_delete),
        IconKind::Edit => Some(st::menu_icon_edit),
        IconKind::Info => Some(st::menu_icon_info),
        IconKind::Bookmark => Some(st::menu_icon_saved_messages),
        IconKind::ChevronRight => Some(st::default_menu_arrow),
        IconKind::Lock => Some(st::menu_icon_lock),
        _ => None,
    }
}
