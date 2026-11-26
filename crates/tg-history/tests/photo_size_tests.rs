//! Photo bubble sizes against tdesktop's `HistoryView::Photo::countOptimalSize` /
//! `countCurrentSize` (TGB-2970). The expected numbers are worked out by hand from
//! `history_view_photo.cpp` with `st::maxMediaSize = 430`, `st::minPhotoSize = 100`,
//! `st::historyPhotoBubbleMinWidth = 200`, `st::msgMaxWidth = 430`, `st::msgPadding = 11`.

use tg_history::{
    ExpandDecision, PhotoInput, Size, count_photo_current_size, count_photo_optimal_size,
    decide_frame_resize,
};

/// `Element::textualMaxWidth` for a caption of `text_width` px.
const fn caption(text_width: i32) -> i32 {
    11 + text_width + 11
}

fn photo(width: i32, height: i32, has_bubble: bool, caption_width: i32) -> PhotoInput {
    PhotoInput {
        dimensions: Size::new(width, height),
        has_bubble,
        // `Message::minWidthForMedia`: info width + 2 * (msgDateImgDelta + msgDateImgPadding.x).
        min_width_for_media: 34 + 2 * (4 + 8),
        textual_max_width: caption_width,
        ..PhotoInput::default()
    }
}

fn current(input: &PhotoInput, new_width: i32) -> Size {
    let optimal = count_photo_optimal_size(input);
    count_photo_current_size(input, optimal, new_width)
}

/// The owner's case: a forwarded 1080x2340 phone screenshot with a short caption. tdesktop:
/// desired 198x430, widened to the bubble minimum 200, then `adjustHeightForLessCrop` 433.
#[test]
fn tall_forwarded_screenshot_with_a_short_caption() {
    let input = photo(1080, 2340, true, caption(110));
    assert_eq!(count_photo_optimal_size(&input), Size::new(430, 430));
    assert_eq!(current(&input, 600), Size::new(200, 433));
    // The box keeps the picture's aspect: covering it cuts nothing off.
    assert_eq!(
        decide_frame_resize(Size::new(200, 433), Size::new(1080, 2340), 3, 4),
        ExpandDecision {
            result: Size::new(200, 433),
            expanding: true
        }
    );
}

/// The same photo without a bubble (no caption, no header): exactly the desired 198x430.
#[test]
fn tall_photo_without_a_bubble() {
    let input = photo(1080, 2340, false, 0);
    assert_eq!(current(&input, 600), Size::new(198, 430));
}

/// A long caption widens the box to 400; the picture is then shown whole (198x430) over its
/// blurred thumbnail, because covering would cut off more than a quarter.
#[test]
fn tall_photo_with_a_long_caption_is_letterboxed() {
    let input = photo(1080, 2340, true, caption(378));
    assert_eq!(current(&input, 600), Size::new(400, 430));
    assert_eq!(
        decide_frame_resize(Size::new(400, 430), Size::new(1080, 2340), 3, 4),
        ExpandDecision {
            result: Size::new(198, 430),
            expanding: false
        }
    );
}

#[test]
fn wide_photo_fills_the_max_width() {
    let input = photo(2340, 1080, true, caption(110));
    assert_eq!(count_photo_optimal_size(&input), Size::new(430, 198));
    assert_eq!(current(&input, 600), Size::new(430, 198));
    // A narrow column scales it down keeping the aspect.
    assert_eq!(current(&input, 300), Size::new(300, 138));
}

#[test]
fn square_photos() {
    let big = photo(800, 800, true, caption(60));
    assert_eq!(current(&big, 600), Size::new(430, 430));
    let small = photo(300, 300, false, 0);
    assert_eq!(current(&small, 600), Size::new(300, 300));
}
