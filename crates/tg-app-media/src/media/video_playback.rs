//! Shared video playback plumbing for the media viewer and Picture-in-Picture.
//!
//! The desktop binary decodes on a background thread ([`ThreadedDecoder`], like tdesktop's
//! `VideoTrackObject` thread) so the UI thread never waits for FFmpeg or for streamed bytes;
//! tests and screenshot harnesses keep decoding synchronously so each step is deterministic
//! (the same switch as `inline_media::enable_background_decode`).

use std::time::Duration;

use std::borrow::Cow;

use egui::ColorImage;
use image::{ImageBuffer, Rgba, RgbaImage, imageops};
use tg_video::{
    Decoder, FfmpegStreamDecoder, FrameData, FramePopOutcome, StreamingPlayer, ThreadedDecoder,
    VideoFrame,
};

/// Player type used by the media viewer and Picture-in-Picture.
pub type ViewerPlayer = StreamingPlayer<Box<dyn Decoder>>;

/// Longest wait between two playback steps (one 60 Hz frame).
const MAX_STEP_DELAY_MS: i64 = 16;

/// Wraps `decoder` for playback: on a worker thread in the app, inline in tests.
pub fn viewer_decoder(decoder: FfmpegStreamDecoder) -> tg_video::Result<Box<dyn Decoder>> {
    if tg_emoji::background::background_decode_enabled() {
        Ok(Box::new(ThreadedDecoder::spawn(decoder)?))
    } else {
        Ok(Box::new(decoder))
    }
}

/// Drops decoded audio when no sound output is attached (see `StreamingPlayer::attach_audio`),
/// so the queue does not grow for the whole video (about 11 MB per minute of 48 kHz stereo).
pub fn drop_unplayed_audio(player: &mut ViewerPlayer) {
    while player.pop_audio_frame().is_some() {}
}

/// When to step playback again: when the next frame is due, at most one 60 Hz frame later.
#[must_use]
pub fn next_step_delay(outcome: Option<&FramePopOutcome>) -> Duration {
    let ms = match outcome {
        Some(FramePopOutcome::Wait { delay_ms }) => (*delay_ms).clamp(1, MAX_STEP_DELAY_MS),
        _ => MAX_STEP_DELAY_MS,
    };
    Duration::from_millis(ms.unsigned_abs())
}

/// Converts a presented video frame into an [`egui::ColorImage`], applying the viewer's
/// rotation (degrees, multiples of 90) and flips, with one pass over the pixels when there is no
/// transform. Opaque video is copied as is (straight and premultiplied RGBA are the same at
/// alpha 255); only frames with alpha go through the premultiplying conversion. `None` when
/// the frame is empty, not RGBA/BGRA, or its buffer is shorter than `width * height * 4`
/// (egui would panic on it).
#[must_use]
pub fn frame_color_image(
    frame: &VideoFrame,
    rotation: i32,
    flip_h: bool,
    flip_v: bool,
) -> Option<ColorImage> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let len = w.checked_mul(h)?.checked_mul(4)?;
    if len == 0 {
        return None;
    }
    let rotation = rotation.rem_euclid(360);
    let transformed = rotation != 0 || flip_h || flip_v;
    let rgba: Cow<'_, [u8]> = match &frame.data {
        FrameData::Rgba(bytes) => Cow::Borrowed(bytes.get(..len)?),
        FrameData::Bgra(bytes) => Cow::Owned(
            bytes
                .get(..len)?
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|&[b, g, r, a]| [r, g, b, a])
                .collect(),
        ),
        FrameData::Yuv420(_) | FrameData::Nv12(_) => return None,
    };
    if !transformed {
        return Some(rgba_to_image([w, h], &rgba, frame.has_alpha));
    }
    let view: ImageBuffer<Rgba<u8>, &[u8]> =
        ImageBuffer::from_raw(frame.width, frame.height, &*rgba)?;
    let mut out: RgbaImage = match rotation {
        90 => imageops::rotate90(&view),
        180 => imageops::rotate180(&view),
        270 => imageops::rotate270(&view),
        _ => ImageBuffer::from_raw(frame.width, frame.height, rgba.into_owned())?,
    };
    if flip_h {
        imageops::flip_horizontal_in_place(&mut out);
    }
    if flip_v {
        imageops::flip_vertical_in_place(&mut out);
    }
    Some(rgba_to_image(
        [out.width() as usize, out.height() as usize],
        out.as_raw(),
        frame.has_alpha,
    ))
}

