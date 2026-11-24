//! Pure-Rust SVG to multi-size Windows `.ico` generator using `resvg` and `ico`.

use std::fs::{self, File};
use std::path::Path;

use resvg::{tiny_skia, usvg};

use crate::XtaskError;

/// Standard Windows icon sizes embedded in `rustgram.ico`.
pub const ICON_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];

/// Renders an SVG file at `svg_path` into a multi-size Windows `.ico` at `out_path`.
pub fn render_svg_to_ico(svg_path: &Path, out_path: &Path) -> Result<(), XtaskError> {
    let svg_data = fs::read(svg_path)?;
    let ico_bytes = render_svg_bytes_to_ico(&svg_data, ICON_SIZES)?;
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(out_path)?;
    std::io::Write::write_all(&mut file, &ico_bytes)?;
    Ok(())
}

/// Renders SVG XML bytes into a multi-size `.ico` byte buffer for the requested pixel `sizes`.
pub fn render_svg_bytes_to_ico(svg_data: &[u8], sizes: &[u32]) -> Result<Vec<u8>, XtaskError> {
    if sizes.is_empty() {
        return Err(XtaskError::InvalidArgument(
            "at least one icon size is required".into(),
        ));
    }

    let mut options = usvg::Options::default();
    {
        let db = options.fontdb_mut();
        db.load_system_fonts();
        // Ensure a reliable sans-serif fallback is mapped on Windows and Linux so
        // `<text font-family="..., sans-serif">` renders cleanly.
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

    let tree = usvg::Tree::from_data(svg_data, &options)
        .map_err(|e| XtaskError::SvgRender(e.to_string()))?;

    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);

    for &size in sizes {
        let rgba = render_tree_rgba(&tree, size)?;
        let image = ico::IconImage::from_rgba_data(size, size, rgba);
        let entry = if size >= 256 {
            ico::IconDirEntry::encode_as_png(&image)
        } else {
            ico::IconDirEntry::encode(&image)
        }
        .map_err(|e| XtaskError::IcoEncode(e.to_string()))?;
        icon_dir.add_entry(entry);
    }

    let mut out = Vec::new();
    icon_dir
        .write(&mut out)
        .map_err(|e| XtaskError::IcoEncode(e.to_string()))?;
    Ok(out)
}

/// Rasterizes `tree` into a square `size x size` straight-alpha RGBA pixel buffer.
pub fn render_tree_rgba(tree: &usvg::Tree, size: u32) -> Result<Vec<u8>, XtaskError> {
    if size == 0 || size > 1024 {
        return Err(XtaskError::InvalidArgument(format!(
            "unsupported icon size {size}"
        )));
    }
    let mut pixmap = tiny_skia::Pixmap::new(size, size)
        .ok_or_else(|| XtaskError::SvgRender(format!("failed to allocate {size}x{size} pixmap")))?;

    let tree_size = tree.size();
    let sx = size as f32 / tree_size.width();
    let sy = size as f32 / tree_size.height();
    let transform = tiny_skia::Transform::from_scale(sx, sy);

    resvg::render(tree, transform, &mut pixmap.as_mut());

    // tiny-skia stores premultiplied RGBA; convert to straight RGBA for `ico::IconImage`.
    let mut straight = Vec::with_capacity((size * size * 4) as usize);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        straight.push(c.red());
        straight.push(c.green());
        straight.push(c.blue());
        straight.push(c.alpha());
    }
    Ok(straight)
}
