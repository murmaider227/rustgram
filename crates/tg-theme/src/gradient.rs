//! Wallpaper gradient generation, dithering, pattern compositing, and blur.
//!
//! Port of `lib_ui/ui/image/image_prepare.cpp` (`GenerateGradient`, `GenerateLinearGradient`,
//! `GenerateSmallComplexGradient`, `GenerateComplexGradient`, `DitherImage`, `BlurLargeImage`)
//! and `SourceFiles/ui/chat/chat_theme.cpp` (`PreparePatternImage`, `InvertPatternImage`,
//! `PrepareBlurredBackground`, `PreprocessBackgroundImage`, `CalculateImageMonoColor`,
//! `PrepareImageForTiled`, `CountAverageColor`, `ComputeChatBackgroundRects`,
//! `GenerateBackgroundImage`).

use std::sync::OnceLock;

use image::{Rgba, RgbaImage, imageops};

use crate::color::Color;

const COMPLEX_GRID_W: usize = 64;
const COMPLEX_GRID_H: usize = 64;
const MINIMUM_TILED_SIZE: u32 = 512;
const MAX_BACKGROUND_SIZE: u32 = 2960;
const BLUR_MAX_SIZE: u32 = 900;
const BLUR_RADIUS: u32 = 24;
const DITHERED_GRADIENT_SIZE: u32 = 512;

/// Default fallback background color when a wallpaper has no colors (`DefaultBackgroundColor`).
pub const DEFAULT_BACKGROUND_COLOR: Color = Color::rgb(213, 223, 233);

/// Control-point positions for the 8-phase Telegram freeform gradient animation
/// (`GenerateSmallComplexGradient` in `image_prepare.cpp`).
pub const COMPLEX_GRADIENT_POSITIONS: [(f32, f32); 8] = [
    (0.80, 0.10),
    (0.60, 0.20),
    (0.35, 0.25),
    (0.25, 0.60),
    (0.20, 0.90),
    (0.40, 0.80),
    (0.65, 0.75),
    (0.75, 0.40),
];

/// Source and destination crop rectangles for aspect-fill background rendering
/// (`Ui::ChatBackgroundRects`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatBackgroundRects {
    /// Crop rectangle in source image coordinates `(x, y, w, h)`.
    pub from: (i32, i32, u32, u32),
    /// Destination rectangle in viewport coordinates `(x, y, w, h)`.
    pub to: (i32, i32, u32, u32),
}

fn complex_swirl_cache() -> &'static [(f32, f32)] {
    static CACHE: OnceLock<Vec<(f32, f32)>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut result = Vec::with_capacity(COMPLEX_GRID_W * COMPLEX_GRID_H);
        let inv_w = 1.0f32 / (COMPLEX_GRID_W as f32);
        let inv_h = 1.0f32 / (COMPLEX_GRID_H as f32);
        for y in 0..COMPLEX_GRID_H {
            let direct_y = (y as f32) * inv_h;
            let center_dy = direct_y - 0.5;
            let center_dy2 = center_dy * center_dy;
            for x in 0..COMPLEX_GRID_W {
                let direct_x = (x as f32) * inv_w;
                let center_dx = direct_x - 0.5;
                let center_dist = (center_dx * center_dx + center_dy2).sqrt();

                let swirl_factor = 0.35 * center_dist;
                let theta = swirl_factor * swirl_factor * 0.8 * 8.0;
                let sin_t = theta.sin();
                let cos_t = theta.cos();
                let px = (0.5 + center_dx * cos_t - center_dy * sin_t).clamp(0.0, 1.0);
                let py = (0.5 + center_dx * sin_t + center_dy * cos_t).clamp(0.0, 1.0);
                result.push((px, py));
            }
        }
        result
    })
}

/// Computes the effective rotation angle (degrees) for a complex gradient animation step
/// (`ComputeRealRotation` in `chat_theme.cpp`).
#[must_use]
pub fn compute_real_rotation(colors_len: usize, rotation: i32, rotation_add: i32) -> i32 {
    if colors_len < 3 {
        return rotation;
    }
    let doubled = (rotation + rotation_add).rem_euclid(720);
    let adjusted = if (doubled % 2) != 0 {
        doubled - 45
    } else {
        doubled
    };
    (adjusted / 2).rem_euclid(360)
}

/// Computes the effective interpolation progress (`0.5` or `1.0`) for a complex gradient
/// animation step (`ComputeRealProgress` in `chat_theme.cpp`).
#[must_use]
pub fn compute_real_progress(colors_len: usize, rotation: i32, rotation_add: i32) -> f32 {
    if colors_len < 3 {
        return 1.0;
    }
    let doubled = (rotation + rotation_add).rem_euclid(720);
    if (doubled % 2) != 0 { 0.5 } else { 1.0 }
}

