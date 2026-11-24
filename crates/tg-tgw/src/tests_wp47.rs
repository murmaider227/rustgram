//! Comprehensive WP-47 unit tests:
//! 1. W3C Compositing Level 1 non-separable HSL blend modes (`Hue`, `Saturation`, `Color`, `Luminosity`)
//!    verified against exact independent W3C reference formulas within ±1 channel rounding with 0
//!    `"is not supported"` diagnostics.
//! 2. Software filter pipeline (`Blur`, `DropShadow`, `Brightness`, `Contrast`, `Grayscale`,
//!    `Saturate`, `Invert`, `Sepia`, `Opacity`, `HueRotate`), cost/reach estimation, downsampled
//!    large blur, and nested clip-path masks including zero-area / disjoint / NaN safety.
//! 3. Disk-backed widget storage (`WidgetStorageDisk`) round-trip across reopen in a tempdir,
//!    per-widget quota, global byte quota with LRU eviction, orphan cleanup, expiry, and atomic replace.
//! 4. `WidgetMeter` watchdog throttle/suspend verdicts under synthetic slow-frame timings,
//!    sustained overload, single-frame timeout, memory pressure/budget, and nested operations.
//! 5. Hosted wire helpers (`is_exactly`, `decode_press`, `EmbeddingCaps`, suspend stop-control).

use std::collections::BTreeMap;
use std::fs;

use tempfile::TempDir;
use tiny_skia::{Color as SkColor, Pixmap};

use crate::*;

// =============================================================================
// Independent W3C Compositing Level 1 reference implementation for known-answer tests
// =============================================================================

fn ref_lum(r: f64, g: f64, b: f64) -> f64 {
    0.3 * r + 0.59 * g + 0.11 * b
}

fn ref_sat(r: f64, g: f64, b: f64) -> f64 {
    r.max(g).max(b) - r.min(g).min(b)
}

fn ref_clip_color(mut r: f64, mut g: f64, mut b: f64) -> (f64, f64, f64) {
    let l = ref_lum(r, g, b);
    let n = r.min(g).min(b);
    let x = r.max(g).max(b);
    if n < 0.0 {
        r = l + (((r - l) * l) / (l - n));
        g = l + (((g - l) * l) / (l - n));
        b = l + (((b - l) * l) / (l - n));
    }
    if x > 1.0 {
        r = l + (((r - l) * (1.0 - l)) / (x - l));
        g = l + (((g - l) * (1.0 - l)) / (x - l));
        b = l + (((b - l) * (1.0 - l)) / (x - l));
    }
    (r, g, b)
}

fn ref_set_lum(r: f64, g: f64, b: f64, l: f64) -> (f64, f64, f64) {
    let d = l - ref_lum(r, g, b);
    ref_clip_color(r + d, g + d, b + d)
}

fn ref_set_sat(r: f64, g: f64, b: f64, s: f64) -> (f64, f64, f64) {
    let mut vals = [(0usize, r), (1, g), (2, b)];
    vals.sort_by(|a, b| a.1.total_cmp(&b.1));
    let min_v = vals[0].1;
    let mid_v = vals[1].1;
    let max_v = vals[2].1;
    let mut out = [0.0; 3];
    if max_v > min_v {
        out[vals[1].0] = ((mid_v - min_v) * s) / (max_v - min_v);
        out[vals[2].0] = s;
    }
    out[vals[0].0] = 0.0;
    (out[0], out[1], out[2])
}

