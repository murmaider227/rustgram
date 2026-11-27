//! TGB-3099: a sticker is `UnwrappedMedia` like large emoji. The media reserves the info width to
//! the right of the sticker, an incoming sticker draws its time there (`calculateFullRight`), a
//! right-aligned one at the sticker's bottom right.

use tg_history::consts::{MSG_DATE_IMG_DELTA, MSG_DATE_IMG_PADDING, MSG_REPLY_PADDING};
use tg_history::*;

fn message(is_out: bool) -> (MessageInput, i32) {
    let fonts = FontMetrics::default();
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
        media: Some(MediaInput::Sticker(StickerInput {
            kind: StickerKind::Regular,
            dimensions: Size::new(512, 512),
            custom_size: 0,
            option_sticker_size: None,
            bottom_info_width: Some(bottom_info_width),
        })),
        bottom_info,
        ..Default::default()
    };
    (input, bottom_info_width)
}

#[test]
fn an_incoming_sticker_has_its_time_to_the_right() {
    let (input, info_w) = message(false);
    let layout = layout_message(&input, 800, &FontMetrics::default());
    assert!(!layout.has_bubble && layout.unwrapped);
    assert_eq!(layout.media_rect.size(), Size::new(224 + info_w, 224));
    let sticker = layout.unwrapped_content_rect;
    assert_eq!(sticker.size(), Size::new(224, 224));
    assert_eq!(sticker.x, layout.media_rect.x);
    let pill = layout.bottom_info_rect;
    // Old placement: over the sticker's bottom-right corner.
    assert!(
        pill.x >= sticker.x + sticker.width,
        "{pill:?} vs {sticker:?}"
    );
    assert_eq!(
        pill.x + pill.width,
        sticker.x + sticker.width + info_w - MSG_DATE_IMG_DELTA
    );
    assert_eq!(
        pill.y + pill.height,
        sticker.y + sticker.height - MSG_DATE_IMG_DELTA
    );
}

#[test]
fn an_outgoing_sticker_has_its_time_at_its_bottom_right() {
    let (input, _) = message(true);
    let layout = layout_message(&input, 800, &FontMetrics::default());
    assert!(layout.has_right_layout);
    let sticker = layout.unwrapped_content_rect;
    assert_eq!(sticker.size(), Size::new(224, 224));
    assert_eq!(
        sticker.x + sticker.width,
        layout.media_rect.x + layout.media_rect.width
    );
    let pill = layout.bottom_info_rect;
    assert_eq!(
        pill.x + pill.width,
        sticker.x + sticker.width - MSG_DATE_IMG_DELTA
    );
    assert_eq!(
        pill.y + pill.height,
        sticker.y + sticker.height - MSG_DATE_IMG_DELTA
    );
}

#[test]
fn a_video_message_with_its_transcription_keeps_its_layout() {
    // Laid out as a sticker but not `UnwrappedMedia`: no info width, no content rect.
    let input = MessageInput {
        media: Some(MediaInput::Sticker(StickerInput {
            kind: StickerKind::Regular,
            dimensions: Size::new(240, 300),
            custom_size: 300,
            option_sticker_size: None,
            bottom_info_width: None,
        })),
        ..Default::default()
    };
    let layout = layout_message(&input, 800, &FontMetrics::default());
    assert_eq!(layout.unwrapped_content_rect, Rect::ZERO);
}
