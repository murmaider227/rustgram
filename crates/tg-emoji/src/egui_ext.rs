//! `egui` texture caching and inline rich-text emoji painting helpers.
//!
//! Enables `tg-ui` text rendering and `tg-app` chat views to render crisp color
//! Noto Emoji sprites inline alongside standard proportional text.

use egui::{
    Color32, Context, FontId, Painter, Pos2, Rect, TextureHandle, TextureId, TextureOptions, Ui,
    Vec2,
};
use tg_text::emoji::{self, EmojiRef};

use crate::atlas::{EmojiAtlas, SHEET_COUNT};

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

/// Per-`egui::Context` GPU texture cache for [`EmojiAtlas`] sprite sheets.
///
/// Each sheet is uploaded lazily on the first frame an emoji from that sheet is painted.
pub struct EmojiTextureCache {
    atlas: &'static EmojiAtlas,
    textures: [Option<TextureHandle>; SHEET_COUNT],
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
            textures: std::array::from_fn(|_| None),
        }
    }

    /// Returns the underlying [`EmojiAtlas`].
    #[must_use]
    pub const fn atlas(&self) -> &'static EmojiAtlas {
        self.atlas
    }

    /// Ensures sprite sheet `sheet` is uploaded as an `egui` texture and returns its handle.
    pub fn sheet_texture(&mut self, ctx: &Context, sheet: usize) -> Option<&TextureHandle> {
        if sheet >= SHEET_COUNT {
            return None;
        }
        if self.textures[sheet].is_none() {
            let decoded = self.atlas.decode_sheet(sheet).ok()?;
            let color_image = decoded.to_color_image();
            let handle = ctx.load_texture(
                format!("tg_emoji_sheet_{sheet}"),
                color_image,
                TextureOptions::LINEAR,
            );
            self.textures[sheet] = Some(handle);
        }
        self.textures[sheet].as_ref()
    }

    /// Resolves `(TextureId, UV Rect)` for `emoji_ref`, uploading its sheet lazily if needed.
    pub fn texture_and_uv(
        &mut self,
        ctx: &Context,
        emoji_ref: EmojiRef,
    ) -> Option<(TextureId, Rect)> {
        let sprite = self.atlas.sprite(emoji_ref)?;
        let tex_id = self.sheet_texture(ctx, sprite.sheet)?.id();
        Some((tex_id, sprite.egui_uv()))
    }

    /// Resolves `(TextureId, UV Rect)` for a Unicode emoji string (e.g. `"🚀"`, `"👍🏽"`).
    pub fn texture_and_uv_str(&mut self, ctx: &Context, seq: &str) -> Option<(TextureId, Rect)> {
        let (emoji_ref, _) = emoji::find_emoji(seq, 0)?;
        self.texture_and_uv(ctx, emoji_ref)
    }

    /// Paints `emoji_ref` centered inside `rect` with `tint` (use [`Color32::WHITE`] for normal color).
    ///
    /// Returns `true` if a sprite was painted.
    pub fn paint_emoji(
        &mut self,
        painter: &Painter,
        ctx: &Context,
        emoji_ref: EmojiRef,
        rect: Rect,
        tint: Color32,
    ) -> bool {
        let Some((tex_id, uv)) = self.texture_and_uv(ctx, emoji_ref) else {
            return false;
        };
        let side = rect.width().min(rect.height());
        let square = Rect::from_center_size(rect.center(), Vec2::splat(side));
        painter.image(tex_id, square, uv, tint);
        true
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

    /// Measures and paints single-line text containing inline Noto Emoji starting at `origin`.
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

    /// Renders wrapping multi-line text with inline Noto Emoji inside `ui`.
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
