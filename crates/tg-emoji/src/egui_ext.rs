//! `egui` texture caching for the emoji sheets and painting of single emoji (pickers, reactions,
//! large emoji).
//!
//! Emoji inside text are not painted here: every egui galley draws them from the sheets at
//! tdesktop's size through [`crate::text_glyphs`] (TGB-3048, TGB-3055).

use std::sync::{Arc, Mutex, PoisonError};

use egui::{
    Color32, Context, FontId, Id, Painter, Rect, TextureHandle, TextureId, TextureOptions, Vec2,
};
use tg_text::emoji::{self, EmojiRef};

use crate::atlas::{EmojiAtlas, TILE_COUNT};
use crate::background::background_decode_enabled;
use crate::tile_worker;

/// Single token produced by splitting a UTF-8 string into plain text and emoji runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineEmojiSegment<'a> {
    /// Non-emoji plain text slice.
    Text(&'a str),
    /// Recognized Unicode emoji sequence from `tg-text`.
    Emoji {
        /// Resolved static emoji reference.
        emoji: EmojiRef,
        /// Original UTF-8 slice in the input string.
        raw: &'a str,
    },
}

/// Splits `text` into alternating [`InlineEmojiSegment::Text`] and [`InlineEmojiSegment::Emoji`]
/// runs using `tg-text`'s trie matcher (including skin-tone variants and `U+FE0F` postfixes).
#[must_use]
pub fn split_inline_emoji(text: &str) -> Vec<InlineEmojiSegment<'_>> {
    let mut segments = Vec::new();
    let mut pos = 0usize;
    let mut plain_start = 0usize;

    while pos < text.len() {
        if let Some((found, matched_bytes)) = emoji::find_emoji(text, pos) {
            if pos > plain_start {
                segments.push(InlineEmojiSegment::Text(&text[plain_start..pos]));
            }
            segments.push(InlineEmojiSegment::Emoji {
                emoji: found,
                raw: &text[pos..pos + matched_bytes],
            });
            pos += matched_bytes;
            plain_start = pos;
        } else {
            let step = text[pos..].chars().next().map_or(1, char::len_utf8);
            pos += step;
        }
    }
    if plain_start < text.len() {
        segments.push(InlineEmojiSegment::Text(&text[plain_start..]));
    }
    segments
}

/// Upload state of one tile in the shared per-context texture set.
#[derive(Clone, Default)]
enum TileSlot {
    #[default]
    Empty,
    /// A worker thread is decoding and uploading the tile.
    Decoding,
    Ready(TextureHandle),
    Failed,
}

/// One set of tile textures per `(egui::Context, EmojiAtlas)`, shared by every
/// [`EmojiTextureCache`] handle, so a tile is uploaded once (512 KiB of GPU memory each) instead
/// of once per widget.
#[derive(Clone)]
struct SharedTiles(Arc<Mutex<Vec<TileSlot>>>);

impl Default for SharedTiles {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(vec![TileSlot::Empty; TILE_COUNT])))
    }
}

impl SharedTiles {
    fn for_context(ctx: &Context, atlas: &'static EmojiAtlas) -> Self {
        let id = Id::unique(("tg_emoji_shared_tiles", std::ptr::from_ref(atlas) as usize));
        ctx.data_mut(|d| d.get_temp_mut_or_default::<Self>(id).clone())
    }

    #[cfg(test)]
    fn slot(&self, tile: usize) -> TileSlot {
        let slots = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        slots.get(tile).cloned().unwrap_or(TileSlot::Failed)
    }

    /// Returns the tile's state, claiming it for this caller (marked decoding, returned as
    /// `Empty`) when nobody started it. One lock: reading the slot and claiming it under two
    /// locks let another handle finish the tile in between, and the tile then read as ready
    /// without its handle (`tile_texture` returned `None` for that frame).
    fn lookup_or_claim(&self, tile: usize) -> TileSlot {
        let mut slots = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match slots.get_mut(tile) {
            Some(slot @ TileSlot::Empty) => {
                *slot = TileSlot::Decoding;
                TileSlot::Empty
            }
            Some(slot) => slot.clone(),
            None => TileSlot::Failed,
        }
    }

    #[cfg(test)]
    fn claim(&self, tile: usize) -> bool {
        matches!(self.lookup_or_claim(tile), TileSlot::Empty)
    }

