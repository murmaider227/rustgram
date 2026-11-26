//! Round video message layout parity (`Message::drawBubble`, `Gif::needsBubble`, `Gif::draw`).

use tg_history::*;

fn round_post(from_name: bool, is_out: bool) -> MessageInput {
    MessageInput {
        is_out,
        is_post: !is_out,
        from_name_width: from_name.then_some(96),
        media: Some(MediaInput::Gif(GifInput {
            kind: GifKind::VideoMessage,
            dimensions: Size::new(384, 384),
            is_streaming: true,
            ..Default::default()
        })),
        bottom_info: BottomInfoInput {
            author_edited_date_width: 36,
            ..Default::default()
        },
        has_comments_button: !is_out,
        comments_text_width: 90,
        ..Default::default()
    }
}

#[test]
fn round_video_with_a_header_still_has_no_bubble() {
    let fonts = FontMetrics::default();
    let layout = layout_message(&round_post(true, false), 700, &fonts);
    assert!(
        !layout.has_bubble,
        "a channel name alone must not wrap a round video"
    );
    assert!(layout.unwrapped);
    assert_eq!(layout.header_heights.from_name_or_via, 0);
    assert!(layout.comments_rect.is_empty());
    assert_eq!(layout.media_rect.size(), Size::new(240, 240));
}

#[test]
fn round_video_info_pill_hangs_off_the_circle() {
    let fonts = FontMetrics::default();
    let input = round_post(true, false);
    let layout = layout_message(&input, 700, &fonts);
    let info_width = BottomInfoLayout::compute(&input.bottom_info, i32::MAX, &fonts)
        .optimal_size
        .width;
    let media = layout.media_rect;
    let info = layout.bottom_info_rect;
    let circle_right = media.x + 240;
    assert_eq!(layout.bottom_info_display, InfoDisplayType::Background);
    // fullRight = circle right + infoWidth - normalFont height (history_view_gif.cpp:1062-1074).
    assert_eq!(
        info.x + info.width,
        circle_right + info_width - fonts.normal.height
    );
    assert_eq!(info.y + info.height, media.y + media.height - 4);

    // Outgoing (right layout) keeps the pill inside the circle's right edge.
    let out = layout_message(&round_post(false, true), 700, &fonts);
    assert_eq!(
        out.bottom_info_rect.x + out.bottom_info_rect.width,
        out.media_rect.x + 240
    );
}

#[test]
fn round_video_with_caption_text_gets_a_bubble() {
    let fonts = FontMetrics::default();
    let mut input = round_post(false, false);
    input.text_max_width = 120;
    input.text_min_height = fonts.normal.height;
    input.text_real_width = 120;
    let layout = layout_message(&input, 700, &fonts);
    assert!(layout.has_bubble);
}
