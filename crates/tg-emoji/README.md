# `tg-emoji`

Telegram Desktop's emoji sprite sheets, split into lazily decoded tiles, and `egui` inline
rendering helpers for Rustgram.

## Where the images come from, and their license

`assets/emoji_1.webp` .. `assets/emoji_8.webp` are Telegram Desktop's built-in emoji sheets
(`Telegram/Resources/emoji/`, tdesktop 7.4.5), copied byte for byte. tdesktop's build draws them
from Apple's "Apple Color Emoji" font. **The artwork is Apple's: it is not covered by Rustgram's
GPL-3.0 license and the Rustgram authors do not license it.** See [LICENSE-EMOJI](LICENSE-EMOJI),
which also says which commit to revert to switch back to the Apache-2.0 Noto Emoji set.

## Atlas Layout

- **8 WebP sprite sheets** (`assets/emoji_1.webp` .. `assets/emoji_8.webp`), 9.6 MB in the repo
- **Cell size**: `72 × 72 px` (tdesktop's largest size, kept without resampling), **32 columns × 16 rows** (`512` emojis per sheet, `2304 × 1152 px` per sheet)
- **Indexing**: direct O(1) mapping from `tg_text::emoji::EmojiRef` (`sprite = index >> 9`, `row = (index >> 5) & 0x0F`, `col = index & 0x1F`), the order tdesktop's codegen draws the sheets in (`tests_emoji_order`)
- **Tiles**: `build.rs` decodes the sheets once and embeds each sheet row as a lossless PNG tile (`2304 × 72 px`, 32 emojis, `tile = index >> 5`), so the first use of an emoji decodes one row (~1 ms) instead of a whole sheet. Colour channels hidden by premultiplied alpha are normalised; the textures egui uploads are byte-identical to the sheet path (`tests_tile_pixels`).
- **Coverage**: all 3,900 entries of `tg-text`'s table have their own cell; `build.rs` writes the presence bitmap from the sheets' alpha.

## Usage

```rust
use tg_emoji::{EmojiAtlas, EmojiTextureCache};

let atlas = EmojiAtlas::global();
let sprite = atlas.sprite_for_str("🚀").unwrap();
// In egui:
let mut cache = EmojiTextureCache::new();
// cache.paint_emoji_str(ui.painter(), ui.ctx(), "👍🏽", rect, egui::Color32::WHITE);
// cache.paint_wrapped_text_with_emoji(ui, "Hello 👋🏽 world 🚀", &font, color, max_w);
```
