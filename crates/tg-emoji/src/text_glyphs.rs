//! Emoji inside egui text, drawn from tdesktop's sheets (TGB-3048).
//!
//! tdesktop turns every emoji sequence in a `Ui::Text::String` into an emoji block
//! (`text_block_parser.cpp`, `BlockParser::parseEmojiFromCurrent`) that is
//! `st::emojiSize + 2 * st::emojiPadding` wide (`text_block.cpp`) and paints it from the emoji
//! sheets with `Ui::Emoji::Draw` at `x + st::emojiPadding`, centred on the font's height
//! (`text_renderer.cpp`, `emojiY = (font->height - st::emojiSize) / 2`).
//!
//! Here the same happens inside egui's own text layout: [`text_emoji_rasterizer`] is a priority
//! [`GlyphRasterizer`] that egui asks about every grapheme cluster before any font. For a cluster
//! made of emoji it returns the sheet pixels as one colour glyph with tdesktop's advance, so every
//! galley (message text, chat-list previews, names, the compose field) lays the emoji out, wraps,
//! selects and copies it like a character while drawing tdesktop's image. egui keeps one glyph per
//! `char` (the emoji's first char carries the image, the rest are zero-width), caches the result
//! per (cluster, family, size, pixels per point) and stores the pixels in its font atlas, so text
//! with emoji costs nothing extra per frame.
//!
//! The rasterizer callback is not told the pixels per point, which the emoji size needs (tdesktop's
//! emoji does not grow with the font). [`EmojiTextPlugin`] records it at the start of every pass of
//! every viewport on the UI thread, and [`set_text_pixels_per_point`] lets code that lays text out
//! outside a pass do the same.

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use egui::epaint::text::{
    FontPriority, GlyphBitmap, GlyphRasterizer, GlyphRasterizerRequest, RasterizedGlyph,
};
use egui::{Color32, ColorImage, Context, Vec2};
use tg_text::emoji::{self, EmojiRef};

use crate::atlas::{EmojiAtlas, EmojiTileImage};
use crate::resample::area_downscale;

/// [`GlyphRasterizer::key`] of [`text_emoji_rasterizer`].
pub const TEXT_EMOJI_RASTERIZER_KEY: &str = "tg_emoji::text_glyphs";

/// `st::emojiSize` at 100% (`lib_ui/ui/basic.style`).
pub const EMOJI_SIZE_PT: f32 = 18.0;
/// `st::emojiPadding` at 100% (`lib_ui/ui/basic.style`).
pub const EMOJI_PADDING_PT: f32 = 1.0;

/// Open Sans' Windows ascent and descent (`OS/2` `usWinAscent` / `usWinDescent`, 2048 units per
/// em): the font box Qt's GDI engine gives tdesktop, which centres the emoji on it.
const FONT_ASCENT_PER_EM: f32 = 2302.0 / 2048.0;
const FONT_DESCENT_PER_EM: f32 = 651.0 / 2048.0;

/// Emoji per cluster we draw as one glyph (a ZWJ sequence tdesktop does not know is drawn as its
/// parts, like tdesktop's parser does; longer clusters are left to the fonts).
const MAX_EMOJI_PER_CLUSTER: usize = 8;

/// Scaled emoji metrics in points: `(st::emojiSize, st::emojiPadding)` at the interface scale.
pub type EmojiMetricsFn = dyn Fn() -> (f32, f32) + Send + Sync;

thread_local! {
    static PIXELS_PER_POINT: Cell<f32> = const { Cell::new(1.0) };
}

/// Records the pixels per point of the text about to be laid out on this thread.
pub fn set_text_pixels_per_point(pixels_per_point: f32) {
    if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        PIXELS_PER_POINT.with(|ppp| ppp.set(pixels_per_point));
    }
}

/// The pixels per point last recorded on this thread (`1.0` until something records one).
#[must_use]
pub fn text_pixels_per_point() -> f32 {
    PIXELS_PER_POINT.with(Cell::get)
}

