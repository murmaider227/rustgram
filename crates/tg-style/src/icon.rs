//! Multi-layer icon descriptors, PNG/SVG mask loading, DPI variant selection, modifiers, and colorization.
//!
//! Port of `Telegram/lib_ui/ui/style/style_core_icon.h`, `style_core_icon.cpp`,
//! and `colorizeImage` from `Telegram/lib_ui/ui/style/style_core.cpp`.

use crate::color::{Color, ColorRef, ColorResolver};
use crate::scale::{Scale, convert_scale, device_pixel_ratio, scale};
use crate::types::{Margins, Pixels, ScaledSize};
use image::imageops::FilterType;
use image::{GrayImage, ImageError};
use thiserror::Error;

include!(concat!(env!("OUT_DIR"), "/icon_assets.rs"));

/// Errors returned while decoding or rasterizing an icon mask.
#[derive(Debug, Error)]
pub enum IconError {
    /// Referenced icon asset was not found in the bundled icon archive.
    #[error("icon asset not found: {0}")]
    AssetNotFound(String),
    /// PNG decoding error.
    #[error("failed to decode PNG icon '{path}': {source}")]
    PngDecode {
        /// Relative asset path.
        path: String,
        /// Underlying image decoder error.
        #[source]
        source: ImageError,
    },
    /// SVG parsing error.
    #[error("failed to parse SVG icon '{path}': {message}")]
    SvgParse {
        /// Relative asset path.
        path: String,
        /// Error details.
        message: String,
    },
    /// Invalid target pixel dimensions (e.g. zero).
    #[error("invalid rasterization dimensions: {0}x{1}")]
    InvalidDimensions(u32, u32),
}

/// Modifier applied to an icon mask, matching `codegen/style/parsed_file.cpp::GetModifier`
/// and `GetSizeModifier`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconModifier {
    /// Invert mask intensities (`255 - v`).
    Invert,
    /// Mirror horizontally (left-right).
    FlipHorizontal,
    /// Mirror vertically (top-bottom).
    FlipVertical,
    /// Rotate 90 degrees clockwise.
    RotateCw,
    /// Rotate 90 degrees counter-clockwise.
    RotateCcw,
    /// Resize to explicit 1x logical dimensions `(width, height)`.
    Size(u32, u32),
}

/// Source specification for a single icon mask layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconMaskRef {
    /// Multi-resolution PNG mask (`<path>.png`, `<path>@2x.png`, `<path>@3x.png`).
    Png {
        /// Relative path under `assets/icons/` without extension or `@Nx` suffix.
        path: &'static str,
        /// Ordered list of image modifiers from the `-modifier` suffix in `.style`.
        modifiers: &'static [IconModifier],
    },
    /// Vector SVG mask (`<path>.svg`).
    Svg {
        /// Relative path under `assets/icons/` without `.svg`.
        path: &'static str,
        /// Optional explicit 1x logical render size from `-WxH` modifier.
        size_override: Option<(u32, u32)>,
        /// Optional pixel modifiers.
        modifiers: &'static [IconModifier],
    },
    /// Solid generated rectangle (`icon {{ size(w, h), color }}`).
    SolidRect {
        /// Unscaled width.
        width: Pixels,
        /// Unscaled height.
        height: Pixels,
    },
}

/// One tinted layer of a multi-layer [`Icon`] (`style::internal::MonoIcon`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MonoIcon {
    /// Mask source (PNG, SVG, or solid rectangle).
    pub mask: IconMaskRef,
    /// Palette color reference used to tint this layer.
    pub color: ColorRef,
    /// Padding/offset around the mask (`left, top, right, bottom` in unscaled pixels).
    pub padding: Margins,
}

/// Multi-layer icon style value (`style::icon`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Icon {
    /// Ordered layers composited bottom-to-top.
    pub parts: &'static [MonoIcon],
}