/// Generates the 64x64 freeform swirl gradient for 3 or 4 colors (`GenerateSmallComplexGradient`).
#[must_use]
pub fn generate_small_complex_gradient(
    colors: &[Color],
    rotation: i32,
    progress: f32,
) -> RgbaImage {
    let mut result = RgbaImage::new(COMPLEX_GRID_W as u32, COMPLEX_GRID_H as u32);
    if colors.is_empty() {
        return result;
    }
    let count = colors.len().min(4);

    let positions_for_phase = |phase: usize| -> [(f32, f32); 4] {
        let mut pts = [(0.0f32, 0.0f32); 4];
        for (i, slot) in pts.iter_mut().enumerate() {
            let (px, py) = COMPLEX_GRADIENT_POSITIONS[(phase + i * 2) % 8];
            *slot = (px, 1.0 - py);
        }
        pts
    };

    let phase = (rotation.clamp(0, 315) / 45) as usize;
    let previous_phase = (phase + 1) % 8;
    let previous = positions_for_phase(previous_phase);
    let current = positions_for_phase(phase);

    let mut color_pos = [(0.0f32, 0.0f32); 4];
    let mut color_rgb = [[0.0f32; 3]; 4];
    for i in 0..count {
        color_pos[i] = (
            previous[i].0 + (current[i].0 - previous[i].0) * progress,
            previous[i].1 + (current[i].1 - previous[i].1) * progress,
        );
        color_rgb[i] = [
            f32::from(colors[i].r),
            f32::from(colors[i].g),
            f32::from(colors[i].b),
        ];
    }

    let cache = complex_swirl_cache();
    for y in 0..COMPLEX_GRID_H {
        for x in 0..COMPLEX_GRID_W {
            let (pixel_x, pixel_y) = cache[y * COMPLEX_GRID_W + x];
            let mut distance_sum = 0.0f32;
            let mut r = 0.0f32;
            let mut g = 0.0f32;
            let mut b = 0.0f32;
            for i in 0..count {
                let dx = pixel_x - color_pos[i].0;
                let dy = pixel_y - color_pos[i].1;
                let distance = (0.9 - (dx * dx + dy * dy).sqrt()).max(0.0);
                let square = distance * distance;
                let fourth = square * square;
                distance_sum += fourth;
                r += fourth * color_rgb[i][0];
                g += fourth * color_rgb[i][1];
                b += fourth * color_rgb[i][2];
            }
            let (red, green, blue) = if distance_sum > 0.0 {
                (
                    (r / distance_sum).clamp(0.0, 255.0) as u8,
                    (g / distance_sum).clamp(0.0, 255.0) as u8,
                    (b / distance_sum).clamp(0.0, 255.0) as u8,
                )
            } else {
                (0, 0, 0)
            };
            result.put_pixel(x as u32, y as u32, Rgba([red, green, blue, 255]));
        }
    }
    result
}

/// Generates a 3- or 4-color freeform gradient scaled smoothly to `(width, height)`
/// (`Images::GenerateComplexGradient`).
#[must_use]
pub fn generate_complex_gradient(
    width: u32,
    height: u32,
    colors: &[Color],
    rotation: i32,
    progress: f32,
) -> RgbaImage {
    if width == 0 || height == 0 {
        return RgbaImage::new(width, height);
    }
    let exact = generate_small_complex_gradient(colors, rotation, progress);
    if exact.width() == width && exact.height() == height {
        exact
    } else {
        imageops::resize(&exact, width, height, imageops::FilterType::Triangle)
    }
}

