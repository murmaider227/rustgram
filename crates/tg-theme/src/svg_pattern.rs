//! SVG and gzip-compressed `.tgv` pattern wallpaper decompression, rasterization,
//! tiling, and multi-stop gradient compositing.
//!
//! Port of the SVG/`.tgv` wallpaper pipeline from tdesktop's
//! `SourceFiles/window/themes/window_theme.cpp`, `SourceFiles/ui/chat/chat_theme.cpp`,
//! and `lib_ui/ui/image/image_prepare.cpp`.

use std::io::{Cursor, Read, Write};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use image::{Rgba, RgbaImage};
use thiserror::Error;

use crate::color::Color;
use crate::gradient::{
    DEFAULT_BACKGROUND_COLOR, compose_pattern_on_gradient, dither_image, generate_gradient,
    is_pattern_inverted, soft_light_channel,
};

/// Maximum compressed `.tgv` payload size accepted by default (4 MiB).
pub const TGV_MAX_COMPRESSED_BYTES: usize = 4 * 1024 * 1024;
/// Maximum decompressed `.tgv` XML size accepted by default (8 MiB decompression-bomb cap).
pub const TGV_MAX_DECOMPRESSED_BYTES: usize = 8 * 1024 * 1024;
/// Maximum raw SVG XML size accepted by default (8 MiB).
pub const SVG_MAX_XML_BYTES: usize = 8 * 1024 * 1024;
/// Maximum rasterized pixel area (`kBackgroundSizeLimit` = 25 megapixels).
pub const SVG_MAX_PIXEL_AREA: u64 = 25 * 1024 * 1024;
/// Maximum natural dimension for pattern wallpapers (`kMaxSize` in `chat_theme.cpp`).
pub const SVG_PATTERN_MAX_DIMENSION: u32 = 2960;
/// Maximum allowed XML element nesting depth.
pub const SVG_MAX_NESTING_DEPTH: usize = 128;
/// Maximum allowed XML entity references in a single SVG document.
pub const SVG_MAX_ENTITY_REFS: usize = 4096;

/// Errors returned while decompressing, validating, or rasterizing SVG / `.tgv` wallpapers.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SvgPatternError {
    /// Input payload was empty.
    #[error("empty SVG/TGV input")]
    EmptyInput,
    /// Compressed `.tgv` input exceeded the allowed size limit.
    #[error("compressed TGV input too large ({size} bytes, limit {limit})")]
    CompressedSizeExceeded {
        /// Actual compressed byte length.
        size: usize,
        /// Configured limit.
        limit: usize,
    },
    /// Input was not a valid gzip stream.
    #[error("invalid gzip TGV stream: {0}")]
    InvalidGzip(String),
    /// Decompressed `.tgv` stream exceeded the decompression-bomb byte cap.
    #[error("TGV decompression bomb detected (exceeded {limit} bytes)")]
    DecompressionBomb {
        /// Configured decompressed byte cap.
        limit: usize,
    },
    /// Raw SVG XML exceeded the maximum allowed byte length.
    #[error("SVG XML too large ({size} bytes, limit {limit})")]
    XmlSizeExceeded {
        /// Actual XML byte length.
        size: usize,
        /// Configured limit.
        limit: usize,
    },
    /// XML failed safety validation (e.g. DOCTYPE/ENTITY, custom entity, nesting depth).
    #[error("unsafe SVG XML rejected: {0}")]
    UnsafeXml(String),
    /// SVG document failed parsing in `usvg`.
    #[error("invalid SVG document: {0}")]
    InvalidSvg(String),
    /// Computed or requested dimensions were zero, non-finite, or exceeded pixel-area limits.
    #[error("invalid SVG raster dimensions {width}x{height} (pixel limit {limit})")]
    InvalidDimensions {
        /// Computed width.
        width: u32,
        /// Computed height.
        height: u32,
        /// Configured pixel-area limit.
        limit: u64,
    },
    /// Rasterizer failed to allocate or render the output pixmap.
    #[error("SVG rasterization failed: {0}")]
    RasterizationFailed(String),
}

