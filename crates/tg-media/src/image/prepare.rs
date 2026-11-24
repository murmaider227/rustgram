//! Port of `Telegram/lib_ui/ui/image/image_prepare.cpp` and `image_blur_simd_generic.h`:
//! scaling with tdesktop's rounding, `Blur`, `BlurLargeImage` (exact StackBlur),
//! `Circle`/`EllipseMask`, `Round`/`CornersMask`, `Colored`, `Opaque`, `DitherImage`,
//! and gradient/shadow generators.

use bitflags::bitflags;
use image::{Rgba, RgbaImage, imageops::FilterType};
use rand::RngCore;

/// Large corner rounding radius in logical pixels (`roundRadiusLarge` in `basic.style`).
pub const ROUND_RADIUS_LARGE: u32 = 6;
/// Small corner rounding radius in logical pixels (`roundRadiusSmall` in `basic.style`).
pub const ROUND_RADIUS_SMALL: u32 = 3;

bitflags! {
    /// Image preparation options matching `Images::Options` in `image_prepare.h`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct PrepareOptions: u32 {
        /// Use nearest-neighbor (`FastTransformation`) instead of smooth filtering.
        const FAST_TRANSFORM         = 1 << 0;
        /// Apply tdesktop's 3px triangle blur (`Images::Blur`) before scaling.
        const BLUR                   = 1 << 1;
        /// Mask image to an ellipse/circle (`Images::Circle`).
        const ROUND_CIRCLE           = 1 << 2;
        /// Round corners with `ROUND_RADIUS_LARGE` (6px).
        const ROUND_LARGE            = 1 << 3;
        /// Round corners with `ROUND_RADIUS_SMALL` (3px).
        const ROUND_SMALL            = 1 << 4;
        /// Do not round the top-left corner.
        const ROUND_SKIP_TOP_LEFT    = 1 << 5;
        /// Do not round the top-right corner.
        const ROUND_SKIP_TOP_RIGHT   = 1 << 6;
        /// Do not round the bottom-left corner.
        const ROUND_SKIP_BOTTOM_LEFT = 1 << 7;
        /// Do not round the bottom-right corner.
        const ROUND_SKIP_BOTTOM_RIGHT = 1 << 8;
        /// Colorize flag (used alongside `PrepareArgs::colored`).
        const COLORIZE               = 1 << 9;
        /// Fill outer letterbox/pillarbox canvas with transparent instead of opaque black.
        const TRANSPARENT_BACKGROUND = 1 << 10;
    }
}

bitflags! {
    /// Corner bitmask matching `RectPart` corners in `ui/rect_part.h`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct RectCorners: u8 {
        /// Top-left corner.
        const TOP_LEFT     = 1 << 0;
        /// Top-right corner.
        const TOP_RIGHT    = 1 << 1;
        /// Bottom-left corner.
        const BOTTOM_LEFT  = 1 << 2;
        /// Bottom-right corner.
        const BOTTOM_RIGHT = 1 << 3;
        /// All four corners.
        const ALL          = Self::TOP_LEFT.bits()
            | Self::TOP_RIGHT.bits()
            | Self::BOTTOM_LEFT.bits()
            | Self::BOTTOM_RIGHT.bits();
    }
}

/// Rounding radius presets matching `ImageRoundRadius` in `image_prepare.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageRoundRadius {
    /// No corner rounding.
    None,
    /// Large corner radius (`ROUND_RADIUS_LARGE`, 6px).
    Large,
    /// Small corner radius (`ROUND_RADIUS_SMALL`, 3px).
    Small,
    /// Full ellipse/circle mask.
    Ellipse,
    /// Custom corner radius in pixels.
    Custom(u32),
}

/// Arguments for [`prepare_image`], mirroring `Images::PrepareArgs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrepareArgs {
    /// Optional color overlay applied at the end (`Images::Colored`).
    pub colored: Option<[u8; 4]>,
    /// Bitflags controlling blur, scaling quality, rounding, and outer canvas fill.
    pub options: PrepareOptions,
    /// Optional outer canvas size `(width, height)`. When non-zero and different from `(w, h)`,
    /// the scaled image is centered on this canvas.
    pub outer: Option<(u32, u32)>,
    /// Device pixel ratio multiplier for rounding radii and `outer` (default `1`).
    pub device_pixel_ratio: u32,
}

impl PrepareArgs {
    /// Returns a copy with [`PrepareOptions::BLUR`] enabled.
    #[must_use]
    pub fn blurred(mut self) -> Self {
        self.options |= PrepareOptions::BLUR;
        self
    }

    fn effective_dpr(&self) -> u32 {
        self.device_pixel_ratio.max(1)
    }
}

/// Rectangle sub-region for masking operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetRect {
    /// Left coordinate in pixels.
    pub x: u32,
    /// Top coordinate in pixels.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Helper matching `base::SafeRound` (round half away from zero for positive values).
#[inline]
fn safe_round_u32(v: f64) -> u32 {
    if !v.is_finite() || v <= 0.0 {
        0
    } else if v >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        v.round() as u32
    }
}

