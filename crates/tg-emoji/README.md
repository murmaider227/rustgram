# `tg-emoji`

Packed Noto Emoji sprite sheet atlas and `egui` inline rendering helpers for Rustgram.

## Attribution & License

The emoji artwork packed into `assets/sheet_0.webp`..`assets/sheet_7.webp` is derived from **Google Noto Emoji** (`googlefonts/noto-emoji`, tag `v2.042`, `png/72`), licensed under the **Apache License, Version 2.0**.

- Upstream repository: <https://github.com/googlefonts/noto-emoji>
- Release tag: `v2.042` (commit `d79d23e`)
- License text: see [LICENSE-NOTO](LICENSE-NOTO) (Apache License, Version 2.0)

Telegram Desktop's Apple-style emoji artwork is **not** used or redistributed here.

## Atlas Layout

- **8 WebP sprite sheets** (`assets/sheet_0.webp` .. `assets/sheet_7.webp`), total ~5.3 MB
- **Cell size**: `64 × 64 px`, **32 columns × 16 rows** (`512` emojis per sheet, `2048 × 1024 px` per sheet)
- **Indexing**: direct O(1) mapping from `tg_text::emoji::EmojiRef` (`sprite = index >> 9`, `row = (index >> 5) & 0x0F`, `col = index & 0x1F`)
- **Coverage**: 3,499 / 3,900 entries directly present (3,484 exact Noto `v2.042` matches + 15 fallbacks), with automatic fallback to `EmojiRef::original()` for rare multi-skin-tone sequences not present in `v2.042`.

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
