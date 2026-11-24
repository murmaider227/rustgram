//! `tg-emoji`: Apache-2.0 Noto Emoji packed sprite atlas and `egui` rendering helpers.
//!
//! Port origin: `Telegram/lib_ui/ui/emoji_config.cpp` (sprite sheet indexing and drawing),
//! using Noto Emoji PNG artwork from `googlefonts/noto-emoji` (`v2.042`, Apache-2.0).

#![deny(clippy::all)]

pub mod atlas;
pub mod egui_ext;

pub use atlas::{
    AtlasStats, CELL_SIZE_PX, EMOJIS_PER_SHEET, EmojiAtlas, EmojiError, EmojiSheetImage,
    EmojiSprite, SHEET_COLS, SHEET_COUNT, SHEET_HEIGHT_PX, SHEET_ROWS, SHEET_WIDTH_PX,
};
pub use egui_ext::{EmojiTextureCache, InlineEmojiSegment, split_inline_emoji};

#[cfg(test)]
mod tests {
    use super::*;
    use tg_text::emoji;

    #[test]
    fn atlas_covers_core_emoji_and_skin_tone_variants() {
        let atlas = EmojiAtlas::new();
        let stats = atlas.stats();
        assert_eq!(stats.sheet_count, SHEET_COUNT);
        assert_eq!(stats.cell_size_px, CELL_SIZE_PX);
        assert_eq!(stats.total_entries, emoji::full_count());
        // Over 88% of tg-text's emoji table must be directly present in the Apache-2.0 v2.042 atlas.
        assert!(
            stats.present_entries * 100 / stats.total_entries >= 88,
            "expected >=88% coverage, got {}/{}",
            stats.present_entries,
            stats.total_entries
        );

        // Common emojis and default recent entries must resolve.
        for seq in ["😀", "😂", "❤️", "🔥", "🚀", "👍", "👍🏻", "👍🏽", "👍🏿", "👋🏾"]
        {
            let sprite = atlas
                .sprite_for_str(seq)
                .unwrap_or_else(|| panic!("missing sprite for {seq}"));
            assert!(sprite.sheet < SHEET_COUNT);
            assert!(sprite.col < SHEET_COLS);
            assert!(sprite.row < SHEET_ROWS);
            assert!(sprite.uv[0] < sprite.uv[2]);
            assert!(sprite.uv[1] < sprite.uv[3]);
        }

        // Skin tone variants must map to distinct cells.
        let base = atlas.sprite_for_str("👍").unwrap();
        let light = atlas.sprite_for_str("👍🏻").unwrap();
        let dark = atlas.sprite_for_str("👍🏿").unwrap();
        assert_ne!(base.emoji_index, light.emoji_index);
        assert_ne!(light.emoji_index, dark.emoji_index);
    }

    #[test]
    fn sheet_decoding_and_single_emoji_rgba_non_empty() {
        let atlas = EmojiAtlas::new();
        let grin = emoji::find_emoji("😀", 0).unwrap().0;
        let rgba = atlas.emoji_rgba(grin).unwrap().unwrap();
        assert_eq!(rgba.len(), (CELL_SIZE_PX * CELL_SIZE_PX * 4) as usize);
        // Center pixel of grinning face must be opaque (non-zero alpha).
        let center_alpha =
            rgba[((CELL_SIZE_PX / 2) * CELL_SIZE_PX + CELL_SIZE_PX / 2) as usize * 4 + 3];
        assert!(
            center_alpha > 200,
            "expected opaque center pixel, got alpha={center_alpha}"
        );

        // Out-of-bounds sheet returns structured error.
        assert!(matches!(
            atlas.decode_sheet(SHEET_COUNT),
            Err(EmojiError::SheetOutOfBounds(SHEET_COUNT))
        ));
    }

    #[test]
    fn split_inline_emoji_handles_mixed_text_and_variants() {
        let segs = split_inline_emoji("Hi 👋🏽 world 🚀!");
        assert_eq!(segs.len(), 5);
        assert_eq!(segs[0], InlineEmojiSegment::Text("Hi "));
        assert!(matches!(
            segs[1],
            InlineEmojiSegment::Emoji {
                raw: "👋🏽", ..
            }
        ));
        assert_eq!(segs[2], InlineEmojiSegment::Text(" world "));
        assert!(matches!(
            segs[3],
            InlineEmojiSegment::Emoji { raw: "🚀", .. }
        ));
        assert_eq!(segs[4], InlineEmojiSegment::Text("!"));
    }
}