/// Scales `(width, height)` to fit inside `(to_width, to_height)` while preserving aspect ratio,
/// using tdesktop's exact rounding from `shrinkToKeepAspect` (`ui/image/image_location.h:653`).
#[must_use]
pub fn shrink_to_keep_aspect(width: u32, height: u32, to_width: u32, to_height: u32) -> (u32, u32) {
    let w = u64::from(width.max(1));
    let h = u64::from(height.max(1));
    let tw = u64::from(to_width.max(1));
    let th = u64::from(to_height.max(1));

    let (rw, rh) = if w * th > h * tw {
        let new_h = safe_round_u32((h as f64) * (tw as f64) / (w as f64));
        (to_width.max(1), new_h)
    } else {
        let new_w = safe_round_u32((w as f64) * (th as f64) / (h as f64));
        (new_w, to_height.max(1))
    };
    (rw.max(1), rh.max(1))
}

/// Computes the height for scaling to target width while keeping aspect ratio (`QImage::scaledToWidth` rounding).
#[must_use]
pub fn scaled_height_for_width(src_w: u32, src_h: u32, target_w: u32) -> u32 {
    if src_w == 0 || src_h == 0 || target_w == 0 {
        return 1;
    }
    safe_round_u32((f64::from(src_h) * f64::from(target_w)) / f64::from(src_w)).max(1)
}

/// Converts a straight-alpha RGBA pixel `[r, g, b, a]` to premultiplied RGBA matching Qt's `ARGB32_Premultiplied`.
#[inline]
#[must_use]
pub fn premultiply_pixel(p: [u8; 4]) -> [u8; 4] {
    let a = u32::from(p[3]);
    if a == 255 {
        p
    } else if a == 0 {
        [0, 0, 0, 0]
    } else {
        [
            ((u32::from(p[0]) * a + 127) / 255) as u8,
            ((u32::from(p[1]) * a + 127) / 255) as u8,
            ((u32::from(p[2]) * a + 127) / 255) as u8,
            p[3],
        ]
    }
}

/// Converts a premultiplied RGBA pixel back to straight-alpha RGBA.
#[inline]
#[must_use]
pub fn unpremultiply_pixel(p: [u8; 4]) -> [u8; 4] {
    let a = u32::from(p[3]);
    if a == 255 || a == 0 {
        p
    } else {
        let r = ((u32::from(p[0]) * 255 + (a / 2)) / a).min(255) as u8;
        let g = ((u32::from(p[1]) * 255 + (a / 2)) / a).min(255) as u8;
        let b = ((u32::from(p[2]) * 255 + (a / 2)) / a).min(255) as u8;
        [r, g, b, p[3]]
    }
}

/// Converts an `RgbaImage` from straight alpha to premultiplied alpha in place.
pub fn premultiply_in_place(image: &mut RgbaImage) {
    for p in image.pixels_mut() {
        p.0 = premultiply_pixel(p.0);
    }
}

/// Converts an `RgbaImage` from premultiplied alpha to straight alpha in place.
pub fn unpremultiply_in_place(image: &mut RgbaImage) {
    for p in image.pixels_mut() {
        p.0 = unpremultiply_pixel(p.0);
    }
}

/// Builds `PrepareOptions` for a rounding radius and corner mask (`Images::RoundOptions`).
#[must_use]
pub fn round_options(radius: ImageRoundRadius, corners: RectCorners) -> PrepareOptions {
    let base = match radius {
        ImageRoundRadius::Large => PrepareOptions::ROUND_LARGE,
        ImageRoundRadius::Small => PrepareOptions::ROUND_SMALL,
        ImageRoundRadius::Ellipse => PrepareOptions::ROUND_CIRCLE,
        ImageRoundRadius::None | ImageRoundRadius::Custom(_) => PrepareOptions::empty(),
    };
    if base.is_empty() {
        return base;
    }
    let mut out = base;
    if !corners.contains(RectCorners::TOP_LEFT) {
        out |= PrepareOptions::ROUND_SKIP_TOP_LEFT;
    }
    if !corners.contains(RectCorners::TOP_RIGHT) {
        out |= PrepareOptions::ROUND_SKIP_TOP_RIGHT;
    }
    if !corners.contains(RectCorners::BOTTOM_LEFT) {
        out |= PrepareOptions::ROUND_SKIP_BOTTOM_LEFT;
    }
    if !corners.contains(RectCorners::BOTTOM_RIGHT) {
        out |= PrepareOptions::ROUND_SKIP_BOTTOM_RIGHT;
    }
    out
}

// ============================================================================
// Images::Blur (fixed radius = 3 separable triangle blur)
// ============================================================================

#[inline]
fn blur_get_colors(p: &[u8]) -> u64 {
    u64::from(p[0]) + (u64::from(p[1]) << 16) + (u64::from(p[2]) << 32) + (u64::from(p[3]) << 48)
}