/// Rendered RGBA8 bitmap returned by icon rasterization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    /// Physical pixel width.
    pub width: u32,
    /// Physical pixel height.
    pub height: u32,
    /// Device pixel ratio used during rasterization (`1`, `2`, `3`, ...).
    pub dpr: i32,
    /// Row-major premultiplied RGBA8 pixels (`width * height * 4` bytes).
    pub pixels: Vec<u8>,
}

impl RgbaImage {
    /// Logical width in density-independent pixels (`width / dpr`).
    #[must_use]
    pub fn logical_width(&self) -> u32 {
        self.width / self.dpr.max(1) as u32
    }

    /// Logical height in density-independent pixels (`height / dpr`).
    #[must_use]
    pub fn logical_height(&self) -> u32 {
        self.height / self.dpr.max(1) as u32
    }

    /// Convert premultiplied RGBA8 pixels into straight (unpremultiplied) RGBA8 bytes.
    #[must_use]
    pub fn to_straight_rgba(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.pixels.len()];
        for (src, dst) in self
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(out.as_chunks_mut::<4>().0.iter_mut())
        {
            let a = src[3];
            if a == 0 {
                dst.copy_from_slice(&[0, 0, 0, 0]);
            } else if a == 255 {
                dst.copy_from_slice(src);
            } else {
                let af = f32::from(a);
                dst[0] = ((f32::from(src[0]) * 255.0 / af).round()).clamp(0.0, 255.0) as u8;
                dst[1] = ((f32::from(src[1]) * 255.0 / af).round()).clamp(0.0, 255.0) as u8;
                dst[2] = ((f32::from(src[2]) * 255.0 / af).round()).clamp(0.0, 255.0) as u8;
                dst[3] = a;
            }
        }
        out
    }
}

/// Which `@Nx` PNG asset variant to choose for a given `real_scale = scale * dpr`.
///
/// Port of `CreateIconMask` variant selection in `style_core_icon.cpp`:
/// - `real_scale == 100` -> `""` (1x, exact)
/// - `real_scale == 200` -> `"@2x"` (2x, exact)
/// - `real_scale == 300` -> `"@3x"` (3x, exact)
/// - `real_scale > 200` -> `"@3x"` (then smoothly scaled)
/// - otherwise -> `"@2x"` (then smoothly scaled)
#[must_use]
pub const fn png_variant_for_real_scale(real_scale: i32) -> (&'static str, bool) {
    match real_scale {
        100 => ("", true),
        200 => ("@2x", true),
        300 => ("@3x", true),
        s if s > 200 => ("@3x", false),
        _ => ("@2x", false),
    }
}

/// Colorize a single-channel opacity mask (`0..=255`) with `color`, producing premultiplied RGBA8
/// pixels using tdesktop's exact `colorizeImage` / `anim::shifted` integer math.
#[must_use]
pub fn colorize_mask_premultiplied(mask: &GrayImage, color: Color) -> Vec<u8> {
    let [pr, pg, pb, pa] = color.to_premultiplied();
    let pr = u32::from(pr);
    let pg = u32::from(pg);
    let pb = u32::from(pb);
    let pa = u32::from(pa);

    let mut out = vec![0u8; (mask.width() * mask.height() * 4) as usize];
    for (m, dst) in mask
        .as_raw()
        .iter()
        .zip(out.as_chunks_mut::<4>().0.iter_mut())
    {
        let mult = u32::from(*m) + 1;
        dst[0] = ((pr * mult) >> 8) as u8;
        dst[1] = ((pg * mult) >> 8) as u8;
        dst[2] = ((pb * mult) >> 8) as u8;
        dst[3] = ((pa * mult) >> 8) as u8;
    }
    out
}

