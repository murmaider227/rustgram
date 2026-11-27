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

/// Tray icon sizes (small icon at 100/125/150/200 % scaling), rendered from the same SVG.
pub const TRAY_ICON_SIZES: [u32; 4] = [16, 20, 24, 32];

/// The app icon at [`TRAY_ICON_SIZES`], pre-rendered by `build.rs` or rendered now.
#[must_use]
pub fn tray_icon_images() -> Vec<tg_platform::tray::RgbaImage> {
    TRAY_ICON_SIZES
        .iter()
        .filter_map(|&size| {
            let rgba = prerendered_tray_icon(size).or_else(|| {
                render_svg_to_icon_data(APP_ICON_SVG, size)
                    .ok()
                    .map(|icon| icon.rgba)
            })?;
            Some(tg_platform::tray::RgbaImage {
                width: size,
                height: size,
                pixels: rgba,
            })
        })
        .collect()
}

/// Makes the tray (and toasts) use the Rustgram app icon instead of the generated
/// placeholder (TGB-3123). Called once before the window opens.
pub fn install_tray_app_icon() {
    tg_platform::tray::set_app_icon_images(tray_icon_images());
}

fn prerendered_tray_icon(size: u32) -> Option<Vec<u8>> {
    #[cfg(rustgram_tray_icon_rgba)]
    {
        let bytes: &[u8] = match size {
            16 => include_bytes!(concat!(env!("OUT_DIR"), "/tray_icon_16.rgba")),
            20 => include_bytes!(concat!(env!("OUT_DIR"), "/tray_icon_20.rgba")),
            24 => include_bytes!(concat!(env!("OUT_DIR"), "/tray_icon_24.rgba")),
            32 => include_bytes!(concat!(env!("OUT_DIR"), "/tray_icon_32.rgba")),
            _ => return None,
        };
        if bytes.len() == (size * size * 4) as usize {
            return Some(bytes.to_vec());
        }
    }
    #[cfg(not(rustgram_tray_icon_rgba))]
    let _ = size;
    None
}

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

#[cfg(test)]
mod tests {
    use tg_platform::tray::{
        RgbaImage, TrayCounterColors, app_icon, compose_icon_with_counter, default_app_icon,
    };

    use super::{APP_ICON_SVG, TRAY_ICON_SIZES, install_tray_app_icon, render_svg_to_icon_data};

    fn max_channel_diff(a: &RgbaImage, b: &[u8]) -> u8 {
        a.pixels
            .iter()
            .zip(b)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(u8::MAX)
    }

    /// TGB-3123: the tray shows the app icon (the exe's `assets/icon.svg`) at every tray size,
    /// not the generated chat bubble; the unread badge is drawn on top of it.
    #[test]
    fn tray_icon_is_the_app_icon_with_the_badge_on_top() {
        install_tray_app_icon();
        for size in TRAY_ICON_SIZES {
            let tray = app_icon(size, false, true);
            assert_eq!((tray.width, tray.height), (size, size));
            let svg = render_svg_to_icon_data(APP_ICON_SVG, size).expect("render svg");
            assert!(
                max_channel_diff(&tray, &svg.rgba) <= 2,
                "{size} px tray icon must be the app icon"
            );
            assert_ne!(
                tray,
                default_app_icon(size, false, true),
                "{size} px: no bubble"
            );
        }

        let icon = app_icon(32, false, true);
        let colors = TrayCounterColors::default();
        let none = compose_icon_with_counter(&icon, 0, false, true, false, Some(true), colors);
        assert_eq!(none, icon, "no unread: the plain app icon");
        let unread = compose_icon_with_counter(&icon, 7, false, true, false, Some(true), colors);
        let muted = compose_icon_with_counter(&icon, 7, true, true, false, Some(true), colors);
        assert_ne!(unread, icon, "the counter is drawn");
        assert_ne!(unread, muted, "a muted counter has its own colour");
        // `WithSmallCounter` draws in the bottom part: the top-left quarter stays the logo.
        for y in 0..12_usize {
            for x in 0..12_usize {
                let i = (y * 32 + x) * 4;
                assert_eq!(unread.pixels[i..i + 4], icon.pixels[i..i + 4], "({x},{y})");
            }
        }
    }

    #[test]
    fn monochrome_tray_icon_is_one_colour_with_the_glyph_cut_out() {
        install_tray_app_icon();
        for (dark, fg) in [(true, 255_u8), (false, 0_u8)] {
            let mono = app_icon(32, true, dark);
            assert!(
                mono.pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| p[3] == 0 || (p[0] == fg && p[1] == fg && p[2] == fg)),
                "single colour on a {} taskbar",
                if dark { "dark" } else { "light" }
            );
            let colour = app_icon(32, false, dark);
            // The whitest opaque pixel of the logo is its plane (cut out); a pixel at the
            // circle's lower left is its purple background (kept).
            let whitest = colour
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .filter(|(_, p)| p[3] == 255)
                .max_by_key(|(_, p)| p[0].min(p[1]).min(p[2]))
                .map(|(i, _)| i)
                .expect("opaque pixels");
            let alpha = |img: &RgbaImage, i: usize| img.pixels[i * 4 + 3];
            let background = 22 * 32 + 8;
            assert!(alpha(&colour, background) > 200 && alpha(&mono, background) > 150);
            assert!(alpha(&mono, whitest) < 40, "the white glyph is cut out");
        }
    }

    /// Before | after of the 32 px tray icon (TGB-3123), written to
    /// `$W8_SNAP_DIR/{before,after}/tray_icon_32[_unread].png`.
    #[test]
    #[ignore = "captures the tray icon under W8_SNAP_DIR"]
    fn capture_w8_tray_icon() {
        let Some(dir) = std::env::var_os("W8_SNAP_DIR").map(std::path::PathBuf::from) else {
            return;
        };
        install_tray_app_icon();
        let colors = TrayCounterColors::default();
        for (stage, icon) in [
            ("before", default_app_icon(32, false, true)),
            ("after", app_icon(32, false, true)),
        ] {
            let out = dir.join(stage);
            std::fs::create_dir_all(&out).expect("snap dir");
            let unread =
                compose_icon_with_counter(&icon, 3, false, true, false, Some(true), colors);
            for (name, img) in [("tray_icon_32", &icon), ("tray_icon_32_unread", &unread)] {
                let png = img.encode_png().expect("png");
                std::fs::write(out.join(format!("{name}.png")), png).expect("write png");
            }
        }
    }
}
