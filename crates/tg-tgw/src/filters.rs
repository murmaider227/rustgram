//! Software CSS/Canvas filter pipeline and nested clip-path mask stack.
//!
//! Port origin: `Telegram/SourceFiles/tgw/tgw_painter_filters.h/.cpp`,
//! `tgw_painter_clips.h/.cpp`, and `tgw_canvas.cpp` clip coverage.

use std::f32::consts::PI;

use serde::{Deserialize, Serialize};
use tiny_skia::{
    BlendMode, Color as SkColor, FillRule, Paint, PathBuilder, Pixmap, Rect, Transform,
};

use crate::canvas::{Matrix2D, PathOp, Rgba};

/// Maximum number of filters in one chain (`kMaxChain = 32` in `tgw_painter_filters.cpp:17`).
pub const MAX_FILTER_CHAIN: usize = 32;

/// Maximum filter length / offset in CSS pixels (`kMaxLength = 32767.0`).
pub const MAX_FILTER_LENGTH: f64 = 32_767.0;

/// Maximum numeric factor for color/hue filters (`kMaxFactor = 1e6`).
pub const MAX_FILTER_FACTOR: f64 = 1e6;

/// Maximum pixel count eligible for software filtering (`kMaxPixels = 1 << 24`).
pub const MAX_FILTER_PIXELS: u64 = 1 << 24;

/// Scratch bytes per RGBA32F pixel (`kPixelScratch = 4 * sizeof(float)`).
pub const FILTER_PIXEL_SCRATCH_BYTES: u64 = 16;

/// Minimum Gaussian blur sigma that performs work (`kMinSigma = 0.5`).
pub const MIN_BLUR_SIGMA: f32 = 0.5;

/// Extra blur margin slack (`kBlurSlack = 3.0`).
pub const BLUR_SLACK: f64 = 3.0;

/// Extra offset margin slack (`kOffsetSlack = 1.0`).
pub const OFFSET_SLACK: f64 = 1.0;

/// Sigma threshold above which lone blurs downsample (`kReducedSigma = 12.0`).
pub const REDUCED_BLUR_SIGMA: f32 = 12.0;

/// Pixel work factor for downsampled blur resampling (`kResamplePixelWork = 2`).
pub const RESAMPLE_PIXEL_WORK: u64 = 2;

/// Estimated bytes per entry in the clip history (`kClipHistoryEntryBytes = 192` in `tgw_painter_clips.h:14`).
pub const CLIP_HISTORY_ENTRY_BYTES: usize = 192;

/// Base clip structure bytes (`kClipStructBytes = 256` in `tgw_painter_clips.cpp:20`).
pub const CLIP_STRUCT_BYTES: usize = 256;

/// Per-row clip scanline metadata bytes (`kClipRowBytes = 32` in `tgw_painter_clips.cpp:21`).
pub const CLIP_ROW_BYTES: usize = 32;

/// W3C Filter Effects Level 1 filter operation (`twlite::Filter`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FilterOp {
    /// Gaussian blur with standard deviation `radius` in CSS pixels (`blur()`).
    Blur {
        /// Blur standard deviation (must be finite and `>= 0`).
        radius: f32,
    },
    /// Offset blurred silhouette drawn behind the source (`drop-shadow()`).
    DropShadow {
        /// Horizontal offset in CSS pixels.
        dx: f32,
        /// Vertical offset in CSS pixels.
        dy: f32,
        /// Blur radius in CSS pixels (`sigma = blur / 2`).
        blur: f32,
        /// Shadow RGBA color.
        color: Rgba,
    },
    /// Linear brightness multiplier (`brightness()`, `1.0` = identity).
    Brightness {
        /// Brightness factor (`>= 0`).
        amount: f32,
    },
    /// Linear contrast multiplier around `0.5` (`contrast()`, `1.0` = identity).
    Contrast {
        /// Contrast factor (`>= 0`).
        amount: f32,
    },
    /// Desaturation toward Rec.709 luminance (`grayscale()`, `0.0` = identity, `1.0` = full).
    Grayscale {
        /// Grayscale fraction (`0.0..=1.0`, values `> 1.0` clamp to `1.0`).
        amount: f32,
    },
    /// Saturation multiplier (`saturate()`, `1.0` = identity, `0.0` = full grayscale).
    Saturate {
        /// Saturation multiplier (`>= 0`).
        amount: f32,
    },
    /// Linear color inversion (`invert()`, `0.0` = identity, `1.0` = full).
    Invert {
        /// Inversion fraction (`0.0..=1.0`, values `> 1.0` clamp to `1.0`).
        amount: f32,
    },
    /// Sepia toning (`sepia()`, `0.0` = identity, `1.0` = full).
    Sepia {
        /// Sepia fraction (`0.0..=1.0`, values `> 1.0` clamp to `1.0`).
        amount: f32,
    },
    /// Alpha scaling (`opacity()`, `1.0` = identity, `0.0` = transparent).
    Opacity {
        /// Opacity fraction (`0.0..=1.0`, values `> 1.0` clamp to `1.0`).
        amount: f32,
    },
    /// Hue rotation in degrees (`hue-rotate()`).
    HueRotate {
        /// Rotation angle in degrees.
        degrees: f32,
    },
}

/// Outward expansion margins required by a filter chain (`QMarginsF` in `tgw_painter_filters.h`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FilterMargins {
    /// Left margin in device pixels.
    pub left: f64,
    /// Top margin in device pixels.
    pub top: f64,
    /// Right margin in device pixels.
    pub right: f64,
    /// Bottom margin in device pixels.
    pub bottom: f64,
}

/// Estimated work units and scratch memory for a filter chain (`Tgw::FilterCost`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FilterCost {
    /// Estimated pixel-work operations.
    pub work: u64,
    /// Estimated temporary scratch buffer bytes.
    pub scratch: u64,
}

