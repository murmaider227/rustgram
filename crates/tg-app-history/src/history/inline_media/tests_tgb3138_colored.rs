//! TGB-3138: a selected message's Lottie sticker is drawn from frames coloured with
//! `msgStickerOverlay` (`Images::Colored`, what `Lottie::FrameRequest::colored` gives), cached
//! apart from the plain frames.

use std::sync::Arc;

use egui::Color32;
use tg_lottie::Composition;

use super::decode_pool::{DecodeJob, DecodeOutput, run_job};

/// A one-layer Lottie: a filled circle (the custom emoji and sticker of the TGB-3138 shots).
pub(crate) fn circle_lottie_json() -> String {
    r#"{"v":"5.5.2","fr":20,"ip":0,"op":10,"w":64,"h":64,"layers":[{"ty":4,"nm":"dot","ip":0,"op":10,"st":0,"ks":{"o":{"a":0,"k":100},"r":{"a":0,"k":0},"p":{"a":0,"k":[32,32,0]},"a":{"a":0,"k":[0,0,0]},"s":{"a":0,"k":[100,100,100]}},"shapes":[{"ty":"gr","it":[{"ty":"el","p":{"a":0,"k":[0,0]},"s":{"a":0,"k":[48,48]}},{"ty":"fl","c":{"a":0,"k":[0.95,0.75,0.2,1]},"o":{"a":0,"k":100}},{"ty":"tr","p":{"a":0,"k":[0,0]},"a":{"a":0,"k":[0,0]},"s":{"a":0,"k":[100,100]},"r":{"a":0,"k":0},"o":{"a":0,"k":100}}]}]}]}"#
        .to_owned()
}

fn frame(overlay: Option<Color32>) -> egui::ColorImage {
    let json = circle_lottie_json();
    let comp = Arc::new(Composition::from_json(&json).expect("lottie parses"));
    match run_job(DecodeJob::LottieFrame {
        key: (3138, 64, 0, overlay),
        comp,
        sample_frame: 0.0,
    }) {
        DecodeOutput::LottieFrame { image, .. } => image,
        _ => panic!("a Lottie frame"),
    }
}

#[test]
fn a_selected_sticker_frame_is_coloured_where_it_is_opaque() {
    let overlay = Color32::from_rgba_unmultiplied(0x35, 0x85, 0xD4, 0x7F);
    let plain = frame(None);
    let selected = frame(Some(overlay));
    assert_eq!(plain.size, selected.size);

    let mut expected = plain.clone();
    tg_ui::colored::colored(&mut expected, overlay);
    assert_eq!(selected.pixels, expected.pixels);

    let center = plain.size[0] / 2 + plain.size[1] / 2 * plain.size[0];
    assert_eq!(
        plain.pixels[center].a(),
        255,
        "the circle covers the centre"
    );
    assert_ne!(
        selected.pixels[center], plain.pixels[center],
        "the centre is tinted"
    );
    assert_eq!(selected.pixels[center].a(), plain.pixels[center].a());
    // The transparent corner stays transparent.
    assert_eq!(selected.pixels[0], Color32::TRANSPARENT);
}
