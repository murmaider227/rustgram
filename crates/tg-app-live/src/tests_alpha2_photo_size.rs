//! Live check (alpha.2): bubbles downloaded the 2560 px `w` size (871 KB on the test DC) where
//! tdesktop loads `y` (1280 px) first (`LargeLevels = "ydxcwmbsa"`, `data/data_session.cpp:119`).

use tg_data::{PhotoData, PhotoId, PhotoSize};
use tg_tl::enums as tl_enums;

use crate::media::photo_to_location;

fn size(kind: &str, side: i32, bytes: i64) -> PhotoSize {
    PhotoSize {
        kind: kind.to_owned(),
        width: side,
        height: side * 3 / 4,
        size: bytes,
    }
}

fn thumb_size_of(photo: &PhotoData) -> String {
    match photo_to_location(photo).location {
        tl_enums::InputFileLocation::InputPhotoFileLocation(loc) => loc.thumb_size,
        other => panic!("unexpected location {other:?}"),
    }
}

#[test]
fn large_photo_prefers_y_over_the_bigger_w() {
    let photo = PhotoData {
        id: PhotoId(1),
        sizes: vec![
            size("s", 90, 1_500),
            size("m", 320, 20_000),
            size("x", 800, 120_000),
            size("y", 1280, 300_000),
            size("w", 2560, 871_000),
        ],
        ..PhotoData::default()
    };
    assert_eq!(thumb_size_of(&photo), "y");
    assert_eq!(photo_to_location(&photo).size, 300_000);
}

#[test]
fn large_photo_falls_back_through_the_levels() {
    let only_w_and_m = PhotoData {
        id: PhotoId(2),
        sizes: vec![size("m", 320, 20_000), size("w", 2560, 871_000)],
        ..PhotoData::default()
    };
    assert_eq!(thumb_size_of(&only_w_and_m), "w");
    let unknown = PhotoData {
        id: PhotoId(3),
        sizes: vec![size("i", 40, 100), size("z", 640, 50_000)],
        ..PhotoData::default()
    };
    assert_eq!(thumb_size_of(&unknown), "z");
}
