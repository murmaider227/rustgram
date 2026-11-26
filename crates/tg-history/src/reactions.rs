//! Reaction chips under a message (`HistoryView::Reactions::InlineList`,
//! `history/view/reactions/history_view_reactions.cpp`).
//!
//! The chips flow left to right and wrap onto new rows. When they sit inside the bubble, the
//! bottom info (views, edited mark, time) takes a skip block after the last chip, like the skip
//! block after the last text line: it stays on the last row when it fits and gets a short row
//! of its own otherwise (`InlineList::countCurrentSize`).

use crate::geometry::{Rect, Size};

/// `st::reactionInlinePadding` (`margins(5px, 2px, 7px, 2px)`).
pub const REACTION_INLINE_PADDING_LEFT: i32 = 5;
/// `st::reactionInlinePadding.top()`.
pub const REACTION_INLINE_PADDING_TOP: i32 = 2;
/// `st::reactionInlinePadding.right()`.
pub const REACTION_INLINE_PADDING_RIGHT: i32 = 7;
/// `st::reactionInlinePadding.bottom()`.
pub const REACTION_INLINE_PADDING_BOTTOM: i32 = 2;
/// `st::reactionInlineSize` (the emoji square).
pub const REACTION_INLINE_SIZE: i32 = 18;
/// `st::reactionInlineSkip` (emoji to count).
pub const REACTION_INLINE_SKIP: i32 = 3;
/// `st::reactionInlineBetween` (between chips, horizontally and vertically).
pub const REACTION_INLINE_BETWEEN: i32 = 4;
/// `st::reactionInlineInBubbleLeft`: chips inside a bubble start 3px left of the text.
pub const REACTION_INLINE_IN_BUBBLE_LEFT: i32 = -3;
/// `st::reactionInlineEmptySkip`.
pub const REACTION_INLINE_EMPTY_SKIP: i32 = 2;

/// Chip height: `padding.top() + reactionInlineSize + padding.bottom()`.
pub const REACTION_INLINE_HEIGHT: i32 =
    REACTION_INLINE_PADDING_TOP + REACTION_INLINE_SIZE + REACTION_INLINE_PADDING_BOTTOM;

/// Width of a chip that shows a count (`widthBaseCount + textWidth`, `count_text_width` is the
/// `st::semiboldFont` width of `Lang::FormatCountToShort(count)`).
#[must_use]
pub const fn reaction_count_button_width(count_text_width: i32) -> i32 {
    if count_text_width <= 0 {
        return REACTION_INLINE_PADDING_LEFT + REACTION_INLINE_SIZE + REACTION_INLINE_PADDING_RIGHT
            - REACTION_INLINE_EMPTY_SKIP;
    }
    REACTION_INLINE_PADDING_LEFT
        + REACTION_INLINE_SIZE
        + REACTION_INLINE_SKIP
        + REACTION_INLINE_PADDING_RIGHT
        + count_text_width
}

/// Placed chips of one reactions block, in block coordinates (`(0, 0)` = the block's top-left;
/// chips inside a bubble start at `x = -3`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InlineReactionsLayout {
    /// One rectangle per chip, in input order.
    pub buttons: Vec<Rect>,
    /// Block height, including the bottom-info row when the info did not fit on the last row.
    pub height: i32,
    /// `true` when the bottom info got a row of its own below the chips.
    pub info_on_own_row: bool,
}

const fn block_left(in_bubble: bool) -> i32 {
    if in_bubble {
        REACTION_INLINE_IN_BUBBLE_LEFT
    } else {
        0
    }
}

/// `InlineList::countOptimalSize`: one row of chips plus the skip block.
#[must_use]
pub fn inline_reactions_optimal_size(button_widths: &[i32], skip: Size, in_bubble: bool) -> Size {
    if button_widths.is_empty() {
        return skip;
    }
    let mut x = block_left(in_bubble);
    for w in button_widths {
        x += w + REACTION_INLINE_BETWEEN;
    }
    Size::new(
        x - REACTION_INLINE_BETWEEN + skip.width,
        REACTION_INLINE_HEIGHT.max(skip.height),
    )
}