/// Applies tdesktop's fixed radius-3 triangle blur (`Images::Blur` in `image_prepare.cpp:607`).
///
/// Operates on RGBA pixels (converting via premultiplied alpha if `ignore_alpha` is false
/// and the image has non-opaque pixels, shrinking by `radius` on each side into a transparent
/// border exactly like tdesktop's `withalpha` path).
#[must_use]
pub fn blur(mut image: RgbaImage, ignore_alpha: bool) -> RgbaImage {
    let w = image.width() as usize;
    let h = image.height() as usize;
    let radius = 3usize;
    let r1 = radius + 1;
    let div = radius * 2 + 1;
    if w == 0 || h == 0 || div >= w || div >= h {
        return image;
    }

    let has_alpha = !ignore_alpha && image.pixels().any(|p| p.0[3] < 255);
    if has_alpha {
        let inner_w = (w - 2 * radius) as u32;
        let inner_h = (h - 2 * radius) as u32;
        let scaled = image::imageops::resize(&image, inner_w, inner_h, FilterType::Triangle);
        let mut smaller = RgbaImage::from_pixel(w as u32, h as u32, Rgba([0, 0, 0, 0]));
        for y in 0..inner_h {
            for x in 0..inner_w {
                let p = premultiply_pixel(scaled.get_pixel(x, y).0);
                smaller.put_pixel(x + radius as u32, y + radius as u32, Rgba(p));
            }
        }
        image = smaller;
    }

    let stride = w * 4;
    let pix = image.as_mut();
    let mut rgb = vec![0u64; w * h];

    let we = w - r1;
    for y in 0..h {
        let yw = y * stride;
        let cur = blur_get_colors(&pix[yw..yw + 4]);
        let mut rgballsum = (-(radius as i64) as u64).wrapping_mul(cur);
        let mut rgbsum = cur.wrapping_mul(((r1 * (r1 + 1)) >> 1) as u64);

        for i in 1..=radius {
            let c = blur_get_colors(&pix[yw + i * 4..yw + i * 4 + 4]);
            rgbsum = rgbsum.wrapping_add(c.wrapping_mul((r1 - i) as u64));
            rgballsum = rgballsum.wrapping_add(c);
        }

        let mut x = 0usize;
        while x < w {
            rgb[y * w + x] = (rgbsum >> 4) & 0x00FF_00FF_00FF_00FF;
            let start = x.saturating_sub(r1);
            let middle = x;
            let end = if x < we { x + r1 } else { w - 1 };
            let c_start = blur_get_colors(&pix[yw + start * 4..yw + start * 4 + 4]);
            let c_mid = blur_get_colors(&pix[yw + middle * 4..yw + middle * 4 + 4]);
            let c_end = blur_get_colors(&pix[yw + end * 4..yw + end * 4 + 4]);
            rgballsum = rgballsum
                .wrapping_add(c_start)
                .wrapping_sub(c_mid.wrapping_mul(2))
                .wrapping_add(c_end);
            rgbsum = rgbsum.wrapping_add(rgballsum);
            x += 1;
        }
    }

    let he = h - r1;
    for x in 0..w {
        let cur = rgb[x];
        let mut rgballsum = (-(radius as i64) as u64).wrapping_mul(cur);
        let mut rgbsum = cur.wrapping_mul(((r1 * (r1 + 1)) >> 1) as u64);
        for i in 1..=radius {
            let c = rgb[i * w + x];
            rgbsum = rgbsum.wrapping_add(c.wrapping_mul((r1 - i) as u64));
            rgballsum = rgballsum.wrapping_add(c);
        }

        let mut y = 0usize;
        let mut yi = x * 4;
        while y < h {
            let res = rgbsum >> 4;
            pix[yi] = (res & 0xFF) as u8;
            pix[yi + 1] = ((res >> 16) & 0xFF) as u8;
            pix[yi + 2] = ((res >> 32) & 0xFF) as u8;
            pix[yi + 3] = ((res >> 48) & 0xFF) as u8;

            let start = y.saturating_sub(r1);
            let middle = y;
            let end = if y < he { y + r1 } else { h - 1 };
            rgballsum = rgballsum
                .wrapping_add(rgb[x + start * w])
                .wrapping_sub(rgb[x + middle * w].wrapping_mul(2))
                .wrapping_add(rgb[x + end * w]);
            rgbsum = rgbsum.wrapping_add(rgballsum);
            y += 1;
            yi += stride;
        }
    }

    if has_alpha {
        unpremultiply_in_place(&mut image);
    }
    image
}

// ============================================================================
// Images::BlurLargeImage (exact StackBlur from image_prepare.cpp + image_blur_simd_generic.h)
// ============================================================================

#[derive(Clone, Copy, Default)]
struct StackQuad {
    lane: [u32; 4],
}

#[inline]
fn stack_make_divider(radius: usize) -> u32 {
    let step = (radius as u64) + 1;
    0x8000_0000u64.div_ceil(step) as u32
}

/// Halving twice by `radius + 1` is `floor(value / ((radius + 1)^2))`, exact up to radius 213.
#[inline]
fn stack_divide(mut q: StackQuad, divider: u32) -> StackQuad {
    let d = u64::from(divider);
    for lane in &mut q.lane {
        let once = ((u64::from(*lane) * d) >> 31) as u32;
        *lane = ((u64::from(once) * d) >> 31) as u32;
    }
    q
}

#[inline]
fn stack_add(a: StackQuad, b: StackQuad) -> StackQuad {
    StackQuad {
        lane: [
            a.lane[0].wrapping_add(b.lane[0]),
            a.lane[1].wrapping_add(b.lane[1]),
            a.lane[2].wrapping_add(b.lane[2]),
            a.lane[3].wrapping_add(b.lane[3]),
        ],
    }
}

#[inline]
fn stack_sub(a: StackQuad, b: StackQuad) -> StackQuad {
    StackQuad {
        lane: [
            a.lane[0].wrapping_sub(b.lane[0]),
            a.lane[1].wrapping_sub(b.lane[1]),
            a.lane[2].wrapping_sub(b.lane[2]),
            a.lane[3].wrapping_sub(b.lane[3]),
        ],
    }
}

