//! Messages of only 1-3 emoji, drawn large without a bubble (TGB-3054).
//!
//! tdesktop: a message whose text is only emoji (`Ui::Text::String::toIsolatedEmoji`, at most
//! `kIsolatedEmojiLimit = 3`) gets `UnwrappedMedia` around `HistoryView::LargeEmoji` instead of a
//! text bubble when Settings › Chat › "Large emoji" is on (`Element::refreshMedia`). Each emoji is
//! `st::largeEmojiSize` (36 px) from the emoji sheets with a `st::largeEmojiOutline` (1 px) white
//! outline (`EmojiImageLoader::prepare`). The layout lives in `tg_history::MediaInput::LargeEmoji`.
//!
//! Internals differ from tdesktop: the outlined image is built here once per (emoji, pixel size)
//! and kept as an egui texture, instead of tdesktop's per-session `EmojiPack` image loader thread.

use std::collections::HashMap;

use egui::{Color32, ColorImage, Context, Id, Painter, Pos2, Rect, TextureHandle, TextureOptions};
use tg_emoji::atlas::EmojiAtlas;
use tg_history::consts::{
    ISOLATED_EMOJI_LIMIT, LARGE_EMOJI_OUTLINE, LARGE_EMOJI_SIZE, LARGE_EMOJI_SKIP,
};
use tg_text::emoji::{EmojiRef, find_emoji};
use tg_text::{EntityType, TextWithEntities};

use crate::history::backend::HistoryMessage;

/// The emoji of a message drawn large, `1..=ISOLATED_EMOJI_LIMIT` of them.
pub type IsolatedEmoji = Vec<EmojiRef>;

/// The emoji of `text` when it is only 1-3 emoji (`String::toIsolatedEmoji` after
/// `BlockParser::finalize`): spaces may follow an emoji, a newline, any other character, a link or
/// a spoiler makes it ordinary text.
#[must_use]
pub fn isolated_emoji(text: &TextWithEntities) -> Option<IsolatedEmoji> {
    let disqualifying = text.entities.iter().any(|entity| {
        // Links get a link index, spoilers, code and quotes their own blocks; custom emoji are
        // `isOnlyCustomEmoji` / custom `LargeEmoji` items, not drawn here yet (TGB-3097).
        !matches!(
            entity.entity_type(),
            EntityType::Bold
                | EntityType::Semibold
                | EntityType::Italic
                | EntityType::Underline
                | EntityType::StrikeOut
        )
    });
    if disqualifying {
        return None;
    }
    let s = text.text.as_str();
    let mut found: IsolatedEmoji = Vec::with_capacity(ISOLATED_EMOJI_LIMIT);
    let mut pos = 0usize;
    while pos < s.len() {
        if let Some((emoji, len)) = find_emoji(s, pos)
            && len > 0
        {
            if found.len() == ISOLATED_EMOJI_LIMIT {
                return None;
            }
            found.push(emoji);
            pos += len;
            continue;
        }
        let ch = s[pos..].chars().next()?;
        let joins_emoji = matches!(
            ch,
            '\u{200D}' | '\u{FE0E}' | '\u{FE0F}' | '\u{E0020}'..='\u{E007F}'
        );
        // A space joins the emoji block before it; a leading one is a text block of its own.
        let trailing_space = !found.is_empty() && ch.is_whitespace() && ch != '\u{A0}';
        if (!joins_emoji && !trailing_space) || ch == '\n' || ch == '\r' {
            return None;
        }
        pos += ch.len_utf8();
    }
    (!found.is_empty()).then_some(found)
}

/// The emoji `msg` is drawn as, when "Large emoji" is `enabled` and the message is only emoji
/// text (no media, link preview or rich page).
#[must_use]
pub fn large_emoji_of(msg: &HistoryMessage, enabled: bool) -> Option<IsolatedEmoji> {
    if !enabled
        || msg.media.is_some()
        || msg.service.is_some()
        || msg.extra.link_preview.is_some()
        || msg.extra.rich_message.is_some()
    {
        return None;
    }
    isolated_emoji(&msg.text)
}