impl IconMaskRef {
    /// Load and rasterize the single-channel grayscale opacity mask at `(target_scale, dpr)`.
    ///
    /// Returns `(mask_gray_image, logical_width, logical_height)`.
    pub fn load_mask(
        &self,
        target_scale: Scale,
        dpr: i32,
    ) -> Result<(GrayImage, i32, i32), IconError> {
        let dpr = dpr.max(1);
        match *self {
            IconMaskRef::SolidRect { width, height } => {
                let lw = width.scaled_at(target_scale).max(0);
                let lh = height.scaled_at(target_scale).max(0);
                let pw = (lw * dpr).max(1) as u32;
                let ph = (lh * dpr).max(1) as u32;
                let mask = GrayImage::from_pixel(pw, ph, image::Luma([255]));
                Ok((mask, lw, lh))
            }
            IconMaskRef::Png { path, modifiers } => {
                let real_scale = target_scale.percent() * dpr;
                let (suffix, is_exact) = png_variant_for_real_scale(real_scale);

                // Read 1x header dimensions to compute exact logical 1x size after modifiers.
                let key_1x = format!("{path}.png");
                let bytes_1x = icon_asset_bytes(&key_1x)
                    .ok_or_else(|| IconError::AssetNotFound(key_1x.clone()))?;
                let (mut base_w, mut base_h) = read_png_ihdr_dims(bytes_1x, &key_1x)?;

                let variant_key = format!("{path}{suffix}.png");
                let variant_bytes = icon_asset_bytes(&variant_key)
                    .ok_or_else(|| IconError::AssetNotFound(variant_key.clone()))?;
                let decoded =
                    image::load_from_memory_with_format(variant_bytes, image::ImageFormat::Png)
                        .map_err(|source| IconError::PngDecode {
                            path: variant_key.clone(),
                            source,
                        })?;

                // tdesktop PNG icon masks are white-on-black RGB; blue channel (same as R/G/luma)
                // carries the mask intensity.
                let mut mask = rgb_to_mask_gray(&decoded);

                let variant_mult: u32 = match suffix {
                    "" => 1,
                    "@2x" => 2,
                    "@3x" => 3,
                    _ => 1,
                };

                for &m in modifiers {
                    match m {
                        IconModifier::Invert => {
                            for p in mask.as_mut() {
                                *p = 255 - *p;
                            }
                        }
                        IconModifier::FlipHorizontal => {
                            mask = image::imageops::flip_horizontal(&mask);
                        }
                        IconModifier::FlipVertical => {
                            mask = image::imageops::flip_vertical(&mask);
                        }
                        IconModifier::RotateCw => {
                            mask = image::imageops::rotate90(&mask);
                            std::mem::swap(&mut base_w, &mut base_h);
                        }
                        IconModifier::RotateCcw => {
                            mask = image::imageops::rotate270(&mask);
                            std::mem::swap(&mut base_w, &mut base_h);
                        }
                        IconModifier::Size(sw, sh) => {
                            base_w = sw;
                            base_h = sh;
                            mask = image::imageops::resize(
                                &mask,
                                sw * variant_mult,
                                sh * variant_mult,
                                FilterType::CatmullRom,
                            );
                        }
                    }
                }

                let logical_w = convert_scale(base_w as i32, target_scale);
                let logical_h = convert_scale(base_h as i32, target_scale);
                let target_pw = (logical_w * dpr).max(1) as u32;
                let target_ph = (logical_h * dpr).max(1) as u32;

                if !is_exact || mask.width() != target_pw || mask.height() != target_ph {
                    mask = image::imageops::resize(
                        &mask,
                        target_pw,
                        target_ph,
                        FilterType::CatmullRom,
                    );
                }

                Ok((mask, logical_w, logical_h))
            }
            IconMaskRef::Svg {
                path,
                size_override,
                modifiers,
            } => {
                let key = format!("{path}.svg");
                let bytes =
                    icon_asset_bytes(&key).ok_or_else(|| IconError::AssetNotFound(key.clone()))?;
                let opt = resvg::usvg::Options::default();
                let tree =
                    resvg::usvg::Tree::from_data(bytes, &opt).map_err(|e| IconError::SvgParse {
                        path: key.clone(),
                        message: e.to_string(),
                    })?;

                let svg_size = tree.size();
                let (base_w, base_h) = match size_override {
                    Some((w, h)) => (w as i32, h as i32),
                    None => (
                        svg_size.width().round() as i32,
                        svg_size.height().round() as i32,
                    ),
                };
                let mut logical_w = convert_scale(base_w, target_scale);
                let mut logical_h = convert_scale(base_h, target_scale);
                let target_pw = (logical_w * dpr).max(1) as u32;
                let target_ph = (logical_h * dpr).max(1) as u32;

                let mut pixmap = resvg::tiny_skia::Pixmap::new(target_pw, target_ph)
                    .ok_or(IconError::InvalidDimensions(target_pw, target_ph))?;
                let sx = (target_pw as f32) / svg_size.width().max(1.0);
                let sy = (target_ph as f32) / svg_size.height().max(1.0);
                let transform = resvg::tiny_skia::Transform::from_scale(sx, sy);
                resvg::render(&tree, transform, &mut pixmap.as_mut());

                // SVG masks are white on transparent: alpha channel is the opacity mask.
                let mut mask_buf = Vec::with_capacity((target_pw * target_ph) as usize);
                for px in pixmap.data().as_chunks::<4>().0 {
                    mask_buf.push(px[3]);
                }
                let mut mask = GrayImage::from_raw(target_pw, target_ph, mask_buf)
                    .ok_or(IconError::InvalidDimensions(target_pw, target_ph))?;

                for &m in modifiers {
                    match m {
                        IconModifier::Invert => {
                            for p in mask.as_mut() {
                                *p = 255 - *p;
                            }
                        }
                        IconModifier::FlipHorizontal => {
                            mask = image::imageops::flip_horizontal(&mask);
                        }
                        IconModifier::FlipVertical => {
                            mask = image::imageops::flip_vertical(&mask);
                        }
                        IconModifier::RotateCw => {
                            mask = image::imageops::rotate90(&mask);
                            std::mem::swap(&mut logical_w, &mut logical_h);
                        }
                        IconModifier::RotateCcw => {
                            mask = image::imageops::rotate270(&mask);
                            std::mem::swap(&mut logical_w, &mut logical_h);
                        }
                        IconModifier::Size(_, _) => {}
                    }
                }

                Ok((mask, logical_w, logical_h))
            }
        }
    }