#[inline]
fn stack_scale(a: StackQuad, by: u32) -> StackQuad {
    StackQuad {
        lane: [
            a.lane[0].wrapping_mul(by),
            a.lane[1].wrapping_mul(by),
            a.lane[2].wrapping_mul(by),
            a.lane[3].wrapping_mul(by),
        ],
    }
}

#[inline]
fn stack_load(slice: &[u8]) -> StackQuad {
    StackQuad {
        lane: [
            u32::from(slice[0]),
            u32::from(slice[1]),
            u32::from(slice[2]),
            u32::from(slice[3]),
        ],
    }
}

#[inline]
fn stack_store(dst: &mut [u8], q: StackQuad, keep_alpha: bool) {
    dst[0] = q.lane[0] as u8;
    dst[1] = q.lane[1] as u8;
    dst[2] = q.lane[2] as u8;
    if !keep_alpha {
        dst[3] = q.lane[3] as u8;
    }
}

#[allow(clippy::too_many_arguments)]
fn stack_blur_pass_1lane(
    src: &[u8],
    dst: &mut [u8],
    base_src: usize,
    base_dst: usize,
    count: usize,
    step: usize,
    radius: usize,
    divider: u32,
    stack: &mut [StackQuad],
    ahead: &[usize],
    keep_alpha: bool,
) {
    let div = 2 * radius + 1;
    let radius_p1 = radius + 1;
    let last = count - 1;

    let mut insum = StackQuad::default();
    let mut outsum = StackQuad::default();
    let mut sum = StackQuad::default();

    for i_signed in -(radius as isize)..=(radius as isize) {
        let idx = (i_signed + radius as isize) as usize;
        let clamped = i_signed.clamp(0, last as isize) as usize;
        let off = base_src + clamped * step;
        let q = stack_load(&src[off..off + 4]);
        stack[idx] = q;
        let weight = (radius_p1 - i_signed.unsigned_abs()) as u32;
        sum = stack_add(sum, stack_scale(q, weight));
        if i_signed > 0 {
            insum = stack_add(insum, q);
        } else {
            outsum = stack_add(outsum, q);
        }
    }

    let mut stackpointer = radius;
    let mut stackstart = 0usize;
    for (i, &ahead_off) in ahead.iter().enumerate().take(count) {
        let result = stack_divide(sum, divider);
        let dst_off = base_dst + i * step;
        stack_store(&mut dst[dst_off..dst_off + 4], result, keep_alpha);

        let out_idx = stackstart;
        stackstart += 1;
        if stackstart == div {
            stackstart = 0;
        }
        let src_off = base_src + ahead_off;
        let pixel = stack_load(&src[src_off..src_off + 4]);
        stackpointer += 1;
        if stackpointer == div {
            stackpointer = 0;
        }

        let out_q = stack[out_idx];
        sum = stack_sub(sum, outsum);
        outsum = stack_sub(outsum, out_q);
        stack[out_idx] = pixel;
        insum = stack_add(insum, pixel);
        sum = stack_add(sum, insum);
        let in_q = stack[stackpointer];
        outsum = stack_add(outsum, in_q);
        insum = stack_sub(insum, in_q);
    }
}

/// Applies tdesktop's exact StackBlur algorithm (`Images::BlurLargeImage` in
/// `Telegram/lib_ui/ui/image/image_prepare.cpp:839` and `image_blur_simd_generic.h`).
///
/// Preserves the original alpha channel in the vertical pass (`KeepAlpha = true`)
/// and leaves the image unchanged when `width <= radius || height <= radius || radius < 1`.
#[must_use]
pub fn blur_large_image(mut image: RgbaImage, radius: u32) -> RgbaImage {
    let width = image.width() as usize;
    let height = image.height() as usize;
    let radius = radius as usize;
    if width <= radius || height <= radius || radius < 1 {
        return image;
    }

    let stride = width * 4;
    let divider = stack_make_divider(radius);
    let mut stack = vec![StackQuad::default(); 2 * radius + 1];
    let mut ahead = vec![0usize; width.max(height)];
    let mut middle = vec![0u8; width * height * 4];

    // Horizontal pass (writes all 4 channels to `middle`).
    for (x, slot) in ahead.iter_mut().enumerate().take(width) {
        *slot = (x + radius + 1).min(width - 1) * 4;
    }
    let pixels = image.as_mut();
    for y in 0..height {
        let row_base = y * stride;
        stack_blur_pass_1lane(
            pixels,
            &mut middle,
            row_base,
            row_base,
            width,
            4,
            radius,
            divider,
            &mut stack,
            &ahead,
            false,
        );
    }

    // Vertical pass (writes RGB back to `pixels`, preserving alpha as in `KeepAlpha = true`).
    for (y, slot) in ahead.iter_mut().enumerate().take(height) {
        *slot = (y + radius + 1).min(height - 1) * stride;
    }
    for x in 0..width {
        let col_base = x * 4;
        stack_blur_pass_1lane(
            &middle, pixels, col_base, col_base, height, stride, radius, divider, &mut stack,
            &ahead, true,
        );
    }

    image
}

// ============================================================================
// Circle / EllipseMask & Round / CornersMask
// ============================================================================

