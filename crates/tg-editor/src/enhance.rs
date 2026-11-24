//! Local histogram auto-enhance filter.
//!
//! Port of `editor/editor_enhance.h` and `editor/editor_enhance.cpp`.

use image::RgbaImage;

const TILES_PER_SIDE: usize = 4;
const TILE_COUNT: usize = TILES_PER_SIDE * TILES_PER_SIDE;
const LEVELS: usize = 256;

/// Single tile CDF table for [`EnhanceLut`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnhanceTile {
    /// Cumulative distribution function for 256 luminance levels.
    pub cdf: [u8; LEVELS],
    /// First non-zero CDF entry.
    pub cdf_min: u8,
    /// Last CDF entry (`cdf[255]`).
    pub cdf_max: u8,
}

impl Default for EnhanceTile {
    fn default() -> Self {
        Self {
            cdf: [0; LEVELS],
            cdf_min: 0,
            cdf_max: 255,
        }
    }
}

/// Precomputed 4x4 tiled CLAHE lookup table (`Editor::EnhanceLut`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnhanceLut {
    /// 4x4 grid of tile CDFs in row-major order.
    pub tiles: [EnhanceTile; TILE_COUNT],
    /// Whether the LUT was built from a valid image (`width >= 4 && height >= 4`).
    pub valid: bool,
}

