//! Packed Noto Emoji sprite sheet atlas indexed by `tg-text`'s `EmojiRef` table.
//!
//! Port origin: `Telegram/lib_ui/ui/emoji_config.cpp` (`Ui::Emoji::One::sprite/row/column`
//! and sprite sheet loading), using Apache-2.0 Noto Emoji (`googlefonts/noto-emoji` v2.042)
//! instead of Apple's proprietary emoji artwork.

use std::sync::{Arc, OnceLock};

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

static SHEET_BYTES: [&[u8]; SHEET_COUNT] = [
    include_bytes!("../assets/sheet_0.webp"),
    include_bytes!("../assets/sheet_1.webp"),
    include_bytes!("../assets/sheet_2.webp"),
    include_bytes!("../assets/sheet_3.webp"),
    include_bytes!("../assets/sheet_4.webp"),
    include_bytes!("../assets/sheet_5.webp"),
    include_bytes!("../assets/sheet_6.webp"),
    include_bytes!("../assets/sheet_7.webp"),
];

static PRESENT_BITMAP: &[u8] = include_bytes!("../assets/present.bin");

/// Errors produced when decoding or querying an emoji sprite sheet.
#[derive(Debug, Error)]
pub enum EmojiError {
    /// Requested sprite sheet index is out of bounds.
    #[error("emoji sprite sheet index {0} out of bounds (max {max})", max = SHEET_COUNT - 1)]
    SheetOutOfBounds(usize),
    /// Decoding the embedded WebP sprite sheet failed.
    #[error("failed to decode emoji sprite sheet {sheet}: {message}")]
    DecodeFailed {
        /// Sheet index that failed to decode.
        sheet: usize,
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
    /// Pixel rectangle `[x, y, width, height]` inside the sprite sheet.
    pub pixel_rect: [u32; 4],
    /// Normalized texture UV coordinates `[u_min, v_min, u_max, v_max]` in `0.0..=1.0`.
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

/// Decoded RGBA8 pixel buffer for one sprite sheet.
#[derive(Debug, Clone)]
pub struct EmojiSheetImage {
    /// Width in pixels (`SHEET_WIDTH_PX`).
    pub width: u32,
    /// Height in pixels (`SHEET_HEIGHT_PX`).
    pub height: u32,
    /// Straight (unpremultiplied) RGBA8 pixels (`width * height * 4` bytes).
    pub rgba: Vec<u8>,
}

impl EmojiSheetImage {
    /// Converts this decoded sheet into an [`egui::ColorImage`].
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
/// Sprite sheets are embedded in the binary and decoded on demand on first lookup,
/// keeping startup instant and memory bounded to only the sheets actually displayed.
#[derive(Debug)]
pub struct EmojiAtlas {
    sheets: [OnceLock<Arc<EmojiSheetImage>>; SHEET_COUNT],
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
            sheets: std::array::from_fn(|_| OnceLock::new()),
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

    /// Lazily decodes and caches the RGBA8 pixel buffer for sprite sheet `sheet`.
    pub fn decode_sheet(&self, sheet: usize) -> Result<Arc<EmojiSheetImage>, EmojiError> {
        let slot = self
            .sheets
            .get(sheet)
            .ok_or(EmojiError::SheetOutOfBounds(sheet))?;
        if let Some(existing) = slot.get() {
            return Ok(Arc::clone(existing));
        }
        let bytes = SHEET_BYTES[sheet];
        let img = image::load_from_memory_with_format(bytes, image::ImageFormat::WebP)
            .map_err(|err| EmojiError::DecodeFailed {
                sheet,
                message: err.to_string(),
            })?
            .to_rgba8();
        let decoded = Arc::new(EmojiSheetImage {
            width: img.width(),
            height: img.height(),
            rgba: img.into_raw(),
        });
        let _ = slot.set(Arc::clone(&decoded));
        Ok(slot.get().map_or(decoded, Arc::clone))
    }

    /// Extracts a standalone `CELL_SIZE_PX x CELL_SIZE_PX` RGBA8 buffer for a single emoji.
    pub fn emoji_rgba(&self, emoji_ref: EmojiRef) -> Result<Option<Vec<u8>>, EmojiError> {
        let Some(sprite) = self.sprite(emoji_ref) else {
            return Ok(None);
        };
        let sheet = self.decode_sheet(sprite.sheet)?;
        let [px, py, pw, ph] = sprite.pixel_rect;
        let stride = (sheet.width as usize) * 4;
        let row_bytes = (pw as usize) * 4;
        let mut out = vec![0u8; (pw as usize) * (ph as usize) * 4];
        for y in 0..(ph as usize) {
            let src_start = ((py as usize) + y) * stride + (px as usize) * 4;
            let dst_start = y * row_bytes;
            if src_start + row_bytes <= sheet.rgba.len() {
                out[dst_start..dst_start + row_bytes]
                    .copy_from_slice(&sheet.rgba[src_start..src_start + row_bytes]);
            }
        }
        Ok(Some(out))
    }
}

fn sprite_for_index_inner(index: usize, is_fallback: bool) -> EmojiSprite {
    let sheet = index >> 9;
    let row = (index >> 5) & 0x0F;
    let col = index & 0x1F;
    let px = (col as u32) * CELL_SIZE_PX;
    let py = (row as u32) * CELL_SIZE_PX;

    // Inset UV by 0.5 texels to prevent bilinear bleeding from neighboring cells.
    let half_u = 0.5 / (SHEET_WIDTH_PX as f32);
    let half_v = 0.5 / (SHEET_HEIGHT_PX as f32);
    let u0 = (px as f32) / (SHEET_WIDTH_PX as f32) + half_u;
    let v0 = (py as f32) / (SHEET_HEIGHT_PX as f32) + half_v;
    let u1 = ((px + CELL_SIZE_PX) as f32) / (SHEET_WIDTH_PX as f32) - half_u;
    let v1 = ((py + CELL_SIZE_PX) as f32) / (SHEET_HEIGHT_PX as f32) - half_v;

    EmojiSprite {
        emoji_index: index,
        sheet,
        col,
        row,
        pixel_rect: [px, py, CELL_SIZE_PX, CELL_SIZE_PX],
        uv: [u0, v0, u1, v1],
        cell_size: CELL_SIZE_PX,
        is_fallback,
    }
}
