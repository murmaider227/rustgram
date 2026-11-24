//! Non-visual chat helper logic and data models ported from Telegram Desktop.
//!
//! Covers:
//! - [`autocomplete`]: Field autocomplete (`@mention`, `#hashtag`, `/command`, inline bot queries),
//!   ranking/filtering, hashtag history, recent inline bots, and insertion results.
//! - [`emoji`]: Recent emoji with usage weights and decay, skin tone variant preferences,
//!   multi-language emoji keyword search with diff application, legacy `:shortcode` completion,
//!   and typing trigger/replacement logic.
//! - [`stickers`]: Sticker sets, flags, installed/featured/archived orders, recent/faved stickers,
//!   saved GIFs, featured read state, emoji-based sticker search with premium interleaving,
//!   tabbed selector section/icon list building, and Lottie/sticker sizing.
//! - [`send_options`]: Send context menu rules (silent, schedule, send-when-online, reminders,
//!   media toggles, shortcuts) per chat type.
//! - [`media_prepare`]: File preparation validation, photo/video album dimension and aspect-ratio
//!   limits, compression flags (`SendFilesWay`), and album grouping (max 10 items, mixed media,
//!   slowmode rules).
//! - [`types`]: Shared chat helper domain types and re-exports from `tg-data` and `tg-text`.

#![deny(clippy::all)]

pub mod autocomplete;
pub mod emoji;
pub mod media_prepare;
pub mod send_options;
pub mod stickers;
pub mod types;

pub use types::{
    BotCommand, BotStatus, BusinessShortcut, BusinessShortcutId, ComposeFeatures, DocumentId,
    EffectId, LastseenStatus, MENTION_TAG_START, MentionNameEntity, PeerId, SECONDS_IN_DAY, Size,
    StickerSetId, TimeId, UserId, UserInfo, is_diacritic, remove_accents, sort_by_online_value,
    utf16_len,
};