/// One pass through egui's own constructors, which are optimized in every build profile (this
/// crate is not in dev builds: a per-pixel loop here took 63 ms per 1080p frame there).
/// `bytes` is exactly `size[0] * size[1] * 4` long.
fn rgba_to_image(size: [usize; 2], bytes: &[u8], has_alpha: bool) -> ColorImage {
    if has_alpha {
        ColorImage::from_rgba_unmultiplied(size, bytes)
    } else {
        ColorImage::from_rgba_premultiplied(size, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_delay_follows_the_next_due_frame() {
        let wait = |delay_ms| FramePopOutcome::Wait { delay_ms };
        assert_eq!(next_step_delay(Some(&wait(4))), Duration::from_millis(4));
        assert_eq!(next_step_delay(Some(&wait(40))), Duration::from_millis(16));
        assert_eq!(next_step_delay(Some(&wait(0))), Duration::from_millis(1));
        assert_eq!(
            next_step_delay(Some(&FramePopOutcome::Empty)),
            Duration::from_millis(16)
        );
        assert_eq!(next_step_delay(None), Duration::from_millis(16));
    }

    fn frame(width: u32, height: u32, data: FrameData, has_alpha: bool) -> VideoFrame {
        VideoFrame {
            width,
            height,
            pts_ms: 0,
            duration_ms: 33,
            index: 0,
            has_alpha,
            rotation: tg_video::Rotation::Deg0,
            data,
        }
    }

    /// 2x1 frame: a red and a half-transparent green pixel.
    fn two_pixels() -> Vec<u8> {
        vec![255, 0, 0, 255, 0, 255, 0, 128]
    }

    #[test]
    fn frame_image_matches_the_old_unmultiplied_conversion() {
        let bytes = two_pixels();
        let old = ColorImage::from_rgba_unmultiplied([2, 1], &bytes);
        let with_alpha = frame(2, 1, FrameData::Rgba(bytes.clone()), true);
        assert_eq!(frame_color_image(&with_alpha, 0, false, false), Some(old));

        let opaque: Vec<u8> = vec![10, 20, 30, 255, 40, 50, 60, 255];
        let old = ColorImage::from_rgba_unmultiplied([2, 1], &opaque);
        let f = frame(2, 1, FrameData::Rgba(opaque), false);
        assert_eq!(frame_color_image(&f, 0, false, false), Some(old));

        let bgra: Vec<u8> = vec![30, 20, 10, 255, 60, 50, 40, 255];
        let f = frame(2, 1, FrameData::Bgra(bgra), false);
        let expected =
            ColorImage::from_rgba_unmultiplied([2, 1], &[10, 20, 30, 255, 40, 50, 60, 255]);
        assert_eq!(frame_color_image(&f, 0, false, false), Some(expected));
    }

    #[test]
    fn frame_image_applies_rotation_and_flips_like_the_photo_path() {
        let bytes = two_pixels();
        let f = frame(2, 1, FrameData::Rgba(bytes.clone()), true);
        let src =
            image::DynamicImage::ImageRgba8(RgbaImage::from_raw(2, 1, bytes).expect("2x1 image"));
        for (rot, fh, fv) in [
            (90, false, false),
            (180, true, false),
            (270, false, true),
            (0, true, true),
            (-90, false, false),
        ] {
            let want = tg_app_core::core_types::misc::transform_dynamic_image(&src, rot, fh, fv)
                .to_rgba8();
            let want = ColorImage::from_rgba_unmultiplied(
                [want.width() as usize, want.height() as usize],
                want.as_raw(),
            );
            assert_eq!(
                frame_color_image(&f, rot, fh, fv),
                Some(want),
                "rot {rot} fh {fh} fv {fv}"
            );
        }
    }

    /// TGB-3043 timing: one presented 1080p frame on the UI thread, the old way (the player
    /// cloned the frame, then `from_rgba_unmultiplied` converted every pixel) and the new way
    /// (shared frame, one opaque copy). Prints the medians; run with `--ignored --nocapture`.
    #[test]
    #[ignore = "timing probe"]
    fn presented_frame_upload_timing() {
        use std::sync::Arc;
        use std::time::Instant;
        let bytes: Vec<u8> = (0..1920 * 1080 * 4).map(|i| (i % 251) as u8).collect();
        let source = frame(1920, 1080, FrameData::Rgba(bytes), false);
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let mut old = Vec::new();
        let mut new = Vec::new();
        for _ in 0..40 {
            let t = Instant::now();
            let cloned = source.clone();
            let FrameData::Rgba(b) = &cloned.data else {
                unreachable!()
            };
            std::hint::black_box(ColorImage::from_rgba_unmultiplied([1920, 1080], b));
            old.push(t.elapsed().as_secs_f64() * 1000.0);

            let shared = Arc::new(source.clone());
            let t = Instant::now();
            let handle = Arc::clone(&shared);
            std::hint::black_box(frame_color_image(&handle, 0, false, false));
            new.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        println!(
            "1080p frame to ColorImage: old (clone + unmultiplied) median {:.2} ms, new (shared + opaque copy) median {:.2} ms",
            median(old),
            median(new)
        );
    }

    /// egui's constructors assert the buffer length; a short decoder buffer used to reach them
    /// straight from the viewer and PiP.
    #[test]
    fn short_or_empty_frames_are_skipped_not_a_panic() {
        assert!(
            frame_color_image(
                &frame(4, 4, FrameData::Rgba(vec![0; 15]), false),
                0,
                false,
                false
            )
            .is_none()
        );
        assert!(
            frame_color_image(
                &frame(0, 4, FrameData::Rgba(Vec::new()), false),
                0,
                false,
                false
            )
            .is_none()
        );
        assert!(
            frame_color_image(
                &frame(u32::MAX, u32::MAX, FrameData::Rgba(Vec::new()), false),
                90,
                false,
                false
            )
            .is_none()
        );
    }
}
