//! Custom emoji status after a peer's name: chat-list rows (`Ui::PeerBadge::drawPremiumEmojiStatus`,
//! `ui/unread_badge.cpp`) and the profile header (`Info::Profile::Badge`,
//! `info/profile/info_profile_badge.cpp`).
//!
//! The emoji comes from `tg_ui`'s shared custom emoji registry, which the live session fills in
//! the background; until the document is registered nothing is painted here and the caller keeps
//! the premium star in its place.

use egui::{Color32, Id, Painter, Pos2, Rect, Vec2};
use tg_ui::custom_emoji::{
    CustomEmojiPaintContext, has_registered_custom_emoji, paint_registered_custom_emoji,
    registered_custom_emoji_loop_secs,
};

/// `st::emojiSize`.
const EMOJI_SIZE: i32 = 18;
/// `Ui::Text::AdjustCustomEmojiSize(st::emojiSize)`: the custom emoji frame.
pub const CUSTOM_EMOJI_SIZE: i32 = 20;
/// `EmojiStatus::skip = (emojiSize - AdjustCustomEmojiSize(emojiSize)) / 2`.
const SKIP: i32 = (EMOJI_SIZE - CUSTOM_EMOJI_SIZE) / 2;
/// `st::dialogsPremiumIcon` width.
const PREMIUM_ICON_WIDTH: i32 = 18;
/// Width a chat-list row gives the status after the name: `premium->width() - 4 * skip`.
pub const ROW_STATUS_WIDTH: i32 = PREMIUM_ICON_WIDTH - 4 * SKIP;
/// `kPlayStatusLimit`: a row's status is a `LimitedLoopsEmoji` that plays twice, then stops on
/// its first frame.
pub const ROW_PLAY_LOOPS: f64 = 2.0;

/// How long a status animates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StatusLoops {
    /// `LimitedLoopsEmoji(…, n)`: `n` loops from the first time it is painted, then frame 0.
    Limited(f64),
    /// A plain custom emoji that loops forever (the profile header's `Badge` without a limit).
    Forever,
}

/// Top-left of the emoji frame for a badge whose premium icon would sit at `icon_left_top`
/// (`lastPosition = (iconx - 2 * skip, icony + skip)`).
#[must_use]
pub fn status_rect(icon_left_top: Pos2) -> Rect {
    Rect::from_min_size(
        Pos2::new(
            icon_left_top.x - 2.0 * SKIP as f32,
            icon_left_top.y + SKIP as f32,
        ),
        Vec2::splat(CUSTOM_EMOJI_SIZE as f32),
    )
}

/// Whether the status `document_id` can be painted now.
#[must_use]
pub fn is_loaded(document_id: u64) -> bool {
    has_registered_custom_emoji(&document_id.to_string())
}

/// Paints the status `document_id` into `rect` tinted with `tint` (for text-coloured emoji);
/// returns `false` when the document is not registered yet. `key` identifies the badge so the
/// limited loops count from the first frame this badge painted.
pub fn paint_status(
    painter: &Painter,
    key: Id,
    document_id: u64,
    rect: Rect,
    tint: Color32,
    time_secs: f64,
    loops: StatusLoops,
) -> bool {
    let id = document_id.to_string();
    if !has_registered_custom_emoji(&id) {
        return false;
    }
    let paint_ctx = match loops {
        StatusLoops::Forever => CustomEmojiPaintContext::playing(time_secs, tint),
        StatusLoops::Limited(limit) => {
            let started = painter.ctx().data_mut(|d| {
                *d.get_temp_mut_or_insert_with(
                    key.with(("emoji_status_start", document_id)),
                    || time_secs,
                )
            });
            let elapsed = (time_secs - started).max(0.0);
            let done =
                registered_custom_emoji_loop_secs(&id).is_none_or(|secs| elapsed >= secs * limit);
            CustomEmojiPaintContext {
                time_secs: elapsed,
                paused: done,
                force_first_frame: done,
                force_last_frame: false,
                text_color: tint,
            }
        }
    };
    paint_registered_custom_emoji(&painter.ctx().clone(), painter, &id, rect, paint_ctx)
}
