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

/// One emoji of a large-emoji message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LargeEmojiItem {
    /// A regular emoji, drawn from the emoji sheets with a white outline.
    Emoji(EmojiRef),
    /// A custom emoji: its document id (`EntityInText::data`).
    Custom(String),
}

/// How a message of only emoji is drawn without a bubble (`Element::refreshMedia`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LargeEmoji {
    /// `HistoryView::CustomEmoji`: only custom emoji, any number per line, one entry per line
    /// (`Ui::Text::OnlyCustomEmoji`, checked first).
    OnlyCustom(Vec<Vec<String>>),
    /// `HistoryView::LargeEmoji`: 1-3 emoji, regular or custom (`Ui::Text::IsolatedEmoji`).
    Isolated(IsolatedEmoji),
}

impl LargeEmoji {
    /// The `tg-history` layout input of this content.
    #[must_use]
    pub fn media_input(&self, bottom_info_width: i32) -> tg_history::MediaInput {
        match self {
            Self::Isolated(items) => {
                tg_history::MediaInput::LargeEmoji(tg_history::LargeEmojiInput {
                    count: items.len(),
                    bottom_info_width,
                })
            }
            Self::OnlyCustom(lines) => {
                tg_history::MediaInput::CustomEmoji(tg_history::CustomEmojiInput {
                    lines: lines.iter().map(Vec::len).collect(),
                    bottom_info_width,
                })
            }
        }
    }

    /// `alwaysShowOutTimestamp` of the content: always for `LargeEmoji`, for `CustomEmoji` only
    /// with one line of more than three emoji.
    #[must_use]
    pub fn always_show_out_timestamp(&self) -> bool {
        match self {
            Self::Isolated(_) => true,
            Self::OnlyCustom(lines) => lines.len() == 1 && lines[0].len() > 3,
        }
    }
}

/// The emoji of a message drawn large, `1..=ISOLATED_EMOJI_LIMIT` of them.
pub type IsolatedEmoji = Vec<LargeEmojiItem>;

/// Byte ranges and document ids of the custom emoji of `text`, in order; `None` when two
/// overlap or one is outside the text.
fn custom_emoji_ranges(text: &TextWithEntities) -> Option<Vec<(std::ops::Range<usize>, &str)>> {
    let mut ranges = Vec::new();
    for entity in &text.entities {
        if entity.entity_type() != EntityType::CustomEmoji {
            continue;
        }
        let offset = usize::try_from(entity.offset()).ok()?;
        let length = usize::try_from(entity.length()).ok().filter(|&l| l > 0)?;
        let range = tg_text::utf16::utf16_range_to_utf8(&text.text, offset, length)?;
        ranges.push((range, entity.data()));
    }
    ranges.sort_by_key(|(range, _)| range.start);
    ranges
        .windows(2)
        .all(|pair| pair[0].0.end <= pair[1].0.start)
        .then_some(ranges)
}