    fn finish(&self, tile: usize, result: Option<TextureHandle>) {
        let mut slots = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = slots.get_mut(tile) {
            *slot = result.map_or(TileSlot::Failed, TileSlot::Ready);
        }
    }
}

/// Finishes a claimed tile from a worker job; marks it failed if the job ends without a result
/// (a panic in the decoder), so the tile never stays "decoding" forever.
struct TileJob {
    ctx: Context,
    shared: SharedTiles,
    atlas: &'static EmojiAtlas,
    tile: usize,
    done: bool,
}

impl TileJob {
    fn run(mut self) {
        let handle = upload_tile(&self.ctx, self.atlas, self.tile);
        self.shared.finish(self.tile, handle);
        self.done = true;
        self.ctx.request_repaint();
    }
}

impl Drop for TileJob {
    fn drop(&mut self) {
        if !self.done {
            self.shared.finish(self.tile, None);
            self.ctx.request_repaint();
        }
    }
}

/// Decodes tile `tile` (without keeping the CPU copy) and uploads it as an `egui` texture.
fn upload_tile(ctx: &Context, atlas: &EmojiAtlas, tile: usize) -> Option<TextureHandle> {
    let decoded = atlas.decode_tile_transient(tile).ok()?;
    let color_image = decoded.to_color_image();
    drop(decoded);
    Some(ctx.load_texture(
        format!("tg_emoji_tile_{tile:03}"),
        color_image,
        TextureOptions::LINEAR,
    ))
}

/// Where a tile's texture stands for the current paint.
enum TileStatus {
    Ready(TextureId),
    /// Being decoded on a worker; it appears on a later frame.
    Pending,
    Unavailable,
}

/// Handle to the shared per-`egui::Context` GPU textures of the [`EmojiAtlas`] tiles.
///
/// Each tile (one sheet row of 32 emoji) is uploaded once per context, on the first frame an
/// emoji from it is painted; every handle on the same context reuses that texture. With
/// [`crate::background::set_background_decode`] on, the tile is decoded on a worker thread (about
/// 1 ms) and its emoji appear on the next frame instead of the paint stalling for the decode.
pub struct EmojiTextureCache {
    atlas: &'static EmojiAtlas,
    textures: Box<[Option<TextureHandle>]>,
    background: Option<bool>,
}

impl Default for EmojiTextureCache {
    fn default() -> Self {
        Self::new()
    }
}

