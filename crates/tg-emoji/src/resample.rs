//! Area-average downscale of straight RGBA8 pixels, shared by `build.rs` (which shrinks
//! tdesktop's 72 px emoji cells to the atlas' 64 px) and the tests that check the tiles.
//!
//! Each target pixel is the coverage-weighted mean of the source pixels under it, taken in
//! premultiplied alpha so transparent pixels add no colour. The sheet maps 32 cells of 72 px onto
//! 32 cells of 64 px, so cell edges line up exactly and no emoji bleeds into its neighbour.

/// Source spans and coverage weights for each of `dst` target pixels over `src` source pixels.
fn spans(src: usize, dst: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = src as f64 / dst as f64;
    (0..dst)
        .map(|d| {
            let start = d as f64 * scale;
            let end = (d + 1) as f64 * scale;
            let mut taps = Vec::new();
            let mut s = start.floor() as usize;
            while (s as f64) < end && s < src {
                let lo = start.max(s as f64);
                let hi = end.min((s + 1) as f64);
                if hi > lo {
                    taps.push((s, ((hi - lo) / scale) as f32));
                }
                s += 1;
            }
            taps
        })
        .collect()
}

/// Shrinks `rgba` (`width x height`, straight alpha) to `dst_width x dst_height`.
#[must_use]
pub fn area_downscale(
    rgba: &[u8],
    width: usize,
    height: usize,
    dst_width: usize,
    dst_height: usize,
) -> Vec<u8> {
    let premultiplied: Vec<[f32; 4]> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let a = f32::from(p[3]) / 255.0;
            [
                f32::from(p[0]) * a,
                f32::from(p[1]) * a,
                f32::from(p[2]) * a,
                f32::from(p[3]),
            ]
        })
        .collect();

    let columns = spans(width, dst_width);
    let mut horizontal = vec![[0f32; 4]; dst_width * height];
    for y in 0..height {
        let row = &premultiplied[y * width..(y + 1) * width];
        for (x, taps) in columns.iter().enumerate() {
            let mut acc = [0f32; 4];
            for &(s, w) in taps {
                for c in 0..4 {
                    acc[c] += row[s][c] * w;
                }
            }
            horizontal[y * dst_width + x] = acc;
        }
    }

    let rows = spans(height, dst_height);
    let mut out = vec![0u8; dst_width * dst_height * 4];
    for (y, taps) in rows.iter().enumerate() {
        for x in 0..dst_width {
            let mut acc = [0f32; 4];
            for &(s, w) in taps {
                let p = horizontal[s * dst_width + x];
                for c in 0..4 {
                    acc[c] += p[c] * w;
                }
            }
            let alpha = acc[3].round().clamp(0.0, 255.0);
            if alpha < 1.0 {
                continue;
            }
            let unpremultiply = 255.0 / acc[3];
            let i = (y * dst_width + x) * 4;
            for c in 0..3 {
                out[i + c] = (acc[c] * unpremultiply).round().clamp(0.0, 255.0) as u8;
            }
            out[i + 3] = alpha as u8;
        }
    }
    out
}