fn level_u8(value: f32) -> u8 {
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Builds the 4x4 tiled histogram-equalization LUT (`Editor::PrepareEnhance`).
#[must_use]
pub fn prepare_enhance(image: &RgbaImage) -> EnhanceLut {
    let mut result = EnhanceLut::default();
    let tile_width = image.width() as usize / TILES_PER_SIDE;
    let tile_height = image.height() as usize / TILES_PER_SIDE;
    let tile_area = tile_width * tile_height;
    if tile_area == 0 {
        return result;
    }

    let clip_limit = ((1.25f32 * tile_area as f32 / LEVELS as f32) as usize).max(1);
    let img_width = image.width() as usize;
    let raw = image.as_raw();

    for ty in 0..TILES_PER_SIDE {
        let from_y = ty * tile_height;
        let to_y = from_y + tile_height;
        for tx in 0..TILES_PER_SIDE {
            let from_x = tx * tile_width;
            let to_x = from_x + tile_width;
            let mut histogram = [0usize; LEVELS];
            for y in from_y..to_y {
                let row = y * img_width * 4;
                for x in from_x..to_x {
                    let idx = row + x * 4;
                    let v = raw[idx].max(raw[idx + 1]).max(raw[idx + 2]) as usize;
                    histogram[v] += 1;
                }
            }
            let mut excess = 0usize;
            for count in &mut histogram {
                if *count > clip_limit {
                    excess += *count - clip_limit;
                    *count = clip_limit;
                }
            }
            let share = excess / LEVELS;
            let remainder = excess % LEVELS;
            for (i, count) in histogram.iter_mut().enumerate() {
                *count += share + usize::from(i < remainder);
            }
            let tile = &mut result.tiles[ty * TILES_PER_SIDE + tx];
            let mut sum = 0usize;
            for (i, &count) in histogram.iter().enumerate() {
                sum += count;
                tile.cdf[i] =
                    ((255.0f32 * sum as f32 / tile_area as f32) as i32).clamp(0, 255) as u8;
            }
            tile.cdf_min = tile.cdf.iter().copied().find(|&v| v > 0).unwrap_or(0);
            tile.cdf_max = tile.cdf[LEVELS - 1];
        }
    }

    result.valid = true;
    result
}

/// Applies auto-enhance using a precomputed LUT (`Editor::ApplyEnhance`).
///
/// `amount` is clamped to `0..=100`; values `<= 0` return a clone unchanged.
#[must_use]
pub fn apply_enhance(image: &RgbaImage, lut: &EnhanceLut, amount: i32) -> RgbaImage {
    if amount <= 0 || !lut.valid || image.width() == 0 || image.height() == 0 {
        return image.clone();
    }

    let intensity = amount.clamp(0, 100) as f32 / 100.0;
    let mut normalized = [[0.0f32; LEVELS]; TILE_COUNT];
    for i in 0..TILE_COUNT {
        let tile = &lut.tiles[i];
        let min_val = tile.cdf_min as i32;
        let max_val = tile.cdf_max as i32;
        let range = max_val - min_val;
        for j in 0..LEVELS {
            normalized[i][j] = if range > 0 {
                (tile.cdf[j] as i32 - min_val) as f32 / range as f32
            } else {
                tile.cdf[j] as f32 / 255.0
            };
        }
    }

    let mut out = image.clone();
    let width = out.width() as usize;
    let height = out.height() as usize;
    let raw = out.as_mut();

    for y in 0..height {
        let v = (y as f32 + 0.5) / height as f32;
        let cv = (v.clamp(0.125, 0.87499) - 0.125) * 4.0;
        let ty0 = (cv.floor() as isize).clamp(0, 2) as usize;
        let ty1 = ty0 + 1;
        let frac_y = cv - ty0 as f32;

        for x in 0..width {
            let idx = (y * width + x) * 4;
            let red = raw[idx] as f32 / 255.0;
            let green = raw[idx + 1] as f32 / 255.0;
            let blue = raw[idx + 2] as f32 / 255.0;
            let value = red.max(green).max(blue);
            let minimum = red.min(green).min(blue);
            let delta = value - minimum;
            let saturation = if value > 1e-6 { delta / value } else { 0.0 };

            let mut hue = 0.0f32;
            if delta > 0.0 {
                if value == red {
                    hue = (green - blue) / delta + if green < blue { 6.0 } else { 0.0 };
                } else if value == green {
                    hue = (blue - red) / delta + 2.0;
                } else {
                    hue = (red - green) / delta + 4.0;
                }
                hue /= 6.0;
            }

            let u = (x as f32 + 0.5) / width as f32;
            let cu = (u.clamp(0.125, 0.87499) - 0.125) * 4.0;
            let tx0 = (cu.floor() as isize).clamp(0, 2) as usize;
            let tx1 = tx0 + 1;
            let frac_x = cu - tx0 as f32;

            let level = ((value * 255.0 + 0.5) as isize).clamp(0, 255) as usize;
            let n00 = normalized[ty0 * TILES_PER_SIDE + tx0][level];
            let n01 = normalized[ty0 * TILES_PER_SIDE + tx1][level];
            let n10 = normalized[ty1 * TILES_PER_SIDE + tx0][level];
            let n11 = normalized[ty1 * TILES_PER_SIDE + tx1][level];
            let top = n00 + (n01 - n00) * frac_x;
            let bottom = n10 + (n11 - n10) * frac_x;
            let enhanced = (top + (bottom - top) * frac_y).clamp(0.0, 1.0);

            let target_sat = (saturation * 1.2).min(1.0);
            let target_val = (enhanced * 1.1).min(1.0);
            let final_sat = saturation + (target_sat - saturation) * intensity;
            let final_val = value + (target_val - value) * intensity;

            let mut final_r = final_val;
            let mut final_g = final_val;
            let mut final_b = final_val;
            if final_sat > 0.0 {
                let sector = hue * 6.0;
                let sector_idx = (sector as i32) % 6;
                let frac = sector - sector.floor();
                let p = final_val * (1.0 - final_sat);
                let q = final_val * (1.0 - final_sat * frac);
                let t = final_val * (1.0 - final_sat * (1.0 - frac));
                match sector_idx {
                    0 => {
                        final_g = t;
                        final_b = p;
                    }
                    1 => {
                        final_r = q;
                        final_b = p;
                    }
                    2 => {
                        final_r = p;
                        final_b = t;
                    }
                    3 => {
                        final_r = p;
                        final_g = q;
                    }
                    4 => {
                        final_r = t;
                        final_g = p;
                    }
                    _ => {
                        final_g = p;
                        final_b = q;
                    }
                }
            }
            raw[idx] = level_u8(final_r);
            raw[idx + 1] = level_u8(final_g);
            raw[idx + 2] = level_u8(final_b);
            // Alpha (raw[idx + 3]) preserved.
        }
    }

    out
}

/// Convenience wrapper preparing the LUT and applying enhance (`Editor::EnhanceImage`).
#[must_use]
pub fn enhance_image(image: &RgbaImage, amount: i32) -> RgbaImage {
    if amount <= 0 {
        return image.clone();
    }
    let lut = prepare_enhance(image);
    apply_enhance(image, &lut, amount)
}