/// Keeps [`set_text_pixels_per_point`] in step with each viewport pass.
#[derive(Debug, Default)]
pub struct EmojiTextPlugin;

impl egui::Plugin for EmojiTextPlugin {
    fn debug_name(&self) -> &'static str {
        "tg_emoji::EmojiTextPlugin"
    }

    fn setup(&mut self, ctx: &Context) {
        set_text_pixels_per_point(ctx.pixels_per_point());
    }

    fn on_begin_pass(&mut self, ui: &mut egui::Ui) {
        set_text_pixels_per_point(ui.ctx().pixels_per_point());
    }
}

/// Installs [`text_emoji_rasterizer`] and [`EmojiTextPlugin`] on `ctx` (idempotent).
///
/// `metrics` returns `(st::emojiSize, st::emojiPadding)` in points at the current interface scale.
pub fn install_text_emoji(ctx: &Context, metrics: Arc<EmojiMetricsFn>) {
    set_text_pixels_per_point(ctx.pixels_per_point());
    ctx.add_plugin(EmojiTextPlugin);
    ctx.add_glyph_rasterizer(text_emoji_rasterizer(metrics));
}

/// Drops every emoji glyph egui has cached (after the interface scale changed the emoji size).
pub fn reset_text_emoji(ctx: &Context, metrics: Arc<EmojiMetricsFn>) {
    set_text_pixels_per_point(ctx.pixels_per_point());
    ctx.add_plugin(EmojiTextPlugin);
    ctx.set_glyph_rasterizers(vec![text_emoji_rasterizer(metrics)]);
}

/// The priority glyph rasterizer that draws emoji clusters from tdesktop's sheets.
#[must_use]
pub fn text_emoji_rasterizer(metrics: Arc<EmojiMetricsFn>) -> GlyphRasterizer {
    GlyphRasterizer::new(TEXT_EMOJI_RASTERIZER_KEY, move |request| {
        let (size_pt, padding_pt) = metrics();
        rasterize_emoji_cluster(request, size_pt, padding_pt, text_pixels_per_point())
    })
    .with_priority(FontPriority::Highest)
}

/// The emoji a grapheme cluster consists of, or `None` when it holds anything else.
///
/// Mirrors tdesktop's parser: the longest known emoji at each position
/// (`Ui::Emoji::Find`); joiners, variation selectors and tag characters between them draw nothing.
#[must_use]
pub fn cluster_emoji(cluster: &str) -> Option<Vec<EmojiRef>> {
    if cluster.is_ascii() {
        return None;
    }
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < cluster.len() {
        if let Some((found, len)) = emoji::find_emoji(cluster, pos)
            && len > 0
        {
            if out.len() == MAX_EMOJI_PER_CLUSTER {
                return None;
            }
            out.push(found);
            pos += len;
            continue;
        }
        let ch = cluster[pos..].chars().next()?;
        if !is_invisible_in_emoji(ch) {
            return None;
        }
        pos += ch.len_utf8();
    }
    (!out.is_empty()).then_some(out)
}

fn is_invisible_in_emoji(ch: char) -> bool {
    matches!(
        ch,
        '\u{200D}' | '\u{FE0E}' | '\u{FE0F}' | '\u{E0020}'..='\u{E007F}'
    )
}

/// Physical-pixel geometry of emoji glyphs for one font size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmojiGlyphMetrics {
    /// Emoji image side.
    pub size_px: u32,
    /// Space left and right of each emoji.
    pub padding_px: u32,
    /// Image top relative to the baseline (negative: above it).
    pub top_px: i32,
}

impl EmojiGlyphMetrics {
    /// Metrics for emoji `size_pt` / `padding_pt` points in a font of `font_size_px` pixels.
    #[must_use]
    pub fn new(size_pt: f32, padding_pt: f32, pixels_per_point: f32, font_size_px: f32) -> Self {
        let size_px = (size_pt * pixels_per_point).round().max(1.0) as u32;
        // tdesktop's `style::ConvertScale` keeps a non-zero length at least 1 px.
        let padding_px = if padding_pt > 0.0 {
            (padding_pt * pixels_per_point).round().max(1.0) as u32
        } else {
            0
        };
        let box_centre = 0.5 * (FONT_ASCENT_PER_EM - FONT_DESCENT_PER_EM) * font_size_px;
        let top_px = -(box_centre + 0.5 * size_px as f32).round() as i32;
        Self {
            size_px,
            padding_px,
            top_px,
        }
    }

