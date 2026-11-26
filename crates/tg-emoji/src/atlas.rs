//! Packed Noto Emoji sprite sheet atlas indexed by `tg-text`'s `EmojiRef` table.
//!
//! Port origin: `Telegram/lib_ui/ui/emoji_config.cpp` (`Ui::Emoji::One::sprite/row/column`
//! and sprite sheet loading), using Apache-2.0 Noto Emoji (`googlefonts/noto-emoji` v2.042)
//! instead of Apple's proprietary emoji artwork.
//!
//! The emoji keep their sheet/row/column indexing, but the binary embeds each sheet row as its own
//! tile (`build.rs` splits `assets/sheet_*.webp`), so drawing an emoji for the first time decodes
//! one 2048x64 row of 32 emoji (about 1 ms) instead of a whole 2048x1024 sheet.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tg_text::emoji::{self, EmojiRef};
use thiserror::Error;

/// Number of sprite sheets in the checked-in Noto Emoji atlas (`ceil(full_count / 512)`).
pub const SHEET_COUNT: usize = 8;
/// Pixel width and height of each emoji cell inside a sprite sheet.
pub const CELL_SIZE_PX: u32 = 64;
/// Number of emoji columns per sprite sheet (matches `EmojiRef::column() = idx & 0x1F`).
pub const SHEET_COLS: usize = 32;
/// Number of emoji rows per sprite sheet (matches `EmojiRef::row() = (idx >> 5) & 0x0F`).
pub const SHEET_ROWS: usize = 16;
/// Maximum number of emojis stored in a single sprite sheet (`32 * 16 = 512`).
pub const EMOJIS_PER_SHEET: usize = SHEET_COLS * SHEET_ROWS;
/// Sprite sheet pixel width (`32 * 64 = 2048`).
pub const SHEET_WIDTH_PX: u32 = (SHEET_COLS as u32) * CELL_SIZE_PX;
/// Sprite sheet pixel height (`16 * 64 = 1024`).
pub const SHEET_HEIGHT_PX: u32 = (SHEET_ROWS as u32) * CELL_SIZE_PX;

/// Number of embedded tiles: one per sheet row (`8 * 16 = 128`), indexed `emoji_index >> 5`.
pub const TILE_COUNT: usize = SHEET_COUNT * SHEET_ROWS;
/// Tile pixel width: a full sheet row (`2048`).
pub const TILE_WIDTH_PX: u32 = SHEET_WIDTH_PX;
/// Tile pixel height: one emoji cell (`64`).
pub const TILE_HEIGHT_PX: u32 = CELL_SIZE_PX;

include!(concat!(env!("OUT_DIR"), "/tiles.rs"));

static PRESENT_BITMAP: &[u8] = include_bytes!("../assets/present.bin");

/// Errors produced when decoding or querying an emoji tile.
#[derive(Debug, Error)]
pub enum EmojiError {
    /// Requested tile index is out of bounds.
    #[error("emoji tile index {0} out of bounds (max {max})", max = TILE_COUNT - 1)]
    TileOutOfBounds(usize),
    /// The tile has no emoji pixels, so nothing is embedded for it.
    #[error("emoji tile {0} is empty")]
    EmptyTile(usize),
    /// Decoding the embedded PNG tile failed.
    #[error("failed to decode emoji tile {tile}: {message}")]
    DecodeFailed {
        /// Tile index that failed to decode.
        tile: usize,
        /// Underlying decoder error string.
        message: String,
    },
}

/// Summary statistics for the packed Noto Emoji atlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasStats {
    /// Total emoji entries defined in `tg-text`.
    pub total_entries: usize,
    /// Number of entries with a rendered sprite cell in this atlas.
    pub present_entries: usize,
    /// Number of packed sprite sheets.
    pub sheet_count: usize,
    /// Cell width/height in pixels.
    pub cell_size_px: u32,
}

/// Located sprite cell inside one of the packed emoji atlas sheets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmojiSprite {
    /// `tg-text` 0-based emoji index whose cell is referenced.
    pub emoji_index: usize,
    /// Sprite sheet index (`0..SHEET_COUNT`).
    pub sheet: usize,
    /// Column within the sheet (`0..SHEET_COLS`).
    pub col: usize,
    /// Row within the sheet (`0..SHEET_ROWS`).
    pub row: usize,
    /// Tile holding the cell (`sheet * SHEET_ROWS + row`, `0..TILE_COUNT`).
    pub tile: usize,
    /// Pixel rectangle `[x, y, width, height]` inside the tile.
    pub pixel_rect: [u32; 4],
    /// Normalized UV coordinates `[u_min, v_min, u_max, v_max]` inside the tile texture.
    pub uv: [f32; 4],
    /// Cell dimension in pixels (`CELL_SIZE_PX`).
    pub cell_size: u32,
    /// `true` if a fallback sprite (such as the uncolored base emoji) was used.
    pub is_fallback: bool,
}

