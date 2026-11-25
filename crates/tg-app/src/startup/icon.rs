//! Runtime window icon loader for `eframe::ViewportBuilder::with_icon`.
//!
//! Port origin: `Telegram/SourceFiles/window/main_window.cpp` (`Window::CreateIcon`)
//! and `Telegram/SourceFiles/platform/win/specific_win.cpp` (`SetApplicationIcon`).
//!
//! Renders the shared workspace `assets/icon.svg` at runtime via `resvg` into a straight-alpha
//! RGBA [`egui::IconData`] so the window titlebar and Alt-Tab icon match the embedded `.exe` icon.

use std::sync::Arc;

use egui::IconData;
use resvg::{tiny_skia, usvg};

/// Embedded SVG source shared with `build.rs` and `crates/tg-xtask/src/icon.rs`.
pub const APP_ICON_SVG: &[u8] = include_bytes!("../../../../assets/icon.svg");

/// Default rasterized dimension (px) for the runtime window icon.
pub const DEFAULT_WINDOW_ICON_SIZE: u32 = 64;

/// Rasterizes `APP_ICON_SVG` into an [`Arc<IconData>`] suitable for
/// [`egui::ViewportBuilder::with_icon`].
///
/// Falls back to a solid Rustgram-teal square if SVG rasterization unexpectedly fails.
#[must_use]
pub fn load_window_icon() -> Arc<IconData> {
    if let Some(icon) = prerendered_window_icon() {
        return Arc::new(icon);
    }
    Arc::new(
        render_svg_to_icon_data(APP_ICON_SVG, DEFAULT_WINDOW_ICON_SIZE)
            .unwrap_or_else(|_| fallback_icon_data(DEFAULT_WINDOW_ICON_SIZE)),
    )
}

/// The window icon rendered by `build.rs` from the same SVG with the same `resvg` pipeline, which
/// spares loading the system font database at startup.
fn prerendered_window_icon() -> Option<IconData> {
    #[cfg(rustgram_window_icon_rgba)]
    {
        const RGBA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/window_icon_64.rgba"));
        let size = DEFAULT_WINDOW_ICON_SIZE;
        if RGBA.len() == (size * size * 4) as usize {
            return Some(IconData {
                rgba: RGBA.to_vec(),
                width: size,
                height: size,
            });
        }
    }
    None
}

/// Renders raw SVG XML bytes into a square `size x size` straight-alpha RGBA [`IconData`].
pub fn render_svg_to_icon_data(svg_bytes: &[u8], size: u32) -> Result<IconData, String> {
    if size == 0 || size > 1024 {
        return Err(format!("unsupported icon size {size}"));
    }

    let mut options = usvg::Options::default();
    {
        let db = options.fontdb_mut();
        db.load_system_fonts();
        for family in [
            "Segoe UI",
            "Arial",
            "Liberation Sans",
            "DejaVu Sans",
            "Noto Sans",
        ] {
            if db.faces().any(|f| {
                f.families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(family))
            }) {
                db.set_sans_serif_family(family);
                break;
            }
        }
    }

    let tree = usvg::Tree::from_data(svg_bytes, &options).map_err(|e| e.to_string())?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size)
        .ok_or_else(|| format!("failed to allocate {size}x{size} pixmap"))?;

    let tree_size = tree.size();
    let sx = size as f32 / tree_size.width();
    let sy = size as f32 / tree_size.height();
    let transform = tiny_skia::Transform::from_scale(sx, sy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // Convert premultiplied RGBA from tiny-skia to straight RGBA for egui::IconData.
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        rgba.push(c.red());
        rgba.push(c.green());
        rgba.push(c.blue());
        rgba.push(c.alpha());
    }

    Ok(IconData {
        rgba,
        width: size,
        height: size,
    })
}

/// Fallback solid `#0891b2` square icon if SVG rendering ever fails.
fn fallback_icon_data(size: u32) -> IconData {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for _ in 0..(size * size) {
        rgba.extend_from_slice(&[0x08, 0x91, 0xB2, 0xFF]);
    }
    IconData {
        rgba,
        width: size,
        height: size,
    }
}
