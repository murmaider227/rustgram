//! Media sizing with extreme dimensions from the server (TGB-3062): no overflow panic, and no
//! size that wrapped around to a negative value. Huge proportional sizes (a 1×100000 picture
//! alone in an album) are what tdesktop computes too and are not checked here.

use tg_history::Size;
use tg_history::grouped::layout_media_group;
use tg_history::media::{
    GifInput, GifKind, PhotoInput, StickerInput, StickerKind, count_gif_current_size,
    count_gif_optimal_size, count_photo_current_size, count_photo_optimal_size,
    count_sticker_optimal_size, gif_adjust_height_for_less_crop, photo_adjust_height_for_less_crop,
};

const DIMS: [i32; 10] = [
    -5,
    0,
    1,
    7,
    90,
    1280,
    100_000,
    1 << 24,
    i32::MAX - 1,
    i32::MAX,
];
const WIDTHS: [i32; 4] = [1, 120, 430, 2000];

fn sane(size: Size, what: &str) {
    assert!(
        (0..).contains(&size.width) && (0..).contains(&size.height),
        "{what}: {size:?}"
    );
}

fn sane_height(height: i32, what: &str) {
    assert!((0..).contains(&height), "{what}: {height}");
}

#[test]
fn photo_and_gif_sizes_stay_sane_for_any_server_dimensions() {
    for w in DIMS {
        for h in DIMS {
            let dimensions = Size::new(w, h);
            let photo = PhotoInput {
                dimensions,
                has_bubble: true,
                ..PhotoInput::default()
            };
            let optimal = count_photo_optimal_size(&photo);
            sane(optimal, &format!("photo optimal {dimensions:?}"));
            for kind in [GifKind::Gif, GifKind::VideoFile, GifKind::VideoMessage] {
                let gif = GifInput {
                    kind,
                    dimensions,
                    has_bubble: true,
                    min_width_for_media: 0,
                    info_width: 40,
                    hides_bottom_info: false,
                    textual_max_width: 0,
                    max_status_width: 40,
                    is_streaming: false,
                    unwrapped_additional_width: 0,
                    reactions_optimal_width: 0,
                    hosted_instant_view: false,
                };
                sane(
                    count_gif_optimal_size(&gif),
                    &format!("gif optimal {kind:?} {dimensions:?}"),
                );
                for new_width in WIDTHS {
                    sane(
                        count_gif_current_size(&gif, new_width),
                        &format!("gif current {kind:?} {dimensions:?} at {new_width}"),
                    );
                }
            }
            for kind in [
                StickerKind::Regular,
                StickerKind::AnimatedEmoji,
                StickerKind::Dice,
            ] {
                let sticker = StickerInput {
                    kind,
                    dimensions,
                    custom_size: 0,
                    option_sticker_size: None,
                    bottom_info_width: None,
                };
                sane(
                    count_sticker_optimal_size(&sticker),
                    &format!("sticker {kind:?} {dimensions:?}"),
                );
            }
            for new_width in WIDTHS {
                sane(
                    count_photo_current_size(&photo, optimal, new_width),
                    &format!("photo current {dimensions:?} at {new_width}"),
                );
                let current = Size::new(new_width, new_width);
                sane_height(
                    photo_adjust_height_for_less_crop(dimensions, current),
                    &format!("photo less crop {dimensions:?} at {new_width}"),
                );
                sane_height(
                    gif_adjust_height_for_less_crop(dimensions, current),
                    &format!("gif less crop {dimensions:?} at {new_width}"),
                );
            }
        }
    }
}

#[test]
fn album_layout_stays_sane_for_any_server_dimensions() {
    for w in DIMS {
        for h in DIMS {
            for count in 1..=5usize {
                // One odd item among ordinary photos, at each position.
                for odd in 0..count {
                    let sizes: Vec<Size> = (0..count)
                        .map(|i| {
                            if i == odd {
                                Size::new(w, h)
                            } else {
                                Size::new(1280, 960)
                            }
                        })
                        .collect();
                    for part in layout_media_group(&sizes, 430, 100, 2) {
                        let g = part.geometry;
                        assert!(
                            (0..).contains(&g.width) && (0..).contains(&g.height),
                            "album {sizes:?}: {g:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn mul_div_saturates_instead_of_wrapping() {
    use tg_history::geometry::mul_div;
    assert_eq!(mul_div(430, 960, 1280), 322);
    assert_eq!(mul_div(16_777_216, 430, 1), i32::MAX);
    assert_eq!(mul_div(-16_777_216, 430, 1), i32::MIN);
    assert_eq!(mul_div(5, 7, 0), 0);
    assert_eq!(mul_div(i32::MAX, i32::MAX, i32::MAX), i32::MAX);
}
