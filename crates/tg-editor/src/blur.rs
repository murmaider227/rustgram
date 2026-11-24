//! Separable StackBlur for the blur brush tool.
//!
//! Port of `Images::BlurLargeImage` from `lib_ui/ui/image/image_prepare.cpp`.

use image::RgbaImage;

/// Default blur brush radius (`photoEditorBlurRadius` in `editor.style`).
pub const BLUR_BRUSH_RADIUS: u32 = 20;

/// Applies a two-pass separable StackBlur to an RGBA image.
///
/// Mirrors `Images::BlurLargeImage(QImage, int radius)`: if `width <= radius`,
/// `height <= radius`, or `radius < 1`, returns the image unchanged.
/// The horizontal pass blurs all 4 channels; the vertical pass preserves the
/// alpha channel from the source image (matching `KeepAlpha = true` in tdesktop).
#[must_use]
pub fn stack_blur_rgba(image: &RgbaImage, radius: u32) -> RgbaImage {
    let width = image.width() as usize;
    let height = image.height() as usize;
    let r = radius as usize;
    if r < 1 || width <= r || height <= r {
        return image.clone();
    }

    let div = 2 * r + 1;
    let r_plus_1 = (r + 1) as i64;
    let div_sum = ((r + 1) * (r + 1)) as i64;

    let src = image.as_raw();
    let mut middle = vec![0u8; width * height * 4];
    let mut dst = src.to_vec();

    let mut stack = vec![[0i64; 4]; div];
    let mut ahead = vec![0usize; width.max(height)];

    // Horizontal pass (all 4 channels).
    for (x, slot) in ahead.iter_mut().enumerate().take(width) {
        *slot = (x + r + 1).min(width - 1);
    }
    let last_x = width - 1;
    for y in 0..height {
        let row = y * width * 4;
        let mut insum = [0i64; 4];
        let mut outsum = [0i64; 4];
        let mut sum = [0i64; 4];

        for i in -(r as isize)..=(r as isize) {
            let sx = i.clamp(0, last_x as isize) as usize;
            let idx = row + sx * 4;
            let px = [
                src[idx] as i64,
                src[idx + 1] as i64,
                src[idx + 2] as i64,
                src[idx + 3] as i64,
            ];
            stack[(i + r as isize) as usize] = px;
            let weight = r_plus_1 - i.unsigned_abs() as i64;
            for c in 0..4 {
                sum[c] += px[c] * weight;
                if i > 0 {
                    insum[c] += px[c];
                } else {
                    outsum[c] += px[c];
                }
            }
        }

        let mut stack_ptr = r;
        let mut stack_start = 0usize;
        for (x, &next_x) in ahead.iter().enumerate().take(width) {
            let out_idx = row + x * 4;
            for c in 0..4 {
                middle[out_idx + c] = (sum[c] / div_sum).clamp(0, 255) as u8;
            }

            let out_px = stack[stack_start];
            let next_idx = row + next_x * 4;
            let new_px = [
                src[next_idx] as i64,
                src[next_idx + 1] as i64,
                src[next_idx + 2] as i64,
                src[next_idx + 3] as i64,
            ];
            stack[stack_start] = new_px;

            stack_start += 1;
            if stack_start == div {
                stack_start = 0;
            }
            stack_ptr += 1;
            if stack_ptr == div {
                stack_ptr = 0;
            }
            let in_px = stack[stack_ptr];

            for c in 0..4 {
                sum[c] -= outsum[c];
                outsum[c] -= out_px[c];
                insum[c] += new_px[c];
                sum[c] += insum[c];
                outsum[c] += in_px[c];
                insum[c] -= in_px[c];
            }
        }
    }

    // Vertical pass (preserves original alpha channel, matching KeepAlpha = true).
    for (y, slot) in ahead.iter_mut().enumerate().take(height) {
        *slot = (y + r + 1).min(height - 1);
    }
    let last_y = height - 1;
    for x in 0..width {
        let mut insum = [0i64; 3];
        let mut outsum = [0i64; 3];
        let mut sum = [0i64; 3];

        for i in -(r as isize)..=(r as isize) {
            let sy = i.clamp(0, last_y as isize) as usize;
            let idx = (sy * width + x) * 4;
            let px = [
                middle[idx] as i64,
                middle[idx + 1] as i64,
                middle[idx + 2] as i64,
                0,
            ];
            stack[(i + r as isize) as usize] = px;
            let weight = r_plus_1 - i.unsigned_abs() as i64;
            for c in 0..3 {
                sum[c] += px[c] * weight;
                if i > 0 {
                    insum[c] += px[c];
                } else {
                    outsum[c] += px[c];
                }
            }
        }

        let mut stack_ptr = r;
        let mut stack_start = 0usize;
        for (y, &next_y) in ahead.iter().enumerate().take(height) {
            let out_idx = (y * width + x) * 4;
            for c in 0..3 {
                dst[out_idx + c] = (sum[c] / div_sum).clamp(0, 255) as u8;
            }

            let out_px = stack[stack_start];
            let next_idx = (next_y * width + x) * 4;
            let new_px = [
                middle[next_idx] as i64,
                middle[next_idx + 1] as i64,
                middle[next_idx + 2] as i64,
                0,
            ];
            stack[stack_start] = new_px;

            stack_start += 1;
            if stack_start == div {
                stack_start = 0;
            }
            stack_ptr += 1;
            if stack_ptr == div {
                stack_ptr = 0;
            }
            let in_px = stack[stack_ptr];

            for c in 0..3 {
                sum[c] -= outsum[c];
                outsum[c] -= out_px[c];
                insum[c] += new_px[c];
                sum[c] += insum[c];
                outsum[c] += in_px[c];
                insum[c] -= in_px[c];
            }
        }
    }

    RgbaImage::from_raw(image.width(), image.height(), dst).unwrap_or_else(|| image.clone())
}