/// `InlineList::countCurrentSize`: wraps the chips at `width` and adds the skip block's height
/// when the bottom info does not fit after the last chip.
#[must_use]
pub fn layout_inline_reactions(
    button_widths: &[i32],
    skip: Size,
    width: i32,
    in_bubble: bool,
) -> InlineReactionsLayout {
    if button_widths.is_empty() {
        return InlineReactionsLayout {
            buttons: Vec::new(),
            height: skip.height,
            info_on_own_row: false,
        };
    }
    let left = block_left(in_bubble);
    let mut x = left;
    let mut y = 0;
    let mut buttons = Vec::with_capacity(button_widths.len());
    for &w in button_widths {
        if x > left && x + w > left + width {
            x = left;
            y += REACTION_INLINE_HEIGHT + REACTION_INLINE_BETWEEN;
        }
        buttons.push(Rect::new(x, y, w, REACTION_INLINE_HEIGHT));
        x += w + REACTION_INLINE_BETWEEN;
    }
    let last = buttons[buttons.len() - 1];
    let right = last.x + last.width + skip.width;
    let info_on_own_row = right > width;
    let height = y + last.height + if info_on_own_row { skip.height } else { 0 };
    InlineReactionsLayout {
        buttons,
        height,
        info_on_own_row,
    }
}

/// `Lang::FormatCountToShort` (`lang/lang_tag.cpp`): full numbers below 10 000, then one
/// truncated decimal: `9999`, `14.5K`, `150.4K`, `2K`, `1.6M`, `3B`.
#[must_use]
pub fn format_count_to_short(count: i64) -> String {
    let abs = count.unsigned_abs();
    let sign = if count < 0 { "-" } else { "" };
    let (div, suffix) = if abs >= 1_000_000_000 {
        (1_000_000_000, 'B')
    } else if abs >= 1_000_000 {
        (1_000_000, 'M')
    } else if abs >= 10_000 {
        (1_000, 'K')
    } else {
        return count.to_string();
    };
    let rounded = abs / (div / 10);
    if !rounded.is_multiple_of(10) {
        format!("{sign}{}.{}{suffix}", rounded / 10, rounded % 10)
    } else {
        format!("{sign}{}{suffix}", rounded / 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_chip_width_matches_tdesktop_style() {
        // 5 + 18 + 3 + 7 + text.
        assert_eq!(reaction_count_button_width(20), 53);
        assert_eq!(REACTION_INLINE_HEIGHT, 22);
    }

    #[test]
    fn chips_wrap_and_info_takes_own_row_when_it_does_not_fit() {
        let widths = [50, 50, 50];
        let skip = Size::new(60, 12);
        // In-bubble block starts at -3: chips at -3, 51, 105; last right = 155; + 60 > 200.
        let l = layout_inline_reactions(&widths, skip, 200, true);
        assert_eq!(l.buttons[0], Rect::new(-3, 0, 50, 22));
        assert_eq!(l.buttons[2], Rect::new(105, 0, 50, 22));
        assert!(l.info_on_own_row);
        assert_eq!(l.height, 22 + 12);
        // Wide enough: the info shares the last row.
        let l = layout_inline_reactions(&widths, skip, 260, true);
        assert!(!l.info_on_own_row);
        assert_eq!(l.height, 22);
        // Narrow: the third chip wraps to a second row (`y += height + between`).
        let l = layout_inline_reactions(&widths, skip, 120, true);
        assert_eq!(l.buttons[2], Rect::new(-3, 26, 50, 22));
        assert_eq!(l.height, 26 + 22);
    }

    #[test]
    fn optimal_size_is_one_row_plus_skip() {
        let s = inline_reactions_optimal_size(&[50, 50], Size::new(60, 12), true);
        assert_eq!(s, Size::new(-3 + 50 + 4 + 50 + 60, 22));
    }

    #[test]
    fn short_counts() {
        assert_eq!(format_count_to_short(301), "301");
        assert_eq!(format_count_to_short(1616), "1616");
        assert_eq!(format_count_to_short(14_512), "14.5K");
        assert_eq!(format_count_to_short(150_400), "150.4K");
        assert_eq!(format_count_to_short(20_000), "20K");
        assert_eq!(format_count_to_short(1_600_000), "1.6M");
    }
}