impl EmojiSprite {
    /// Converts `self.uv` into an [`egui::Rect`] for textured quad painting.
    #[must_use]
    pub const fn egui_uv(self) -> egui::Rect {
        egui::Rect::from_min_max(
            egui::pos2(self.uv[0], self.uv[1]),
            egui::pos2(self.uv[2], self.uv[3]),
        )
    }

    /// Computes a clean display draw size in logical pixels for a requested `target_px`.
    #[must_use]
    pub fn draw_size_for(self, target_px: f32) -> f32 {
        target_px.clamp(8.0, (self.cell_size * 2) as f32)
    }
}

/// Decoded RGBA8 pixel buffer for one tile (one sheet row of 32 emoji cells).
#[derive(Debug, Clone)]
pub struct EmojiTileImage {
    /// Width in pixels (`TILE_WIDTH_PX`).
    pub width: u32,
    /// Height in pixels (`TILE_HEIGHT_PX`).
    pub height: u32,
    /// Straight (unpremultiplied) RGBA8 pixels (`width * height * 4` bytes). Colour channels
    /// that premultiplication would hide are normalised (see `build.rs`); alpha is exact.
    pub rgba: Vec<u8>,
}

impl EmojiTileImage {
    /// Converts this decoded tile into an [`egui::ColorImage`].
    #[must_use]
    pub fn to_color_image(&self) -> egui::ColorImage {
        egui::ColorImage::from_rgba_unmultiplied(
            [self.width as usize, self.height as usize],
            &self.rgba,
        )
    }
}

/// Thread-safe, lazily decoded Noto Emoji sprite atlas.
///
/// Tiles are embedded in the binary and decoded on demand. [`Self::decode_tile`] keeps the
/// decoded pixels for later single-emoji extraction ([`Self::emoji_rgba`]); the GPU texture path
/// uses [`Self::decode_tile_transient`], which keeps nothing, so an uploaded tile does not also
/// stay in RAM (512 KiB per tile).
#[derive(Debug)]
pub struct EmojiAtlas {
    tiles: [Mutex<Option<Arc<EmojiTileImage>>>; TILE_COUNT],
}

impl Default for EmojiAtlas {
    fn default() -> Self {
        Self::new()
    }
}