/// Computes anti-aliased signed-distance coverage in `[0, 255]` using 4x4 supersampling.
#[inline]
fn ellipse_coverage_u8(px: u32, py: u32, w: f64, h: f64) -> u8 {
    let rx = w * 0.5;
    let ry = h * 0.5;
    if rx <= 0.0 || ry <= 0.0 {
        return 0;
    }
    let inv_rx = 1.0 / rx;
    let inv_ry = 1.0 / ry;
    let mut inside = 0u32;
    for sy in 0..4 {
        let y = (f64::from(py) + (f64::from(sy) + 0.5) * 0.25 - ry) * inv_ry;
        let y2 = y * y;
        for sx in 0..4 {
            let x = (f64::from(px) + (f64::from(sx) + 0.5) * 0.25 - rx) * inv_rx;
            if x * x + y2 <= 1.0 {
                inside += 1;
            }
        }
    }
    ((inside * 255 + 8) / 16) as u8
}

/// Generates an anti-aliased white-on-transparent ellipse alpha mask (`Images::EllipseMask`).
#[must_use]
pub fn ellipse_mask(width: u32, height: u32) -> RgbaImage {
    let mut mask = RgbaImage::new(width, height);
    let wf = f64::from(width);
    let hf = f64::from(height);
    for y in 0..height {
        for x in 0..width {
            let a = ellipse_coverage_u8(x, y, wf, hf);
            mask.put_pixel(x, y, Rgba([a, a, a, a]));
        }
    }
    mask
}

/// Generates the 4 corner masks `[top_left, top_right, bottom_left, bottom_right]` of size `radius x radius`
/// (`Images::PrepareCornersMask` in `image_prepare.cpp:77`).
#[must_use]
pub fn corners_mask(radius: u32) -> [RgbaImage; 4] {
    if radius == 0 {
        return [
            RgbaImage::new(0, 0),
            RgbaImage::new(0, 0),
            RgbaImage::new(0, 0),
            RgbaImage::new(0, 0),
        ];
    }
    let r = f64::from(radius);
    let mut tl = RgbaImage::new(radius, radius);
    let mut tr = RgbaImage::new(radius, radius);
    let mut bl = RgbaImage::new(radius, radius);
    let mut br = RgbaImage::new(radius, radius);

    // For top-left corner, circle center is at (r, r) with radius r.
    for y in 0..radius {
        for x in 0..radius {
            let mut inside = 0u32;
            for sy in 0..4 {
                let dy = (f64::from(y) + (f64::from(sy) + 0.5) * 0.25) - r;
                let dy2 = dy * dy;
                for sx in 0..4 {
                    let dx = (f64::from(x) + (f64::from(sx) + 0.5) * 0.25) - r;
                    if dx * dx + dy2 <= r * r {
                        inside += 1;
                    }
                }
            }
            let a = ((inside * 255 + 8) / 16) as u8;
            let px = Rgba([a, a, a, a]);
            tl.put_pixel(x, y, px);
            tr.put_pixel(radius - 1 - x, y, px);
            bl.put_pixel(x, radius - 1 - y, px);
            br.put_pixel(radius - 1 - x, radius - 1 - y, px);
        }
    }
    [tl, tr, bl, br]
}

/// Applies an elliptical/circular mask to `image` (`Images::Circle` in `image_prepare.cpp:1062`).
#[must_use]
pub fn circle(mut image: RgbaImage, target: Option<TargetRect>) -> RgbaImage {
    let rect = target.unwrap_or(TargetRect {
        x: 0,
        y: 0,
        width: image.width(),
        height: image.height(),
    });
    if rect.width == 0 || rect.height == 0 {
        return image;
    }
    let max_x = rect.x.saturating_add(rect.width).min(image.width());
    let max_y = rect.y.saturating_add(rect.height).min(image.height());
    let wf = f64::from(rect.width);
    let hf = f64::from(rect.height);

    for y in rect.y..max_y {
        for x in rect.x..max_x {
            let cov = u32::from(ellipse_coverage_u8(x - rect.x, y - rect.y, wf, hf));
            let p = image.get_pixel_mut(x, y);
            p.0[3] = ((u32::from(p.0[3]) * (cov + 1)) >> 8) as u8;
        }
    }
    image
}