    /// Unscaled 1x logical dimensions `(width, height)` of this mask after modifiers.
    pub fn base_size(&self) -> Result<(u32, u32), IconError> {
        match *self {
            IconMaskRef::SolidRect { width, height } => {
                Ok((width.raw().max(0) as u32, height.raw().max(0) as u32))
            }
            IconMaskRef::Png { path, modifiers } => {
                let key = format!("{path}.png");
                let bytes =
                    icon_asset_bytes(&key).ok_or_else(|| IconError::AssetNotFound(key.clone()))?;
                let (mut w, mut h) = read_png_ihdr_dims(bytes, &key)?;
                for &m in modifiers {
                    match m {
                        IconModifier::RotateCw | IconModifier::RotateCcw => {
                            std::mem::swap(&mut w, &mut h);
                        }
                        IconModifier::Size(sw, sh) => {
                            w = sw;
                            h = sh;
                        }
                        _ => {}
                    }
                }
                Ok((w, h))
            }
            IconMaskRef::Svg {
                path,
                size_override,
                modifiers,
            } => {
                let (mut w, mut h) = if let Some((sw, sh)) = size_override {
                    (sw, sh)
                } else {
                    let key = format!("{path}.svg");
                    let bytes = icon_asset_bytes(&key)
                        .ok_or_else(|| IconError::AssetNotFound(key.clone()))?;
                    let tree =
                        resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default())
                            .map_err(|e| IconError::SvgParse {
                                path: key,
                                message: e.to_string(),
                            })?;
                    let sz = tree.size();
                    (sz.width().round() as u32, sz.height().round() as u32)
                };
                for &m in modifiers {
                    if matches!(m, IconModifier::RotateCw | IconModifier::RotateCcw) {
                        std::mem::swap(&mut w, &mut h);
                    }
                }
                Ok((w, h))
            }
        }
    }
}