fn usable_length(value: f64) -> bool {
    value.is_finite() && (0.0..=MAX_FILTER_LENGTH).contains(&value)
}

/// Validates a single [`FilterOp`] against finite and range limits (`UsableFilter` in `tgw_painter_filters.cpp:33-57`).
#[must_use]
pub fn usable_filter(filter: &FilterOp) -> bool {
    match *filter {
        FilterOp::Blur { radius } => usable_length(f64::from(radius)),
        FilterOp::DropShadow { dx, dy, blur, .. } => {
            dx.is_finite()
                && dy.is_finite()
                && usable_length(f64::from(blur))
                && f64::from(dx).abs() <= MAX_FILTER_LENGTH
                && f64::from(dy).abs() <= MAX_FILTER_LENGTH
        }
        FilterOp::HueRotate { degrees } => {
            degrees.is_finite() && f64::from(degrees).abs() <= MAX_FILTER_FACTOR
        }
        FilterOp::Brightness { amount }
        | FilterOp::Contrast { amount }
        | FilterOp::Grayscale { amount }
        | FilterOp::Saturate { amount }
        | FilterOp::Invert { amount }
        | FilterOp::Sepia { amount }
        | FilterOp::Opacity { amount } => {
            amount.is_finite() && (0.0..=MAX_FILTER_FACTOR).contains(&f64::from(amount))
        }
    }
}

/// Validates a filter chain (`UsableFilters` in `tgw_painter_filters.cpp:162-165`).
#[must_use]
pub fn usable_filters(filters: &[FilterOp]) -> bool {
    filters.len() <= MAX_FILTER_CHAIN && filters.iter().all(usable_filter)
}

/// Computes outward expansion margins needed to avoid clipping blurs/shadows
/// (`FilterReach` in `tgw_painter_filters.cpp:167-186`).
#[must_use]
pub fn filter_reach(filters: &[FilterOp], scale: f64) -> FilterMargins {
    let mut result = FilterMargins::default();
    if !scale.is_finite() || scale <= 0.0 {
        return result;
    }
    for filter in filters {
        match *filter {
            FilterOp::Blur { radius } => {
                let amount = f64::from(radius).max(0.0) * scale;
                let spread = 3.0 * amount + BLUR_SLACK;
                result.left += spread;
                result.top += spread;
                result.right += spread;
                result.bottom += spread;
            }
            FilterOp::DropShadow { dx, dy, blur, .. } => {
                let amount = f64::from(blur).max(0.0) * scale;
                let spread = 1.5 * amount + BLUR_SLACK + OFFSET_SLACK;
                let x = f64::from(dx) * scale;
                let y = f64::from(dy) * scale;
                result.left += (spread - x).max(0.0);
                result.top += (spread - y).max(0.0);
                result.right += (spread + x).max(0.0);
                result.bottom += (spread + y).max(0.0);
            }
            _ => {}
        }
    }
    result
}

fn narrow_scale(scale: f64) -> Option<f32> {
    if !scale.is_finite() || scale <= 0.0 || scale > f64::from(f32::MAX) {
        return None;
    }
    let narrowed = scale as f32;
    (narrowed.is_finite() && narrowed > 0.0).then_some(narrowed)
}

fn scaled_length(value: f32, scaled: f32, scale: f64) -> bool {
    (f64::from(value) * scale).abs() <= MAX_FILTER_LENGTH
        && f64::from(scaled).abs() <= MAX_FILTER_LENGTH
}

fn gaussian_work(width: u64, height: u64, sigma: f32) -> u64 {
    if sigma < MIN_BLUR_SIGMA {
        return 0;
    }
    let d = ((sigma * 3.0 * (2.0 * PI).sqrt() / 4.0) + 0.5).floor() as u64;
    let r = d / 2 + 1;
    width * height + 3 * (height * (width + 2 * r + 1) + width * (height + 2 * r + 1))
}

fn reduced_divisor(
    filters: &[FilterOp],
    width: u32,
    height: u32,
    scale: f64,
    narrowed: f32,
) -> Option<u32> {
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0 || height == 0 || pixels > MAX_FILTER_PIXELS || !usable_filters(filters) {
        return None;
    }
    for filter in filters {
        match *filter {
            FilterOp::Blur { radius } if !scaled_length(radius, radius * narrowed, scale) => {
                return None;
            }
            FilterOp::DropShadow { dx, dy, blur, .. }
                if !scaled_length(blur, blur * narrowed, scale)
                    || !scaled_length(dx, dx * narrowed, scale)
                    || !scaled_length(dy, dy * narrowed, scale) =>
            {
                return None;
            }
            _ => {}
        }
    }
    if filters.len() != 1 {
        return Some(1);
    }
    let FilterOp::Blur { radius } = filters[0] else {
        return Some(1);
    };
    let sigma = radius * narrowed;
    let side = width.min(height);
    let mut divisor = 1u32;
    while divisor <= side / 2 && (sigma / ((divisor * 2) as f32)) >= REDUCED_BLUR_SIGMA {
        divisor *= 2;
    }
    Some(divisor)
}

fn reduced_size(width: u32, height: u32, divisor: u32) -> (u32, u32) {
    (width.div_ceil(divisor), height.div_ceil(divisor))
}