/// What `text` is drawn as when it is only emoji (`String::toOnlyCustomEmoji`, then
/// `String::toIsolatedEmoji` after `BlockParser::finalize`): spaces may follow an emoji, a
/// newline separates lines of custom emoji, any other character, a link or a spoiler makes it
/// ordinary text.
#[must_use]
pub fn large_emoji_text(text: &TextWithEntities) -> Option<LargeEmoji> {
    let disqualifying = text.entities.iter().any(|entity| {
        // Links get a link index, spoilers, code and quotes their own blocks.
        !matches!(
            entity.entity_type(),
            EntityType::Bold
                | EntityType::Semibold
                | EntityType::Italic
                | EntityType::Underline
                | EntityType::StrikeOut
                | EntityType::CustomEmoji
        )
    });
    if disqualifying {
        return None;
    }
    let mut customs = custom_emoji_ranges(text)?.into_iter().peekable();
    let s = text.text.as_str();
    let mut lines: Vec<Vec<LargeEmojiItem>> = vec![Vec::new()];
    let mut pos = 0usize;
    while pos < s.len() {
        let line = lines.last_mut()?;
        if let Some((range, id)) = customs.peek() {
            if range.start == pos {
                line.push(LargeEmojiItem::Custom((*id).to_owned()));
                pos = range.end;
                customs.next();
                continue;
            }
            if range.start < pos {
                return None;
            }
        }
        if let Some((emoji, len)) = find_emoji(s, pos)
            && len > 0
            && customs
                .peek()
                .is_none_or(|(range, _)| pos + len <= range.start)
        {
            line.push(LargeEmojiItem::Emoji(emoji));
            pos += len;
            continue;
        }
        let ch = s[pos..].chars().next()?;
        pos += ch.len_utf8();
        if ch == '\n' {
            lines.push(Vec::new());
            continue;
        }
        let joins_emoji = matches!(
            ch,
            '\u{200D}' | '\u{FE0E}' | '\u{FE0F}' | '\u{E0020}'..='\u{E007F}'
        );
        // A space joins the emoji block before it; a leading one is a text block of its own.
        let trailing_space = !line.is_empty() && ch.is_whitespace() && ch != '\u{A0}';
        if !joins_emoji && !trailing_space {
            return None;
        }
    }
    let total: usize = lines.iter().map(Vec::len).sum();
    if total == 0 {
        return None;
    }
    let all_custom = lines
        .iter()
        .flatten()
        .all(|item| matches!(item, LargeEmojiItem::Custom(_)));
    if all_custom {
        return Some(LargeEmoji::OnlyCustom(
            lines
                .into_iter()
                .map(|line| {
                    line.into_iter()
                        .filter_map(|item| match item {
                            LargeEmojiItem::Custom(id) => Some(id),
                            LargeEmojiItem::Emoji(_) => None,
                        })
                        .collect()
                })
                .collect(),
        ));
    }
    if lines.len() == 1 && total <= ISOLATED_EMOJI_LIMIT {
        lines.pop().map(LargeEmoji::Isolated)
    } else {
        None
    }
}

