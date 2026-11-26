//! Images and colors of the inline reaction buttons under a message (TGB-3017).
//!
//! Port of the image half of `HistoryView::Reactions::InlineList::paint`
//! (`history/view/reactions/history_view_reactions.cpp:450-620`): a custom emoji reaction draws
//! the emoji document through the shared custom emoji renderer (`paintCustomFrame`), the paid
//! reaction draws the gold star (`Reactions::lookupPaid()->centerIcon`, the bundled
//! `star_reaction/center.tgs`) on a `creditsBg3` button with `creditsFg` text. A reaction key is
//! never drawn as text: before this, `custom:<document id>` fell through to the emoji painter's
//! text fallback and spilled over the whole row.
//!
//! Internal design (differs from tdesktop): the star is rasterized once per process into one
//! texture kept in egui's temp data, instead of a `Lottie::Icon` per `Reactions` instance.

use egui::{
    Color32, ColorImage, Context, Painter, Pos2, Rect, TextureHandle, TextureOptions, Ui, Vec2,
};

use crate::history::theme::HistoryColors;

/// UI key of the paid (Stars) reaction (`Data::ReactionId::Paid()`).
pub const PAID_REACTION_KEY: &str = "⭐";

const CUSTOM_PREFIX: &str = "custom:";

/// Bundled GPL-3.0 `Telegram/Resources/animations/star_reaction/center.tgs`.
pub const BUNDLED_STAR_CENTER_TGS: &[u8] =
    include_bytes!("../../assets/animations/star_reaction_center.tgs");

/// `st::emojiSize`.
const EMOJI_SIZE: f32 = 18.0;
/// `Ui::Text::AdjustCustomEmojiSize(st::emojiSize)`.
const CUSTOM_EMOJI_SIZE: f32 = 20.0;
/// `st::reactionInlineImage`.
const INLINE_IMAGE: f32 = 32.0;
/// Raster side of the star texture (sharp up to 4x scale at 16 logical px).
const STAR_RASTER_PX: u32 = 64;

/// `kInNonChosenOpacity` / `kOutNonChosenOpacity`.
const IN_NON_CHOSEN_OPACITY: f32 = 0.12;
const OUT_NON_CHOSEN_OPACITY: f32 = 0.18;
/// `creditsBg3`.
pub const CREDITS_BG3: Color32 = Color32::from_rgb(0xf0, 0xb4, 0x00);
/// `creditsFg`.
pub const CREDITS_FG: Color32 = Color32::from_rgb(0xba, 0x70, 0x00);

/// What a reaction button shows in its image square.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionImage<'a> {
    /// A regular emoji reaction.
    Emoji(&'a str),
    /// A custom emoji reaction (document id; `0` for a malformed key, which draws nothing).
    Custom(u64),
    /// The paid Stars reaction.
    Paid,
}

/// Classifies a reaction key (`"👍"`, `"custom:<document id>"`, [`PAID_REACTION_KEY`]).
#[must_use]
pub fn reaction_image(key: &str) -> ReactionImage<'_> {
    if key == PAID_REACTION_KEY {
        ReactionImage::Paid
    } else if let Some(id) = key.strip_prefix(CUSTOM_PREFIX) {
        ReactionImage::Custom(id.trim().parse().unwrap_or(0))
    } else {
        ReactionImage::Emoji(key)
    }
}

/// Where `paintCustomFrame` draws a custom emoji: `AdjustCustomEmojiSize(emojiSize)` at the
/// button's inner top-left shifted by `(emojiSize - adjusted) / 2`.
#[must_use]
pub fn custom_emoji_rect(inner_top_left: Pos2) -> Rect {
    let skip = ((EMOJI_SIZE - CUSTOM_EMOJI_SIZE) / 2.0).trunc();
    Rect::from_min_size(
        inner_top_left + Vec2::splat(skip),
        Vec2::splat(CUSTOM_EMOJI_SIZE),
    )
}

/// Where the paid star is drawn: `Reactions::resolveImageFor` renders the icon at half of
/// `st::reactionInlineImage` centered in that image, which `InlineList::paint` centers on the
/// `st::reactionInlineSize` square.
#[must_use]
pub fn paid_star_rect(inner_top_left: Pos2) -> Rect {
    let side = INLINE_IMAGE / 2.0;
    Rect::from_center_size(
        inner_top_left + Vec2::splat(EMOJI_SIZE / 2.0),
        Vec2::splat(side),
    )
}

/// Background and text colors of one in-bubble reaction button (`InlineList::paint`).
#[must_use]
pub fn reaction_button_colors(
    colors: &HistoryColors,
    chosen: bool,
    outgoing: bool,
    paid: bool,
) -> (Color32, Color32) {
    if paid {
        if chosen {
            (CREDITS_BG3, colors.reaction_chosen_fg)
        } else {
            let opacity = if outgoing {
                OUT_NON_CHOSEN_OPACITY
            } else {
                IN_NON_CHOSEN_OPACITY
            };
            (CREDITS_BG3.gamma_multiply(opacity), CREDITS_FG)
        }
    } else if chosen {
        (colors.reaction_chosen_bg, colors.reaction_chosen_fg)
    } else if outgoing {
        (colors.reaction_out_bg, colors.msg_out_date_fg)
    } else {
        (colors.reaction_in_bg, colors.active_text_fg)
    }
}