/// Generates a solid fill (1 color) or rotated linear gradient (2+ colors)
/// (`Images::GenerateLinearGradient`).
///
/// Rotation is quantized to multiples of 45 degrees in `0..=315`:
/// - `0`: top -> bottom
/// - `45`: top-right -> bottom-left
/// - `90`: right -> left
/// - `135`: bottom-right -> top-left
/// - `180`: bottom -> top
/// - `225`: bottom-left -> top-right
/// - `270`: left -> right
/// - `315`: top-left -> bottom-right
#[must_use]
pub fn generate_linear_gradient(
    width: u32,
    height: u32,
    colors: &[Color],
    rotation: i32,
) -> RgbaImage {
    let mut result = RgbaImage::new(width, height);
    if width == 0 || height == 0 || colors.is_empty() {
        return result;
    }
    if colors.len() == 1 {
        let c = colors[0];
        let px = Rgba([c.r, c.g, c.b, 255]);
        for p in result.pixels_mut() {
            *p = px;
        }
        return result;
    }

    let w = f64::from(width);
    let h = f64::from(height);
    let kind = rotation.clamp(0, 315) / 45;
    let ((x0, y0), (x1, y1)) = match kind {
        0 => ((0.0, 0.0), (0.0, h)),
        1 => ((w, 0.0), (0.0, h)),
        2 => ((w, 0.0), (0.0, 0.0)),
        3 => ((w, h), (0.0, 0.0)),
        4 => ((0.0, h), (0.0, 0.0)),
        5 => ((0.0, h), (w, 0.0)),
        6 => ((0.0, 0.0), (w, 0.0)),
        _ => ((0.0, 0.0), (w, h)),
    };
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len_sq = dx * dx + dy * dy;
    let segments = (colors.len() - 1) as f64;

    for y in 0..height {
        let py = f64::from(y) + 0.5;
        for x in 0..width {
            let px = f64::from(x) + 0.5;
            let t = if len_sq > 0.0 {
                (((px - x0) * dx + (py - y0) * dy) / len_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let scaled = t * segments;
            let seg = (scaled.floor() as usize).min(colors.len() - 2);
            let local_t = scaled - (seg as f64);
            let c0 = colors[seg];
            let c1 = colors[seg + 1];
            let lerp = |a: u8, b: u8| -> u8 {
                (f64::from(a) + (f64::from(b) - f64::from(a)) * local_t)
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            result.put_pixel(
                x,
                y,
                Rgba([lerp(c0.r, c1.r), lerp(c0.g, c1.g), lerp(c0.b, c1.b), 255]),
            );
        }
    }
    result
}

/// Dispatches to [`generate_linear_gradient`] (1-2 colors) or [`generate_complex_gradient`]
/// (3-4 colors), matching `Images::GenerateGradient`.
#[must_use]
pub fn generate_gradient(
    width: u32,
    height: u32,
    colors: &[Color],
    rotation: i32,
    progress: f32,
) -> RgbaImage {
    if width == 0 || height == 0 {
        return RgbaImage::new(width, height);
    }
    if colors.is_empty() {
        return generate_linear_gradient(width, height, &[DEFAULT_BACKGROUND_COLOR], 0);
    }
    if colors.len() > 2 {
        generate_complex_gradient(width, height, colors, rotation, progress)
    } else {
        generate_linear_gradient(width, height, colors, rotation)
    }
}

/// Spatial jitter dithering matching `Images::DitherImage` using a deterministic PRNG seed.
#[must_use]
pub fn dither_image_with_seed(image: &RgbaImage, seed: u64) -> RgbaImage {
    let width = image.width() as usize;
    let height = image.height() as usize;
    let min_dim = width.min(height);
    let max_dim = width.max(height);
    let bits: u32 = if max_dim >= 1024 && min_dim >= 512 {
        4
    } else if max_dim >= 512 && min_dim >= 256 {
        3
    } else if max_dim >= 256 && min_dim >= 128 {
        2
    } else if min_dim >= 32 {
        1
    } else {
        return image.clone();
    };

    let square_side = 1usize << bits;
    let shift = (square_side / 2) as isize;
    let mask = (square_side - 1) as u8;

    let area = width * height;
    let mut state = if seed == 0 {
        0x9e37_79b9_7f4a_7c15u64
    } else {
        seed
    };
    let mut shifts = vec![0u8; area];
    for slot in &mut shifts {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        *slot = (state.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 56) as u8;
    }

    let mut result = RgbaImage::new(image.width(), image.height());
    let src = image.as_raw();
    let dst = result.as_mut();
    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            let raw = shifts[idx];
            let sx = ((raw & mask) as isize) - shift;
            let sy = (((raw >> 4) & mask) as isize) - shift;
            let rx = (x as isize + sx).clamp(0, (width - 1) as isize) as usize;
            let ry = (y as isize + sy).clamp(0, (height - 1) as isize) as usize;
            let src_off = (ry * width + rx) * 4;
            let dst_off = idx * 4;
            dst[dst_off..dst_off + 4].copy_from_slice(&src[src_off..src_off + 4]);
        }
    }
    result
}

/// Applies spatial dithering to reduce gradient banding (`Images::DitherImage`).
#[must_use]
pub fn dither_image(image: &RgbaImage) -> RgbaImage {
    dither_image_with_seed(image, 0x5447_464f_524b_0001)
}

/// Generates a 512x512 dithered gradient (`Ui::GenerateDitheredGradient`).
#[must_use]
pub fn generate_dithered_gradient(colors: &[Color], rotation: i32) -> RgbaImage {
    if colors.is_empty() {
        return generate_gradient(
            DITHERED_GRADIENT_SIZE,
            DITHERED_GRADIENT_SIZE,
            &[DEFAULT_BACKGROUND_COLOR],
            0,
            1.0,
        );
    }
    let result = generate_gradient(
        DITHERED_GRADIENT_SIZE,
        DITHERED_GRADIENT_SIZE,
        colors,
        rotation,
        1.0,
    );
    if colors.len() > 1 {
        dither_image(&result)
    } else {
        result
    }
}

/// Computes the mean RGB color of a slice of colors (`Ui::CountAverageColor(vector<QColor>)`).
#[must_use]
pub fn count_average_color_slice(colors: &[Color]) -> Color {
    if colors.is_empty() {
        return Color::BLACK;
    }
    let mut r = 0u64;
    let mut g = 0u64;
    let mut b = 0u64;
    for &c in colors {
        r += u64::from(c.r);
        g += u64::from(c.g);
        b += u64::from(c.b);
    }
    let n = colors.len() as u64;
    Color::rgb((r / n) as u8, (g / n) as u8, (b / n) as u8)
}

/// Computes the mean RGB color of an `RgbaImage` (`Ui::CountAverageColor(QImage)`).
#[must_use]
pub fn count_average_color_image(image: &RgbaImage) -> Color {
    let total = u64::from(image.width()) * u64::from(image.height());
    if total == 0 {
        return Color::BLACK;
    }
    let mut r = 0u64;
    let mut g = 0u64;
    let mut b = 0u64;
    for p in image.pixels() {
        r += u64::from(p.0[0]);
        g += u64::from(p.0[1]);
        b += u64::from(p.0[2]);
    }
    Color::rgb((r / total) as u8, (g / total) as u8, (b / total) as u8)
}

/// Returns `true` if a positive-opacity pattern should be inverted to white
/// on a dark background (`Ui::IsPatternInverted`).
#[must_use]
pub fn is_pattern_inverted(background: &[Color], pattern_opacity: f64) -> bool {
    pattern_opacity > 0.0 && count_average_color_slice(background).value_f() <= 0.3
}

/// Converts a black-on-transparent pattern mask to white-on-transparent by
/// replicating the alpha byte into R, G, B (`Ui::InvertPatternImage`).
#[must_use]
pub fn invert_pattern_image(pattern: &RgbaImage) -> RgbaImage {
    let mut out = pattern.clone();
    for p in out.pixels_mut() {
        let a = p.0[3];
        p.0 = [a, a, a, a];
    }
    out
}

pub(crate) fn soft_light_channel(cb: f64, cs: f64) -> f64 {
    if cs <= 0.5 {
        cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
    } else {
        let d = if cb <= 0.25 {
            ((16.0 * cb - 12.0) * cb + 4.0) * cb
        } else {
            cb.sqrt()
        };
        cb + (2.0 * cs - 1.0) * (d - cb)
    }
}

/// Composites `pattern` onto `gradient` with `pattern_opacity` (`-1.0..=1.0`),
/// matching `GenerateBackgroundImage` / `PreparePatternImage` in `chat_theme.cpp`.
///
/// - When `pattern_opacity >= 0.0`: inverts pattern if `is_pattern_inverted(bg_colors, pattern_opacity)`,
///   then blends using W3C `SoftLight` at `pattern_opacity`.
/// - When `pattern_opacity < 0.0`: dark-mode masked pattern (`DestinationIn`), followed by
///   `SourceOver` black at opacity `1.0 + pattern_opacity` when `pattern_opacity > -1.0`.
#[must_use]
pub fn compose_pattern_on_gradient(
    gradient: &RgbaImage,
    pattern: &RgbaImage,
    bg_colors: &[Color],
    pattern_opacity: f64,
) -> RgbaImage {
    let width = gradient.width();
    let height = gradient.height();
    let resized_pattern = if pattern.width() == width && pattern.height() == height {
        pattern.clone()
    } else {
        imageops::resize(pattern, width, height, imageops::FilterType::Triangle)
    };
    let effective_pattern = if is_pattern_inverted(bg_colors, pattern_opacity) {
        invert_pattern_image(&resized_pattern)
    } else {
        resized_pattern
    };

    let mut out = gradient.clone();
    let opacity = pattern_opacity.clamp(-1.0, 1.0);

    if opacity >= 0.0 {
        for y in 0..height {
            for x in 0..width {
                let dst = out.get_pixel(x, y).0;
                let src = effective_pattern.get_pixel(x, y).0;
                let sa = (f64::from(src[3]) / 255.0) * opacity;
                if sa <= 0.0 {
                    continue;
                }
                let blend = |d: u8, s: u8| -> u8 {
                    let cb = f64::from(d) / 255.0;
                    // Pattern pixels may be premultiplied or straight; recover straight color.
                    let cs = if src[3] > 0 {
                        (f64::from(s) / f64::from(src[3])).min(1.0)
                    } else {
                        0.0
                    };
                    let sl = soft_light_channel(cb, cs);
                    let res = (1.0 - sa) * cb + sa * sl;
                    (res.clamp(0.0, 1.0) * 255.0).round() as u8
                };
                out.put_pixel(
                    x,
                    y,
                    Rgba([
                        blend(dst[0], src[0]),
                        blend(dst[1], src[1]),
                        blend(dst[2], src[2]),
                        255,
                    ]),
                );
            }
        }
    } else {
        // Negative intensity: DestinationIn with pattern alpha, then SourceOver black
        // with opacity (1 + pattern_opacity) when pattern_opacity > -1.0.
        let black_alpha = if opacity > -1.0 { 1.0 + opacity } else { 0.0 };
        let keep = 1.0 - black_alpha;
        for y in 0..height {
            for x in 0..width {
                let dst = out.get_pixel(x, y).0;
                let src = effective_pattern.get_pixel(x, y).0;
                let da = f64::from(dst[3]) / 255.0;
                let sa = f64::from(src[3]) / 255.0;
                // Premultiplied after DestinationIn:
                let di_a = da * sa;
                let di_r = (f64::from(dst[0]) / 255.0) * di_a;
                let di_g = (f64::from(dst[1]) / 255.0) * di_a;
                let di_b = (f64::from(dst[2]) / 255.0) * di_a;

                // SourceOver black (0, 0, 0, black_alpha):
                let out_a = black_alpha + di_a * keep;
                let out_r_pm = di_r * keep;
                let out_g_pm = di_g * keep;
                let out_b_pm = di_b * keep;

                if out_a <= 0.0 {
                    out.put_pixel(x, y, Rgba([0, 0, 0, 0]));
                } else {
                    let to_u8 =
                        |pm: f64| -> u8 { ((pm / out_a).clamp(0.0, 1.0) * 255.0).round() as u8 };
                    out.put_pixel(
                        x,
                        y,
                        Rgba([
                            to_u8(out_r_pm),
                            to_u8(out_g_pm),
                            to_u8(out_b_pm),
                            (out_a.clamp(0.0, 1.0) * 255.0).round() as u8,
                        ]),
                    );
                }
            }
        }
    }
    out
}

/// Generates a background image of `(width, height)` from `bg` colors, optional dithering,
/// and optional pattern overlay (`Ui::GenerateBackgroundImage`).
#[must_use]
pub fn generate_background_image(
    width: u32,
    height: u32,
    bg: &[Color],
    gradient_rotation: i32,
    pattern_opacity: f64,
    pattern: Option<&RgbaImage>,
) -> RgbaImage {
    let mut result = if bg.is_empty() {
        generate_gradient(width, height, &[DEFAULT_BACKGROUND_COLOR], 0, 1.0)
    } else {
        generate_gradient(width, height, bg, gradient_rotation, 1.0)
    };
    if bg.len() > 1 && (pattern.is_none() || pattern_opacity >= 0.0) {
        result = dither_image(&result);
    }
    if let Some(pat) = pattern {
        result = compose_pattern_on_gradient(&result, pat, bg, pattern_opacity);
    }
    result
}

/// Composites `pattern` onto a gradient matching `pattern`'s dimensions
/// (`Ui::PreparePatternImage`).
#[must_use]
pub fn prepare_pattern_image(
    pattern: &RgbaImage,
    bg: &[Color],
    gradient_rotation: i32,
    pattern_opacity: f64,
) -> RgbaImage {
    generate_background_image(
        pattern.width(),
        pattern.height(),
        bg,
        gradient_rotation,
        pattern_opacity,
        Some(pattern),
    )
}

/// Exact StackBlur port from `lib_ui/ui/image/image_prepare.cpp` and
/// `image_blur_simd_generic.h` (`Images::BlurLargeImage`).
#[must_use]
pub fn blur_large_image(image: &RgbaImage, radius: u32) -> RgbaImage {
    let width = image.width() as usize;
    let height = image.height() as usize;
    let rad = radius as usize;
    if width <= rad || height <= rad || rad < 1 {
        return image.clone();
    }

    let step = (rad + 1) as u64;
    let divider = 0x8000_0000u64.div_ceil(step) as u32;
    let divide = |v: [u32; 4]| -> [u8; 4] {
        let mut out = [0u8; 4];
        for i in 0..4 {
            let once = ((u64::from(v[i]) * u64::from(divider)) >> 31) as u32;
            let twice = ((u64::from(once) * u64::from(divider)) >> 31) as u32;
            out[i] = twice as u8;
        }
        out
    };

    let div = 2 * rad + 1;
    let rad_p1 = rad + 1;
    let src = image.as_raw();
    let mut middle = vec![0u8; width * height * 4];
    let mut dest = src.to_vec();
    let mut stack = vec![[0u32; 4]; div];

    // Horizontal pass
    let ahead_x: Vec<usize> = (0..width).map(|x| (x + rad + 1).min(width - 1)).collect();
    for y in 0..height {
        let row_off = y * width * 4;
        let mut insum = [0u32; 4];
        let mut outsum = [0u32; 4];
        let mut sum = [0u32; 4];

        for i in -(rad as isize)..=(rad as isize) {
            let cx = i.clamp(0, (width - 1) as isize) as usize;
            let off = row_off + cx * 4;
            let px = [
                u32::from(src[off]),
                u32::from(src[off + 1]),
                u32::from(src[off + 2]),
                u32::from(src[off + 3]),
            ];
            stack[(i + rad as isize) as usize] = px;
            let weight = (rad_p1 - i.unsigned_abs()) as u32;
            for c in 0..4 {
                sum[c] += px[c] * weight;
                if i > 0 {
                    insum[c] += px[c];
                } else {
                    outsum[c] += px[c];
                }
            }
        }

        let mut sp = rad;
        let mut ss = 0usize;
        for (x, &ax) in ahead_x.iter().enumerate() {
            let res = divide(sum);
            let dst_off = row_off + x * 4;
            middle[dst_off..dst_off + 4].copy_from_slice(&res);

            let out_px = stack[ss];
            let next_off = row_off + ax * 4;
            let pixel = [
                u32::from(src[next_off]),
                u32::from(src[next_off + 1]),
                u32::from(src[next_off + 2]),
                u32::from(src[next_off + 3]),
            ];
            stack[ss] = pixel;
            ss += 1;
            if ss == div {
                ss = 0;
            }
            sp += 1;
            if sp == div {
                sp = 0;
            }
            let in_px = stack[sp];

            for c in 0..4 {
                sum[c] -= outsum[c];
                outsum[c] -= out_px[c];
                insum[c] += pixel[c];
                sum[c] += insum[c];
                outsum[c] += in_px[c];
                insum[c] -= in_px[c];
            }
        }
    }

    // Vertical pass (keeps original alpha channel as in StackBlur::Pass<..., KeepAlpha=true>)
    let ahead_y: Vec<usize> = (0..height).map(|y| (y + rad + 1).min(height - 1)).collect();
    for x in 0..width {
        let mut insum = [0u32; 4];
        let mut outsum = [0u32; 4];
        let mut sum = [0u32; 4];

        for i in -(rad as isize)..=(rad as isize) {
            let cy = i.clamp(0, (height - 1) as isize) as usize;
            let off = (cy * width + x) * 4;
            let px = [
                u32::from(middle[off]),
                u32::from(middle[off + 1]),
                u32::from(middle[off + 2]),
                u32::from(middle[off + 3]),
            ];
            stack[(i + rad as isize) as usize] = px;
            let weight = (rad_p1 - i.unsigned_abs()) as u32;
            for c in 0..4 {
                sum[c] += px[c] * weight;
                if i > 0 {
                    insum[c] += px[c];
                } else {
                    outsum[c] += px[c];
                }
            }
        }

        let mut sp = rad;
        let mut ss = 0usize;
        for (y, &ay) in ahead_y.iter().enumerate() {
            let res = divide(sum);
            let dst_off = (y * width + x) * 4;
            dest[dst_off] = res[0];
            dest[dst_off + 1] = res[1];
            dest[dst_off + 2] = res[2];
            // dest[dst_off + 3] preserved

            let out_px = stack[ss];
            let next_off = (ay * width + x) * 4;
            let pixel = [
                u32::from(middle[next_off]),
                u32::from(middle[next_off + 1]),
                u32::from(middle[next_off + 2]),
                u32::from(middle[next_off + 3]),
            ];
            stack[ss] = pixel;
            ss += 1;
            if ss == div {
                ss = 0;
            }
            sp += 1;
            if sp == div {
                sp = 0;
            }
            let in_px = stack[sp];

            for c in 0..4 {
                sum[c] -= outsum[c];
                outsum[c] -= out_px[c];
                insum[c] += pixel[c];
                sum[c] += insum[c];
                outsum[c] += in_px[c];
                insum[c] -= in_px[c];
            }
        }
    }

    RgbaImage::from_raw(image.width(), image.height(), dest).unwrap_or_else(|| image.clone())
}

/// Scales `image` to fit within 900x900 preserving aspect ratio, then applies StackBlur
/// with radius 24 (`Ui::PrepareBlurredBackground`).
#[must_use]
pub fn prepare_blurred_background(image: &RgbaImage) -> RgbaImage {
    let resized = if image.width() > BLUR_MAX_SIZE || image.height() > BLUR_MAX_SIZE {
        let (nw, nh) = fit_keep_aspect(image.width(), image.height(), BLUR_MAX_SIZE, BLUR_MAX_SIZE);
        imageops::resize(image, nw, nh, imageops::FilterType::Triangle)
    } else {
        image.clone()
    };
    blur_large_image(&resized, BLUR_RADIUS)
}

fn fit_keep_aspect(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (w, h);
    }
    let scale = (f64::from(max_w) / f64::from(w)).min(f64::from(max_h) / f64::from(h));
    (
        ((f64::from(w) * scale).round() as u32).max(1),
        ((f64::from(h) * scale).round() as u32).max(1),
    )
}