/// What `msg` is drawn as when "Large emoji" is `enabled` and the message is only emoji text
/// (no media, link preview or rich page).
#[must_use]
pub fn large_emoji_of(msg: &HistoryMessage, enabled: bool) -> Option<LargeEmoji> {
    if !enabled
        || msg.media.is_some()
        || msg.service.is_some()
        || msg.extra.link_preview.is_some()
        || msg.extra.rich_message.is_some()
    {
        return None;
    }
    large_emoji_text(&msg.text)
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

/// `Images::Colored`: lays `add` over the image where it is opaque (a selected sticker or
/// large emoji gets `msgStickerOverlay`), in tdesktop's fixed-point arithmetic on premultiplied
/// pixels. Alpha is kept.
pub fn colored(image: &mut ColorImage, add: Color32) {
    let [r, g, b, ca] = add.to_srgba_unmultiplied().map(u32::from);
    let (cr, cg, cb) = (r * (ca + 1), g * (ca + 1), b * (ca + 1));
    let ra = (0x100 - ca) * 0x100;
    for px in &mut image.pixels {
        let [pr, pg, pb, pa] = px.to_array().map(u32::from);
        let a = pa + 1;
        // Each sum is at most 0x100 * 0xFF * 0x100 + 0x100 * 0xFF * 0x100 < 2^25, and the
        // result after `>> 16` is at most 0xFF.
        let mix = |p: u32, c: u32| ((ra * p + a * c) >> 16).min(0xFF) as u8;
        *px = Color32::from_rgba_premultiplied(mix(pr, cr), mix(pg, cg), mix(pb, cb), pa as u8);
    }
}

/// What a large emoji texture is: the emoji, its pixel side and outline, and the overlay it is
/// coloured with when the message is selected.
type TextureKey = (&'static str, u32, u32, Option<Color32>);

/// Uploaded large emoji images, with the pass that last painted each.
#[derive(Clone, Default)]
struct LargeEmojiTextures {
    textures: HashMap<TextureKey, (TextureHandle, u64)>,
}

/// Textures kept before the least recently painted half goes.
const MAX_TEXTURES: usize = 256;

fn texture_for(
    ctx: &Context,
    emoji: EmojiRef,
    side: u32,
    outline: u32,
    overlay: Option<Color32>,
) -> Option<TextureHandle> {
    let id = Id::unique("tg_history_large_emoji_textures");
    let key = (emoji.id(), side, outline, overlay);
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
    let mut image = outlined_emoji_image(emoji, side, outline)?;
    if let Some(add) = overlay {
        colored(&mut image, add);
    }
    let texture = ctx.load_texture(
        format!(
            "tg_large_emoji_{}_{side}_{outline}{}",
            emoji.id(),
            if overlay.is_some() { "_selected" } else { "" }
        ),
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
/// (`LargeEmoji::draw`). A selected message passes `msgStickerOverlay` as `overlay`; custom
/// emoji take `text_color` where they are tinted.
pub fn paint_large_emoji(
    ctx: &Context,
    painter: &Painter,
    emoji: &[LargeEmojiItem],
    row: Rect,
    overlay: Option<Color32>,
    text_color: Color32,
) {
    let ppp = ctx.pixels_per_point();
    let (side, outline) = large_emoji_pixel_sizes(ppp);
    let image_pt = (side + 2 * outline) as f32 / ppp;
    let cell = (LARGE_EMOJI_SIZE + 2 * LARGE_EMOJI_OUTLINE) as f32;
    let step = cell + (LARGE_EMOJI_SKIP - 2 * LARGE_EMOJI_OUTLINE) as f32;
    let snap = |v: f32| (v * ppp).round() / ppp;
    let mut x = row.left();
    for one in emoji {
        match one {
            LargeEmojiItem::Emoji(one) => {
                if let Some(texture) = texture_for(ctx, *one, side, outline, overlay) {
                    let rect = Rect::from_min_size(
                        Pos2::new(snap(x), snap(row.top())),
                        egui::vec2(image_pt, image_pt),
                    );
                    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                    painter.image(texture.id(), rect, uv, Color32::WHITE);
                }
            }
            LargeEmojiItem::Custom(id) => {
                // `LargeEmoji::paintCustom`: the `Isolated` custom emoji frame
                // (`AdjustCustomEmojiSize` of the cell) centred on the cell.
                let outer = tg_history::adjust_custom_emoji_size(
                    LARGE_EMOJI_SIZE + 2 * LARGE_EMOJI_OUTLINE,
                ) as f32;
                let skip = ((cell - outer) / 2.0).trunc();
                let rect = Rect::from_min_size(
                    Pos2::new(snap(x + skip), snap(row.top() + skip)),
                    egui::vec2(outer, outer),
                );
                paint_custom(ctx, painter, id, rect, text_color);
            }
        }
        x += step;
    }
}

/// Paints the custom emoji `id` into `rect` from the shared registry once its document is
/// loaded (the live session's custom emoji loader registers it), animated.
pub fn paint_custom(ctx: &Context, painter: &Painter, id: &str, rect: Rect, text_color: Color32) {
    if !tg_ui::custom_emoji::has_registered_custom_emoji(id) {
        return;
    }
    let time = ctx.input(|i| i.time);
    let _ = tg_ui::custom_emoji::paint_registered_custom_emoji(
        ctx,
        painter,
        id,
        rect,
        tg_ui::custom_emoji::CustomEmojiPaintContext::playing(time, text_color),
    );
}

/// Paints a message of only custom emoji (`CustomEmoji::draw`) in `media_rect` (element
/// coordinates, `origin` is the element's top-left on screen): the lines wrapped to the media
/// width, each emoji a square of the side its count gives.
pub fn paint_only_custom(
    ctx: &Context,
    painter: &Painter,
    lines: &[Vec<String>],
    media_rect: tg_history::geometry::Rect,
    has_right_layout: bool,
    origin: Pos2,
    text_color: Color32,
) {
    let input = tg_history::CustomEmojiInput {
        lines: lines.iter().map(Vec::len).collect(),
        bottom_info_width: 0,
    };
    let content = input.current_size(media_rect.width);
    let inner = tg_history::unwrapped_content_rect(media_rect, content, has_right_layout);
    for (line, index, cell) in input.cells(inner) {
        let Some(id) = lines.get(line).and_then(|l| l.get(index)) else {
            continue;
        };
        let rect = Rect::from_min_size(
            origin + egui::vec2(cell.x as f32, cell.y as f32),
            egui::vec2(cell.width as f32, cell.height as f32),
        );
        paint_custom(ctx, painter, id, rect, text_color);
    }
}

#[cfg(test)]
#[path = "large_emoji_tests.rs"]
#[cfg(test)]
mod tests;