/// Estimates pixel-work and scratch memory cost of `filters` on a `(width, height)` surface
/// (`MeasureFilters` in `tgw_painter_filters.cpp:191-233`).
#[must_use]
pub fn measure_filters(
    filters: &[FilterOp],
    width: u32,
    height: u32,
    scale: f64,
) -> Option<FilterCost> {
    let narrowed = narrow_scale(scale)?;
    let divisor = reduced_divisor(filters, width, height, scale, narrowed)?;
    let (rw, rh) = reduced_size(width, height, divisor);
    let w = u64::from(rw);
    let h = u64::from(rh);
    let pixels = w * h;
    let pass_scale = narrowed / (divisor as f32);
    let mut work = 3 * pixels;
    let mut blur = false;
    let mut shadow = false;
    for filter in filters {
        match *filter {
            FilterOp::Blur { radius } => {
                work = work.saturating_add(gaussian_work(w, h, radius * pass_scale));
                blur = true;
            }
            FilterOp::DropShadow { blur: b, .. } => {
                work = work
                    .saturating_add(3 * pixels)
                    .saturating_add(gaussian_work(w, h, (b * pass_scale) / 2.0));
                shadow = true;
            }
            _ => {
                work = work.saturating_add(pixels);
            }
        }
    }
    let copies: u64 = if shadow {
        2
    } else if blur {
        1
    } else {
        0
    };
    let mut cost = FilterCost {
        work,
        scratch: FILTER_PIXEL_SCRATCH_BYTES * pixels * (1 + copies),
    };
    if divisor > 1 {
        let d = u64::from(divisor);
        let padded = pixels * d * d;
        cost.work = cost.work.saturating_add(RESAMPLE_PIXEL_WORK * padded);
        cost.scratch = cost.scratch.saturating_add(4 * (padded + pixels));
    }
    Some(cost)
}

/// Applies a chain of W3C [`FilterOp`]s in-place to a premultiplied RGBA8 [`Pixmap`]
/// (`ApplyFilters` in `tgw_painter_filters.cpp:235-275`).
///
/// Returns `false` if `scale` or `filters` fail validation.
pub fn apply_filters(pixmap: &mut Pixmap, filters: &[FilterOp], scale: f64) -> bool {
    let Some(narrowed) = narrow_scale(scale) else {
        return false;
    };
    let w = pixmap.width();
    let h = pixmap.height();
    let Some(divisor) = reduced_divisor(filters, w, h, scale, narrowed) else {
        return false;
    };
    if filters.is_empty() {
        return true;
    }
    if divisor == 1 {
        run_filter_pass(pixmap, filters, narrowed);
        return true;
    }

    let (rw, rh) = reduced_size(w, h, divisor);
    let Some(mut low) = downsample_pixmap(pixmap, rw, rh, divisor) else {
        return false;
    };
    run_filter_pass(&mut low, filters, narrowed / (divisor as f32));
    upsample_into_pixmap(&low, pixmap, divisor);
    true
}

fn run_filter_pass(pixmap: &mut Pixmap, filters: &[FilterOp], scale: f32) {
    let width = pixmap.width() as usize;
    let height = pixmap.height() as usize;
    let mut buf = pixmap_to_premul_f32(pixmap);

    for filter in filters {
        match *filter {
            FilterOp::Blur { radius } => {
                let sigma = radius * scale;
                if sigma >= MIN_BLUR_SIGMA {
                    gaussian_blur_rgba_f32(&mut buf, width, height, sigma);
                }
            }
            FilterOp::DropShadow {
                dx,
                dy,
                blur,
                color,
            } => {
                apply_drop_shadow_f32(
                    &mut buf,
                    width,
                    height,
                    dx * scale,
                    dy * scale,
                    (blur * scale) / 2.0,
                    color,
                );
            }
            FilterOp::Brightness { amount } => {
                let factor = amount.max(0.0);
                apply_straight_rgb(&mut buf, |[r, g, b]| {
                    [
                        (r * factor).clamp(0.0, 1.0),
                        (g * factor).clamp(0.0, 1.0),
                        (b * factor).clamp(0.0, 1.0),
                    ]
                });
            }
            FilterOp::Contrast { amount } => {
                let factor = amount.max(0.0);
                let intercept = 0.5 * (1.0 - factor);
                apply_straight_rgb(&mut buf, |[r, g, b]| {
                    [
                        (r * factor + intercept).clamp(0.0, 1.0),
                        (g * factor + intercept).clamp(0.0, 1.0),
                        (b * factor + intercept).clamp(0.0, 1.0),
                    ]
                });
            }
            FilterOp::Grayscale { amount } => {
                let s = 1.0 - amount.clamp(0.0, 1.0);
                let m = saturate_matrix(s);
                apply_color_matrix(&mut buf, &m);
            }
            FilterOp::Saturate { amount } => {
                let m = saturate_matrix(amount.max(0.0));
                apply_color_matrix(&mut buf, &m);
            }
            FilterOp::Sepia { amount } => {
                let s = 1.0 - amount.clamp(0.0, 1.0);
                let m = [
                    [0.393 + 0.607 * s, 0.769 - 0.769 * s, 0.189 - 0.189 * s],
                    [0.349 - 0.349 * s, 0.686 + 0.314 * s, 0.168 - 0.168 * s],
                    [0.272 - 0.272 * s, 0.534 - 0.534 * s, 0.131 + 0.869 * s],
                ];
                apply_color_matrix(&mut buf, &m);
            }
            FilterOp::Invert { amount } => {
                let p = amount.clamp(0.0, 1.0);
                let slope = 1.0 - 2.0 * p;
                apply_straight_rgb(&mut buf, |[r, g, b]| {
                    [
                        (r * slope + p).clamp(0.0, 1.0),
                        (g * slope + p).clamp(0.0, 1.0),
                        (b * slope + p).clamp(0.0, 1.0),
                    ]
                });
            }
            FilterOp::Opacity { amount } => {
                let p = amount.clamp(0.0, 1.0);
                for px in &mut buf {
                    px[0] *= p;
                    px[1] *= p;
                    px[2] *= p;
                    px[3] *= p;
                }
            }
            FilterOp::HueRotate { degrees } => {
                let rad = degrees.to_radians();
                let c = rad.cos();
                let s = rad.sin();
                let m = [
                    [
                        0.213 + c * 0.787 - s * 0.213,
                        0.715 - c * 0.715 - s * 0.715,
                        0.072 - c * 0.072 + s * 0.928,
                    ],
                    [
                        0.213 - c * 0.213 + s * 0.143,
                        0.715 + c * 0.285 + s * 0.140,
                        0.072 - c * 0.072 - s * 0.283,
                    ],
                    [
                        0.213 - c * 0.213 - s * 0.787,
                        0.715 - c * 0.715 + s * 0.715,
                        0.072 + c * 0.928 + s * 0.072,
                    ],
                ];
                apply_color_matrix(&mut buf, &m);
            }
        }
    }

    premul_f32_into_pixmap(&buf, pixmap);
}

