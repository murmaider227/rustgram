//! TGB-3054: a message of 1-3 emoji is `UnwrappedMedia` around `HistoryView::LargeEmoji`: no
//! bubble, 36 px emoji with a 1 px outline, the time to the right of an incoming one and below a
//! right-aligned (outgoing) one.

use tg_history::consts::{MSG_DATE_IMG_DELTA, MSG_DATE_IMG_PADDING, MSG_REPLY_PADDING};
use tg_history::*;

fn fonts() -> FontMetrics {
    FontMetrics::default()
}

fn message(count: usize, is_out: bool) -> (MessageInput, i32) {
    let fonts = fonts();
    let bottom_info = BottomInfoInput {
        author_edited_date_width: 30,
        out_layout: is_out,
        ..Default::default()
    };
    let info_w = BottomInfoLayout::compute(&bottom_info, i32::MAX, &fonts)
        .optimal_size
        .width;
    let bottom_info_width = MSG_REPLY_PADDING.left + info_w + 2 * MSG_DATE_IMG_PADDING.x;
    let input = MessageInput {
        is_out,
        media: Some(MediaInput::LargeEmoji(LargeEmojiInput {
            count,
            bottom_info_width,
        })),
        bottom_info,
        ..Default::default()
    };
    (input, bottom_info_width)
}

#[test]
fn emoji_row_size_matches_large_emoji_count_optimal_size() {
    // (36 + 2 * 1) per emoji, (4 - 2 * 1) between them.
    assert_eq!(large_emoji_size(1), Size::new(38, 38));
    assert_eq!(large_emoji_size(2), Size::new(78, 38));
    assert_eq!(large_emoji_size(3), Size::new(118, 38));
}

#[test]
fn incoming_large_emoji_has_no_bubble_and_the_time_to_the_right() {
    let fonts = fonts();
    let (input, info_w) = message(2, false);
    let layout = layout_message(&input, 800, &fonts);
    assert!(!layout.has_bubble);
    assert!(layout.unwrapped);
    assert_eq!(layout.bottom_info_display, InfoDisplayType::Background);
    assert_eq!(layout.media_rect.size(), Size::new(78 + info_w, 38));
    let emoji = large_emoji_inner_rect(layout.media_rect, 2, false);
    assert_eq!(emoji.x, layout.media_rect.x);
    let pill = layout.bottom_info_rect;
    assert!(pill.x >= emoji.x + emoji.width, "{pill:?} vs {emoji:?}");
    assert_eq!(
        pill.x + pill.width,
        emoji.x + emoji.width + info_w - MSG_DATE_IMG_DELTA
    );
    assert_eq!(
        pill.y + pill.height,
        layout.media_rect.y + layout.media_rect.height - MSG_DATE_IMG_DELTA
    );
}

#[test]
fn outgoing_large_emoji_puts_the_time_below_the_emoji() {
    let fonts = fonts();
    let (input, _) = message(1, true);
    let layout = layout_message(&input, 800, &fonts);
    assert!(!layout.has_bubble);
    assert!(layout.has_right_layout);
    let pill = layout.bottom_info_rect;
    // `UnwrappedMedia::countCurrentSize`: 38 + msgDateImgDelta + the pill height.
    assert_eq!(
        layout.media_rect.height,
        38 + MSG_DATE_IMG_DELTA + pill.height
    );
    let emoji = large_emoji_inner_rect(layout.media_rect, 1, true);
    assert_eq!(
        emoji.x + emoji.width,
        layout.media_rect.x + layout.media_rect.width
    );
    assert_eq!(emoji.y, layout.media_rect.y);
    assert!(pill.y >= emoji.y + emoji.height, "{pill:?} vs {emoji:?}");
    assert_eq!(
        pill.x + pill.width,
        emoji.x + emoji.width - MSG_DATE_IMG_DELTA
    );
    // Right-aligned like any outgoing message.
    let right_edge = layout.media_rect.x + layout.media_rect.width;
    assert_eq!(right_edge, 800 - MSG_MARGIN_RIGHT_FOR_TEST);
}

/// `st::msgMargin.right()` for an outgoing message without a photo column (`Message::countGeometry`).
const MSG_MARGIN_RIGHT_FOR_TEST: i32 = tg_history::consts::MSG_MARGIN.left;