/// Rounds selected corners of `image` using `radius` (`Images::Round` in `image_prepare.cpp:1086`).
#[must_use]
pub fn round(
    mut image: RgbaImage,
    radius: ImageRoundRadius,
    corners: RectCorners,
    target: Option<TargetRect>,
) -> RgbaImage {
    if corners.is_empty() {
        return image;
    }
    let r = match radius {
        ImageRoundRadius::None => return image,
        ImageRoundRadius::Ellipse => return circle(image, target),
        ImageRoundRadius::Large => ROUND_RADIUS_LARGE,
        ImageRoundRadius::Small => ROUND_RADIUS_SMALL,
        ImageRoundRadius::Custom(px) => px,
    };
    if r == 0 {
        return image;
    }

    let rect = target.unwrap_or(TargetRect {
        x: 0,
        y: 0,
        width: image.width(),
        height: image.height(),
    });
    if rect.width < r || rect.height < r {
        return image;
    }
    let masks = corners_mask(r);

    let mut apply_corner = |mask: &RgbaImage, right: bool, bottom: bool| {
        let ox = if right {
            rect.x + rect.width - r
        } else {
            rect.x
        };
        let oy = if bottom {
            rect.y + rect.height - r
        } else {
            rect.y
        };
        for my in 0..r {
            let iy = oy + my;
            if iy >= image.height() {
                break;
            }
            for mx in 0..r {
                let ix = ox + mx;
                if ix >= image.width() {
                    break;
                }
                // Matches `anim::unshifted(anim::shifted(*imageInts) * (mask + 1))`
                let opacity = u32::from(mask.get_pixel(mx, my).0[3]) + 1;
                let p = image.get_pixel_mut(ix, iy);
                p.0[3] = ((u32::from(p.0[3]) * opacity) >> 8) as u8;
            }
        }
    };

    if corners.contains(RectCorners::TOP_LEFT) {
        apply_corner(&masks[0], false, false);
    }
    if corners.contains(RectCorners::TOP_RIGHT) {
        apply_corner(&masks[1], true, false);
    }
    if corners.contains(RectCorners::BOTTOM_LEFT) {
        apply_corner(&masks[2], false, true);
    }
    if corners.contains(RectCorners::BOTTOM_RIGHT) {
        apply_corner(&masks[3], true, true);
    }
    image
}

/// Applies rounding according to [`PrepareOptions`] flags (`Images::Round(image, options)`).
#[must_use]
pub fn round_with_options(
    image: RgbaImage,
    options: PrepareOptions,
    dpr: u32,
    target: Option<TargetRect>,
) -> RgbaImage {
    if options.contains(PrepareOptions::ROUND_CIRCLE) {
        return circle(image, target);
    }
    let base_radius = if options.contains(PrepareOptions::ROUND_LARGE) {
        ROUND_RADIUS_LARGE * dpr.max(1)
    } else if options.contains(PrepareOptions::ROUND_SMALL) {
        ROUND_RADIUS_SMALL * dpr.max(1)
    } else {
        return image;
    };
    let mut corners = RectCorners::empty();
    if !options.contains(PrepareOptions::ROUND_SKIP_TOP_LEFT) {
        corners |= RectCorners::TOP_LEFT;
    }
    if !options.contains(PrepareOptions::ROUND_SKIP_TOP_RIGHT) {
        corners |= RectCorners::TOP_RIGHT;
    }
    if !options.contains(PrepareOptions::ROUND_SKIP_BOTTOM_LEFT) {
        corners |= RectCorners::BOTTOM_LEFT;
    }
    if !options.contains(PrepareOptions::ROUND_SKIP_BOTTOM_RIGHT) {
        corners |= RectCorners::BOTTOM_RIGHT;
    }
    round(
        image,
        ImageRoundRadius::Custom(base_radius),
        corners,
        target,
    )
}

// ============================================================================
// Images::Colored & Images::Opaque
// ============================================================================

/// Tints an image with an RGBA overlay color using tdesktop's exact premultiplied formula
/// (`Images::Colored` in `image_prepare.cpp:1217`).
///
/// `add` is `[r, g, b, a]` (straight alpha).
#[must_use]
pub fn colored(mut image: RgbaImage, add: [u8; 4]) -> RgbaImage {
    let ca = u32::from(add[3]);
    let cr = u32::from(add[0]) * (ca + 1);
    let cg = u32::from(add[1]) * (ca + 1);
    let cb = u32::from(add[2]) * (ca + 1);
    let ra = (0x100 - ca) * 0x100;

    for p in image.pixels_mut() {
        let pm = premultiply_pixel(p.0);
        let a = u32::from(pm[3]) + 1;
        let out_pm = [
            ((ra * u32::from(pm[0]) + a * cr) >> 16) as u8,
            ((ra * u32::from(pm[1]) + a * cg) >> 16) as u8,
            ((ra * u32::from(pm[2]) + a * cb) >> 16) as u8,
            pm[3],
        ];
        p.0 = unpremultiply_pixel(out_pm);
    }
    image
}

/// Composites an image over an opaque white background (`Images::Opaque` in `image_prepare.cpp:1248`).
#[must_use]
pub fn opaque(mut image: RgbaImage) -> RgbaImage {
    let has_alpha = image.pixels().any(|p| p.0[3] < 255);
    if !has_alpha {
        return image;
    }
    for p in image.pixels_mut() {
        let pm = premultiply_pixel(p.0);
        let inv_a = 256u32 - u32::from(pm[3]);
        // bg is premultiplied white (255, 255, 255, 255)
        let r = ((u32::from(pm[0]) * 256 + 255 * inv_a) >> 8).min(255) as u8;
        let g = ((u32::from(pm[1]) * 256 + 255 * inv_a) >> 8).min(255) as u8;
        let b = ((u32::from(pm[2]) * 256 + 255 * inv_a) >> 8).min(255) as u8;
        p.0 = [r, g, b, 255];
    }
    image
}

// ============================================================================
// Images::DitherImage (exact DitherGeneric<1..=4> from image_prepare.cpp:102, 927)
// ============================================================================