fn saturate_matrix(s: f32) -> [[f32; 3]; 3] {
    [
        [
            0.2126 + 0.7874 * s,
            0.7152 - 0.7152 * s,
            0.0722 - 0.0722 * s,
        ],
        [
            0.2126 - 0.2126 * s,
            0.7152 + 0.2848 * s,
            0.0722 - 0.0722 * s,
        ],
        [
            0.2126 - 0.2126 * s,
            0.7152 - 0.7152 * s,
            0.0722 + 0.9278 * s,
        ],
    ]
}

fn apply_straight_rgb(buf: &mut [[f32; 4]], mut f: impl FnMut([f32; 3]) -> [f32; 3]) {
    for px in buf {
        let a = px[3];
        if a <= 0.0 {
            *px = [0.0; 4];
            continue;
        }
        let inv_a = 1.0 / a;
        let straight = [
            (px[0] * inv_a).clamp(0.0, 1.0),
            (px[1] * inv_a).clamp(0.0, 1.0),
            (px[2] * inv_a).clamp(0.0, 1.0),
        ];
        let out = f(straight);
        px[0] = out[0].clamp(0.0, 1.0) * a;
        px[1] = out[1].clamp(0.0, 1.0) * a;
        px[2] = out[2].clamp(0.0, 1.0) * a;
    }
}

fn apply_color_matrix(buf: &mut [[f32; 4]], m: &[[f32; 3]; 3]) {
    apply_straight_rgb(buf, |[r, g, b]| {
        [
            (m[0][0] * r + m[0][1] * g + m[0][2] * b).clamp(0.0, 1.0),
            (m[1][0] * r + m[1][1] * g + m[1][2] * b).clamp(0.0, 1.0),
            (m[2][0] * r + m[2][1] * g + m[2][2] * b).clamp(0.0, 1.0),
        ]
    });
}

fn pixmap_to_premul_f32(pixmap: &Pixmap) -> Vec<[f32; 4]> {
    let data = pixmap.data();
    let (chunks, _) = data.as_chunks::<4>();
    let mut out = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        out.push([
            f32::from(chunk[0]) / 255.0,
            f32::from(chunk[1]) / 255.0,
            f32::from(chunk[2]) / 255.0,
            f32::from(chunk[3]) / 255.0,
        ]);
    }
    out
}

fn premul_f32_into_pixmap(buf: &[[f32; 4]], pixmap: &mut Pixmap) {
    let dst = pixmap.data_mut();
    let (chunks, _) = dst.as_chunks_mut::<4>();
    for (px, chunk) in buf.iter().zip(chunks.iter_mut()) {
        let a = (px[3].clamp(0.0, 1.0) * 255.0).round().clamp(0.0, 255.0) as u8;
        let max_c = f32::from(a);
        chunk[0] = (px[0].clamp(0.0, 1.0) * 255.0).round().clamp(0.0, max_c) as u8;
        chunk[1] = (px[1].clamp(0.0, 1.0) * 255.0).round().clamp(0.0, max_c) as u8;
        chunk[2] = (px[2].clamp(0.0, 1.0) * 255.0).round().clamp(0.0, max_c) as u8;
        chunk[3] = a;
    }
}

fn gaussian_blur_rgba_f32(buf: &mut [[f32; 4]], width: usize, height: usize, sigma: f32) {
    if width == 0 || height == 0 || sigma < MIN_BLUR_SIGMA {
        return;
    }
    if sigma < 3.0 {
        exact_gaussian_blur_rgba_f32(buf, width, height, sigma);
    } else {
        box3_gaussian_blur_rgba_f32(buf, width, height, sigma);
    }
}

fn exact_gaussian_blur_rgba_f32(buf: &mut [[f32; 4]], width: usize, height: usize, sigma: f32) {
    let radius = (3.0 * sigma).ceil() as usize;
    let kernel_len = radius * 2 + 1;
    let mut kernel = vec![0.0f32; kernel_len];
    let inv_two_var = 1.0 / (2.0 * sigma * sigma);
    let mut sum = 0.0f32;
    for (idx, slot) in kernel.iter_mut().enumerate() {
        let d = (idx as isize - radius as isize) as f32;
        let w = (-d * d * inv_two_var).exp();
        *slot = w;
        sum += w;
    }
    for w in &mut kernel {
        *w /= sum;
    }

    let mut tmp = vec![[0.0f32; 4]; width * height];
    // Horizontal pass.
    for y in 0..height {
        let row = y * width;
        for x in 0..width {
            let mut acc = [0.0f32; 4];
            for (k, &weight) in kernel.iter().enumerate() {
                let sx = x as isize + k as isize - radius as isize;
                if sx >= 0 && (sx as usize) < width {
                    let p = buf[row + sx as usize];
                    acc[0] += p[0] * weight;
                    acc[1] += p[1] * weight;
                    acc[2] += p[2] * weight;
                    acc[3] += p[3] * weight;
                }
            }
            tmp[row + x] = acc;
        }
    }
    // Vertical pass.
    for y in 0..height {
        for x in 0..width {
            let mut acc = [0.0f32; 4];
            for (k, &weight) in kernel.iter().enumerate() {
                let sy = y as isize + k as isize - radius as isize;
                if sy >= 0 && (sy as usize) < height {
                    let p = tmp[(sy as usize) * width + x];
                    acc[0] += p[0] * weight;
                    acc[1] += p[1] * weight;
                    acc[2] += p[2] * weight;
                    acc[3] += p[3] * weight;
                }
            }
            buf[y * width + x] = acc;
        }
    }
}