impl MonoIcon {
    /// Scaled logical size of this layer including its padding (`size.grownBy(padding)`).
    pub fn scaled_size_at(&self, target_scale: Scale) -> Result<ScaledSize, IconError> {
        let (bw, bh) = self.mask.base_size()?;
        let pad = self.padding.scaled_at(target_scale);
        Ok(ScaledSize {
            width: convert_scale(bw as i32, target_scale) + pad.horizontal(),
            height: convert_scale(bh as i32, target_scale) + pad.vertical(),
        })
    }
}

impl Icon {
    /// Empty icon with zero layers.
    pub const EMPTY: Self = Self { parts: &[] };

    /// Construct a multi-layer icon from static [`MonoIcon`] layers.
    #[must_use]
    pub const fn new(parts: &'static [MonoIcon]) -> Self {
        Self { parts }
    }

    /// Returns `true` if this icon has no layers.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.parts.is_empty()
    }

    /// Logical scaled size of the icon at the current global [`scale()`].
    pub fn scaled_size(self) -> Result<ScaledSize, IconError> {
        self.scaled_size_at(scale())
    }

    /// Logical scaled size of the icon at an explicit [`Scale`].
    ///
    /// Matches `IconData::width()` / `IconData::height()` (`max` across all layers).
    pub fn scaled_size_at(self, target_scale: Scale) -> Result<ScaledSize, IconError> {
        let mut w = 0i32;
        let mut h = 0i32;
        for part in self.parts {
            let ps = part.scaled_size_at(target_scale)?;
            w = w.max(ps.width);
            h = h.max(ps.height);
        }
        Ok(ScaledSize {
            width: w,
            height: h,
        })
    }

    /// Rasterize and composite all layers at the current global [`scale()`] and
    /// [`device_pixel_ratio()`], resolving layer colors through `resolver`.
    pub fn rasterize(self, resolver: &dyn ColorResolver) -> Result<RgbaImage, IconError> {
        self.rasterize_at(scale(), device_pixel_ratio(), resolver)
    }

    /// Rasterize and composite all layers at `(target_scale, dpr)`, resolving layer colors
    /// through `resolver`.
    pub fn rasterize_at(
        self,
        target_scale: Scale,
        dpr: i32,
        resolver: &dyn ColorResolver,
    ) -> Result<RgbaImage, IconError> {
        self.rasterize_inner(target_scale, dpr, |c| c.resolve(resolver))
    }

    /// Rasterize and composite all layers at `(target_scale, dpr)` with a single override color
    /// (matching `Icon::instance(colorOverride, scale)`).
    pub fn rasterize_with_color(
        self,
        target_scale: Scale,
        dpr: i32,
        color_override: Color,
    ) -> Result<RgbaImage, IconError> {
        self.rasterize_inner(target_scale, dpr, |_| color_override)
    }

    fn rasterize_inner(
        self,
        target_scale: Scale,
        dpr: i32,
        mut resolve_color: impl FnMut(ColorRef) -> Color,
    ) -> Result<RgbaImage, IconError> {
        let dpr = dpr.max(1);
        if self.parts.is_empty() {
            return Ok(RgbaImage {
                width: 0,
                height: 0,
                dpr,
                pixels: Vec::new(),
            });
        }

        struct Layer {
            rgba: Vec<u8>,
            w: u32,
            h: u32,
            ox: i32,
            oy: i32,
        }

        let mut layers = Vec::with_capacity(self.parts.len());
        let mut canvas_w = 0u32;
        let mut canvas_h = 0u32;

        for part in self.parts {
            let (mask, _lw, _lh) = part.mask.load_mask(target_scale, dpr)?;
            let color = resolve_color(part.color);
            let rgba = colorize_mask_premultiplied(&mask, color);
            let pad = part.padding.scaled_at(target_scale);
            let ox = pad.left * dpr;
            let oy = pad.top * dpr;
            let total_w = (mask.width() as i32 + pad.horizontal() * dpr).max(0) as u32;
            let total_h = (mask.height() as i32 + pad.vertical() * dpr).max(0) as u32;
            canvas_w = canvas_w.max(total_w);
            canvas_h = canvas_h.max(total_h);
            layers.push(Layer {
                rgba,
                w: mask.width(),
                h: mask.height(),
                ox,
                oy,
            });
        }

        if layers.len() == 1
            && layers[0].ox == 0
            && layers[0].oy == 0
            && layers[0].w == canvas_w
            && layers[0].h == canvas_h
            && let Some(only) = layers.pop()
        {
            return Ok(RgbaImage {
                width: only.w,
                height: only.h,
                dpr,
                pixels: only.rgba,
            });
        }

        let mut canvas = vec![0u8; (canvas_w * canvas_h * 4) as usize];
        for layer in &layers {
            blend_source_over_premultiplied(
                &mut canvas,
                canvas_w,
                canvas_h,
                &layer.rgba,
                layer.w,
                layer.h,
                layer.ox,
                layer.oy,
            );
        }

        Ok(RgbaImage {
            width: canvas_w,
            height: canvas_h,
            dpr,
            pixels: canvas,
        })
    }
}

