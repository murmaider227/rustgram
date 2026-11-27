//! Wave 6 (TGB-3064): frame rotation and row-padding removal, checked against a plain
//! per-pixel reference, plus timings for the report.

use std::time::Instant;

use tg_video::Rotation;
use tg_video::ffmpeg::rotate_pixels_4bpp;
use tg_video::ffmpeg::utility::strip_row_padding;

/// Per-pixel reference: destination (dx, dy) for source (x, y), as in tdesktop's rotation.
fn reference_rotate(src: &[u8], w: usize, h: usize, rotation: Rotation) -> (Vec<u8>, usize, usize) {
    let (dw, dh) = match rotation {
        Rotation::Deg90 | Rotation::Deg270 => (h, w),
        Rotation::Deg0 | Rotation::Deg180 => (w, h),
    };
    let mut dst = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = match rotation {
                Rotation::Deg0 => (x, y),
                Rotation::Deg90 => (h - 1 - y, x),
                Rotation::Deg180 => (w - 1 - x, h - 1 - y),
                Rotation::Deg270 => (y, w - 1 - x),
            };
            let s = (y * w + x) * 4;
            let d = (dy * dw + dx) * 4;
            dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    (dst, dw, dh)
}

fn pattern(w: usize, h: usize) -> Vec<u8> {
    (0..w * h * 4).map(|i| (i * 31 % 251) as u8).collect()
}

#[test]
fn rotation_matches_reference_for_odd_sizes() {
    for (w, h) in [(1, 1), (1, 7), (7, 1), (3, 5), (17, 9), (64, 33)] {
        let src = pattern(w, h);
        for rotation in [
            Rotation::Deg0,
            Rotation::Deg90,
            Rotation::Deg180,
            Rotation::Deg270,
        ] {
            let (got, gw, gh) = rotate_pixels_4bpp(&src, w as u32, h as u32, rotation);
            let (want, ww, wh) = reference_rotate(&src, w, h, rotation);
            assert_eq!((gw as usize, gh as usize), (ww, wh), "{w}x{h} {rotation:?}");
            assert_eq!(got, want, "{w}x{h} {rotation:?}");
        }
    }
}

#[test]
fn rotation_of_short_buffer_returns_it_unchanged() {
    let src = vec![1u8; 10];
    let (out, w, h) = rotate_pixels_4bpp(&src, 4, 4, Rotation::Deg90);
    assert_eq!((out, w, h), (src, 4, 4));
}

#[test]
fn strip_row_padding_keeps_pixels_and_drops_padding() {
    let (w, h, stride) = (5usize, 4usize, 32usize);
    let row = w * 4;
    let mut padded = vec![0xEEu8; stride * h];
    for y in 0..h {
        for x in 0..row {
            padded[y * stride + x] = (y * 100 + x) as u8;
        }
    }
    let tight = strip_row_padding(padded, stride, row, h);
    assert_eq!(tight.len(), row * h);
    for y in 0..h {
        for x in 0..row {
            assert_eq!(tight[y * row + x], (y * 100 + x) as u8);
        }
    }
    // Already tight: returned as is.
    let same = strip_row_padding(vec![1, 2, 3, 4], 4, 4, 1);
    assert_eq!(same, vec![1, 2, 3, 4]);
    // A buffer shorter than the claimed layout is cut to the rows it has, never read past.
    let short = strip_row_padding(vec![9u8; stride + 3], stride, row, h);
    assert_eq!(short, vec![9u8; row]);
}

/// Best of `rounds` runs of `f`, so a busy machine skews the numbers less.
fn best_of(rounds: u32, mut f: impl FnMut()) -> std::time::Duration {
    (0..rounds)
        .map(|_| {
            let start = Instant::now();
            f();
            start.elapsed()
        })
        .min()
        .unwrap_or_default()
}