fn box3_gaussian_blur_rgba_f32(buf: &mut [[f32; 4]], width: usize, height: usize, sigma: f32) {
    let d = ((sigma * 3.0 * (2.0 * PI).sqrt() / 4.0) + 0.5)
        .floor()
        .max(1.0) as usize;
    let passes: [(usize, usize); 3] = if d % 2 == 1 {
        let half = d / 2;
        [(half, half), (half, half), (half, half)]
    } else {
        let half = d / 2;
        [
            (half, half.saturating_sub(1)),
            (half.saturating_sub(1), half),
            (half, half),
        ]
    };

    let mut tmp = vec![[0.0f32; 4]; width * height];
    for (left, right) in passes {
        box_blur_h_rgba_f32(buf, &mut tmp, width, height, left, right);
        box_blur_v_rgba_f32(&tmp, buf, width, height, left, right);
    }
}

fn box_blur_h_rgba_f32(
    src: &[[f32; 4]],
    dst: &mut [[f32; 4]],
    width: usize,
    height: usize,
    left: usize,
    right: usize,
) {
    let inv = 1.0 / ((left + right + 1) as f32);
    let mut prefix = vec![[0.0f32; 4]; width + 1];
    for y in 0..height {
        let row = y * width;
        prefix[0] = [0.0; 4];
        for x in 0..width {
            let p = src[row + x];
            let prev = prefix[x];
            prefix[x + 1] = [
                prev[0] + p[0],
                prev[1] + p[1],
                prev[2] + p[2],
                prev[3] + p[3],
            ];
        }
        for x in 0..width {
            let lo = x.saturating_sub(left);
            let hi = (x + right + 1).min(width);
            let a = prefix[hi];
            let b = prefix[lo];
            dst[row + x] = [
                (a[0] - b[0]) * inv,
                (a[1] - b[1]) * inv,
                (a[2] - b[2]) * inv,
                (a[3] - b[3]) * inv,
            ];
        }
    }
}

fn box_blur_v_rgba_f32(
    src: &[[f32; 4]],
    dst: &mut [[f32; 4]],
    width: usize,
    height: usize,
    top: usize,
    bottom: usize,
) {
    let inv = 1.0 / ((top + bottom + 1) as f32);
    let mut prefix = vec![[0.0f32; 4]; height + 1];
    for x in 0..width {
        prefix[0] = [0.0; 4];
        for y in 0..height {
            let p = src[y * width + x];
            let prev = prefix[y];
            prefix[y + 1] = [
                prev[0] + p[0],
                prev[1] + p[1],
                prev[2] + p[2],
                prev[3] + p[3],
            ];
        }
        for y in 0..height {
            let lo = y.saturating_sub(top);
            let hi = (y + bottom + 1).min(height);
            let a = prefix[hi];
            let b = prefix[lo];
            dst[y * width + x] = [
                (a[0] - b[0]) * inv,
                (a[1] - b[1]) * inv,
                (a[2] - b[2]) * inv,
                (a[3] - b[3]) * inv,
            ];
        }
    }
}

fn apply_drop_shadow_f32(
    buf: &mut [[f32; 4]],
    width: usize,
    height: usize,
    dx: f32,
    dy: f32,
    sigma: f32,
    color: Rgba,
) {
    if width == 0 || height == 0 || color.a == 0 {
        return;
    }
    let mut shadow = vec![[0.0f32; 4]; width * height];
    let sr = f32::from(color.r) / 255.0;
    let sg = f32::from(color.g) / 255.0;
    let sb = f32::from(color.b) / 255.0;
    let sa = f32::from(color.a) / 255.0;

    for y in 0..height {
        let sy = y as f32 - dy;
        for x in 0..width {
            let sx = x as f32 - dx;
            let alpha = sample_alpha_bilinear(buf, width, height, sx, sy) * sa;
            if alpha > 0.0 {
                shadow[y * width + x] = [sr * alpha, sg * alpha, sb * alpha, alpha];
            }
        }
    }

    if sigma >= MIN_BLUR_SIGMA {
        gaussian_blur_rgba_f32(&mut shadow, width, height, sigma);
    }

    // SourceOver: original source on top of shadow.
    for (src, sh) in buf.iter_mut().zip(shadow.iter()) {
        let inv_a = 1.0 - src[3];
        src[0] = (src[0] + sh[0] * inv_a).clamp(0.0, 1.0);
        src[1] = (src[1] + sh[1] * inv_a).clamp(0.0, 1.0);
        src[2] = (src[2] + sh[2] * inv_a).clamp(0.0, 1.0);
        src[3] = (src[3] + sh[3] * inv_a).clamp(0.0, 1.0);
    }
}

fn sample_alpha_bilinear(buf: &[[f32; 4]], width: usize, height: usize, x: f32, y: f32) -> f32 {
    let x0 = x.floor() as isize;
    let y0 = y.floor() as isize;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;

    let at = |ix: isize, iy: isize| -> f32 {
        if ix >= 0 && (ix as usize) < width && iy >= 0 && (iy as usize) < height {
            buf[(iy as usize) * width + (ix as usize)][3]
        } else {
            0.0
        }
    };

    let a00 = at(x0, y0);
    let a10 = at(x0 + 1, y0);
    let a01 = at(x0, y0 + 1);
    let a11 = at(x0 + 1, y0 + 1);
    let top = a00 + (a10 - a00) * fx;
    let bot = a01 + (a11 - a01) * fx;
    top + (bot - top) * fy
}

