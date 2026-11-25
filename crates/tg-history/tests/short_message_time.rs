//! A short one-line message keeps room for its time after the text (`Ui::Text::String` skip
//! block), so the bottom info never overlaps the text.

use tg_history::consts::{MSG_DATE_SPACE, MSG_PADDING};
use tg_history::*;

#[test]
fn short_incoming_message_reserves_skip_block_for_time() {
    let fonts = FontMetrics::default();
    let text_w = 97;
    let msg = MessageInput {
        is_out: false,
        text_max_width: text_w,
        text_real_width: text_w,
        text_min_height: fonts.normal.height,
        text_wrapped_height: fonts.normal.height,
        bottom_info: BottomInfoInput {
            author_edited_date_width: 30,
            ..Default::default()
        },
        ..Default::default()
    };
    let layout = layout_message(&msg, 900, &fonts);
    assert!(layout.has_bubble);
    let text_right = layout.geometry.left() + MSG_PADDING.left + text_w;
    assert!(
        layout.bottom_info_rect.left() >= text_right + MSG_DATE_SPACE - 1,
        "time {:?} must start after the text (right edge {text_right}) plus the date space",
        layout.bottom_info_rect
    );
    assert!(layout.bottom_info_rect.right() <= layout.geometry.right());
}

#[test]
fn long_wrapped_message_does_not_widen_for_time() {
    let fonts = FontMetrics::default();
    let msg = MessageInput {
        is_out: false,
        text_max_width: 430,
        text_real_width: 400,
        text_min_height: fonts.normal.height,
        text_wrapped_height: fonts.normal.height * 3,
        bottom_info: BottomInfoInput {
            author_edited_date_width: 30,
            ..Default::default()
        },
        ..Default::default()
    };
    let narrow = layout_message(&msg, 470, &fonts);
    assert!(narrow.geometry.width <= 470);
}