/// Clamps extreme aspect ratios (> 40:1) by center-cropping and scales down images larger
/// than 2960x2960 (`Ui::PreprocessBackgroundImage`).
#[must_use]
pub fn preprocess_background_image(image: &RgbaImage) -> RgbaImage {
    let mut cur = image.clone();
    let w = cur.width();
    let h = cur.height();
    if w == 0 || h == 0 {
        return cur;
    }
    if w > 40 * h {
        let nw = 40 * h;
        let x = (w - nw) / 2;
        cur = imageops::crop_imm(&cur, x, 0, nw, h).to_image();
    } else if h > 40 * w {
        let nh = 40 * w;
        let y = (h - nh) / 2;
        cur = imageops::crop_imm(&cur, 0, y, w, nh).to_image();
    }
    if cur.width() > MAX_BACKGROUND_SIZE || cur.height() > MAX_BACKGROUND_SIZE {
        let (nw, nh) = fit_keep_aspect(
            cur.width(),
            cur.height(),
            MAX_BACKGROUND_SIZE,
            MAX_BACKGROUND_SIZE,
        );
        cur = imageops::resize(&cur, nw, nh, imageops::FilterType::Triangle);
    }
    cur
}

/// Returns `Some(color)` if every pixel in `image` has the exact same RGBA value
/// (`Ui::CalculateImageMonoColor`).
#[must_use]
pub fn calculate_image_mono_color(image: &RgbaImage) -> Option<Color> {
    if image.width() == 0 || image.height() == 0 {
        return None;
    }
    let first = image.get_pixel(0, 0).0;
    for p in image.pixels() {
        if p.0 != first {
            return None;
        }
    }
    Some(Color::rgba(first[0], first[1], first[2], first[3]))
}

