//! TGB-2970: a photo bubble's width comes from the caption's real width
//! (`Element::textualMaxWidth`), not a fixed 320 px, so a tall photo keeps tdesktop's size.

use egui::Vec2;

use crate::history::tests_photo_snap::{build_photo_harness, photo_message, tall_forwarded};
use tg_app_core::stand_in::types::{MsgId, PeerId};

fn media_size(messages: Vec<crate::history::backend::HistoryMessage>, id: MsgId) -> (i32, i32) {
    let peer = messages[0].peer_id;
    let mut harness =
        build_photo_harness(peer, messages, Vec::new(), Vec2::new(640.0, 760.0), false);
    for _ in 0..4 {
        harness.step();
    }
    let layout = harness
        .state()
        .inner
        .cached_message_layout(id)
        .expect("photo layout")
        .clone();
    (layout.media_rect.width, layout.media_rect.height)
}

/// tdesktop: 1080x2340 -> desired 198x430, widened to `historyPhotoBubbleMinWidth` 200, height
/// `adjustHeightForLessCrop` 433. The old layout took a 320 px caption width for any caption.
#[test]
fn a_tall_forwarded_photo_with_a_short_caption_is_200_by_433() {
    let peer = PeerId::from_user(7001);
    let (msg, _) = tall_forwarded(peer, 1);
    assert_eq!(media_size(vec![msg], MsgId(1)), (200, 433));
}

/// A wide photo with a caption fills `st::maxMediaSize` and keeps its aspect: 430x198.
#[test]
fn a_wide_photo_with_a_caption_keeps_its_aspect() {
    let peer = PeerId::from_user(7001);
    let thumb = image::RgbaImage::new(40, 18);
    let msg = photo_message(peer, 2, (2340, 1080), &thumb, "Sunset");
    assert_eq!(media_size(vec![msg], MsgId(2)), (430, 198));
}

/// A long caption widens the box (the picture is then centered over its blurred thumbnail).
#[test]
fn a_long_caption_widens_a_tall_photo_up_to_the_caption() {
    let peer = PeerId::from_user(7001);
    let thumb = image::RgbaImage::new(18, 40);
    let caption = "A caption that is clearly longer than the photo is wide, on one line";
    let msg = photo_message(peer, 3, (1080, 2340), &thumb, caption);
    let (w, h) = media_size(vec![msg], MsgId(3));
    assert!(w > 200 && w <= 430, "width follows the caption: {w}");
    assert_eq!(h, 430);
}