/// Physical pixel sizes of one large emoji at `pixels_per_point`: the emoji side and its outline
/// (`style::ConvertScale` keeps a non-zero length at least 1 px).
#[must_use]
pub fn large_emoji_pixel_sizes(pixels_per_point: f32) -> (u32, u32) {
    let emoji = (LARGE_EMOJI_SIZE as f32 * pixels_per_point)
        .round()
        .max(1.0) as u32;
    let outline = (LARGE_EMOJI_OUTLINE as f32 * pixels_per_point)
        .round()
        .max(1.0) as u32;
    (emoji, outline)
}

/// Premultiplied RGBA in `0.0..=1.0`.
type Pixel = [f32; 4];

/// The sheet image of `emoji` shrunk to `side` x `side` premultiplied pixels.
fn emoji_pixels(emoji: EmojiRef, side: u32) -> Option<Vec<Pixel>> {
    let atlas = EmojiAtlas::global();
    let sprite = atlas.sprite(emoji)?;
    let tile = atlas.decode_tile_transient(sprite.tile).ok()?;
    let [left, top, width, height] = sprite.pixel_rect;
    let (tile_w, tile_h) = (tile.width as usize, tile.height as usize);
    let (left, top, width, height) = (left as usize, top as usize, width as usize, height as usize);
    if width == 0 || height == 0 || left + width > tile_w || top + height > tile_h {
        return None;
    }
    let source = |x: usize, y: usize| -> Pixel {
        let at = ((top + y) * tile_w + left + x) * 4;
        let Some(px) = tile.rgba.get(at..at + 4) else {
            return [0.0; 4];
        };
        let a = f32::from(px[3]) / 255.0;
        [
            f32::from(px[0]) / 255.0 * a,
            f32::from(px[1]) / 255.0 * a,
            f32::from(px[2]) / 255.0 * a,
            a,
        ]
    };
    Some(area_scale(&source, width, height, side as usize))
}

/// Box-filter resample of a `width` x `height` image to `side` x `side` (each target pixel is
/// the coverage-weighted mean of the source pixels under it).
fn area_scale(
    source: &dyn Fn(usize, usize) -> Pixel,
    width: usize,
    height: usize,
    side: usize,
) -> Vec<Pixel> {
    let spans = |len: usize| -> Vec<Vec<(usize, f32)>> {
        let scale = len as f32 / side as f32;
        (0..side)
            .map(|i| {
                let (from, to) = (i as f32 * scale, (i + 1) as f32 * scale);
                let mut taps = Vec::new();
                let mut at = from.floor() as usize;
                while (at as f32) < to && at < len {
                    let cover = (to.min(at as f32 + 1.0) - from.max(at as f32)).max(0.0);
                    if cover > 0.0 {
                        taps.push((at, cover / scale));
                    }
                    at += 1;
                }
                taps
            })
            .collect()
    };
    let (xs, ys) = (spans(width), spans(height));
    let mut out = vec![[0.0; 4]; side * side];
    for (ty, y_taps) in ys.iter().enumerate() {
        for (tx, x_taps) in xs.iter().enumerate() {
            let mut sum = [0.0f32; 4];
            for &(sy, wy) in y_taps {
                for &(sx, wx) in x_taps {
                    let px = source(sx, sy);
                    for c in 0..4 {
                        sum[c] += px[c] * wx * wy;
                    }
                }
            }
            out[ty * side + tx] = sum;
        }
    }
    out
}

