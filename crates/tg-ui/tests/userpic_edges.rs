//! Userpic circle edges are antialiased at the real pixels-per-point (TGB-2971).
//!
//! tdesktop masks a userpic with `Images::Circle` (an antialiased ellipse drawn at the device
//! pixel ratio), so a pixel the circle's edge crosses gets the share of it the circle covers.
//! Each test paints a white userpic on black, renders it, and compares every pixel near the edge
//! with that coverage: a stepped (0 / 255) edge is off by about half a level.
//!
//! Writes `userpics_<scale>.png` into `$AVATAR_SNAP_DIR` when that variable is set.
// These tests render on a GPU through egui_kittest's wgpu backend. The Linux CI runner has no adapter
// (wgpu setup panics), so they run on Windows only; the Windows CI job and the merge gate cover them.
#![cfg(windows)]

use std::sync::Arc;

use egui::{Color32, Painter, Pos2, Rect, Vec2};
use egui_kittest::Harness;
use tg_ui::userpic::paint_vertical_gradient_circle;
use tg_ui::userpic_images::{
    UserpicImage, UserpicImageSource, install_userpic_source, paint_userpic_image,
};

/// Chat-list userpic size (`st::defaultDialogRow.photoSize`).
const SIDE: f32 = 46.0;
const ORIGIN: f32 = 20.0;
const CANVAS: f32 = 96.0;
const PHOTO_PEER: i64 = 42;
/// 100% and 150% display scaling.
const SCALES: [f32; 2] = [1.0, 1.5];

/// A white square "photo" of the size the live session decodes (160 px), not masked.
struct WhitePhoto;

impl UserpicImageSource for WhitePhoto {
    fn userpic(&self, peer_id: i64) -> Option<Arc<UserpicImage>> {
        (peer_id == PHOTO_PEER).then(|| {
            Arc::new(UserpicImage {
                photo_id: 1,
                width: 160,
                height: 160,
                rgba: vec![255; 160 * 160 * 4],
            })
        })
    }
}

/// One GPU device at a time: two renderers starting at once crashed the test process.
static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn render(scale: f32, paint: impl Fn(&Painter, Rect) + 'static) -> image::RgbaImage {
    let _gpu = GPU
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut harness = Harness::builder()
        .with_size(Vec2::splat(CANVAS))
        .with_pixels_per_point(scale)
        .with_context_setup(|ctx| install_userpic_source(ctx, Arc::new(WhitePhoto)))
        .wgpu()
        .build_ui(move |ui| {
            let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::splat(CANVAS));
            ui.painter().rect_filled(canvas, 0.0, Color32::BLACK);
            let rect = Rect::from_min_size(Pos2::new(ORIGIN, ORIGIN), Vec2::splat(SIDE));
            paint(ui.painter(), rect);
        });
    harness.run();
    harness.render().expect("render the userpic")
}

struct Edge {
    /// Largest difference between a pixel and the share of it the circle covers (0..1).
    max_error: f32,
    /// Pixels near the edge that are neither black nor white.
    partial: usize,
    /// Pixels within one pixel of the edge.
    ring: usize,
}

fn measure_edge(img: &image::RgbaImage, scale: f32) -> Edge {
    let center = (ORIGIN + SIDE / 2.0) * scale;
    let radius = SIDE / 2.0 * scale;
    let mut edge = Edge {
        max_error: 0.0,
        partial: 0,
        ring: 0,
    };
    for (x, y, pixel) in img.enumerate_pixels() {
        let dx = x as f32 + 0.5 - center;
        let dy = y as f32 + 0.5 - center;
        let distance = (dx * dx + dy * dy).sqrt();
        if (distance - radius).abs() > 1.0 {
            continue;
        }
        let covered = (radius - distance + 0.5).clamp(0.0, 1.0);
        let actual = f32::from(pixel.0[0]) / 255.0;
        edge.ring += 1;
        edge.max_error = edge.max_error.max((actual - covered).abs());
        if actual > 0.08 && actual < 0.92 {
            edge.partial += 1;
        }
    }
    edge
}

fn assert_smooth(what: &str, scale: f32, img: &image::RgbaImage) {
    let center = ((ORIGIN + SIDE / 2.0) * scale) as u32;
    assert_eq!(
        img.get_pixel(center, center).0[0],
        255,
        "{what} at {scale}x: the middle of the userpic is painted"
    );
    let edge = measure_edge(img, scale);
    println!(
        "measured: {what} at {scale}x: largest edge error {:.2}, {} of {} edge pixels partly covered",
        edge.max_error, edge.partial, edge.ring
    );
    assert!(
        edge.max_error <= 0.1,
        "{what} at {scale}x: an edge pixel is {:.2} away from the share of it the circle covers \
         (a 0 / 255 step is about 0.5 away)",
        edge.max_error
    );
    assert!(
        edge.partial * 4 >= edge.ring,
        "{what} at {scale}x: only {} of {} edge pixels are partly covered",
        edge.partial,
        edge.ring
    );
}

fn save(name: &str, img: &image::RgbaImage) {
    if let Some(dir) = std::env::var_os("AVATAR_SNAP_DIR") {
        let path = std::path::PathBuf::from(dir).join(name);
        img.save(&path).expect("save the userpic snapshot");
    }
}

#[test]
fn empty_userpic_circle_edge_is_a_smooth_ramp() {
    let rendered = SCALES.map(|scale| {
        let img = render(scale, |painter, rect| {
            paint_vertical_gradient_circle(painter, rect, Color32::WHITE, Color32::WHITE);
        });
        save(&format!("empty_{scale}.png"), &img);
        (scale, img)
    });
    for (scale, img) in &rendered {
        assert_smooth("empty userpic", *scale, img);
    }
}

#[test]
fn photo_userpic_circle_edge_is_a_smooth_ramp() {
    let rendered = SCALES.map(|scale| {
        let img = render(scale, |painter, rect| {
            assert!(paint_userpic_image(painter, rect, PHOTO_PEER));
        });
        save(&format!("photo_{scale}.png"), &img);
        (scale, img)
    });
    for (scale, img) in &rendered {
        assert_smooth("photo userpic", *scale, img);
    }
}
