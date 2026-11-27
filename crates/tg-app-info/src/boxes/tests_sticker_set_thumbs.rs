//! TGB-3126: a sticker set box cell whose preview came without picture bytes shows the picture
//! the session loads (`tg_ui::sticker_thumbs`), whole and centred; before, every such cell kept
//! the fallback emblem for ever.
//!
//! With `$STICKER_BOX_SNAP_DIR` set the box is also saved there as a PNG (synthetic data,
//! software rasterizer).
use std::sync::Arc;

use egui::Vec2;
use tg_app_core::session_api::{StickerSetPreview, StickerSetPreviewItem};
use tg_ui::sticker_thumbs::{StickerThumb, StickerThumbSource, install_sticker_thumb_source};
use tg_ui::theme::DayPalette;

use super::StickerSetBoxState;

/// A colour nothing else in the box uses.
const MAGENTA: [u8; 3] = [0xE0, 0x10, 0xC0];

/// A wide (2:1) magenta picture, as a sticker that is not square.
struct WidePictures;

impl StickerThumbSource for WidePictures {
    fn sticker_thumb(&self, _document_id: u64) -> Option<Arc<StickerThumb>> {
        let (width, height) = (64u32, 32u32);
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            rgba.extend_from_slice(&[MAGENTA[0], MAGENTA[1], MAGENTA[2], 0xFF]);
        }
        Some(Arc::new(StickerThumb {
            width,
            height,
            rgba,
        }))
    }
}

fn pack() -> StickerSetBoxState {
    StickerSetBoxState::from_preview(StickerSetPreview {
        set_id: 88_201,
        access_hash: 99_202,
        title: "Handmade Pack".to_owned(),
        short_name: "HandmadePackThree".to_owned(),
        count: 3,
        is_emoji: false,
        installed: false,
        archived: false,
        official: false,
        items: (0..3)
            .map(|i| StickerSetPreviewItem {
                document_id: 70_201 + i,
                alt_emoji: "\u{1F600}".to_owned(),
                is_animated: false,
                is_video: false,
                is_premium: false,
                thumb_rgba: None,
            })
            .collect(),
    })
}

fn render(with_source: bool, png: &str) -> image::RgbaImage {
    // The shared software rasterizer: no GPU device, so it never races the crate's wgpu tests.
    let ctx = egui::Context::default();
    tg_ui::fonts::register_tdesktop_fonts(&ctx);
    if with_source {
        install_sticker_thumb_source(&ctx, Arc::new(WidePictures));
    }
    let mut state = pack();
    let options = tg_ui::soft_raster::RenderOptions {
        logical_size: Vec2::new(420.0, 320.0),
        pixels_per_point: 1.0,
        passes: 3,
        background: egui::Color32::WHITE,
        central_panel: true,
    };
    let frame = tg_ui::soft_raster::render(&ctx, &options, |ui| {
        state.show_body(ui, &DayPalette::new());
    });
    let image =
        image::RgbaImage::from_raw(frame.width as u32, frame.height as u32, frame.to_rgba8())
            .expect("frame size");
    if let Some(dir) = std::env::var_os("STICKER_BOX_SNAP_DIR") {
        image
            .save(std::path::PathBuf::from(dir).join(png))
            .expect("save snapshot");
    }
    image
}

fn magenta_rows_and_columns(image: &image::RgbaImage) -> (usize, usize) {
    let near = |px: &image::Rgba<u8>| {
        px.0[..3]
            .iter()
            .zip(MAGENTA)
            .all(|(&a, b)| (i32::from(a) - i32::from(b)).abs() <= 12)
    };
    let mut rows = 0;
    let mut cols = vec![false; image.width() as usize];
    for y in 0..image.height() {
        let mut any = false;
        for x in 0..image.width() {
            if near(image.get_pixel(x, y)) {
                any = true;
                cols[x as usize] = true;
            }
        }
        rows += usize::from(any);
    }
    (rows, cols.into_iter().filter(|&c| c).count())
}

#[test]
fn cells_show_the_loaded_pictures_whole() {
    let before = render(false, "sticker_box_no_source.png");
    assert_eq!(
        magenta_rows_and_columns(&before),
        (0, 0),
        "without a source the cells show their emblem"
    );
    let after = render(true, "sticker_box_loaded.png");
    let (rows, cols) = magenta_rows_and_columns(&after);
    assert!(rows > 10, "the loaded pictures are drawn ({rows} rows)");
    // Three wide pictures side by side keep their 2:1 shape: much wider than tall in total.
    assert!(
        cols >= rows * 4,
        "each picture keeps its shape ({cols} columns, {rows} rows)"
    );
}