/// `EmojiImageLoader::prepare`: the emoji on a `side + 2 * outline` canvas over a white copy of
/// its silhouette shifted 1..=outline px in the four planar and (`outline / sqrt 2`) four
/// diagonal directions.
#[must_use]
pub fn outlined_emoji_image(emoji: EmojiRef, side: u32, outline: u32) -> Option<ColorImage> {
    let pixels = emoji_pixels(emoji, side)?;
    let (side, outline) = (side as usize, outline as usize);
    let canvas = side + 2 * outline;
    let mut out = vec![[0.0f32; 4]; canvas * canvas];
    let mut blit = |dx: usize, dy: usize, white: bool| {
        for y in 0..side {
            for x in 0..side {
                let src = pixels[y * side + x];
                let src = if white { [src[3]; 4] } else { src };
                if src[3] <= 0.0 {
                    continue;
                }
                let dst = &mut out[(dy + y) * canvas + dx + x];
                for c in 0..4 {
                    dst[c] = src[c] + dst[c] * (1.0 - src[3]);
                }
            }
        }
    };
    let shifted = |shift: isize, step: usize| (outline as isize + shift * step as isize) as usize;
    for (sx, sy) in [(0, -1), (-1, 0), (1, 0), (0, 1)] {
        for step in 1..=outline {
            blit(shifted(sx, step), shifted(sy, step), true);
        }
    }
    let diagonal = (outline as f32 / std::f32::consts::SQRT_2).round() as usize;
    for (sx, sy) in [(-1, -1), (1, -1), (-1, 1), (1, 1)] {
        for step in 1..=diagonal {
            blit(shifted(sx, step), shifted(sy, step), true);
        }
    }
    blit(outline, outline, false);
    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(ColorImage::new(
        [canvas, canvas],
        out.into_iter()
            .map(|[r, g, b, a]| {
                Color32::from_rgba_premultiplied(to_u8(r), to_u8(g), to_u8(b), to_u8(a))
            })
            .collect(),
    ))
}

/// Uploaded large emoji images, with the pass that last painted each.
#[derive(Clone, Default)]
struct LargeEmojiTextures {
    textures: HashMap<(&'static str, u32, u32), (TextureHandle, u64)>,
}

/// Textures kept before the least recently painted half goes.
const MAX_TEXTURES: usize = 256;

fn texture_for(ctx: &Context, emoji: EmojiRef, side: u32, outline: u32) -> Option<TextureHandle> {
    let id = Id::unique("tg_history_large_emoji_textures");
    let key = (emoji.id(), side, outline);
    let pass = ctx.cumulative_pass_nr();
    let found = ctx.data_mut(|data| {
        let cache = data.get_temp_mut_or_default::<LargeEmojiTextures>(id);
        cache.textures.get_mut(&key).map(|(texture, used)| {
            *used = pass;
            texture.clone()
        })
    });
    if found.is_some() {
        return found;
    }
    let image = outlined_emoji_image(emoji, side, outline)?;
    let texture = ctx.load_texture(
        format!("tg_large_emoji_{}_{side}_{outline}", emoji.id()),
        image,
        TextureOptions::LINEAR,
    );
    ctx.data_mut(|data| {
        let cache = data.get_temp_mut_or_default::<LargeEmojiTextures>(id);
        if cache.textures.len() >= MAX_TEXTURES {
            let mut used: Vec<u64> = cache.textures.values().map(|(_, used)| *used).collect();
            let half = used.len() / 2;
            used.select_nth_unstable(half);
            let cutoff = used[half];
            cache.textures.retain(|_, (_, used)| *used > cutoff);
        }
        cache.textures.insert(key, (texture.clone(), pass));
    });
    Some(texture)
}

/// Paints `emoji` large in `row` (the emoji row `tg_history::large_emoji_inner_rect` places):
/// each `largeEmojiSize + 2 * largeEmojiOutline` cell, `largeEmojiSkip - 2 * outline` apart
/// (`LargeEmoji::draw`).
pub fn paint_large_emoji(ctx: &Context, painter: &Painter, emoji: &[EmojiRef], row: Rect) {
    let ppp = ctx.pixels_per_point();
    let (side, outline) = large_emoji_pixel_sizes(ppp);
    let image_pt = (side + 2 * outline) as f32 / ppp;
    let cell = (LARGE_EMOJI_SIZE + 2 * LARGE_EMOJI_OUTLINE) as f32;
    let step = cell + (LARGE_EMOJI_SKIP - 2 * LARGE_EMOJI_OUTLINE) as f32;
    let snap = |v: f32| (v * ppp).round() / ppp;
    let mut x = row.left();
    for &one in emoji {
        if let Some(texture) = texture_for(ctx, one, side, outline) {
            let rect = Rect::from_min_size(
                Pos2::new(snap(x), snap(row.top())),
                egui::vec2(image_pt, image_pt),
            );
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            painter.image(texture.id(), rect, uv, Color32::WHITE);
        }
        x += step;
    }
}

#[cfg(test)]
#[path = "large_emoji_tests.rs"]
#[cfg(test)]
mod tests;