fn downsample_pixmap(src: &Pixmap, rw: u32, rh: u32, divisor: u32) -> Option<Pixmap> {
    let mut out = Pixmap::new(rw, rh)?;
    let sw = src.width() as usize;
    let sh = src.height() as usize;
    let d = divisor as usize;
    let sdata = src.data();
    let ddata = out.data_mut();
    let area = (d * d) as f32;

    for y in 0..(rh as usize) {
        for x in 0..(rw as usize) {
            let mut acc = [0.0f32; 4];
            for dy in 0..d {
                let sy = y * d + dy;
                if sy >= sh {
                    continue;
                }
                for dx in 0..d {
                    let sx = x * d + dx;
                    if sx >= sw {
                        continue;
                    }
                    let idx = (sy * sw + sx) * 4;
                    acc[0] += f32::from(sdata[idx]);
                    acc[1] += f32::from(sdata[idx + 1]);
                    acc[2] += f32::from(sdata[idx + 2]);
                    acc[3] += f32::from(sdata[idx + 3]);
                }
            }
            let out_idx = (y * (rw as usize) + x) * 4;
            let a = (acc[3] / area).round().clamp(0.0, 255.0) as u8;
            let max_c = f32::from(a);
            ddata[out_idx] = (acc[0] / area).round().clamp(0.0, max_c) as u8;
            ddata[out_idx + 1] = (acc[1] / area).round().clamp(0.0, max_c) as u8;
            ddata[out_idx + 2] = (acc[2] / area).round().clamp(0.0, max_c) as u8;
            ddata[out_idx + 3] = a;
        }
    }
    Some(out)
}

fn upsample_into_pixmap(low: &Pixmap, dst: &mut Pixmap, divisor: u32) {
    let lw = low.width() as usize;
    let lh = low.height() as usize;
    let dw = dst.width() as usize;
    let dh = dst.height() as usize;
    let ldata = low.data();
    let ddata = dst.data_mut();
    let scale = 1.0 / (divisor as f32);

    for y in 0..dh {
        let ly = ((y as f32 + 0.5) * scale - 0.5).clamp(0.0, (lh.saturating_sub(1)) as f32);
        let y0 = ly.floor() as usize;
        let y1 = (y0 + 1).min(lh.saturating_sub(1));
        let fy = ly - y0 as f32;
        for x in 0..dw {
            let lx = ((x as f32 + 0.5) * scale - 0.5).clamp(0.0, (lw.saturating_sub(1)) as f32);
            let x0 = lx.floor() as usize;
            let x1 = (x0 + 1).min(lw.saturating_sub(1));
            let fx = lx - x0 as f32;

            let i00 = (y0 * lw + x0) * 4;
            let i10 = (y0 * lw + x1) * 4;
            let i01 = (y1 * lw + x0) * 4;
            let i11 = (y1 * lw + x1) * 4;
            let mut ch = [0.0f32; 4];
            for c in 0..4 {
                let top = f32::from(ldata[i00 + c]) * (1.0 - fx) + f32::from(ldata[i10 + c]) * fx;
                let bot = f32::from(ldata[i01 + c]) * (1.0 - fx) + f32::from(ldata[i11 + c]) * fx;
                ch[c] = top * (1.0 - fy) + bot * fy;
            }
            let out_idx = (y * dw + x) * 4;
            let a = ch[3].round().clamp(0.0, 255.0) as u8;
            let max_c = f32::from(a);
            ddata[out_idx] = ch[0].round().clamp(0.0, max_c) as u8;
            ddata[out_idx + 1] = ch[1].round().clamp(0.0, max_c) as u8;
            ddata[out_idx + 2] = ch[2].round().clamp(0.0, max_c) as u8;
            ddata[out_idx + 3] = a;
        }
    }
}

// =============================================================================
// Nested Clip-Path Stack & Coverage Masking (`tgw_painter_clips.h/.cpp`)
// =============================================================================

/// Axis-aligned clip rectangle in surface coordinates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipRect {
    /// Left coordinate.
    pub x: f32,
    /// Top coordinate.
    pub y: f32,
    /// Width (values `<= 0` denote an empty clip).
    pub width: f32,
    /// Height (values `<= 0` denote an empty clip).
    pub height: f32,
}

impl ClipRect {
    /// Creates a new [`ClipRect`].
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns `true` if the rectangle has non-positive area or non-finite coordinates.
    #[must_use]
    pub fn is_empty(self) -> bool {
        !self.x.is_finite()
            || !self.y.is_finite()
            || !self.width.is_finite()
            || !self.height.is_finite()
            || self.width <= 0.0
            || self.height <= 0.0
    }

    /// Intersects two clip rectangles, returning `None` if either is empty or disjoint.
    #[must_use]
    pub fn intersect(self, other: Self) -> Option<Self> {
        if self.is_empty() || other.is_empty() {
            return None;
        }
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        if right <= left || bottom <= top {
            return None;
        }
        Some(Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }
}

/// Single clip region pushed onto a [`ClipStack`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipShape {
    /// Axis-aligned rectangle with optional affine transform.
    Rect {
        /// Local-space rectangle.
        rect: ClipRect,
        /// Transform into surface space.
        matrix: Matrix2D,
    },
    /// Arbitrary vector path with fill rule and affine transform.
    Path {
        /// Vector path commands.
        ops: Vec<PathOp>,
        /// `true` for even-odd rule, `false` for non-zero winding.
        even_odd: bool,
        /// Transform into surface space.
        matrix: Matrix2D,
    },
}

/// Computes base clip metadata footprint for a surface of `height` rows
/// (`ClipBaseBytes` in `tgw_painter_clips.cpp:25-27`).
#[must_use]
pub const fn clip_base_bytes(height: u32) -> usize {
    CLIP_STRUCT_BYTES + CLIP_ROW_BYTES * (height as usize)
}