impl EmojiTextureCache {
    /// Creates a texture cache backed by the global [`EmojiAtlas`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_atlas(EmojiAtlas::global())
    }

    /// Creates a texture cache backed by an explicit static [`EmojiAtlas`].
    #[must_use]
    pub fn with_atlas(atlas: &'static EmojiAtlas) -> Self {
        Self {
            atlas,
            textures: vec![None; TILE_COUNT].into_boxed_slice(),
            background: None,
        }
    }

    /// Overrides the process-wide background-decode switch for this handle (`None` follows it).
    pub fn set_background_decode(&mut self, background: Option<bool>) {
        self.background = background;
    }

    fn tile_status(&mut self, ctx: &Context, tile: usize) -> TileStatus {
        if tile >= TILE_COUNT {
            return TileStatus::Unavailable;
        }
        if let Some(handle) = &self.textures[tile] {
            return TileStatus::Ready(handle.id());
        }
        let shared = SharedTiles::for_context(ctx, self.atlas);
        match shared.lookup_or_claim(tile) {
            TileSlot::Ready(handle) => {
                let id = handle.id();
                self.textures[tile] = Some(handle);
                return TileStatus::Ready(id);
            }
            TileSlot::Decoding => return TileStatus::Pending,
            TileSlot::Failed => return TileStatus::Unavailable,
            // Claimed: this caller decodes it.
            TileSlot::Empty => {}
        }
        if self.background.unwrap_or_else(background_decode_enabled) && tile_worker::available() {
            let job = TileJob {
                ctx: ctx.clone(),
                shared: shared.clone(),
                atlas: self.atlas,
                tile,
                done: false,
            };
            // A job that cannot be queued is dropped unrun, which marks the tile failed.
            return match tile_worker::submit(Box::new(move || job.run())) {
                Ok(()) => TileStatus::Pending,
                Err(_) => TileStatus::Unavailable,
            };
        }
        let handle = upload_tile(ctx, self.atlas, tile);
        shared.finish(tile, handle.clone());
        match handle {
            Some(handle) => {
                let id = handle.id();
                self.textures[tile] = Some(handle);
                TileStatus::Ready(id)
            }
            None => TileStatus::Unavailable,
        }
    }

    /// Returns the underlying [`EmojiAtlas`].
    #[must_use]
    pub const fn atlas(&self) -> &'static EmojiAtlas {
        self.atlas
    }

    /// Ensures tile `tile` is uploaded as an `egui` texture and returns its handle (`None` while a
    /// background decode of the tile is still running).
    pub fn tile_texture(&mut self, ctx: &Context, tile: usize) -> Option<&TextureHandle> {
        match self.tile_status(ctx, tile) {
            TileStatus::Ready(_) => self.textures.get(tile)?.as_ref(),
            TileStatus::Pending | TileStatus::Unavailable => None,
        }
    }

    /// Resolves `(TextureId, UV Rect)` for `emoji_ref`, uploading its tile lazily if needed
    /// (`None` while a background decode of the tile is still running).
    pub fn texture_and_uv(
        &mut self,
        ctx: &Context,
        emoji_ref: EmojiRef,
    ) -> Option<(TextureId, Rect)> {
        let sprite = self.atlas.sprite(emoji_ref)?;
        match self.tile_status(ctx, sprite.tile) {
            TileStatus::Ready(tex_id) => Some((tex_id, sprite.egui_uv())),
            TileStatus::Pending | TileStatus::Unavailable => None,
        }
    }

    /// Resolves `(TextureId, UV Rect)` for a Unicode emoji string (e.g. `"🚀"`, `"👍🏽"`).
    pub fn texture_and_uv_str(&mut self, ctx: &Context, seq: &str) -> Option<(TextureId, Rect)> {
        let (emoji_ref, _) = emoji::find_emoji(seq, 0)?;
        self.texture_and_uv(ctx, emoji_ref)
    }

    /// Paints `emoji_ref` centered inside `rect` with `tint` (use [`Color32::WHITE`] for normal color).
    ///
    /// Returns `true` if the emoji has a sprite: it is painted now, or, while its tile is still
    /// being decoded in the background, on the frame the worker finishes (the space stays
    /// reserved, no fallback glyph is drawn in between).
    pub fn paint_emoji(
        &mut self,
        painter: &Painter,
        ctx: &Context,
        emoji_ref: EmojiRef,
        rect: Rect,
        tint: Color32,
    ) -> bool {
        let Some(sprite) = self.atlas.sprite(emoji_ref) else {
            return false;
        };
        match self.tile_status(ctx, sprite.tile) {
            TileStatus::Ready(tex_id) => {
                let side = rect.width().min(rect.height());
                let square = Rect::from_center_size(rect.center(), Vec2::splat(side));
                painter.image(tex_id, square, sprite.egui_uv(), tint);
                true
            }
            TileStatus::Pending => true,
            TileStatus::Unavailable => false,
        }
    }

    /// Paints a Unicode emoji string centered inside `rect`.
    ///
    /// Falls back to rendering the raw glyph with `painter.text` if not in the atlas.
    pub fn paint_emoji_str(
        &mut self,
        painter: &Painter,
        ctx: &Context,
        seq: &str,
        rect: Rect,
        tint: Color32,
    ) -> bool {
        if let Some((emoji_ref, _)) = emoji::find_emoji(seq, 0)
            && self.paint_emoji(painter, ctx, emoji_ref, rect, tint)
        {
            return true;
        }
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            seq,
            FontId::proportional((rect.height() * 0.78).max(10.0)),
            tint,
        );
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tile_job_dropped_without_running_marks_the_tile_failed_not_pending() {
        let atlas: &'static EmojiAtlas = Box::leak(Box::new(EmojiAtlas::new()));
        let ctx = Context::default();
        let shared = SharedTiles::for_context(&ctx, atlas);
        assert!(shared.claim(3));
        assert!(matches!(shared.slot(3), TileSlot::Decoding));
        drop(TileJob {
            ctx: ctx.clone(),
            shared: shared.clone(),
            atlas,
            tile: 3,
            done: false,
        });
        assert!(matches!(shared.slot(3), TileSlot::Failed));

        assert!(shared.claim(4));
        TileJob {
            ctx: ctx.clone(),
            shared: shared.clone(),
            atlas,
            tile: 4,
            done: false,
        }
        .run();
        assert!(matches!(shared.slot(4), TileSlot::Ready(_)));
    }
}