/// Repeats small images into a larger tile of at least 512x512 (`Ui::PrepareImageForTiled`).
#[must_use]
pub fn prepare_image_for_tiled(prepared: &RgbaImage) -> RgbaImage {
    let w = prepared.width();
    let h = prepared.height();
    if w == 0 || h == 0 || (w >= MINIMUM_TILED_SIZE && h >= MINIMUM_TILED_SIZE) {
        return prepared.clone();
    }
    let repeat_x = MINIMUM_TILED_SIZE.div_ceil(w);
    let repeat_y = MINIMUM_TILED_SIZE.div_ceil(h);
    let out_w = w * repeat_x;
    let out_h = h * repeat_y;
    let mut out = RgbaImage::new(out_w, out_h);
    for ty in 0..repeat_y {
        for tx in 0..repeat_x {
            imageops::replace(&mut out, prepared, i64::from(tx * w), i64::from(ty * h));
        }
    }
    out
}

/// Computes centered aspect-fill source and destination rects (`Ui::ComputeChatBackgroundRects`).
#[must_use]
pub fn compute_chat_background_rects(
    fill_w: u32,
    fill_h: u32,
    image_w: u32,
    image_h: u32,
) -> ChatBackgroundRects {
    if u64::from(image_w) * u64::from(fill_h) > u64::from(image_h) * u64::from(fill_w) {
        let px_size = f64::from(fill_h) / f64::from(image_h);
        let mut take_w = (f64::from(fill_w) / px_size).ceil() as u32;
        if take_w > image_w {
            take_w = image_w;
        } else if (image_w % 2) != (take_w % 2) {
            take_w += 1;
        }
        ChatBackgroundRects {
            from: (((image_w - take_w) / 2) as i32, 0, take_w, image_h),
            to: (
                ((f64::from(fill_w) - f64::from(take_w) * px_size) / 2.0) as i32,
                0,
                (f64::from(take_w) * px_size).ceil() as u32,
                fill_h,
            ),
        }
    } else {
        let px_size = f64::from(fill_w) / f64::from(image_w);
        let mut take_h = (f64::from(fill_h) / px_size).ceil() as u32;
        if take_h > image_h {
            take_h = image_h;
        } else if (image_h % 2) != (take_h % 2) {
            take_h += 1;
        }
        ChatBackgroundRects {
            from: (0, ((image_h - take_h) / 2) as i32, image_w, take_h),
            to: (
                0,
                ((f64::from(fill_h) - f64::from(take_h) * px_size) / 2.0) as i32,
                fill_w,
                (f64::from(take_h) * px_size).ceil() as u32,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_gradient_rotations_are_deterministic() {
        let red = Color::rgb(255, 0, 0);
        let blue = Color::rgb(0, 0, 255);

        // Rotation 0: top (red) -> bottom (blue)
        let g0 = generate_linear_gradient(16, 16, &[red, blue], 0);
        assert!(g0.get_pixel(8, 0).0[0] > 230);
        assert!(g0.get_pixel(8, 15).0[2] > 230);

        // Rotation 180: bottom (red) -> top (blue)
        let g180 = generate_linear_gradient(16, 16, &[red, blue], 180);
        assert!(g180.get_pixel(8, 0).0[2] > 230);
        assert!(g180.get_pixel(8, 15).0[0] > 230);

        // Rotation 270: left (red) -> right (blue)
        let g270 = generate_linear_gradient(16, 16, &[red, blue], 270);
        assert!(g270.get_pixel(0, 8).0[0] > 230);
        assert!(g270.get_pixel(15, 8).0[2] > 230);

        // Determinism
        assert_eq!(
            g0.as_raw(),
            generate_linear_gradient(16, 16, &[red, blue], 0).as_raw()
        );
    }

    #[test]
    fn complex_gradient_four_color_deterministic_and_phase_dependent() {
        let colors = [
            Color::rgb(219, 221, 187),
            Color::rgb(107, 165, 135),
            Color::rgb(213, 216, 141),
            Color::rgb(136, 184, 132),
        ];
        let a = generate_complex_gradient(64, 64, &colors, 0, 1.0);
        let b = generate_complex_gradient(64, 64, &colors, 0, 1.0);
        assert_eq!(a.as_raw(), b.as_raw());

        // Different phase / progress produces distinct output
        let c = generate_complex_gradient(64, 64, &colors, 45, 0.5);
        assert_ne!(a.as_raw(), c.as_raw());

        // Every pixel is opaque and within color channel hull
        for p in a.pixels() {
            assert_eq!(p.0[3], 255);
            assert!((107..=219).contains(&p.0[0]));
            assert!((165..=221).contains(&p.0[1]));
            assert!((132..=187).contains(&p.0[2]));
        }

        // Animation rotation/progress helper
        assert_eq!(compute_real_rotation(4, 0, 675), 315);
        assert!((compute_real_progress(4, 0, 675) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn pattern_composition_light_and_dark_negative_intensity() {
        let bg = [Color::rgb(200, 220, 240), Color::rgb(100, 150, 200)];
        let grad = generate_gradient(32, 32, &bg, 0, 1.0);
        let mut pat = RgbaImage::new(32, 32);
        // Half transparent, half opaque black pattern
        for y in 0..32 {
            for x in 0..32 {
                if x < 16 {
                    pat.put_pixel(x, y, Rgba([0, 0, 0, 255]));
                } else {
                    pat.put_pixel(x, y, Rgba([0, 0, 0, 0]));
                }
            }
        }

        // Positive intensity: SoftLight alters masked region, leaves unmasked untouched
        let light = compose_pattern_on_gradient(&grad, &pat, &bg, 0.5);
        assert_eq!(light.get_pixel(24, 16), grad.get_pixel(24, 16));
        assert_ne!(light.get_pixel(4, 16), grad.get_pixel(4, 16));

        // Negative intensity: dark-mode mask dims non-pattern area to black
        let dark = compose_pattern_on_gradient(&grad, &pat, &bg, -0.5);
        // Unmasked pixel (sa = 0) gets black at opacity 0.5 -> RGB = (0,0,0), A = 128
        let unmasked = dark.get_pixel(24, 16).0;
        assert_eq!([unmasked[0], unmasked[1], unmasked[2]], [0, 0, 0]);
        assert!(unmasked[3] > 120 && unmasked[3] < 135);
        // Masked pixel (sa = 1) retains tinted gradient dimmed by 50%
        let masked = dark.get_pixel(4, 16).0;
        assert_eq!(masked[3], 255);
        assert!(masked[2] > 40);
    }

    #[test]
    fn stack_blur_smooths_sharp_edge_and_preserves_solid() {
        // Solid image stays identical under blur
        let solid = RgbaImage::from_pixel(64, 64, Rgba([100, 150, 200, 255]));
        let blurred_solid = blur_large_image(&solid, 8);
        assert_eq!(blurred_solid.as_raw(), solid.as_raw());

        // Step edge becomes smooth gradient
        let mut step = RgbaImage::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                let v = if x < 32 { 0 } else { 255 };
                step.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        let blurred = blur_large_image(&step, 8);
        let mid = blurred.get_pixel(32, 32).0[0];
        assert!(mid > 100 && mid < 200);
        assert_eq!(blurred.get_pixel(32, 32).0[3], 255);
    }

    #[test]
    fn mono_color_and_tiled_helpers() {
        let mono = RgbaImage::from_pixel(16, 16, Rgba([10, 20, 30, 255]));
        assert_eq!(
            calculate_image_mono_color(&mono),
            Some(Color::rgb(10, 20, 30))
        );

        let tiled = prepare_image_for_tiled(&mono);
        assert!(tiled.width() >= MINIMUM_TILED_SIZE);
        assert!(tiled.height() >= MINIMUM_TILED_SIZE);
    }
}