fn dither_generic<const BITS: u32>(image: &RgbaImage, rng: &mut dyn RngCore) -> RgbaImage {
    let square_side = 1usize << BITS;
    let shift_half = square_side / 2;
    let mask = (square_side - 1) as u8;

    let width = image.width() as usize;
    let height = image.height() as usize;
    let area = width * height;
    let mut shifts = vec![0u8; area];
    rng.fill_bytes(&mut shifts);

    // Clamp top edge shifts.
    for y in 0..shift_half {
        let min = (shift_half - y) as u8;
        let shifted = min << 4;
        let row = &mut shifts[y * width..(y + 1) * width];
        for s in row {
            if ((*s >> 4) & mask) < min {
                *s = shifted | (*s & 0x0F);
            }
        }
    }
    // Clamp bottom edge shifts.
    for y in (height - (shift_half - 1))..height {
        let max = (shift_half + (height - y - 1)) as u8;
        let shifted = max << 4;
        let row = &mut shifts[y * width..(y + 1) * width];
        for s in row {
            if ((*s >> 4) & mask) > max {
                *s = shifted | (*s & 0x0F);
            }
        }
    }
    // Clamp left edge shifts.
    for y in 0..height {
        let row = &mut shifts[y * width..y * width + shift_half];
        for (x, s) in row.iter_mut().enumerate() {
            let min = (shift_half - x) as u8;
            if (*s & mask) < min {
                *s = (*s & 0xF0) | min;
            }
        }
    }
    // Clamp right edge shifts.
    let right_margin = shift_half - 1;
    if right_margin > 0 {
        for y in 0..height {
            let start = y * width + (width - right_margin);
            let row = &mut shifts[start..start + right_margin];
            for (i, s) in row.iter_mut().enumerate() {
                let max = (shift_half + (right_margin - i - 1)) as u8;
                if (*s & mask) > max {
                    *s = (*s & 0xF0) | max;
                }
            }
        }
    }

    let src = image.as_raw();
    let mut dst = vec![0u8; area * 4];
    for (index, &shift) in shifts.iter().enumerate() {
        let sx = isize::from(shift & mask) - (shift_half as isize);
        let sy = isize::from((shift >> 4) & mask) - (shift_half as isize);
        let src_index = ((index as isize) + sy * (width as isize) + sx) as usize;
        dst[index * 4..index * 4 + 4].copy_from_slice(&src[src_index * 4..src_index * 4 + 4]);
    }
    RgbaImage::from_raw(image.width(), image.height(), dst).unwrap_or_else(|| image.clone())
}

/// Applies tdesktop's spatial pixel-jitter dithering (`Images::DitherImage` in `image_prepare.cpp:927`).
///
/// Uses a supplied `rng` so callers and tests can obtain deterministic output.
#[must_use]
pub fn dither_image_with_rng(image: &RgbaImage, rng: &mut dyn RngCore) -> RgbaImage {
    let w = image.width();
    let h = image.height();
    let min_side = w.min(h);
    let max_side = w.max(h);
    if max_side >= 1024 && min_side >= 512 {
        dither_generic::<4>(image, rng)
    } else if max_side >= 512 && min_side >= 256 {
        dither_generic::<3>(image, rng)
    } else if max_side >= 256 && min_side >= 128 {
        dither_generic::<2>(image, rng)
    } else if min_side >= 32 {
        dither_generic::<1>(image, rng)
    } else {
        image.clone()
    }
}

/// Applies tdesktop's spatial pixel-jitter dithering using thread-local randomness (`Images::DitherImage`).
#[must_use]
pub fn dither_image(image: &RgbaImage) -> RgbaImage {
    let mut rng = rand::thread_rng();
    dither_image_with_rng(image, &mut rng)
}

// ============================================================================
// Images::Prepare
// ============================================================================

/// Full image preparation pipeline matching `Images::Prepare` (`image_prepare.cpp:1269`):
/// 1. Optional blur (`Option::Blur`)
/// 2. Scaling to `(w, h)` (or proportional width-only when `h == 0`)
/// 3. Optional centering inside `args.outer` (black or transparent background)
/// 4. Optional circle or corner rounding
/// 5. Optional colorize (`args.colored`)
#[must_use]
pub fn prepare_image(mut image: RgbaImage, w: u32, h: u32, args: &PrepareArgs) -> RgbaImage {
    if image.width() == 0 || image.height() == 0 {
        return image;
    }
    if args.options.contains(PrepareOptions::BLUR) {
        image = blur(image, false);
    }

    let filter = if args.options.contains(PrepareOptions::FAST_TRANSFORM) {
        FilterType::Nearest
    } else {
        FilterType::Triangle
    };

    if w == 0 || (w == image.width() && (h == 0 || h == image.height())) {
        // Keep current dimensions.
    } else if h == 0 {
        let target_h = scaled_height_for_width(image.width(), image.height(), w);
        image = image::imageops::resize(&image, w, target_h, filter);
    } else {
        image = image::imageops::resize(&image, w, h, filter);
    }

    let dpr = args.effective_dpr();
    if let Some((outer_w, outer_h)) = args.outer {
        let ow = outer_w.saturating_mul(dpr);
        let oh = outer_h.saturating_mul(dpr);
        if ow > 0 && oh > 0 && (ow != image.width() || oh != image.height()) {
            let bg = if args
                .options
                .contains(PrepareOptions::TRANSPARENT_BACKGROUND)
            {
                Rgba([0, 0, 0, 0])
            } else {
                Rgba([0, 0, 0, 255])
            };
            let mut canvas = RgbaImage::from_pixel(ow, oh, bg);
            let dst_x = (ow as i64 - image.width() as i64) / 2;
            let dst_y = (oh as i64 - image.height() as i64) / 2;
            image::imageops::overlay(&mut canvas, &image, dst_x, dst_y);
            image = canvas;
        }
    }

    if args.options.intersects(
        PrepareOptions::ROUND_CIRCLE | PrepareOptions::ROUND_LARGE | PrepareOptions::ROUND_SMALL,
    ) {
        image = round_with_options(image, args.options, dpr, None);
    }

    if let Some(color) = args.colored {
        image = colored(image, color);
    }

    image
}

