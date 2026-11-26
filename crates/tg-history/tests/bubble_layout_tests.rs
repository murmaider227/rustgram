//! Bubble geometry checked against tdesktop's style constants (TGB-2953): the bottom info skip
//! block after text and after reaction chips, chip wrapping, attached-message gaps.

use tg_history::consts::{
    MEDIA_IN_BUBBLE_SKIP, MSG_DATE_DELTA, MSG_DATE_SPACE, MSG_MARGIN, MSG_MARGIN_TOP_ATTACHED,
    MSG_PADDING,
};
use tg_history::*;

fn channel_post_with_chips(chips: Vec<i32>) -> MessageInput {
    MessageInput {
        is_post: true,
        text_max_width: 420,
        text_min_height: 18,
        text_wrapped_height: 18 * 6,
        text_real_width: 420,
        bottom_info: BottomInfoInput {
            author_edited_date_width: 30,
            views_text_width: 30,
            ..Default::default()
        },
        reaction_button_widths: chips,
        reactions_in_bubble: true,
        ..Default::default()
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

/// `Element::skipBlockWidth()` is `msgDateSpace + infoWidth - msgDateDelta.x` = `12 + w - 2`.
#[test]
fn skip_block_matches_element_skip_block_width() {
    assert_eq!(skip_block_width(40), MSG_DATE_SPACE + 40 - MSG_DATE_DELTA.x);
    assert_eq!(skip_block_width(40), 50);
    let fonts = FontMetrics::default();
    assert_eq!(skip_block_height(&fonts), fonts.date.height - 5);
}

/// A one-line message whose time fits after the text: the bubble is text + skip block wide
/// and one text line tall (`Ui::Text::String::updateSkipBlock`).
#[test]
fn short_text_keeps_the_time_on_its_line() {
    let fonts = FontMetrics::default();
    let info = BottomInfoInput {
        author_edited_date_width: 31,
        ..Default::default()
    };
    let msg = MessageInput {
        has_from_photo: true,
        text_max_width: 70,
        text_min_height: 18,
        text_wrapped_height: 18,
        text_real_width: 70,
        bottom_info: info,
        ..Default::default()
    };
    let l = layout_message(&msg, 800, &fonts);
    assert_eq!(
        l.geometry.width,
        MSG_PADDING.left + 70 + skip_block_width(31) + MSG_PADDING.right
    );
    assert_eq!(
        l.geometry.height,
        MSG_PADDING.top + 18 + MSG_PADDING.bottom,
        "one line of text, the time shares it"
    );
    // Bottom info sits on the text line, right-aligned (`msgDateDelta`).
    assert_eq!(
        l.bottom_info_rect.x + l.bottom_info_rect.width,
        l.geometry.x + l.geometry.width - MSG_PADDING.right + MSG_DATE_DELTA.x
    );
}

/// Attached messages from one sender: `msgMarginTopAttached = 0` instead of `msgMargin.top()`.
#[test]
fn attached_messages_use_zero_top_margin() {
    let fonts = FontMetrics::default();
    let mut msg = MessageInput {
        text_max_width: 70,
        text_min_height: 18,
        text_wrapped_height: 18,
        text_real_width: 70,
        ..Default::default()
    };
    let first = layout_message(&msg, 800, &fonts);
    msg.attach.attached_to_previous = true;
    msg.attach.bubble_attached_to_previous = true;
    let attached = layout_message(&msg, 800, &fonts);
    assert_eq!(first.margin_top, MSG_MARGIN.top);
    assert_eq!(attached.margin_top, MSG_MARGIN_TOP_ATTACHED);
    assert_eq!(attached.margin_bottom, MSG_MARGIN.bottom);
    assert_eq!(
        attached.message_rounding.top_left,
        BubbleCornerRounding::Small
    );
}

/// Channel post with many reactions: the chips wrap inside the bubble and the views + time
/// never overlap a chip (`InlineList::countCurrentSize` with the info skip block).
#[test]
fn channel_reactions_wrap_and_leave_room_for_views_and_time() {
    let fonts = FontMetrics::default();
    let chips = vec![57, 50, 50, 50, 50, 50, 43, 43, 43, 43, 43, 43];
    let l = layout_message(&channel_post_with_chips(chips.clone()), 600, &fonts);
    assert_eq!(l.reaction_buttons.len(), chips.len());
    let inner_left = l.geometry.x + MSG_PADDING.left;
    let inner_right = l.geometry.x + l.geometry.width - MSG_PADDING.right;
    // First chip starts 3px left of the text (`reactionInlineInBubbleLeft`).
    assert_eq!(l.reaction_buttons[0].x, inner_left - 3);
    // Every chip stays inside the bubble, rows are 22 + 4 apart.
    for b in &l.reaction_buttons {
        assert!(b.x + b.width <= inner_right, "{b:?} past {inner_right}");
        assert_eq!(b.height, 22);
        assert_eq!((b.y - l.reaction_buttons[0].y) % 26, 0);
        assert!(
            !overlaps(*b, l.bottom_info_rect),
            "chip {b:?} under info {:?}",
            l.bottom_info_rect
        );
    }
    assert!(
        l.reaction_buttons
            .iter()
            .any(|b| b.y > l.reaction_buttons[0].y),
        "12 chips wrap at a 420px bubble"
    );
    // The chips start `mediaInBubbleSkip` below the text.
    assert_eq!(
        l.reaction_buttons[0].y,
        l.text_rect.y + l.text_rect.height + MEDIA_IN_BUBBLE_SKIP
    );
    // Info right edge is `msgDateDelta.x` past the text column.
    assert_eq!(
        l.bottom_info_rect.x + l.bottom_info_rect.width,
        inner_right + MSG_DATE_DELTA.x
    );
}

/// When the info fits after the last chip it shares the last row; otherwise the block grows
/// by the skip block's height and the info gets a row of its own.
#[test]
fn info_shares_last_reaction_row_only_when_it_fits() {
    let fonts = FontMetrics::default();
    let few = layout_message(&channel_post_with_chips(vec![50, 50]), 600, &fonts);
    assert_eq!(few.reactions_rect.height, 22, "info on the chips' row");
    let last = few.reaction_buttons[1];
    assert!(few.bottom_info_rect.y < last.y + last.height);
    assert!(few.bottom_info_rect.x >= last.x + last.width + MSG_DATE_SPACE - MSG_DATE_DELTA.x);

    // Fill the row exactly so the skip block does not fit after the last chip.
    let inner = few.geometry.width - MSG_PADDING.horizontal();
    let filler = vec![50; ((inner + 3) / 54) as usize];
    let full = layout_message(&channel_post_with_chips(filler), 600, &fonts);
    let last = *full.reaction_buttons.last().unwrap_or(&Rect::ZERO);
    assert_eq!(
        full.reactions_rect.height,
        last.y - full.reaction_buttons[0].y + 22 + skip_block_height(&fonts)
    );
    assert!(full.bottom_info_rect.y >= last.y + last.height - 5);
}