/// Options controlling SVG / `.tgv` rasterization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SvgRasterOptions {
    /// Optional explicit output `(width, height)`. When `None`, uses the SVG's intrinsic size.
    pub target_size: Option<(u32, u32)>,
    /// Optional maximum side length when using intrinsic size (scales down preserving aspect ratio).
    pub max_dimension: Option<u32>,
    /// Maximum allowed output pixel count (`width * height`).
    pub max_pixel_area: u64,
    /// Maximum allowed decompressed XML size in bytes.
    pub max_xml_bytes: usize,
    /// Composite semi-transparent pixels over opaque white (`Images::Read { .forceOpaque = true }`).
    pub force_opaque_white: bool,
}

impl Default for SvgRasterOptions {
    fn default() -> Self {
        Self {
            target_size: None,
            max_dimension: None,
            max_pixel_area: SVG_MAX_PIXEL_AREA,
            max_xml_bytes: SVG_MAX_XML_BYTES,
            force_opaque_white: false,
        }
    }
}

/// Tiling alignment strategy for pattern wallpapers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PatternTileMode {
    /// Seamless origin-aligned tiling (`x % tile_w`, `y % tile_h`).
    #[default]
    Repeat,
    /// Horizontally centered odd-column tiling matching tdesktop's `CacheBackgroundByRequest`.
    Centered,
}

/// Returns `true` if `bytes` begins with the gzip magic header (`0x1f 0x8b`).
#[must_use]
pub fn is_gzip_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

