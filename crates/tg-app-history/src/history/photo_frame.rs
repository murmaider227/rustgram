//! Where a photo's picture goes inside its box in the chat, keeping the picture's aspect ratio
//! (`Photo::prepareImageCacheWithLarge` in `history_view_photo.cpp`, `PrepareWithBlurredBackground`
//! in `history_view_media_common.cpp`, `DecideFrameResize` / `PrepareBlurredBackground` in
//! `media_streaming_utility.cpp`).
//!
//! tdesktop bakes the result into a cached `QImage`; here the same geometry is a texture rect and
//! UV range for the GPU, computed per paint from two sizes (no pixel work, no cache to keep).

use egui::{Pos2, Rect, Vec2};
use tg_history::{Size, decide_frame_resize};

/// How a photo fills its box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhotoFit {
    /// A single photo: cropped to cover the box when at most a quarter of it is cut off,
    /// otherwise shown whole and centered over its blurred thumbnail (`DecideFrameResize` 3/4).
    Single,
    /// An album tile: always cropped to cover the tile (`Photo::validateGroupedCache`).
    Cover,
    /// A sticker: the whole picture, scaled to fit and centered, nothing behind it
    /// (`Sticker::paintPixmap`).
    Contain,
}

/// Where and how a picture is drawn inside its box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhotoFrame {
    /// Screen rect the picture is drawn into.
    pub rect: Rect,
    /// Texture coordinates of the part of the picture that is drawn.
    pub uv: Rect,
    /// The picture does not cover the box: the blurred thumbnail shows around it.
    pub letterboxed: bool,
}

const FULL_UV: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));

/// Texture coordinates that crop a `picture` to the aspect ratio of `outer`, centered
/// (`Qt::KeepAspectRatioByExpanding` followed by a centered crop).
#[must_use]
pub fn cover_uv(outer: Vec2, picture: Vec2) -> Rect {
    if outer.x <= 0.0 || outer.y <= 0.0 || picture.x <= 0.0 || picture.y <= 0.0 {
        return FULL_UV;
    }
    let outer_ratio = outer.x / outer.y;
    let picture_ratio = picture.x / picture.y;
    if picture_ratio > outer_ratio {
        let visible = outer_ratio / picture_ratio;
        let left = (1.0 - visible) / 2.0;
        Rect::from_min_max(Pos2::new(left, 0.0), Pos2::new(left + visible, 1.0))
    } else {
        let visible = picture_ratio / outer_ratio;
        let top = (1.0 - visible) / 2.0;
        Rect::from_min_max(Pos2::new(0.0, top), Pos2::new(1.0, top + visible))
    }
}

fn to_size(v: Vec2) -> Size {
    Size::new(v.x.round() as i32, v.y.round() as i32)
}

/// Places a picture of `picture` pixels into `outer` the way tdesktop does for `fit`.
#[must_use]
pub fn photo_frame(outer: Rect, picture: Vec2, fit: PhotoFit) -> PhotoFrame {
    let covering = PhotoFrame {
        rect: outer,
        uv: cover_uv(outer.size(), picture),
        letterboxed: false,
    };
    if fit == PhotoFit::Cover {
        return covering;
    }
    if fit == PhotoFit::Contain {
        if picture.x <= 0.0 || picture.y <= 0.0 {
            return covering;
        }
        let scale = (outer.width() / picture.x).min(outer.height() / picture.y);
        return PhotoFrame {
            rect: Rect::from_center_size(outer.center(), picture * scale),
            uv: FULL_UV,
            letterboxed: true,
        };
    }
    let outer_px = to_size(outer.size());
    let picture_px = to_size(picture);
    if outer_px.is_empty() || picture_px.is_empty() {
        return covering;
    }
    let decision = decide_frame_resize(outer_px, picture_px, 3, 4);
    if decision.expanding {
        return covering;
    }
    let size = Vec2::new(decision.result.width as f32, decision.result.height as f32);
    let offset = Vec2::new(
        ((outer_px.width - decision.result.width) / 2) as f32,
        ((outer_px.height - decision.result.height) / 2) as f32,
    );
    PhotoFrame {
        rect: Rect::from_min_size(outer.min + offset, size),
        uv: FULL_UV,
        letterboxed: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(w: f32, h: f32) -> Rect {
        Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(w, h))
    }

    fn aspect(r: Rect, uv: Rect, picture: Vec2) -> f32 {
        let shown = Vec2::new(uv.width() * picture.x, uv.height() * picture.y);
        (r.width() / r.height()) / (shown.x / shown.y)
    }

    /// The owner's case: a phone screenshot in a box made wider by a caption. tdesktop shows it
    /// whole and centered (`DecideFrameResize` would cut off more than a quarter); the old paint
    /// stretched it across the box.
    #[test]
    fn a_tall_picture_in_a_wider_box_is_centered_not_stretched() {
        let picture = Vec2::new(1080.0, 2340.0);
        let frame = photo_frame(rect(320.0, 430.0), picture, PhotoFit::Single);
        assert!(frame.letterboxed);
        assert_eq!(
            frame.rect,
            Rect::from_min_size(Pos2::new(71.0, 20.0), Vec2::new(198.0, 430.0))
        );
        assert_eq!(frame.uv, FULL_UV);
    }

    #[test]
    fn a_picture_close_to_the_box_aspect_is_cropped_to_cover_it() {
        // 1280x1000 in 430x300: 335 px tall when covering, a 10% cut (tdesktop crops only
        // top and bottom, at most a quarter).
        let picture = Vec2::new(1280.0, 1000.0);
        let outer = rect(430.0, 300.0);
        let frame = photo_frame(outer, picture, PhotoFit::Single);
        assert!(!frame.letterboxed);
        assert_eq!(frame.rect, outer);
        assert!((aspect(frame.rect, frame.uv, picture) - 1.0).abs() < 0.01);
        assert!(frame.uv.min.x.abs() < f32::EPSILON && frame.uv.min.y > 0.0);
    }

    #[test]
    fn a_wide_picture_in_a_taller_box_is_centered_whole() {
        let picture = Vec2::new(2340.0, 1080.0);
        let frame = photo_frame(rect(430.0, 300.0), picture, PhotoFit::Single);
        assert!(frame.letterboxed);
        assert_eq!(frame.rect.size(), Vec2::new(430.0, 198.0));
        assert_eq!(frame.rect.min, Pos2::new(10.0, 71.0));
    }

    #[test]
    fn album_tiles_always_cover_and_keep_the_aspect() {
        for picture in [
            Vec2::new(1080.0, 2340.0),
            Vec2::new(2340.0, 1080.0),
            Vec2::new(800.0, 800.0),
        ] {
            let outer = rect(200.0, 140.0);
            let frame = photo_frame(outer, picture, PhotoFit::Cover);
            assert_eq!(frame.rect, outer);
            assert!((aspect(frame.rect, frame.uv, picture) - 1.0).abs() < 0.01);
        }
    }
}