fn read_png_ihdr_dims(bytes: &[u8], path: &str) -> Result<(u32, u32), IconError> {
    const SIG: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
    if bytes.len() < 24 || bytes[0..8] != SIG || &bytes[12..16] != b"IHDR" {
        return Err(IconError::InvalidDimensions(0, 0));
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if w == 0 || h == 0 {
        let _ = path;
        return Err(IconError::InvalidDimensions(w, h));
    }
    Ok((w, h))
}

fn rgb_to_mask_gray(decoded: &image::DynamicImage) -> GrayImage {
    // Match Qt's Format_RGB32 + reading byte 0 (blue channel of white-on-black grayscale PNG).
    let rgb = decoded.to_rgb8();
    let (w, h) = rgb.dimensions();
    let mut gray = Vec::with_capacity((w * h) as usize);
    for px in rgb.as_raw().as_chunks::<3>().0 {
        gray.push(px[2]);
    }
    GrayImage::from_raw(w, h, gray).unwrap_or_else(|| GrayImage::new(w, h))
}

#[allow(clippy::too_many_arguments)]
fn blend_source_over_premultiplied(
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    src: &[u8],
    src_w: u32,
    src_h: u32,
    ox: i32,
    oy: i32,
) {
    for sy in 0..src_h {
        let dy = oy + sy as i32;
        if dy < 0 || dy >= dst_h as i32 {
            continue;
        }
        for sx in 0..src_w {
            let dx = ox + sx as i32;
            if dx < 0 || dx >= dst_w as i32 {
                continue;
            }
            let si = ((sy * src_w + sx) * 4) as usize;
            let di = ((dy as u32 * dst_w + dx as u32) * 4) as usize;
            let sa = u32::from(src[si + 3]);
            if sa == 0 {
                continue;
            }
            if sa == 255 {
                dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
                continue;
            }
            let inv_a = 255 - sa;
            for c in 0..4 {
                let s = u32::from(src[si + c]);
                let d = u32::from(dst[di + c]);
                dst[di + c] = (s + ((d * inv_a + 127) / 255)).min(255) as u8;
            }
        }
    }
}