fn ref_w3c_composite(cb: Rgba, cs: Rgba, mode: CompositeMode) -> [u8; 4] {
    let ab = f64::from(cb.a) / 255.0;
    let as_ = f64::from(cs.a) / 255.0;
    let br = f64::from(cb.r) / 255.0;
    let bg = f64::from(cb.g) / 255.0;
    let bb = f64::from(cb.b) / 255.0;
    let sr = f64::from(cs.r) / 255.0;
    let sg = f64::from(cs.g) / 255.0;
    let sb = f64::from(cs.b) / 255.0;

    let (mr, mg, mb) = match mode {
        CompositeMode::Hue => {
            let (tr, tg, tb) = ref_set_sat(sr, sg, sb, ref_sat(br, bg, bb));
            ref_set_lum(tr, tg, tb, ref_lum(br, bg, bb))
        }
        CompositeMode::Saturation => {
            let (tr, tg, tb) = ref_set_sat(br, bg, bb, ref_sat(sr, sg, sb));
            ref_set_lum(tr, tg, tb, ref_lum(br, bg, bb))
        }
        CompositeMode::Color => ref_set_lum(sr, sg, sb, ref_lum(br, bg, bb)),
        CompositeMode::Luminosity => ref_set_lum(br, bg, bb, ref_lum(sr, sg, sb)),
        _ => (sr, sg, sb),
    };

    let ao = as_ + ab * (1.0 - as_);
    if ao == 0.0 {
        return [0, 0, 0, 0];
    }
    let mix = |s: f64, b: f64, m: f64| -> u8 {
        let cr = (1.0 - ab) * s + ab * m;
        let co = (as_ * cr + ab * (1.0 - as_) * b) / ao;
        (co.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    [
        mix(sr, br, mr),
        mix(sg, bg, mg),
        mix(sb, bb, mb),
        (ao.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

fn assert_rgba_near(actual: &[u8], expected: [u8; 4], context: &str) {
    assert_eq!(actual.len(), 4, "{context}: expected 4 bytes");
    for i in 0..4 {
        let diff = (i16::from(actual[i]) - i16::from(expected[i])).abs();
        assert!(
            diff <= 1,
            "{context}: channel {i} mismatch: actual={actual:?}, expected={expected:?} (diff={diff})"
        );
    }
}

// =============================================================================
// 1. W3C Non-Separable HSL Canvas Blend Mode Tests
// =============================================================================

#[test]
fn w3c_hsl_blend_modes_exact_rgba_and_zero_unsupported_diagnostics() {
    let modes = [
        CompositeMode::Hue,
        CompositeMode::Saturation,
        CompositeMode::Color,
        CompositeMode::Luminosity,
    ];
    let pairs = [
        // Opaque chromatic pairs (exercises ClipColor both < 0 and > 1)
        (Rgba::new(220, 80, 40, 255), Rgba::new(30, 180, 230, 255)),
        (Rgba::new(40, 200, 90, 255), Rgba::new(240, 20, 190, 255)),
        // Grayscale backdrop / chromatic source (Sat(Cb) == 0)
        (Rgba::new(128, 128, 128, 255), Rgba::new(255, 0, 0, 255)),
        // Chromatic backdrop / grayscale source (Sat(Cs) == 0)
        (Rgba::new(200, 60, 100, 255), Rgba::new(200, 200, 200, 255)),
        // Semi-transparent alpha compositing
        (Rgba::new(180, 90, 50, 200), Rgba::new(40, 210, 120, 150)),
        (Rgba::new(100, 140, 220, 255), Rgba::new(255, 220, 30, 128)),
    ];

    let mut store = CanvasStore::new();
    for mode in modes {
        assert!(mode.is_non_separable());
        assert_eq!(mode.unsupported_name(), None);

        for (case_idx, &(backdrop, source)) in pairs.iter().enumerate() {
            let canvas_id = (mode as i32) * 100 + case_idx as i32 + 1;
            let ops = vec![
                CanvasOp::FillRect {
                    x: 0.0,
                    y: 0.0,
                    width: 4.0,
                    height: 4.0,
                    color: backdrop,
                    alpha: 1.0,
                    composite: CompositeMode::Copy,
                    matrix: Matrix2D::default(),
                },
                CanvasOp::FillRect {
                    x: 0.0,
                    y: 0.0,
                    width: 4.0,
                    height: 4.0,
                    color: source,
                    alpha: 1.0,
                    composite: mode,
                    matrix: Matrix2D::default(),
                },
            ];
            store.run(canvas_id, 4, 4, &ops).expect("run hsl canvas op");
            let px = store.read(canvas_id, 2, 2, 1, 1).expect("read blended px");
            let expected = ref_w3c_composite(backdrop, source, mode);
            assert_rgba_near(
                &px,
                expected,
                &format!("{mode:?} case {case_idx} backdrop={backdrop:?} source={source:?}"),
            );

            // Also verify direct pixel helper matches reference formula.
            let direct = blend_hsl_pixel(backdrop, source, mode);
            assert_rgba_near(
                &[direct.r, direct.g, direct.b, direct.a],
                expected,
                &format!("direct {mode:?} case {case_idx}"),
            );
        }
    }

    let diags = store.take_diagnostics();
    assert!(
        diags.is_empty(),
        "expected 0 diagnostics for W3C HSL blend modes, got: {diags:?}"
    );
}

#[test]
fn w3c_hsl_blend_works_on_fill_path_and_stroke_path() {
    let mut store = CanvasStore::new();
    let backdrop = Rgba::new(200, 100, 50, 255);
    let source = Rgba::new(30, 80, 230, 255);
    let ops = vec![
        CanvasOp::FillRect {
            x: 0.0,
            y: 0.0,
            width: 16.0,
            height: 16.0,
            color: backdrop,
            alpha: 1.0,
            composite: CompositeMode::Copy,
            matrix: Matrix2D::default(),
        },
        CanvasOp::FillPath {
            ops: vec![
                PathOp::MoveTo(2.0, 2.0),
                PathOp::LineTo(14.0, 2.0),
                PathOp::LineTo(14.0, 14.0),
                PathOp::LineTo(2.0, 14.0),
                PathOp::Close,
            ],
            even_odd: false,
            color: source,
            alpha: 1.0,
            composite: CompositeMode::Color,
            matrix: Matrix2D::default(),
        },
        CanvasOp::StrokePath {
            ops: vec![PathOp::MoveTo(0.0, 8.0), PathOp::LineTo(16.0, 8.0)],
            color: Rgba::new(255, 255, 255, 255),
            alpha: 1.0,
            line_width: 2.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: DEFAULT_MITER_LIMIT,
            dashes: Vec::new(),
            dash_offset: 0.0,
            composite: CompositeMode::Luminosity,
            matrix: Matrix2D::default(),
        },
    ];
    store.run(7, 16, 16, &ops).expect("run path hsl ops");

    // Interior pixel away from stroke should match Color blend.
    let color_px = store.read(7, 5, 4, 1, 1).expect("read color path pixel");
    let expected_color = ref_w3c_composite(backdrop, source, CompositeMode::Color);
    assert_rgba_near(&color_px, expected_color, "FillPath Color blend");

    // Center of horizontal white stroke should have Luminosity = 1.0 -> pure white (255,255,255,255).
    let stroke_px = store.read(7, 8, 8, 1, 1).expect("read stroke pixel");
    assert_rgba_near(&stroke_px, [255, 255, 255, 255], "StrokePath Luminosity");
    assert!(store.take_diagnostics().is_empty());
}

// =============================================================================
// 2. Software Filter Pipeline & Nested Clip-Path Stack Tests
// =============================================================================

fn make_solid_pixmap(w: u32, h: u32, rgba: Rgba) -> Pixmap {
    let mut pm = Pixmap::new(w, h).expect("alloc pixmap");
    pm.fill(SkColor::from_rgba8(rgba.r, rgba.g, rgba.b, rgba.a));
    pm
}

fn read_unpremul(pm: &Pixmap, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y as usize) * (pm.width() as usize) + (x as usize)) * 4;
    let d = pm.data();
    let a = d[idx + 3];
    if a == 0 {
        return [0, 0, 0, 0];
    }
    if a == 255 {
        return [d[idx], d[idx + 1], d[idx + 2], 255];
    }
    let af = f32::from(a) / 255.0;
    [
        (f32::from(d[idx]) / af).round().clamp(0.0, 255.0) as u8,
        (f32::from(d[idx + 1]) / af).round().clamp(0.0, 255.0) as u8,
        (f32::from(d[idx + 2]) / af).round().clamp(0.0, 255.0) as u8,
        a,
    ]
}

#[test]
fn filter_pipeline_grayscale_brightness_contrast_saturate_and_reach() {
    // Grayscale(1.0) on pure red (255, 0, 0) -> Rec.709 luminance 0.2126 * 255 ≈ 54.
    let mut pm = make_solid_pixmap(4, 4, Rgba::new(255, 0, 0, 255));
    assert!(apply_filters(
        &mut pm,
        &[FilterOp::Grayscale { amount: 1.0 }],
        1.0
    ));
    let px = read_unpremul(&pm, 2, 2);
    assert_rgba_near(&px, [54, 54, 54, 255], "grayscale(1.0) red");

    // Brightness(0.5) halves channel intensity.
    let mut pm_b = make_solid_pixmap(4, 4, Rgba::new(200, 100, 50, 255));
    assert!(apply_filters(
        &mut pm_b,
        &[FilterOp::Brightness { amount: 0.5 }],
        1.0
    ));
    assert_rgba_near(
        &read_unpremul(&pm_b, 1, 1),
        [100, 50, 25, 255],
        "brightness(0.5)",
    );

    // Contrast(0.0) collapses all colors to 50% gray (128).
    let mut pm_c = make_solid_pixmap(4, 4, Rgba::new(240, 20, 90, 255));
    assert!(apply_filters(
        &mut pm_c,
        &[FilterOp::Contrast { amount: 0.0 }],
        1.0
    ));
    assert_rgba_near(
        &read_unpremul(&pm_c, 1, 1),
        [128, 128, 128, 255],
        "contrast(0.0)",
    );

    // Saturate(0.0) matches grayscale(1.0).
    let mut pm_s = make_solid_pixmap(4, 4, Rgba::new(0, 255, 0, 255));
    assert!(apply_filters(
        &mut pm_s,
        &[FilterOp::Saturate { amount: 0.0 }],
        1.0
    ));
    // Rec.709 green luminance = 0.7152 * 255 ≈ 182.
    assert_rgba_near(
        &read_unpremul(&pm_s, 1, 1),
        [182, 182, 182, 255],
        "saturate(0.0) green",
    );

    // Reach & cost measurement validation.
    let chain = [
        FilterOp::Blur { radius: 4.0 },
        FilterOp::DropShadow {
            dx: 6.0,
            dy: -2.0,
            blur: 4.0,
            color: Rgba::new(0, 0, 0, 128),
        },
    ];
    assert!(usable_filters(&chain));
    let reach = filter_reach(&chain, 2.0);
    assert!(reach.right > reach.left);
    assert!(reach.top > reach.bottom);
    let cost = measure_filters(&chain, 64, 64, 2.0).expect("measure filter chain");
    assert!(cost.work > 0 && cost.scratch > 0);

    // Invalid chains rejected cleanly.
    assert!(!usable_filters(&[FilterOp::Blur { radius: f32::NAN }]));
    assert!(!usable_filters(&[FilterOp::Brightness { amount: -1.0 }]));
    assert!(measure_filters(&[FilterOp::Blur { radius: 2.0 }], 0, 10, 1.0).is_none());

    // Invert(1.0), Sepia(1.0), Opacity(0.5), HueRotate(180).
    let mut pm_inv = make_solid_pixmap(2, 2, Rgba::new(40, 100, 200, 255));
    assert!(apply_filters(
        &mut pm_inv,
        &[FilterOp::Invert { amount: 1.0 }],
        1.0
    ));
    assert_rgba_near(
        &read_unpremul(&pm_inv, 0, 0),
        [215, 155, 55, 255],
        "invert(1.0)",
    );

    let mut pm_op = make_solid_pixmap(2, 2, Rgba::new(200, 100, 50, 255));
    assert!(apply_filters(
        &mut pm_op,
        &[FilterOp::Opacity { amount: 0.5 }],
        1.0
    ));
    assert_rgba_near(
        &read_unpremul(&pm_op, 0, 0),
        [200, 100, 50, 128],
        "opacity(0.5)",
    );

    let mut pm_sepia = make_solid_pixmap(2, 2, Rgba::new(255, 255, 255, 255));
    assert!(apply_filters(
        &mut pm_sepia,
        &[FilterOp::Sepia { amount: 1.0 }],
        1.0
    ));
    let sep = read_unpremul(&pm_sepia, 0, 0);
    assert!(sep[0] >= sep[1] && sep[1] >= sep[2] && sep[3] == 255);

    let mut pm_hue = make_solid_pixmap(2, 2, Rgba::new(255, 0, 0, 255));
    assert!(apply_filters(
        &mut pm_hue,
        &[FilterOp::HueRotate { degrees: 360.0 }],
        1.0
    ));
    assert_rgba_near(
        &read_unpremul(&pm_hue, 0, 0),
        [255, 0, 0, 255],
        "hue-rotate(360deg)",
    );
}

#[test]
fn filter_pipeline_gaussian_blur_and_drop_shadow() {
    // Draw a bright 8x8 white square in the center of a 32x32 transparent canvas.
    let mut store = CanvasStore::new();
    store
        .run(
            1,
            32,
            32,
            &[CanvasOp::FillRect {
                x: 12.0,
                y: 12.0,
                width: 8.0,
                height: 8.0,
                color: Rgba::new(255, 255, 255, 255),
                alpha: 1.0,
                composite: CompositeMode::Copy,
                matrix: Matrix2D::default(),
            }],
        )
        .expect("draw square");

    // Apply Gaussian blur(2.0).
    store
        .apply_filters(1, &[FilterOp::Blur { radius: 2.0 }], 1.0)
        .expect("apply blur");
    let center = store.read(1, 16, 16, 1, 1).expect("read center");
    let halo = store.read(1, 10, 16, 1, 1).expect("read blur halo");
    let far = store.read(1, 0, 0, 1, 1).expect("read far corner");
    assert!(center[3] > 200, "center should remain opaque: {center:?}");
    assert!(
        halo[3] > 10 && halo[3] < center[3],
        "halo 2px outside square should have soft blurred alpha: {halo:?}"
    );
    assert_eq!(far[3], 0, "far corner should stay transparent");

    // DropShadow test: draw opaque red square and cast blue shadow offset by (+4, +4).
    let mut pm = Pixmap::new(32, 32).expect("alloc shadow canvas");
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(SkColor::from_rgba8(255, 0, 0, 255));
    pm.fill_rect(
        tiny_skia::Rect::from_xywh(8.0, 8.0, 8.0, 8.0).expect("rect"),
        &paint,
        tiny_skia::Transform::identity(),
        None,
    );
    assert!(apply_filters(
        &mut pm,
        &[FilterOp::DropShadow {
            dx: 4.0,
            dy: 4.0,
            blur: 2.0,
            color: Rgba::new(0, 0, 255, 255),
        }],
        1.0,
    ));
    // Original square center stays red (composited SourceOver shadow).
    assert_rgba_near(
        &read_unpremul(&pm, 11, 11),
        [255, 0, 0, 255],
        "source over shadow",
    );
    // Shadow-only region at (17, 17) (inside shifted 12..20, outside original 8..16) should be blue.
    let sh = read_unpremul(&pm, 17, 17);
    assert!(
        sh[2] > 200 && sh[0] < 10 && sh[3] > 100,
        "expected blue shadow at (17,17), got {sh:?}"
    );

    // Large lone blur triggers downsampling path (divisor > 1) cleanly.
    let mut large = make_solid_pixmap(128, 128, Rgba::new(200, 100, 50, 255));
    assert!(apply_filters(
        &mut large,
        &[FilterOp::Blur { radius: 30.0 }],
        1.0
    ));
    let mid = read_unpremul(&large, 64, 64);
    assert!(
        mid[3] > 100,
        "downsampled blur center should remain visible: {mid:?}"
    );
}

#[test]
fn nested_clip_stack_masks_and_zero_area_safety() {
    let mut clips = ClipStack::new(20, 20);
    assert!(!clips.is_empty());

    // Push outer clip [2..18, 2..18] and inner clip [6..14, 6..14].
    clips.push_rect(ClipRect::new(2.0, 2.0, 16.0, 16.0), Matrix2D::default());
    clips.save();
    clips.push_rect(ClipRect::new(6.0, 6.0, 8.0, 8.0), Matrix2D::default());
    assert_eq!(clips.depth(), 2);
    assert_eq!(clips.bounds(), Some(ClipRect::new(6.0, 6.0, 8.0, 8.0)));

    let mut pm = make_solid_pixmap(20, 20, Rgba::new(100, 200, 50, 255));
    clips.apply_mask_to_pixmap(&mut pm);
    assert_eq!(read_unpremul(&pm, 10, 10), [100, 200, 50, 255]); // inside both
    assert_eq!(read_unpremul(&pm, 4, 4), [0, 0, 0, 0]); // outside inner clip
    assert_eq!(read_unpremul(&pm, 1, 1), [0, 0, 0, 0]); // outside outer clip

    // Restore pops inner clip back to [2..18, 2..18].
    assert!(clips.restore());
    assert_eq!(clips.depth(), 1);
    assert_eq!(clips.bounds(), Some(ClipRect::new(2.0, 2.0, 16.0, 16.0)));

    // Push nested vector triangle path clip and verify history bytes.
    clips.push_path(
        vec![
            PathOp::MoveTo(10.0, 2.0),
            PathOp::LineTo(18.0, 18.0),
            PathOp::LineTo(2.0, 18.0),
            PathOp::Close,
        ],
        false,
        Matrix2D::default(),
    );
    assert_eq!(clips.history_bytes(), 2 * CLIP_HISTORY_ENTRY_BYTES);
    assert!(clips.data_bytes() > clip_base_bytes(20));
    let mut tri_pm = make_solid_pixmap(20, 20, Rgba::new(255, 0, 0, 255));
    clips.apply_mask_to_pixmap(&mut tri_pm);
    assert_eq!(read_unpremul(&tri_pm, 10, 14)[3], 255); // inside triangle
    assert_eq!(read_unpremul(&tri_pm, 3, 3)[3], 0); // inside rect but outside triangle

    // Zero-area clips, disjoint clips, empty paths, NaN coordinates, and zero-sized surfaces
    // must NEVER panic.
    let mut zero_rect = ClipStack::new(16, 16);
    zero_rect.push_rect(ClipRect::new(4.0, 4.0, 0.0, 10.0), Matrix2D::default());
    assert!(zero_rect.is_empty());
    assert!(zero_rect.rasterize_coverage().is_none());
    let mut zpm = make_solid_pixmap(16, 16, Rgba::new(255, 255, 255, 255));
    zero_rect.apply_mask_to_pixmap(&mut zpm);
    assert_eq!(read_unpremul(&zpm, 8, 8)[3], 0);

    let mut disjoint = ClipStack::new(16, 16);
    disjoint.push_rect(ClipRect::new(0.0, 0.0, 4.0, 4.0), Matrix2D::default());
    disjoint.push_rect(ClipRect::new(10.0, 10.0, 4.0, 4.0), Matrix2D::default());
    assert!(disjoint.is_empty());
    assert!(disjoint.rasterize_coverage().is_none());

    let mut empty_path = ClipStack::new(16, 16);
    empty_path.push_path(
        vec![PathOp::MoveTo(f32::NAN, 0.0)],
        false,
        Matrix2D::default(),
    );
    assert!(empty_path.is_empty());
    assert!(empty_path.rasterize_coverage().is_none());

    let zero_surface = ClipStack::new(0, 0);
    assert!(zero_surface.is_empty());
    assert!(zero_surface.rasterize_coverage().is_none());

    // History folding combines consecutive axis-aligned rects.
    let mut fold = ClipStack::new(32, 32);
    fold.push_rect(ClipRect::new(0.0, 0.0, 20.0, 20.0), Matrix2D::default());
    fold.push_rect(ClipRect::new(5.0, 5.0, 20.0, 20.0), Matrix2D::default());
    fold.push_rect(ClipRect::new(8.0, 8.0, 4.0, 4.0), Matrix2D::default());
    assert_eq!(fold.shorten_history(), 2);
    assert_eq!(fold.depth(), 1);
    assert_eq!(fold.bounds(), Some(ClipRect::new(8.0, 8.0, 4.0, 4.0)));
}

// =============================================================================
// 3. Disk-Backed Widget Storage Tests (Tempdir Round-Trip, Quotas, LRU Eviction)
// =============================================================================

fn make_area(key: &str, val: &str) -> StorageArea {
    let mut map = BTreeMap::new();
    map.insert(key.to_owned(), val.to_owned());
    map
}

#[test]
fn disk_storage_round_trips_across_reopen_and_persists_heights_and_meta() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = DiskStorageConfig {
        owner: 0xDEAD_BEEF,
        per_widget_quota_bytes: STORAGE_QUOTA_BYTES,
        total_quota_bytes: 64 * 1024,
    };
    let k1 = WidgetMsgKey {
        peer_id: 1001,
        msg_id: 10,
    };
    let k2 = WidgetMsgKey {
        peer_id: 1001,
        msg_id: 20,
    };
    let meta1 = StorageMeta {
        date: 1_700_000_000,
        expires: 1_700_005_000,
        topic_root_id: 5,
        sublist_peer_id: 0,
        from_peer_id: 777,
    };
    let area1 = make_area("score", "42");

    {
        let mut disk = WidgetStorageDisk::open(tmp.path(), cfg).expect("open disk");
        let outcome = disk.write(k1, meta1, &area1).expect("write k1");
        assert!(outcome.changed);
        assert!(outcome.evicted.is_empty());
        assert!(disk.set_height(
            k2,
            StorageMeta {
                date: 1_700_000_100,
                expires: 1_700_003_000,
                ..StorageMeta::default()
            },
            HeightRecord {
                height: 312.5,
                bottom: true,
            },
        ));
        disk.flush().expect("flush index");
        assert_eq!(disk.next_expiry(), Some(1_700_003_000));
    }

    // Drop an orphan file to verify reopen cleans it up without harming live records.
    let orphan = tmp.path().join("000000000000beef.rec");
    fs::write(&orphan, b"orphan-garbage").expect("write orphan");

    // Reopen in the same tempdir.
    let mut reopened = WidgetStorageDisk::open(tmp.path(), cfg).expect("reopen disk");
    assert!(!orphan.exists(), "orphan file must be cleaned on reopen");
    assert_eq!(reopened.len(), 2);
    assert_eq!(reopened.lookup(k1), Some(meta1));
    assert_eq!(reopened.read(k1).expect("read k1"), Some(area1));
    assert_eq!(
        reopened.height(k2),
        Some(HeightRecord {
            height: 312.5,
            bottom: true,
        })
    );

    // Prune expired entry k2 (expires 1_700_003_000) while keeping k1 (expires 1_700_005_000).
    assert_eq!(reopened.prune_expired(1_700_004_000).expect("prune"), 1);
    assert_eq!(reopened.len(), 1);
    assert!(reopened.height(k2).is_none());
    assert!(reopened.lookup(k1).is_some());
}

#[test]
fn disk_storage_enforces_per_widget_and_global_quotas_with_lru_eviction() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = DiskStorageConfig {
        owner: 42,
        per_widget_quota_bytes: 128,
        total_quota_bytes: 220,
    };

    let k1 = WidgetMsgKey {
        peer_id: 10,
        msg_id: 1,
    };
    let k2 = WidgetMsgKey {
        peer_id: 10,
        msg_id: 2,
    };
    let k3 = WidgetMsgKey {
        peer_id: 10,
        msg_id: 3,
    };

    let mut disk = WidgetStorageDisk::open(tmp.path(), cfg).expect("open quota disk");

    // Per-widget quota violation (> 128 bytes) must be rejected.
    let oversized = make_area("k", &"x".repeat(130));
    assert!(disk.write(k1, StorageMeta::default(), &oversized).is_err());

    // Write k1 (100 bytes) and k2 (100 bytes) -> total 200 <= 220.
    let payload_100 = |tag: &str| make_area(tag, &"a".repeat(100 - tag.len()));
    disk.write(k1, StorageMeta::default(), &payload_100("w1"))
        .expect("write k1");
    disk.write(k2, StorageMeta::default(), &payload_100("w2"))
        .expect("write k2");
    assert_eq!(disk.total_bytes(), 200);

    // Touch k1 via read so k2 becomes least-recently-used.
    assert!(disk.read(k1).expect("read k1").is_some());

    // Write k3 (100 bytes) -> total would be 300 > 220 -> must evict k2 (LRU), keeping k1 & k3!
    let outcome = disk
        .write(k3, StorageMeta::default(), &payload_100("w3"))
        .expect("write k3");
    assert_eq!(outcome.evicted, vec![k2]);
    assert_eq!(disk.total_bytes(), 200);
    assert!(disk.lookup(k2).is_none());
    assert!(disk.lookup(k1).is_some());
    assert!(disk.lookup(k3).is_some());

    // Verify LRU eviction persisted cleanly across reopen.
    drop(disk);
    let mut reopened = WidgetStorageDisk::open(tmp.path(), cfg).expect("reopen after eviction");
    assert_eq!(reopened.len(), 2);
    assert_eq!(reopened.total_bytes(), 200);
    assert_eq!(reopened.read(k2).expect("read evicted k2"), None);
    assert_eq!(reopened.read(k1).expect("read k1"), Some(payload_100("w1")));
    assert_eq!(reopened.read(k3).expect("read k3"), Some(payload_100("w3")));

    // Move k3 -> k4, clear chat, and recover from corrupt index cleanly.
    let k4 = WidgetMsgKey {
        peer_id: 20,
        msg_id: 4,
    };
    assert!(reopened.move_entry(k3, k4).expect("move k3 -> k4"));
    assert_eq!(reopened.read(k4).expect("read k4"), Some(payload_100("w3")));
    assert_eq!(reopened.clear_chat(10).expect("clear peer 10"), 1);
    assert_eq!(reopened.len(), 1);

    // Corrupt index.bin -> reopen resets cleanly without panicking.
    fs::write(tmp.path().join("index.bin"), b"corrupt-index-bytes").expect("corrupt index");
    let recovered = WidgetStorageDisk::open(tmp.path(), cfg).expect("recover corrupt index");
    assert!(recovered.is_empty());
    assert_eq!(recovered.total_bytes(), 0);
}

// =============================================================================
// 4. WidgetMeter Watchdog Throttle & Suspend Tests
// =============================================================================

#[test]
fn widget_meter_trips_throttle_and_suspend_under_synthetic_slow_frames() {
    let cfg = MeterConfig {
        max_consecutive_slow_frames: 3,
        suspend_hold_ms: 4_000,
        single_frame_suspend_secs: 0.8,
        memory_soft_limit_bytes: 1_000,
        memory_hard_limit_bytes: 2_000,
        ..MeterConfig::default()
    };

    // (a) Load grace -> single-pass throttle (3x) -> sustained overload throttle (19x) -> suspend.
    let mut meter = WidgetMeter::with_config(cfg);
    assert_eq!(meter.record_frame(10.0, 0.08), MeterVerdict::Allow); // within 1s load grace
    assert_eq!(meter.record_frame(10.5, 0.04), MeterVerdict::Allow); // still within load grace

    // Past 1s grace: single 40ms frame (>16ms, <=50ms) -> 3x throttle (120ms), FrameBudgetExceeded.
    match meter.record_frame(11.2, 0.04) {
        MeterVerdict::Throttle { hold_ms, reason } => {
            assert_eq!(hold_ms, 120);
            assert_eq!(reason, WatchdogReason::FrameBudgetExceeded);
        }
        other => panic!("expected FrameBudgetExceeded throttle, got {other:?}"),
    }

    // Two heavy frames (>50ms) pushing window CPU > 0.20s -> 19x sustained throttle.
    let v1 = meter.record_frame(11.5, 0.12);
    assert!(v1.is_throttled());
    let v2 = meter.record_frame(11.8, 0.12);
    match v2 {
        MeterVerdict::Throttle { hold_ms, reason } => {
            assert_eq!(reason, WatchdogReason::SustainedCpuOverload);
            assert!(
                hold_ms >= 2_000,
                "expected 19x sustained hold, got {hold_ms} ms"
            );
        }
        other => panic!("expected SustainedCpuOverload throttle, got {other:?}"),
    }

    // Third consecutive slow frame (>50ms) trips ConsecutiveSlowFrames suspension.
    let v3 = meter.record_frame(12.0, 0.12);
    match &v3 {
        MeterVerdict::Suspend { reason, detail } => {
            assert_eq!(*reason, WatchdogReason::ConsecutiveSlowFrames);
            assert!(!detail.is_empty());
        }
        other => panic!("expected ConsecutiveSlowFrames suspend, got {other:?}"),
    }
    // Suspend converts cleanly to a hosted wire Stopped frame.
    let stop_ctrl = v3.to_stop_control().expect("stop control");
    let wire_bytes = stop_ctrl.encode().expect("encode stop");
    assert_eq!(
        ControlMessage::decode(&wire_bytes).expect("decode"),
        stop_ctrl
    );

    // Suspended widget rejects work admission even with user input exemption.
    meter.note_input(12.0);
    assert!(meter.admit_work(12.05, false).is_err());

    // (b) Hard single-frame raster timeout immediately suspends even during load grace.
    let mut timeout_meter = WidgetMeter::with_config(cfg);
    match timeout_meter.record_frame(1.0, 0.85) {
        MeterVerdict::Suspend { reason, .. } => assert_eq!(reason, WatchdogReason::FrameTimeout),
        other => panic!("expected FrameTimeout suspend, got {other:?}"),
    }

    // (c) Memory soft pressure throttles; hard limit suspends.
    let mut mem_meter = WidgetMeter::with_config(cfg);
    assert_eq!(mem_meter.update_memory(500, 1.0), MeterVerdict::Allow);
    match mem_meter.update_memory(1_500, 1.0) {
        MeterVerdict::Throttle { reason, .. } => {
            assert_eq!(reason, WatchdogReason::MemoryPressure);
        }
        other => panic!("expected MemoryPressure throttle, got {other:?}"),
    }
    match mem_meter.update_memory(2_500, 1.0) {
        MeterVerdict::Suspend { reason, .. } => {
            assert_eq!(reason, WatchdogReason::MemoryBudgetExceeded);
        }
        other => panic!("expected MemoryBudgetExceeded suspend, got {other:?}"),
    }

    // (d) Nested operation defers charging until outermost operation closes.
    let mut op_meter = WidgetMeter::new();
    op_meter.charge(0.0, 0.001); // seed first_secs = 0.0
    op_meter.begin_operation();
    op_meter.spend_work(2.0, 0.02);
    op_meter.spend_work(2.0, 0.02);
    // Not charged until end_operation -> still admitted with 0 hold.
    assert_eq!(op_meter.held_for_ms(2.0), 0);
    assert!(op_meter.admit_work(2.0, true).expect("admitted in op"));
    op_meter.end_operation(2.0);
    // Combined 0.04s charged -> 3x = 120ms hold.
    assert_eq!(op_meter.held_for_ms(2.0), 120);
    assert!(!op_meter.admit_work(2.0, false).expect("not refused"));
    assert!(op_meter.admit_work(2.0, true).is_err());
}

// =============================================================================
// 5. Hosted Wire Helper Completeness Tests
// =============================================================================

#[test]
fn hosted_wire_helpers_and_caps_validation() {
    let loaded = ControlMessage::Loaded.encode().expect("encode loaded");
    assert!(is_exactly(&loaded, ControlKind::Loaded));
    assert!(!is_exactly(&loaded, ControlKind::Content));

    let hold = ControlMessage::FingerHold(77)
        .encode()
        .expect("encode hold");
    assert_eq!(
        decode_press(&hold).expect("decode press"),
        (ControlKind::FingerHold, 77)
    );
    let clicked = ControlMessage::Clicked(77)
        .encode()
        .expect("encode clicked");
    assert!(decode_press(&clicked).is_err());

    let caps = EmbeddingCaps::default();
    assert!(caps.validate_message(MAX_MESSAGE_BYTES).is_ok());
    assert!(caps.validate_message(MAX_MESSAGE_BYTES + 1).is_err());
    assert!(caps.validate_commands(MAX_LIST_COMMANDS).is_ok());
    assert!(caps.validate_commands(MAX_LIST_COMMANDS + 1).is_err());
    assert!(caps.validate_canvas_ops(MAX_CANVAS_RUN_OPS).is_ok());
    assert!(caps.validate_canvas_ops(MAX_CANVAS_RUN_OPS + 1).is_err());
}