/// Nested clip-path stack supporting save/restore, history folding, zero-area safety, and
/// software anti-aliased coverage rasterization (`tgw_painter_clips.h/.cpp` & `tgw_canvas.cpp`).
#[derive(Debug, Clone)]
pub struct ClipStack {
    width: u32,
    height: u32,
    entries: Vec<ClipShape>,
    checkpoints: Vec<usize>,
    cached_bounds: Option<ClipRect>,
    empty: bool,
}

impl ClipStack {
    /// Creates an unclipped stack bound to a `(width, height)` canvas.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        let empty = width == 0 || height == 0;
        let cached_bounds =
            (!empty).then_some(ClipRect::new(0.0, 0.0, width as f32, height as f32));
        Self {
            width,
            height,
            entries: Vec::new(),
            checkpoints: Vec::new(),
            cached_bounds,
            empty,
        }
    }

    /// Returns the surface dimensions `(width, height)`.
    #[must_use]
    pub const fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Returns the number of active clip shapes on the stack.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the intersected clip area is empty (zero area, disjoint, or degenerate).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.empty
    }

    /// Returns the conservative bounding rectangle of the intersected clips, or `None` if empty.
    #[must_use]
    pub const fn bounds(&self) -> Option<ClipRect> {
        if self.empty { None } else { self.cached_bounds }
    }

    /// Returns estimated clip history bytes (`depth * CLIP_HISTORY_ENTRY_BYTES`).
    #[must_use]
    pub fn history_bytes(&self) -> usize {
        self.entries.len().saturating_mul(CLIP_HISTORY_ENTRY_BYTES)
    }

    /// Returns estimated total clip data bytes including scanline overhead (`ClipDataBytes`).
    #[must_use]
    pub fn data_bytes(&self) -> usize {
        if self.entries.is_empty() {
            return 0;
        }
        clip_base_bytes(self.height).saturating_add(self.history_bytes())
    }

    /// Saves the current clip stack depth for a matching [`Self::restore`].
    pub fn save(&mut self) {
        self.checkpoints.push(self.entries.len());
    }

    /// Restores the clip stack to the most recent [`Self::save`] checkpoint.
    ///
    /// Returns `false` if no checkpoint was saved.
    pub fn restore(&mut self) -> bool {
        let Some(target) = self.checkpoints.pop() else {
            return false;
        };
        if target < self.entries.len() {
            self.entries.truncate(target);
            self.recompute_bounds();
        }
        true
    }

    /// Pushes a rectangular clip transformed by `matrix`.
    ///
    /// Zero-area, negative, or non-finite rectangles safely collapse the clip region to empty.
    pub fn push_rect(&mut self, rect: ClipRect, matrix: Matrix2D) {
        self.entries.push(ClipShape::Rect { rect, matrix });
        self.recompute_bounds();
    }

    /// Pushes a vector clip path transformed by `matrix`.
    ///
    /// Empty or non-finite paths safely collapse the clip region to empty.
    pub fn push_path(&mut self, ops: Vec<PathOp>, even_odd: bool, matrix: Matrix2D) {
        self.entries.push(ClipShape::Path {
            ops,
            even_odd,
            matrix,
        });
        self.recompute_bounds();
    }

    /// Pops the top clip entry, recomputing bounds. Returns `false` if already empty.
    pub fn pop(&mut self) -> bool {
        if self.entries.pop().is_some() {
            self.checkpoints.retain(|&cp| cp <= self.entries.len());
            self.recompute_bounds();
            true
        } else {
            false
        }
    }

    /// Clears all clips and checkpoints, restoring the full surface bounds.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.checkpoints.clear();
        self.recompute_bounds();
    }

    /// Folds consecutive axis-aligned rectangular clips into a single intersected rect when safe
    /// (`ShortenClipHistory` in `tgw_painter_clips.cpp:34-64`).
    ///
    /// Returns the number of clip entries removed.
    pub fn shorten_history(&mut self) -> usize {
        if self.entries.len() < 2 || !self.checkpoints.is_empty() {
            return 0;
        }
        let all_axis_aligned_rects = self.entries.iter().all(|shape| {
            matches!(
                shape,
                ClipShape::Rect { matrix, .. }
                    if matrix.is_finite() && matrix.0[1] == 0.0 && matrix.0[2] == 0.0
            )
        });
        if !all_axis_aligned_rects {
            return 0;
        }
        let before = self.entries.len();
        let folded = self
            .cached_bounds
            .unwrap_or(ClipRect::new(0.0, 0.0, 0.0, 0.0));
        self.entries = vec![ClipShape::Rect {
            rect: folded,
            matrix: Matrix2D::default(),
        }];
        before - self.entries.len()
    }

    /// Rasterizes the accumulated anti-aliased clip coverage (`0..=255` per pixel).
    ///
    /// Returns `None` if the surface or intersected clip region has zero area.
    #[must_use]
    pub fn rasterize_coverage(&self) -> Option<Vec<u8>> {
        if self.empty || self.width == 0 || self.height == 0 {
            return None;
        }
        let pixels = (self.width as usize).checked_mul(self.height as usize)?;
        if self.entries.is_empty() {
            return Some(vec![255u8; pixels]);
        }

        let mut coverage = vec![255u8; pixels];
        let mut scratch = Pixmap::new(self.width, self.height)?;
        let mut paint = Paint::default();
        paint.set_color(SkColor::WHITE);
        paint.blend_mode = BlendMode::Source;
        paint.anti_alias = true;

        for shape in &self.entries {
            scratch.fill(SkColor::TRANSPARENT);
            match shape {
                ClipShape::Rect { rect, matrix } => {
                    if rect.is_empty() || !matrix.is_finite() {
                        return None;
                    }
                    let sk_rect = Rect::from_xywh(rect.x, rect.y, rect.width, rect.height)?;
                    let tr = matrix_to_transform(*matrix);
                    scratch.fill_rect(sk_rect, &paint, tr, None);
                }
                ClipShape::Path {
                    ops,
                    even_odd,
                    matrix,
                } => {
                    if !matrix.is_finite() {
                        return None;
                    }
                    let path = build_path(ops)?;
                    let rule = if *even_odd {
                        FillRule::EvenOdd
                    } else {
                        FillRule::Winding
                    };
                    let tr = matrix_to_transform(*matrix);
                    scratch.fill_path(&path, &paint, rule, tr, None);
                }
            }

            let sdata = scratch.data();
            let (s_chunks, _) = sdata.as_chunks::<4>();
            let mut any_nonzero = false;
            for (cov, chunk) in coverage.iter_mut().zip(s_chunks.iter()) {
                let alpha = chunk[3];
                let merged = ((u16::from(*cov) * u16::from(alpha) + 127) / 255) as u8;
                *cov = merged;
                if merged != 0 {
                    any_nonzero = true;
                }
            }
            if !any_nonzero {
                return None;
            }
        }

        Some(coverage)
    }

    /// Multiplies `pixmap` in-place by the current clip mask coverage.
    ///
    /// If the clip region is empty, `pixmap` is cleared to transparent without panicking.
    pub fn apply_mask_to_pixmap(&self, pixmap: &mut Pixmap) {
        if pixmap.width() != self.width || pixmap.height() != self.height {
            return;
        }
        if self.entries.is_empty() && !self.empty {
            return;
        }
        let Some(coverage) = self.rasterize_coverage() else {
            pixmap.fill(SkColor::TRANSPARENT);
            return;
        };
        let dst = pixmap.data_mut();
        let (d_chunks, _) = dst.as_chunks_mut::<4>();
        for (cov, chunk) in coverage.iter().zip(d_chunks.iter_mut()) {
            let c = u16::from(*cov);
            if c == 255 {
                continue;
            }
            if c == 0 {
                chunk.copy_from_slice(&[0, 0, 0, 0]);
            } else {
                let a = ((u16::from(chunk[3]) * c + 127) / 255) as u8;
                chunk[0] = ((u16::from(chunk[0]) * c + 127) / 255).min(u16::from(a)) as u8;
                chunk[1] = ((u16::from(chunk[1]) * c + 127) / 255).min(u16::from(a)) as u8;
                chunk[2] = ((u16::from(chunk[2]) * c + 127) / 255).min(u16::from(a)) as u8;
                chunk[3] = a;
            }
        }
    }

    fn recompute_bounds(&mut self) {
        if self.width == 0 || self.height == 0 {
            self.empty = true;
            self.cached_bounds = None;
            return;
        }
        let mut current = ClipRect::new(0.0, 0.0, self.width as f32, self.height as f32);
        for shape in &self.entries {
            let Some(shape_bounds) = shape_surface_bounds(shape) else {
                self.empty = true;
                self.cached_bounds = None;
                return;
            };
            let Some(next) = current.intersect(shape_bounds) else {
                self.empty = true;
                self.cached_bounds = None;
                return;
            };
            current = next;
        }
        self.empty = false;
        self.cached_bounds = Some(current);
    }
}