/// Paints the image of a custom emoji or paid reaction into the button whose inner (padding
/// removed) top-left is `inner_top_left`. Returns `false` for a regular emoji, which the caller
/// paints. A custom emoji whose document is not loaded yet leaves the square empty, like
/// tdesktop's custom emoji before its first frame.
pub fn paint_special_reaction_image(
    ui: &Ui,
    key: &str,
    inner_top_left: Pos2,
    text_color: Color32,
) -> bool {
    paint_special_reaction(
        ui.ctx(),
        ui.painter(),
        key,
        custom_emoji_rect(inner_top_left),
        paid_star_rect(inner_top_left),
        text_color,
    )
}

/// [`paint_special_reaction_image`] with explicit rects for the custom emoji and the star (the
/// context menu reaction strip uses its own cell geometry).
pub fn paint_special_reaction(
    ctx: &Context,
    painter: &Painter,
    key: &str,
    custom_rect: Rect,
    star_rect: Rect,
    text_color: Color32,
) -> bool {
    match reaction_image(key) {
        ReactionImage::Emoji(_) => false,
        ReactionImage::Custom(0) => true,
        ReactionImage::Custom(id) => {
            let id = id.to_string();
            if tg_ui::custom_emoji::has_registered_custom_emoji(&id) {
                let time = ctx.input(|i| i.time);
                let _ = tg_ui::custom_emoji::paint_registered_custom_emoji(
                    ctx,
                    painter,
                    &id,
                    custom_rect,
                    tg_ui::custom_emoji::CustomEmojiPaintContext::playing(time, text_color),
                );
            }
            true
        }
        ReactionImage::Paid => {
            if let Some(tex) = paid_star_texture(ctx) {
                painter.image(
                    tex.id(),
                    star_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            true
        }
    }
}

/// The first frame of the bundled star (`Lottie::Icon::frame` of `centerIcon`), uploaded once.
fn paid_star_texture(ctx: &Context) -> Option<TextureHandle> {
    let id = egui::Id::unique("tg_paid_reaction_star_texture");
    if let Some(cached) = ctx.data(|d| d.get_temp::<Option<TextureHandle>>(id)) {
        return cached;
    }
    let texture = crate::history::birthday_cake::rasterize_cake_tgs_frame(
        BUNDLED_STAR_CENTER_TGS,
        STAR_RASTER_PX,
        Some(0),
    )
    .map(|(w, h, rgba)| {
        ctx.load_texture(
            "tg_paid_reaction_star",
            ColorImage::from_rgba_premultiplied([w as usize, h as usize], &rgba),
            TextureOptions::LINEAR,
        )
    });
    ctx.data_mut(|d| d.insert_temp(id, texture.clone()));
    texture
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_app_core::stand_in::theme::{Palette, ThemeMode};

    #[test]
    fn reaction_keys_are_classified() {
        assert_eq!(reaction_image("👍"), ReactionImage::Emoji("👍"));
        assert_eq!(
            reaction_image("custom:5199576980080601299"),
            ReactionImage::Custom(5_199_576_980_080_601_299)
        );
        assert_eq!(reaction_image("custom:junk"), ReactionImage::Custom(0));
        assert_eq!(reaction_image(PAID_REACTION_KEY), ReactionImage::Paid);
    }

    #[test]
    fn custom_and_paid_images_stay_inside_the_button() {
        // Button at (100, 50): inner top-left = padding (5, 2).
        let inner = Pos2::new(105.0, 52.0);
        let custom = custom_emoji_rect(inner);
        assert_eq!(
            custom,
            Rect::from_min_size(Pos2::new(104.0, 51.0), Vec2::splat(20.0))
        );
        let star = paid_star_rect(inner);
        assert_eq!(
            star,
            Rect::from_min_size(Pos2::new(106.0, 53.0), Vec2::splat(16.0))
        );
    }

    #[test]
    fn the_paid_button_is_gold() {
        let colors = HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day));
        let (bg, fg) = reaction_button_colors(&colors, false, false, true);
        assert_eq!(fg, CREDITS_FG);
        assert_eq!(bg, CREDITS_BG3.gamma_multiply(IN_NON_CHOSEN_OPACITY));
        let (bg, _) = reaction_button_colors(&colors, true, false, true);
        assert_eq!(bg, CREDITS_BG3);
        let (bg, _) = reaction_button_colors(&colors, false, false, false);
        assert_eq!(bg, colors.reaction_in_bg);
    }

    #[test]
    fn the_bundled_star_decodes() {
        let (w, h, rgba) = crate::history::birthday_cake::rasterize_cake_tgs_frame(
            BUNDLED_STAR_CENTER_TGS,
            STAR_RASTER_PX,
            Some(0),
        )
        .expect("star frame");
        assert_eq!((w, h), (STAR_RASTER_PX, STAR_RASTER_PX));
        assert!(
            rgba.as_chunks::<4>().0.iter().any(|px| px[3] > 200),
            "the star is visible"
        );
    }
}