// ============================================================================
// Gradient & Shadow generators (Images::GenerateGradient / GenerateShadow)
// ============================================================================

/// Generates a vertical 1px-wide alpha shadow strip (`Images::GenerateShadow` in `image_prepare.cpp:1015`).
#[must_use]
pub fn generate_shadow(height: u32, top_alpha: u8, bottom_alpha: u8, rgb: [u8; 3]) -> RgbaImage {
    if height == 0 {
        return RgbaImage::new(0, 0);
    }
    let mut out = RgbaImage::new(1, height);
    if top_alpha == bottom_alpha || height == 1 {
        for y in 0..height {
            out.put_pixel(0, y, Rgba([rgb[0], rgb[1], rgb[2], top_alpha]));
        }
        return out;
    }
    const SHIFT: u32 = 16;
    const MULTIPLY: u32 = 1 << SHIFT;
    let values = u32::from(top_alpha.abs_diff(bottom_alpha));
    let step = (values * MULTIPLY) / (height - 1);
    for y in 0..height {
        let delta = ((y * step) >> SHIFT) as u8;
        let a = if top_alpha < bottom_alpha {
            top_alpha.saturating_add(delta)
        } else {
            top_alpha.saturating_sub(delta)
        };
        out.put_pixel(0, y, Rgba([rgb[0], rgb[1], rgb[2], a]));
    }
    out
}

/// Generates Telegram's 3- or 4-color swirled wallpaper gradient (`GenerateSmallComplexGradient`
/// in `image_prepare.cpp:174`), scaled to `(width, height)`.
#[must_use]
pub fn generate_complex_gradient(
    width: u32,
    height: u32,
    colors: &[[u8; 3]],
    rotation: i32,
    progress: f32,
) -> RgbaImage {
    if width == 0 || height == 0 || colors.is_empty() {
        return RgbaImage::new(0, 0);
    }
    const POSITIONS: [(f32, f32); 8] = [
        (0.80, 0.10),
        (0.60, 0.20),
        (0.35, 0.25),
        (0.25, 0.60),
        (0.20, 0.90),
        (0.40, 0.80),
        (0.65, 0.75),
        (0.75, 0.40),
    ];
    let positions_for_phase = |phase: usize| -> [(f32, f32); 4] {
        let mut res = [(0.0f32, 0.0f32); 4];
        for (i, slot) in res.iter_mut().enumerate() {
            let (px, py) = POSITIONS[(phase + i * 2) % 8];
            *slot = (px, 1.0 - py);
        }
        res
    };
    let phase = (rotation.clamp(0, 315) / 45) as usize;
    let previous = positions_for_phase((phase + 1) % 8);
    let current = positions_for_phase(phase);
    let count = colors.len().min(4);

    const KW: u32 = 64;
    const KH: u32 = 64;
    let mut small = RgbaImage::new(KW, KH);
    for y in 0..KH {
        let dy_c = (y as f32) / (KH as f32) - 0.5;
        let dy_c2 = dy_c * dy_c;
        for x in 0..KW {
            let dx_c = (x as f32) / (KW as f32) - 0.5;
            let center_dist = (dx_c * dx_c + dy_c2).sqrt();
            let swirl = 0.35 * center_dist;
            let theta = swirl * swirl * 0.8 * 8.0;
            let (sin_t, cos_t) = theta.sin_cos();
            let px = (0.5 + dx_c * cos_t - dy_c * sin_t).clamp(0.0, 1.0);
            let py = (0.5 + dx_c * sin_t + dy_c * cos_t).clamp(0.0, 1.0);

            let mut dist_sum = 0.0f32;
            let (mut r, mut g, mut b) = (0.0f32, 0.0f32, 0.0f32);
            for i in 0..count {
                let cx = previous[i].0 + (current[i].0 - previous[i].0) * progress;
                let cy = previous[i].1 + (current[i].1 - previous[i].1) * progress;
                let dx = px - cx;
                let dy = py - cy;
                let d = (0.9 - (dx * dx + dy * dy).sqrt()).max(0.0);
                let d2 = d * d;
                let d4 = d2 * d2;
                dist_sum += d4;
                r += d4 * f32::from(colors[i][0]);
                g += d4 * f32::from(colors[i][1]);
                b += d4 * f32::from(colors[i][2]);
            }
            let pixel = if dist_sum > 0.0 {
                Rgba([
                    (r / dist_sum).clamp(0.0, 255.0) as u8,
                    (g / dist_sum).clamp(0.0, 255.0) as u8,
                    (b / dist_sum).clamp(0.0, 255.0) as u8,
                    255,
                ])
            } else {
                Rgba([0, 0, 0, 255])
            };
            small.put_pixel(x, y, pixel);
        }
    }

    if width == KW && height == KH {
        small
    } else {
        image::imageops::resize(&small, width, height, FilterType::Triangle)
    }
}