/// Prints per-frame timings for a rotated 1080x1920 phone video frame, next to the plain
/// per-pixel loop the port used before; the report quotes them, nothing is asserted on time.
#[test]
fn rotation_and_padding_timing() {
    let (w, h) = (1080usize, 1920usize);
    let src = pattern(w, h);
    for rotation in [Rotation::Deg90, Rotation::Deg180, Rotation::Deg270] {
        let new = best_of(15, || {
            let out = rotate_pixels_4bpp(&src, w as u32, h as u32, rotation);
            assert_eq!(out.0.len(), src.len());
        });
        let old = best_of(15, || {
            let out = reference_rotate(&src, w, h, rotation);
            assert_eq!(out.0.len(), src.len());
        });
        println!("rotate 1080x1920 {rotation:?}: {new:?}/frame (per-pixel loop: {old:?})");
    }
    let (pw, ph) = (1000usize, 1000usize);
    let stride = (pw * 4).div_ceil(64) * 64;
    let mut inputs: Vec<Vec<u8>> = (0..15).map(|_| vec![1u8; stride * ph]).collect();
    let new = best_of(15, || {
        let buf = inputs.pop().unwrap_or_default();
        let out = strip_row_padding(buf, stride, pw * 4, ph);
        assert_eq!(out.len(), pw * 4 * ph);
    });
    let padded = vec![1u8; stride * ph];
    let old = best_of(15, || {
        let mut out = vec![0u8; pw * 4 * ph];
        for (d, s) in out
            .chunks_exact_mut(pw * 4)
            .zip(padded.chunks_exact(stride))
        {
            d.copy_from_slice(&s[..pw * 4]);
        }
        assert_eq!(out.len(), pw * 4 * ph);
    });
    println!("strip padding 1000x1000: {new:?}/frame (copy to a new buffer: {old:?})");
}

mod untrusted_bytes {
    use tg_video::streaming::{PartsMap, parse_cached_map, serialize_complex_parts_map};
    use tg_video::{PART_SIZE, check_streaming_support};

    fn next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    /// Slice maps come from the on-disk media cache; any bytes must parse without a panic and
    /// every part must stay inside the slice.
    #[test]
    fn cached_slice_maps_survive_mutation() {
        let max_size = (8 * PART_SIZE) as usize;
        let mut parts = PartsMap::new();
        parts.insert(0, vec![1u8; 300]);
        parts.insert(PART_SIZE as u32, vec![2u8; 17]);
        let seed = serialize_complex_parts_map(&parts);
        let (parsed, rest) = parse_cached_map(&seed, max_size, PART_SIZE);
        assert_eq!(parsed, parts);
        assert!(rest.is_empty());

        let mut state = 0xA076_1D64_78BD_642Fu64;
        for round in 0..3000u32 {
            let mut bytes = seed.clone();
            match round % 3 {
                0 => {
                    let at = (next(&mut state) as usize) % bytes.len();
                    bytes[at] = next(&mut state) as u8;
                }
                1 => bytes.truncate((next(&mut state) as usize) % bytes.len()),
                _ => {
                    let len = (next(&mut state) % 64) as usize;
                    bytes = (0..len).map(|_| next(&mut state) as u8).collect();
                }
            }
            let (map, _) = parse_cached_map(&bytes, max_size, PART_SIZE);
            for (offset, data) in &map {
                assert!((*offset as usize) + data.len() <= max_size);
            }
        }
    }

    #[test]
    fn moov_check_survives_any_atoms() {
        let mut state = 0xE703_7ED1_A0B4_28DBu64;
        // ftyp, then a 64-bit-size atom, then moov.
        let mut seed = Vec::new();
        seed.extend_from_slice(&[0, 0, 0, 16]);
        seed.extend_from_slice(b"ftypisom\0\0\0\0");
        seed.extend_from_slice(&[0, 0, 0, 1]);
        seed.extend_from_slice(b"free");
        seed.extend_from_slice(&24u64.to_be_bytes());
        seed.extend_from_slice(&[0u8; 8]);
        seed.extend_from_slice(&[0, 0, 0, 8]);
        seed.extend_from_slice(b"moov");
        assert!(check_streaming_support(&seed));
        for round in 0..3000u32 {
            let mut bytes = seed.clone();
            if round % 2 == 0 {
                let at = (next(&mut state) as usize) % bytes.len();
                bytes[at] = next(&mut state) as u8;
            } else {
                bytes.truncate((next(&mut state) as usize) % bytes.len());
            }
            let _ = check_streaming_support(&bytes);
        }
    }
}