    /// Horizontal advance of one emoji (`st::emojiSize + 2 * st::emojiPadding`).
    #[must_use]
    pub const fn advance_px(self) -> u32 {
        self.size_px + 2 * self.padding_px
    }
}

/// Rasterizes `request.cluster` if it is made of emoji with sprites in the sheets.
#[must_use]
pub fn rasterize_emoji_cluster(
    request: &GlyphRasterizerRequest<'_>,
    size_pt: f32,
    padding_pt: f32,
    pixels_per_point: f32,
) -> Option<RasterizedGlyph> {
    let emoji = cluster_emoji(request.cluster)?;
    let metrics =
        EmojiGlyphMetrics::new(size_pt, padding_pt, pixels_per_point, request.font_size_px);
    let side = metrics.size_px as usize;
    let advance = metrics.advance_px() as usize;
    let count = emoji.len();
    let width = count * advance - 2 * metrics.padding_px as usize;
    let mut image = ColorImage::filled([width, side], Color32::TRANSPARENT);
    for (i, emoji_ref) in emoji.into_iter().enumerate() {
        let pixels = scaled_emoji_pixels(emoji_ref, metrics.size_px)?;
        let left = i * advance;
        for y in 0..side {
            let row = &pixels[y * side..(y + 1) * side];
            let start = y * width + left;
            image.pixels[start..start + side].copy_from_slice(row);
        }
    }
    Some(RasterizedGlyph {
        bitmap: GlyphBitmap {
            image,
            offset_px: Vec2::new(metrics.padding_px as f32, metrics.top_px as f32),
            is_color: true,
        },
        advance_px: (count * advance) as f32,
    })
}

/// Decoded tiles kept for the next few emoji (one sheet row each, 512 KiB).
const KEPT_TILES: usize = 4;
/// Scaled emoji images kept; when full, the least recently used quarter is dropped.
const KEPT_IMAGES: usize = 4096;

/// Key of a scaled image: the emoji's index in the sheets and the image side in pixels.
type ImageKey = (usize, u32);

/// Scaled emoji images with least-recently-used eviction (TGB-3102).
///
/// Before, a full cache was cleared at once, so the next frame missed on every glyph and rescaled
/// them all. Now only the quarter used longest ago goes, so the emoji on screen stay.
pub(crate) struct ScaledImages {
    images: HashMap<ImageKey, (Arc<[Color32]>, u64)>,
    clock: u64,
    capacity: usize,
}

impl ScaledImages {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            images: HashMap::new(),
            clock: 0,
            capacity: capacity.max(1),
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.images.len()
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// The image under `key`, marked as just used.
    pub(crate) fn get(&mut self, key: ImageKey) -> Option<Arc<[Color32]>> {
        let now = self.tick();
        let (pixels, used) = self.images.get_mut(&key)?;
        *used = now;
        Some(Arc::clone(pixels))
    }

    /// Stores `pixels` under `key`, first dropping the least recently used quarter when full.
    pub(crate) fn insert(&mut self, key: ImageKey, pixels: Arc<[Color32]>) {
        if !self.images.contains_key(&key) && self.images.len() >= self.capacity {
            self.evict_least_recent(self.capacity.div_ceil(4));
        }
        let now = self.tick();
        self.images.insert(key, (pixels, now));
    }

    fn evict_least_recent(&mut self, count: usize) {
        let mut ages: Vec<u64> = self.images.values().map(|(_, used)| *used).collect();
        let Some(nth) = count.checked_sub(1).filter(|n| *n < ages.len()) else {
            self.images.clear();
            return;
        };
        let (_, oldest_kept, _) = ages.select_nth_unstable(nth);
        let cutoff = *oldest_kept;
        // Use ticks are unique, so exactly `count` entries are at or below the cutoff.
        self.images.retain(|_, (_, used)| *used > cutoff);
    }
}