fn starts_with_ascii_ci(haystack: &[u8], prefix: &[u8]) -> bool {
    haystack.len() >= prefix.len() && haystack[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn contains_ascii_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}

fn strip_bom_and_ws(bytes: &[u8]) -> &[u8] {
    let without_bom = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    without_bom
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map_or(&[], |i| &without_bom[i..])
}

/// Returns `true` if `bytes` looks like an inline SVG / XML-SVG document.
#[must_use]
pub fn is_svg_bytes(bytes: &[u8]) -> bool {
    let trimmed = strip_bom_and_ws(bytes);
    if trimmed.is_empty() {
        return false;
    }
    if starts_with_ascii_ci(trimmed, b"<svg") || starts_with_ascii_ci(trimmed, b"<?xml") {
        return true;
    }
    if starts_with_ascii_ci(trimmed, b"<!doctype") || starts_with_ascii_ci(trimmed, b"<!--") {
        let probe = &trimmed[..trimmed.len().min(8192)];
        return contains_ascii_ci(probe, b"<svg");
    }
    false
}

/// Validates SVG XML bytes against size, UTF-8, DTD/entity-expansion, custom entity,
/// and nesting-depth safety limits.
pub fn validate_svg_xml_safety(
    svg_bytes: &[u8],
    max_xml_bytes: usize,
) -> Result<(), SvgPatternError> {
    if svg_bytes.is_empty() {
        return Err(SvgPatternError::EmptyInput);
    }
    if svg_bytes.len() > max_xml_bytes {
        return Err(SvgPatternError::XmlSizeExceeded {
            size: svg_bytes.len(),
            limit: max_xml_bytes,
        });
    }
    if svg_bytes.contains(&0) {
        return Err(SvgPatternError::UnsafeXml(
            "SVG contains NUL bytes".to_string(),
        ));
    }
    let text = std::str::from_utf8(svg_bytes)
        .map_err(|_| SvgPatternError::UnsafeXml("SVG is not valid UTF-8".to_string()))?;
    let bytes = text.strip_prefix('\u{feff}').unwrap_or(text).as_bytes();

    let mut pos = 0usize;
    let mut depth = 0usize;
    let mut entity_refs = 0usize;

    while pos < bytes.len() {
        match bytes[pos] {
            b'<' => {
                if starts_with_ascii_ci(&bytes[pos..], b"<!--") {
                    let Some(rel_end) = bytes[pos + 4..].windows(3).position(|w| w == b"-->")
                    else {
                        return Err(SvgPatternError::UnsafeXml(
                            "unterminated XML comment".to_string(),
                        ));
                    };
                    pos += 4 + rel_end + 3;
                } else if starts_with_ascii_ci(&bytes[pos..], b"<![cdata[") {
                    let Some(rel_end) = bytes[pos + 9..].windows(3).position(|w| w == b"]]>")
                    else {
                        return Err(SvgPatternError::UnsafeXml(
                            "unterminated CDATA section".to_string(),
                        ));
                    };
                    pos += 9 + rel_end + 3;
                } else if starts_with_ascii_ci(&bytes[pos..], b"<?") {
                    let Some(rel_end) = bytes[pos + 2..].windows(2).position(|w| w == b"?>") else {
                        return Err(SvgPatternError::UnsafeXml(
                            "unterminated XML processing instruction".to_string(),
                        ));
                    };
                    pos += 2 + rel_end + 2;
                } else if starts_with_ascii_ci(&bytes[pos..], b"<!entity") {
                    return Err(SvgPatternError::UnsafeXml(
                        "XML ENTITY declarations are forbidden".to_string(),
                    ));
                } else if starts_with_ascii_ci(&bytes[pos..], b"<!doctype") {
                    return Err(SvgPatternError::UnsafeXml(
                        "XML DOCTYPE declarations are forbidden".to_string(),
                    ));
                } else if starts_with_ascii_ci(&bytes[pos..], b"<!") {
                    return Err(SvgPatternError::UnsafeXml(
                        "XML DTD declarations are forbidden".to_string(),
                    ));
                } else if pos + 1 < bytes.len() && bytes[pos + 1] == b'/' {
                    // Closing tag
                    let Some(rel_end) = bytes[pos + 2..].iter().position(|&b| b == b'>') else {
                        return Err(SvgPatternError::UnsafeXml(
                            "unterminated closing XML tag".to_string(),
                        ));
                    };
                    depth = depth.saturating_sub(1);
                    pos += 2 + rel_end + 1;
                } else {
                    // Opening or self-closing tag: scan until '>' outside quotes
                    pos += 1;
                    let mut quote: Option<u8> = None;
                    let mut last_non_ws = 0u8;
                    let mut closed = false;
                    while pos < bytes.len() {
                        let b = bytes[pos];
                        if let Some(q) = quote {
                            if b == q {
                                quote = None;
                            } else if b == b'&' {
                                pos = validate_entity_ref(bytes, pos, &mut entity_refs)?;
                                continue;
                            }
                            pos += 1;
                        } else if b == b'"' || b == b'\'' {
                            quote = Some(b);
                            last_non_ws = b;
                            pos += 1;
                        } else if b == b'>' {
                            closed = true;
                            pos += 1;
                            break;
                        } else {
                            if !b.is_ascii_whitespace() {
                                last_non_ws = b;
                            }
                            pos += 1;
                        }
                    }
                    if !closed {
                        return Err(SvgPatternError::UnsafeXml(
                            "unterminated opening XML tag".to_string(),
                        ));
                    }
                    if last_non_ws != b'/' {
                        depth += 1;
                        if depth > SVG_MAX_NESTING_DEPTH {
                            return Err(SvgPatternError::UnsafeXml(format!(
                                "XML nesting depth exceeds limit ({SVG_MAX_NESTING_DEPTH})"
                            )));
                        }
                    }
                }
            }
            b'&' => {
                pos = validate_entity_ref(bytes, pos, &mut entity_refs)?;
            }
            _ => {
                pos += 1;
            }
        }
    }

    Ok(())
}

fn validate_entity_ref(
    bytes: &[u8],
    amp_pos: usize,
    entity_refs: &mut usize,
) -> Result<usize, SvgPatternError> {
    *entity_refs += 1;
    if *entity_refs > SVG_MAX_ENTITY_REFS {
        return Err(SvgPatternError::UnsafeXml(format!(
            "XML entity reference count exceeds limit ({SVG_MAX_ENTITY_REFS})"
        )));
    }
    let window_end = (amp_pos + 1 + 32).min(bytes.len());
    let Some(rel_semi) = bytes[amp_pos + 1..window_end]
        .iter()
        .position(|&b| b == b';')
    else {
        return Err(SvgPatternError::UnsafeXml(
            "unterminated XML entity reference".to_string(),
        ));
    };
    let name = &bytes[amp_pos + 1..amp_pos + 1 + rel_semi];
    let allowed = matches!(name, b"amp" | b"lt" | b"gt" | b"quot" | b"apos")
        || is_valid_numeric_char_ref(name);
    if !allowed {
        let rendered = String::from_utf8_lossy(name);
        return Err(SvgPatternError::UnsafeXml(format!(
            "custom XML entity '&{rendered};' is forbidden"
        )));
    }
    Ok(amp_pos + 1 + rel_semi + 1)
}

fn is_valid_numeric_char_ref(name: &[u8]) -> bool {
    let Some(rest) = name.strip_prefix(b"#") else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    if let Some(hex) = rest.strip_prefix(b"x").or_else(|| rest.strip_prefix(b"X")) {
        (1..=6).contains(&hex.len()) && hex.iter().all(u8::is_ascii_hexdigit)
    } else {
        (1..=7).contains(&rest.len()) && rest.iter().all(u8::is_ascii_digit)
    }
}

/// Compresses raw SVG bytes into a gzip `.tgv` blob.
pub fn compress_tgv(svg_bytes: &[u8]) -> Result<Vec<u8>, SvgPatternError> {
    if svg_bytes.is_empty() {
        return Err(SvgPatternError::EmptyInput);
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(svg_bytes)
        .map_err(|e| SvgPatternError::InvalidGzip(e.to_string()))?;
    encoder
        .finish()
        .map_err(|e| SvgPatternError::InvalidGzip(e.to_string()))
}

/// Decompresses a gzip `.tgv` payload using the default compressed (4 MiB) and
/// decompressed (8 MiB) safety caps.
pub fn decompress_tgv(bytes: &[u8]) -> Result<Vec<u8>, SvgPatternError> {
    if bytes.is_empty() {
        return Err(SvgPatternError::EmptyInput);
    }
    if bytes.len() > TGV_MAX_COMPRESSED_BYTES {
        return Err(SvgPatternError::CompressedSizeExceeded {
            size: bytes.len(),
            limit: TGV_MAX_COMPRESSED_BYTES,
        });
    }
    decompress_tgv_with_limit(bytes, TGV_MAX_DECOMPRESSED_BYTES)
}

/// Decompresses a gzip `.tgv` payload with an explicit decompressed-byte bomb cap.
pub fn decompress_tgv_with_limit(
    bytes: &[u8],
    max_decompressed_bytes: usize,
) -> Result<Vec<u8>, SvgPatternError> {
    if bytes.is_empty() {
        return Err(SvgPatternError::EmptyInput);
    }
    if !is_gzip_bytes(bytes) {
        return Err(SvgPatternError::InvalidGzip(
            "missing gzip magic header".to_string(),
        ));
    }
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let initial_cap = bytes
        .len()
        .saturating_mul(4)
        .min(max_decompressed_bytes)
        .min(64 * 1024);
    let mut out = Vec::with_capacity(initial_cap);
    let read_limit = (max_decompressed_bytes as u64).saturating_add(1);
    decoder
        .take(read_limit)
        .read_to_end(&mut out)
        .map_err(|e| SvgPatternError::InvalidGzip(e.to_string()))?;
    if out.len() > max_decompressed_bytes {
        return Err(SvgPatternError::DecompressionBomb {
            limit: max_decompressed_bytes,
        });
    }
    if out.is_empty() {
        return Err(SvgPatternError::EmptyInput);
    }
    Ok(out)
}

/// Decodes either raw SVG bytes or a gzip-compressed `.tgv` blob into validated SVG XML bytes.
pub fn decode_svg_or_tgv_bytes(
    bytes: &[u8],
    max_decompressed_bytes: usize,
) -> Result<Vec<u8>, SvgPatternError> {
    if bytes.is_empty() {
        return Err(SvgPatternError::EmptyInput);
    }
    let xml = if is_gzip_bytes(bytes) {
        decompress_tgv_with_limit(bytes, max_decompressed_bytes)?
    } else {
        if bytes.len() > max_decompressed_bytes {
            return Err(SvgPatternError::XmlSizeExceeded {
                size: bytes.len(),
                limit: max_decompressed_bytes,
            });
        }
        bytes.to_vec()
    };
    validate_svg_xml_safety(&xml, max_decompressed_bytes)?;
    Ok(xml)
}

fn fit_within_max_dimension(w: f32, h: f32, max_dim: u32) -> (u32, u32) {
    let max_f = max_dim as f32;
    if w <= max_f && h <= max_f {
        return ((w.round() as u32).max(1), (h.round() as u32).max(1));
    }
    let scale = (max_f / w).min(max_f / h);
    (
        ((w * scale).round() as u32).max(1),
        ((h * scale).round() as u32).max(1),
    )
}

fn resolve_raster_dimensions(
    tree: &resvg::usvg::Tree,
    options: &SvgRasterOptions,
) -> Result<(u32, u32, f32, f32), SvgPatternError> {
    let svg_size = tree.size();
    let nat_w = svg_size.width();
    let nat_h = svg_size.height();
    if !nat_w.is_finite() || !nat_h.is_finite() || nat_w <= 0.0 || nat_h <= 0.0 {
        return Err(SvgPatternError::InvalidDimensions {
            width: 0,
            height: 0,
            limit: options.max_pixel_area,
        });
    }

    let (out_w, out_h) = match options.target_size {
        Some((w, h)) => (w, h),
        None => match options.max_dimension {
            Some(max_dim) => fit_within_max_dimension(nat_w, nat_h, max_dim),
            None => {
                if nat_w.round() > (u32::MAX as f32) || nat_h.round() > (u32::MAX as f32) {
                    return Err(SvgPatternError::InvalidDimensions {
                        width: u32::MAX,
                        height: u32::MAX,
                        limit: options.max_pixel_area,
                    });
                }
                ((nat_w.round() as u32).max(1), (nat_h.round() as u32).max(1))
            }
        },
    };

    if out_w == 0 || out_h == 0 || u64::from(out_w) * u64::from(out_h) > options.max_pixel_area {
        return Err(SvgPatternError::InvalidDimensions {
            width: out_w,
            height: out_h,
            limit: options.max_pixel_area,
        });
    }

    Ok((out_w, out_h, nat_w, nat_h))
}

fn render_svg_pixmap(
    xml_bytes: &[u8],
    options: &SvgRasterOptions,
) -> Result<resvg::tiny_skia::Pixmap, SvgPatternError> {
    validate_svg_xml_safety(xml_bytes, options.max_xml_bytes)?;
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(xml_bytes, &opt)
        .map_err(|e| SvgPatternError::InvalidSvg(e.to_string()))?;
    let (out_w, out_h, nat_w, nat_h) = resolve_raster_dimensions(&tree, options)?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(out_w, out_h).ok_or_else(|| {
        SvgPatternError::RasterizationFailed(format!("failed to allocate {out_w}x{out_h} pixmap"))
    })?;
    let sx = (out_w as f32) / nat_w;
    let sy = (out_h as f32) / nat_h;
    let transform = resvg::tiny_skia::Transform::from_scale(sx, sy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(pixmap)
}

/// Rasterizes raw SVG or gzipped `.tgv` bytes into an [`RgbaImage`].
pub fn rasterize_svg_rgba(
    bytes: &[u8],
    options: &SvgRasterOptions,
) -> Result<RgbaImage, SvgPatternError> {
    let xml = decode_svg_or_tgv_bytes(bytes, options.max_xml_bytes)?;
    let pixmap = render_svg_pixmap(&xml, options)?;
    let width = pixmap.width();
    let height = pixmap.height();
    let src = pixmap.data();
    let mut dst = Vec::with_capacity(src.len());

    if options.force_opaque_white {
        for px in src.as_chunks::<4>().0 {
            let a = px[3];
            let inv = 255u8 - a;
            dst.push(px[0].saturating_add(inv));
            dst.push(px[1].saturating_add(inv));
            dst.push(px[2].saturating_add(inv));
            dst.push(255);
        }
    } else {
        for px in src.as_chunks::<4>().0 {
            let a = px[3];
            if a == 0 {
                dst.extend_from_slice(&[0, 0, 0, 0]);
            } else if a == 255 {
                dst.extend_from_slice(px);
            } else {
                let a16 = u16::from(a);
                let half = a16 / 2;
                let unpremult =
                    |c: u8| -> u8 { ((u16::from(c) * 255 + half) / a16).min(255) as u8 };
                dst.push(unpremult(px[0]));
                dst.push(unpremult(px[1]));
                dst.push(unpremult(px[2]));
                dst.push(a);
            }
        }
    }

    RgbaImage::from_raw(width, height, dst).ok_or_else(|| {
        SvgPatternError::RasterizationFailed("output RGBA buffer size mismatch".to_string())
    })
}

fn pixmap_to_alpha_mask(pixmap: &resvg::tiny_skia::Pixmap) -> Result<RgbaImage, SvgPatternError> {
    let width = pixmap.width();
    let height = pixmap.height();
    let src = pixmap.data();
    let mut dst = Vec::with_capacity(src.len());
    for px in src.as_chunks::<4>().0 {
        dst.push(0);
        dst.push(0);
        dst.push(0);
        dst.push(px[3]);
    }
    RgbaImage::from_raw(width, height, dst).ok_or_else(|| {
        SvgPatternError::RasterizationFailed("alpha mask buffer size mismatch".to_string())
    })
}

/// Rasterizes raw SVG or gzipped `.tgv` pattern bytes into a canonical black-on-transparent
/// alpha mask (`[0, 0, 0, alpha]`) suitable for pattern compositing.
pub fn rasterize_svg_pattern_mask(
    bytes: &[u8],
    target_size: Option<(u32, u32)>,
) -> Result<RgbaImage, SvgPatternError> {
    let options = SvgRasterOptions {
        target_size,
        max_dimension: target_size.is_none().then_some(SVG_PATTERN_MAX_DIMENSION),
        max_pixel_area: SVG_MAX_PIXEL_AREA,
        max_xml_bytes: TGV_MAX_DECOMPRESSED_BYTES,
        force_opaque_white: false,
    };
    let xml = decode_svg_or_tgv_bytes(bytes, options.max_xml_bytes)?;
    let pixmap = render_svg_pixmap(&xml, &options)?;
    pixmap_to_alpha_mask(&pixmap)
}

/// Decompresses a gzip `.tgv` blob with an explicit decompression-bomb cap and rasterizes
/// it into a canonical black-on-transparent alpha mask (`[0, 0, 0, alpha]`).
pub fn rasterize_tgv_pattern_mask(
    tgv_bytes: &[u8],
    target_size: Option<(u32, u32)>,
    max_decompressed_bytes: usize,
) -> Result<RgbaImage, SvgPatternError> {
    let xml = decompress_tgv_with_limit(tgv_bytes, max_decompressed_bytes)?;
    let options = SvgRasterOptions {
        target_size,
        max_dimension: target_size.is_none().then_some(SVG_PATTERN_MAX_DIMENSION),
        max_pixel_area: SVG_MAX_PIXEL_AREA,
        max_xml_bytes: max_decompressed_bytes,
        force_opaque_white: false,
    };
    let pixmap = render_svg_pixmap(&xml, &options)?;
    pixmap_to_alpha_mask(&pixmap)
}

/// Tiles a pattern mask seamlessly across `(width, height)`.
#[must_use]
pub fn tile_pattern_mask(
    tile: &RgbaImage,
    width: u32,
    height: u32,
    mode: PatternTileMode,
) -> RgbaImage {
    let mut out = RgbaImage::new(width, height);
    let tw = tile.width();
    let th = tile.height();
    if width == 0 || height == 0 || tw == 0 || th == 0 {
        return out;
    }

    let x_shift: i64 = match mode {
        PatternTileMode::Repeat => 0,
        PatternTileMode::Centered => {
            let cx = i64::from(width.div_ceil(tw));
            let cols = (cx / 2) * 2 + 1;
            (i64::from(width) - cols * i64::from(tw)) / 2
        }
    };

    let tw_i = i64::from(tw);
    let th_u = th;
    for y in 0..height {
        let sy = y % th_u;
        for x in 0..width {
            let sx = (i64::from(x) - x_shift).rem_euclid(tw_i) as u32;
            out.put_pixel(x, y, *tile.get_pixel(sx, sy));
        }
    }
    out
}

/// Computes the final opaque tinted pixel for a single gradient pixel `bg`, pattern
/// mask coverage `pattern_alpha` (`0..=255`), and `WallPaper` intensity (`-100..=100`).
///
/// - Positive `intensity` (`>= 0`): overlays pattern onto `bg` using W3C SoftLight
///   (using white strokes when `inverted` is `true`, black strokes otherwise).
/// - Negative `intensity` (`< 0`): renders dark-mode tinted pattern over a black background
///   (`bg * (pattern_alpha / 255) * (|intensity| / 100)`).
#[must_use]
pub fn tint_pattern_pixel(bg: Color, pattern_alpha: u8, intensity: i32, inverted: bool) -> Color {
    let clamped = intensity.clamp(-100, 100);
    let pa = f64::from(pattern_alpha) / 255.0;
    if clamped >= 0 {
        let sa = pa * (f64::from(clamped) / 100.0);
        if sa <= 0.0 {
            return Color::rgb(bg.r, bg.g, bg.b);
        }
        let cs = if inverted { 1.0 } else { 0.0 };
        let blend = |d: u8| -> u8 {
            let cb = f64::from(d) / 255.0;
            let sl = soft_light_channel(cb, cs);
            let res = (1.0 - sa) * cb + sa * sl;
            (res.clamp(0.0, 1.0) * 255.0).round() as u8
        };
        Color::rgb(blend(bg.r), blend(bg.g), blend(bg.b))
    } else {
        let factor = pa * (f64::from(-clamped) / 100.0);
        let scale = |d: u8| -> u8 { (f64::from(d) * factor).round().clamp(0.0, 255.0) as u8 };
        Color::rgb(scale(bg.r), scale(bg.g), scale(bg.b))
    }
}

/// Composites a pattern alpha mask onto a gradient image into an opaque `RgbaImage`,
/// handling both positive intensity (SoftLight overlay on gradient) and negative
/// dark-mode intensity (dark background with gradient-tinted pattern).
#[must_use]
pub fn composite_pattern_on_gradient_opaque(
    gradient: &RgbaImage,
    pattern_mask: &RgbaImage,
    bg_colors: &[Color],
    intensity: i32,
) -> RgbaImage {
    let clamped = intensity.clamp(-100, 100);
    let opacity = f64::from(clamped) / 100.0;
    if clamped >= 0 {
        return compose_pattern_on_gradient(gradient, pattern_mask, bg_colors, opacity);
    }

    let width = gradient.width();
    let height = gradient.height();
    let resized_mask = if pattern_mask.width() == width && pattern_mask.height() == height {
        None
    } else {
        Some(image::imageops::resize(
            pattern_mask,
            width,
            height,
            image::imageops::FilterType::Triangle,
        ))
    };
    let mask = resized_mask.as_ref().unwrap_or(pattern_mask);
    let strength = f64::from(-clamped) / 100.0;
    let mut out = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let g = gradient.get_pixel(x, y).0;
            let pa = f64::from(mask.get_pixel(x, y).0[3]) / 255.0;
            let factor = pa * strength;
            let scale = |c: u8| -> u8 { (f64::from(c) * factor).round().clamp(0.0, 255.0) as u8 };
            out.put_pixel(x, y, Rgba([scale(g[0]), scale(g[1]), scale(g[2]), 255]));
        }
    }
    out
}

/// End-to-end renderer for SVG / gzipped `.tgv` pattern wallpapers tiled seamlessly
/// over a 1-to-4-color background gradient with positive or negative dark-mode intensity.
#[allow(clippy::too_many_arguments)]
pub fn render_tiled_pattern_wallpaper(
    svg_or_tgv: &[u8],
    width: u32,
    height: u32,
    bg_colors: &[Color],
    gradient_rotation: i32,
    intensity: i32,
    tile_size: Option<(u32, u32)>,
    tile_mode: PatternTileMode,
) -> Result<RgbaImage, SvgPatternError> {
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > SVG_MAX_PIXEL_AREA {
        return Err(SvgPatternError::InvalidDimensions {
            width,
            height,
            limit: SVG_MAX_PIXEL_AREA,
        });
    }

    let mask_tile = rasterize_svg_pattern_mask(svg_or_tgv, tile_size)?;
    let tiled_mask = tile_pattern_mask(&mask_tile, width, height, tile_mode);

    let effective_colors: &[Color] = if bg_colors.is_empty() {
        &[DEFAULT_BACKGROUND_COLOR]
    } else {
        bg_colors
    };

    let clamped = intensity.clamp(-100, 100);
    let mut gradient = generate_gradient(width, height, effective_colors, gradient_rotation, 1.0);
    if effective_colors.len() > 1 && clamped >= 0 {
        gradient = dither_image(&gradient);
    }

    Ok(composite_pattern_on_gradient_opaque(
        &gradient,
        &tiled_mask,
        effective_colors,
        clamped,
    ))
}

/// Returns whether a positive-intensity pattern should invert to white strokes on `bg_colors`.
#[must_use]
pub fn should_invert_pattern_for_intensity(bg_colors: &[Color], intensity: i32) -> bool {
    is_pattern_inverted(bg_colors, f64::from(intensity.clamp(-100, 100)) / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><rect x="0" y="0" width="8" height="16" fill="#000000"/></svg>"##;

    #[test]
    fn xml_safety_rejects_doctype_entity_and_deep_nesting() {
        let doctype = br#"<!DOCTYPE svg [<!ENTITY lol "aaaa">]><svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><text>&lol;</text></svg>"#;
        assert!(matches!(
            validate_svg_xml_safety(doctype, SVG_MAX_XML_BYTES),
            Err(SvgPatternError::UnsafeXml(_))
        ));

        let custom_entity =
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">&evil;</svg>"#;
        assert!(matches!(
            validate_svg_xml_safety(custom_entity, SVG_MAX_XML_BYTES),
            Err(SvgPatternError::UnsafeXml(_))
        ));

        let mut deep =
            String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">"#);
        for _ in 0..=SVG_MAX_NESTING_DEPTH {
            deep.push_str("<g>");
        }
        for _ in 0..=SVG_MAX_NESTING_DEPTH {
            deep.push_str("</g>");
        }
        deep.push_str("</svg>");
        assert!(matches!(
            validate_svg_xml_safety(deep.as_bytes(), SVG_MAX_XML_BYTES),
            Err(SvgPatternError::UnsafeXml(_))
        ));

        // Standard XML entities and numeric char refs remain valid.
        let benign = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><title>A &amp; B &#65; &#x42;</title><rect width="8" height="8"/></svg>"#;
        assert!(validate_svg_xml_safety(benign, SVG_MAX_XML_BYTES).is_ok());
    }

    #[test]
    fn tgv_round_trip_and_bomb_cap() {
        let tgv = compress_tgv(SIMPLE_SVG).unwrap();
        assert!(is_gzip_bytes(&tgv));
        let raw = decompress_tgv(&tgv).unwrap();
        assert_eq!(raw, SIMPLE_SVG);

        // Bomb exceeding small custom cap is rejected before full expansion.
        let big = vec![b' '; 64 * 1024];
        let bomb = compress_tgv(&big).unwrap();
        assert!(matches!(
            decompress_tgv_with_limit(&bomb, 4096),
            Err(SvgPatternError::DecompressionBomb { limit: 4096 })
        ));
    }

    #[test]
    fn rasterize_mask_and_tile_seamlessly() {
        let mask = rasterize_svg_pattern_mask(SIMPLE_SVG, None).unwrap();
        assert_eq!(mask.dimensions(), (16, 16));
        // Left half opaque black mask, right half transparent.
        assert_eq!(mask.get_pixel(2, 8).0, [0, 0, 0, 255]);
        assert_eq!(mask.get_pixel(12, 8).0, [0, 0, 0, 0]);

        let tiled = tile_pattern_mask(&mask, 48, 32, PatternTileMode::Repeat);
        assert_eq!(tiled.dimensions(), (48, 32));
        for y in 0..32 {
            for x in 0..32 {
                assert_eq!(tiled.get_pixel(x, y), tiled.get_pixel(x + 16, y));
            }
        }
    }

    #[test]
    fn pixel_area_limit_enforced_before_allocation() {
        let huge = br#"<svg xmlns="http://www.w3.org/2000/svg" width="6000" height="6000"><rect width="6000" height="6000"/></svg>"#;
        assert!(matches!(
            rasterize_svg_rgba(huge, &SvgRasterOptions::default()),
            Err(SvgPatternError::InvalidDimensions { .. })
        ));
    }
}