fn matrix_to_transform(matrix: Matrix2D) -> Transform {
    let [sx, ky, kx, sy, tx, ty] = matrix.0;
    Transform::from_row(sx, ky, kx, sy, tx, ty)
}

fn map_point(matrix: Matrix2D, x: f32, y: f32) -> (f32, f32) {
    let [sx, ky, kx, sy, tx, ty] = matrix.0;
    (sx * x + kx * y + tx, ky * x + sy * y + ty)
}

fn shape_surface_bounds(shape: &ClipShape) -> Option<ClipRect> {
    match shape {
        ClipShape::Rect { rect, matrix } => {
            if rect.is_empty() || !matrix.is_finite() {
                return None;
            }
            let corners = [
                map_point(*matrix, rect.x, rect.y),
                map_point(*matrix, rect.x + rect.width, rect.y),
                map_point(*matrix, rect.x, rect.y + rect.height),
                map_point(*matrix, rect.x + rect.width, rect.y + rect.height),
            ];
            let mut min_x = f32::INFINITY;
            let mut min_y = f32::INFINITY;
            let mut max_x = f32::NEG_INFINITY;
            let mut max_y = f32::NEG_INFINITY;
            for (x, y) in corners {
                if !x.is_finite() || !y.is_finite() {
                    return None;
                }
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
            let mapped = ClipRect::new(min_x, min_y, max_x - min_x, max_y - min_y);
            (!mapped.is_empty()).then_some(mapped)
        }
        ClipShape::Path { ops, matrix, .. } => {
            if !matrix.is_finite() {
                return None;
            }
            let path = build_path(ops)?;
            let transformed = path.transform(matrix_to_transform(*matrix))?;
            let bounds = transformed.bounds();
            let rect = ClipRect::new(bounds.x(), bounds.y(), bounds.width(), bounds.height());
            (!rect.is_empty()).then_some(rect)
        }
    }
}

fn build_path(ops: &[PathOp]) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    for &op in ops {
        match op {
            PathOp::MoveTo(x, y) if x.is_finite() && y.is_finite() => pb.move_to(x, y),
            PathOp::LineTo(x, y) if x.is_finite() && y.is_finite() => pb.line_to(x, y),
            PathOp::QuadTo(cx, cy, x, y)
                if cx.is_finite() && cy.is_finite() && x.is_finite() && y.is_finite() =>
            {
                pb.quad_to(cx, cy, x, y);
            }
            PathOp::CubicTo(c1x, c1y, c2x, c2y, x, y)
                if c1x.is_finite()
                    && c1y.is_finite()
                    && c2x.is_finite()
                    && c2y.is_finite()
                    && x.is_finite()
                    && y.is_finite() =>
            {
                pb.cubic_to(c1x, c1y, c2x, c2y, x, y);
            }
            PathOp::Close => pb.close(),
            _ => {}
        }
    }
    pb.finish()
}