pub(crate) struct PixelCache {
    tiles: VecDeque<(usize, Arc<EmojiTileImage>)>,
    images: ScaledImages,
}

impl Default for PixelCache {
    fn default() -> Self {
        Self {
            tiles: VecDeque::new(),
            images: ScaledImages::with_capacity(KEPT_IMAGES),
        }
    }
}

impl PixelCache {
    /// A kept decoded tile, moved to the front.
    fn tile(&mut self, index: usize) -> Option<Arc<EmojiTileImage>> {
        let at = self.tiles.iter().position(|(t, _)| *t == index)?;
        let kept = self.tiles.remove(at)?;
        let tile = Arc::clone(&kept.1);
        self.tiles.push_front(kept);
        Some(tile)
    }

    fn keep_tile(&mut self, index: usize, tile: Arc<EmojiTileImage>) {
        if self.tiles.iter().any(|(t, _)| *t == index) {
            return;
        }
        self.tiles.push_front((index, tile));
        self.tiles.truncate(KEPT_TILES);
    }
}

fn pixel_cache() -> &'static Mutex<PixelCache> {
    static CACHE: OnceLock<Mutex<PixelCache>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

fn lock(cache: &Mutex<PixelCache>) -> std::sync::MutexGuard<'_, PixelCache> {
    cache.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The sheet image of `emoji_ref` shrunk to `side` x `side` premultiplied pixels.
fn scaled_emoji_pixels(emoji_ref: EmojiRef, side: u32) -> Option<Arc<[Color32]>> {
    let atlas = EmojiAtlas::global();
    let sprite = atlas.sprite(emoji_ref)?;
    cached_scaled_pixels(
        pixel_cache(),
        (sprite.emoji_index, side),
        sprite.tile,
        || atlas.decode_tile_transient(sprite.tile).ok(),
        |tile| scale_cell(tile, sprite.pixel_rect, side),
    )
}

/// Looks `key` up in `cache`; on a miss decodes the tile (unless kept) and scales the cell with
/// the lock released, so a layout on another thread is not held up by a ~1 ms tile decode.
pub(crate) fn cached_scaled_pixels(
    cache: &Mutex<PixelCache>,
    key: ImageKey,
    tile_index: usize,
    decode_tile: impl FnOnce() -> Option<Arc<EmojiTileImage>>,
    scale: impl FnOnce(&EmojiTileImage) -> Option<Arc<[Color32]>>,
) -> Option<Arc<[Color32]>> {
    let kept_tile = {
        let mut cache = lock(cache);
        if let Some(found) = cache.images.get(key) {
            return Some(found);
        }
        cache.tile(tile_index)
    };
    let tile = match kept_tile {
        Some(tile) => tile,
        None => {
            let decoded = decode_tile()?;
            lock(cache).keep_tile(tile_index, Arc::clone(&decoded));
            decoded
        }
    };
    let pixels = scale(&tile)?;
    lock(cache).images.insert(key, Arc::clone(&pixels));
    Some(pixels)
}

/// The `pixel_rect` cell of `tile` shrunk to `side` x `side` premultiplied pixels.
fn scale_cell(tile: &EmojiTileImage, pixel_rect: [u32; 4], side: u32) -> Option<Arc<[Color32]>> {
    let [x, y, w, h] = pixel_rect.map(|v| v as usize);
    let stride = tile.width as usize * 4;
    let mut cell = Vec::with_capacity(w * h * 4);
    for row in y..y + h {
        let start = row * stride + x * 4;
        cell.extend_from_slice(tile.rgba.get(start..start + w * 4)?);
    }
    let side = side as usize;
    let scaled = area_downscale(&cell, w, h, side, side);
    Some(
        ColorImage::from_rgba_unmultiplied([side, side], &scaled)
            .pixels
            .into(),
    )
}
