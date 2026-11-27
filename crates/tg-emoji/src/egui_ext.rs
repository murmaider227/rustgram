//! `egui` texture caching and inline rich-text emoji painting helpers.
//!
//! Enables `tg-ui` text rendering and `tg-app` chat views to render crisp color
//! emoji sprites inline alongside standard proportional text.

use std::sync::{Arc, Mutex, PoisonError};

use egui::{
    Color32, Context, FontId, Id, Painter, Pos2, Rect, TextureHandle, TextureId, TextureOptions,
    Ui, Vec2,
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

    fn slot(&self, tile: usize) -> TileSlot {
        let slots = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        slots.get(tile).cloned().unwrap_or(TileSlot::Failed)
    }

    /// Marks `tile` as being decoded; returns `false` when another caller already started it.
    fn claim(&self, tile: usize) -> bool {
        let mut slots = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match slots.get_mut(tile) {
            Some(slot @ TileSlot::Empty) => {
                *slot = TileSlot::Decoding;
                true
            }
            _ => false,
        }
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
        match shared.slot(tile) {
            TileSlot::Ready(handle) => {
                let id = handle.id();
                self.textures[tile] = Some(handle);
                return TileStatus::Ready(id);
            }
            TileSlot::Decoding => return TileStatus::Pending,
            TileSlot::Failed => return TileStatus::Unavailable,
            TileSlot::Empty => {}
        }
        if !shared.claim(tile) {
            return match shared.slot(tile) {
                TileSlot::Ready(handle) => TileStatus::Ready(handle.id()),
                TileSlot::Decoding => TileStatus::Pending,
                TileSlot::Empty | TileSlot::Failed => TileStatus::Unavailable,
            };
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

    /// Measures and paints single-line text containing inline emoji starting at `origin`.
    ///
    /// Returns the total `(width, height)` occupied.
    pub fn paint_single_line_with_emoji(
        &mut self,
        painter: &Painter,
        ctx: &Context,
        origin: Pos2,
        text: &str,
        font_id: &FontId,
        color: Color32,
    ) -> Vec2 {
        let segments = split_inline_emoji(text);
        let emoji_size = (font_id.size * 1.18).round();
        let line_h = emoji_size.max(font_id.size * 1.25);
        let mut cursor_x = origin.x;

        for seg in segments {
            match seg {
                InlineEmojiSegment::Text(slice) => {
                    let galley = painter.layout_no_wrap(slice.to_owned(), font_id.clone(), color);
                    let y = origin.y + (line_h - galley.size().y) * 0.5;
                    let w = galley.size().x;
                    painter.galley(Pos2::new(cursor_x, y), galley, color);
                    cursor_x += w;
                }
                InlineEmojiSegment::Emoji { emoji, raw } => {
                    let y = origin.y + (line_h - emoji_size) * 0.5;
                    let rect = Rect::from_min_size(Pos2::new(cursor_x, y), Vec2::splat(emoji_size));
                    if !self.paint_emoji(painter, ctx, emoji, rect, Color32::WHITE) {
                        let galley = painter.layout_no_wrap(raw.to_owned(), font_id.clone(), color);
                        let w = galley.size().x;
                        painter.galley(Pos2::new(cursor_x, y), galley, color);
                        cursor_x += w;
                    } else {
                        cursor_x += emoji_size + 1.0;
                    }
                }
            }
        }

        Vec2::new(cursor_x - origin.x, line_h)
    }

    /// Renders wrapping multi-line text with inline emoji inside `ui`.
    ///
    /// Allocates the exact laid-out rectangle and returns it.
    pub fn paint_wrapped_text_with_emoji(
        &mut self,
        ui: &mut Ui,
        text: &str,
        font_id: &FontId,
        color: Color32,
        max_width: f32,
    ) -> Rect {
        let wrap_w = max_width.max(24.0);
        let emoji_size = (font_id.size * 1.18).round();
        let line_h = emoji_size.max(font_id.size * 1.30);
        let ctx = ui.ctx().clone();

        // Pre-layout into wrapped visual tokens so height is exact before allocating.
        enum VisualToken {
            Galley(std::sync::Arc<egui::Galley>),
            Emoji(EmojiRef),
            Newline,
        }

        let mut tokens = Vec::new();
        for seg in split_inline_emoji(text) {
            match seg {
                InlineEmojiSegment::Emoji { emoji, .. } => {
                    tokens.push(VisualToken::Emoji(emoji));
                }
                InlineEmojiSegment::Text(slice) => {
                    for (line_idx, line) in slice.split('\n').enumerate() {
                        if line_idx > 0 {
                            tokens.push(VisualToken::Newline);
                        }
                        if line.is_empty() {
                            continue;
                        }
                        // Split into whitespace-preserving words for clean wrapping.
                        let mut start = 0usize;
                        let bytes = line.as_bytes();
                        while start < line.len() {
                            let is_ws = line[start..]
                                .chars()
                                .next()
                                .is_some_and(char::is_whitespace);
                            let mut end = start;
                            for (rel, ch) in line[start..].char_indices() {
                                if ch.is_whitespace() != is_ws {
                                    break;
                                }
                                end = start + rel + ch.len_utf8();
                            }
                            if end == start {
                                end = bytes.len();
                            }
                            let word = &line[start..end];
                            let galley = ui.painter().layout_no_wrap(
                                word.to_owned(),
                                font_id.clone(),
                                color,
                            );
                            tokens.push(VisualToken::Galley(galley));
                            start = end;
                        }
                    }
                }
            }
        }

        // Compute positions.
        struct Placed {
            x: f32,
            y: f32,
            token: VisualToken,
        }
        let mut placed = Vec::with_capacity(tokens.len());
        let mut x = 0.0f32;
        let mut y = 0.0f32;
        let mut max_used_x = 0.0f32;

        for token in tokens {
            match token {
                VisualToken::Newline => {
                    x = 0.0;
                    y += line_h;
                }
                VisualToken::Emoji(e) => {
                    let w = emoji_size + 1.0;
                    if x > 0.0 && x + w > wrap_w {
                        x = 0.0;
                        y += line_h;
                    }
                    placed.push(Placed {
                        x,
                        y,
                        token: VisualToken::Emoji(e),
                    });
                    x += w;
                    max_used_x = max_used_x.max(x);
                }
                VisualToken::Galley(g) => {
                    let w = g.size().x;
                    if x > 0.0 && x + w > wrap_w {
                        x = 0.0;
                        y += line_h;
                    }
                    placed.push(Placed {
                        x,
                        y,
                        token: VisualToken::Galley(g),
                    });
                    x += w;
                    max_used_x = max_used_x.max(x);
                }
            }
        }

        let total_size = Vec2::new(max_used_x.min(wrap_w), y + line_h);
        let (rect, _resp) = ui.allocate_exact_size(total_size, egui::Sense::hover());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            for item in placed {
                match item.token {
                    VisualToken::Galley(g) => {
                        let gy = rect.min.y + item.y + (line_h - g.size().y) * 0.5;
                        painter.galley(Pos2::new(rect.min.x + item.x, gy), g, color);
                    }
                    VisualToken::Emoji(e) => {
                        let ey = rect.min.y + item.y + (line_h - emoji_size) * 0.5;
                        let er = Rect::from_min_size(
                            Pos2::new(rect.min.x + item.x, ey),
                            Vec2::splat(emoji_size),
                        );
                        self.paint_emoji(painter, &ctx, e, er, Color32::WHITE);
                    }
                    VisualToken::Newline => {}
                }
            }
        }

        rect
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