impl EmojiAtlas {
    /// Creates a new lazily-decoded [`EmojiAtlas`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            tiles: std::array::from_fn(|_| Mutex::new(None)),
        }
    }

    /// Returns a process-wide shared singleton atlas instance.
    #[must_use]
    pub fn global() -> &'static Self {
        static GLOBAL: OnceLock<EmojiAtlas> = OnceLock::new();
        GLOBAL.get_or_init(Self::new)
    }

    /// Returns atlas coverage and dimension statistics.
    #[must_use]
    pub fn stats(&self) -> AtlasStats {
        let total = emoji::full_count();
        let mut present = 0usize;
        for idx in 0..total {
            if Self::has_index(idx) {
                present += 1;
            }
        }
        AtlasStats {
            total_entries: total,
            present_entries: present,
            sheet_count: SHEET_COUNT,
            cell_size_px: CELL_SIZE_PX,
        }
    }

    /// Returns `true` if `index` has a sprite directly packed in the atlas.
    #[must_use]
    pub fn has_index(index: usize) -> bool {
        let byte_idx = index >> 3;
        let bit = 1u8 << (index & 7);
        PRESENT_BITMAP
            .get(byte_idx)
            .is_some_and(|&b| (b & bit) != 0)
    }

    /// Returns `true` if `emoji_ref` has a sprite (either exact or via its uncolored base).
    #[must_use]
    pub fn has_emoji(&self, emoji_ref: EmojiRef) -> bool {
        Self::has_index(emoji_ref.index()) || Self::has_index(emoji_ref.original().index())
    }

    /// Looks up the packed sprite cell for an [`EmojiRef`].
    ///
    /// If a specific variant is absent from the sheet, automatically falls back to
    /// `emoji_ref.original()` with `is_fallback = true`.
    #[must_use]
    pub fn sprite(&self, emoji_ref: EmojiRef) -> Option<EmojiSprite> {
        if Self::has_index(emoji_ref.index()) {
            return Some(sprite_for_index_inner(emoji_ref.index(), false));
        }
        let orig = emoji_ref.original();
        if orig.index() != emoji_ref.index() && Self::has_index(orig.index()) {
            return Some(sprite_for_index_inner(orig.index(), true));
        }
        None
    }

    /// Looks up the packed sprite cell by 0-based `tg-text` emoji index.
    #[must_use]
    pub fn sprite_by_index(&self, index: usize) -> Option<EmojiSprite> {
        let emoji_ref = emoji::by_index(index)?;
        self.sprite(emoji_ref)
    }

    /// Looks up the packed sprite cell for a Unicode emoji sequence (e.g. `"😀"`, `"👍🏽"`, `"❤️"`).
    #[must_use]
    pub fn sprite_for_str(&self, seq: &str) -> Option<EmojiSprite> {
        let (emoji_ref, _) = emoji::find_emoji(seq, 0)?;
        self.sprite(emoji_ref)
    }

    /// Returns the sprite cell together with a clamped display size for `target_px`.
    #[must_use]
    pub fn sprite_for_size(
        &self,
        emoji_ref: EmojiRef,
        target_px: f32,
    ) -> Option<(EmojiSprite, f32)> {
        let sprite = self.sprite(emoji_ref)?;
        let size = sprite.draw_size_for(target_px);
        Some((sprite, size))
    }

    /// Lazily decodes and keeps the RGBA8 pixel buffer for tile `tile`.
    ///
    /// The decoded tile stays in memory until [`Self::release_tile`]; use
    /// [`Self::decode_tile_transient`] when the pixels are only needed once (texture upload).
    pub fn decode_tile(&self, tile: usize) -> Result<Arc<EmojiTileImage>, EmojiError> {
        let slot = self
            .tiles
            .get(tile)
            .ok_or(EmojiError::TileOutOfBounds(tile))?;
        // Decoding under the slot lock makes concurrent callers share one decode.
        let mut guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = guard.as_ref() {
            return Ok(Arc::clone(existing));
        }
        let decoded = Arc::new(decode_tile_pixels(tile)?);
        *guard = Some(Arc::clone(&decoded));
        Ok(decoded)
    }

    /// Decodes tile `tile` without keeping it: returns the retained copy when one exists,
    /// otherwise a fresh decode that is freed as soon as the caller drops it.
    pub fn decode_tile_transient(&self, tile: usize) -> Result<Arc<EmojiTileImage>, EmojiError> {
        let slot = self
            .tiles
            .get(tile)
            .ok_or(EmojiError::TileOutOfBounds(tile))?;
        if let Some(existing) = slot.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            return Ok(Arc::clone(existing));
        }
        decode_tile_pixels(tile).map(Arc::new)
    }

    /// Drops the retained decoded copy of `tile` (if any).
    pub fn release_tile(&self, tile: usize) {
        if let Some(slot) = self.tiles.get(tile) {
            slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        }
    }

    /// Number of tiles whose decoded pixels are currently kept in memory.
    #[must_use]
    pub fn retained_tile_count(&self) -> usize {
        self.tiles
            .iter()
            .filter(|slot| {
                slot.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .is_some()
            })
            .count()
    }

    /// Extracts a standalone `CELL_SIZE_PX x CELL_SIZE_PX` RGBA8 buffer for a single emoji.
    pub fn emoji_rgba(&self, emoji_ref: EmojiRef) -> Result<Option<Vec<u8>>, EmojiError> {
        let Some(sprite) = self.sprite(emoji_ref) else {
            return Ok(None);
        };
        let tile = self.decode_tile(sprite.tile)?;
        let [px, py, pw, ph] = sprite.pixel_rect;
        let stride = (tile.width as usize) * 4;
        let row_bytes = (pw as usize) * 4;
        let mut out = vec![0u8; (pw as usize) * (ph as usize) * 4];
        for y in 0..(ph as usize) {
            let src_start = ((py as usize) + y) * stride + (px as usize) * 4;
            let dst_start = y * row_bytes;
            if src_start + row_bytes <= tile.rgba.len() {
                out[dst_start..dst_start + row_bytes]
                    .copy_from_slice(&tile.rgba[src_start..src_start + row_bytes]);
            }
        }
        Ok(Some(out))
    }
}

fn decode_tile_pixels(tile: usize) -> Result<EmojiTileImage, EmojiError> {
    let bytes = TILE_PNG
        .get(tile)
        .ok_or(EmojiError::TileOutOfBounds(tile))?;
    if bytes.is_empty() {
        return Err(EmojiError::EmptyTile(tile));
    }
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|err| EmojiError::DecodeFailed {
            tile,
            message: err.to_string(),
        })?
        .into_rgba8();
    Ok(EmojiTileImage {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    })
}

fn sprite_for_index_inner(index: usize, is_fallback: bool) -> EmojiSprite {
    let sheet = index >> 9;
    let row = (index >> 5) & 0x0F;
    let col = index & 0x1F;
    let px = (col as u32) * CELL_SIZE_PX;

    // Inset UV by 0.5 texels to prevent bilinear bleeding from neighboring cells. The tile is the
    // sheet row, so these are the sheet's texel coordinates shifted up by `row * CELL_SIZE_PX`.
    let half_u = 0.5 / (TILE_WIDTH_PX as f32);
    let half_v = 0.5 / (TILE_HEIGHT_PX as f32);
    let u0 = (px as f32) / (TILE_WIDTH_PX as f32) + half_u;
    let u1 = ((px + CELL_SIZE_PX) as f32) / (TILE_WIDTH_PX as f32) - half_u;
    let v0 = half_v;
    let v1 = 1.0 - half_v;

    EmojiSprite {
        emoji_index: index,
        sheet,
        col,
        row,
        tile: index >> 5,
        pixel_rect: [px, 0, CELL_SIZE_PX, CELL_SIZE_PX],
        uv: [u0, v0, u1, v1],
        cell_size: CELL_SIZE_PX,
        is_fallback,
    }
}
