//! Common types, constants, gzip/UTF-8 helpers, and frame buffers.
//!
//! Port of `lottie/lottie_common.h`, `lottie/lottie_common.cpp`,
//! `lottie/details/lottie_frame_renderer.h`, and `ui/image/image_prepare.cpp`
//! (`UnpackGzip`, `Colored`).

use std::io::Read;

use flate2::read::GzDecoder;
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

use crate::error::Error;

/// Sentinel timestamp indicating an unknown time (`Lottie::kTimeUnknown`).
pub const TIME_UNKNOWN: i64 = i64::MIN;

/// Sentinel timestamp indicating the frame was already scheduled (`kFrameDisplayTimeAlreadyDone`).
pub const FRAME_DISPLAY_TIME_ALREADY_DONE: i64 = i64::MAX;

/// Initial value for a frame's displayed timestamp (`kDisplayedInitial`).
pub const DISPLAYED_INITIAL: i64 = -1;

/// Maximum uncompressed Lottie JSON size accepted by tdesktop (`Lottie::kMaxFileSize`, 2 MiB).
pub const MAX_FILE_SIZE: usize = 2 * 1024 * 1024;

/// Maximum decompressed gzip payload size (`Images::kMaxGzipFileSize`, 5 MiB).
pub const MAX_GZIP_FILE_SIZE: usize = 5 * 1024 * 1024;

/// Telegram `.tgs` animated sticker compressed size limit (64 KiB).
pub const TGS_MAX_COMPRESSED_SIZE: usize = 64 * 1024;

/// Normal sticker playback frame rate (`Lottie::kNormalFrameRate`).
pub const NORMAL_FRAME_RATE: u32 = 30;

/// Maximum supported frame rate (`Lottie::kMaxFrameRate`).
pub const MAX_FRAME_RATE: u32 = 60;

/// Maximum supported composition width or height (`Lottie::kMaxSize`).
pub const MAX_SIZE: u32 = 4096;

/// Maximum supported frame count (`Lottie::kMaxFramesCount`).
pub const MAX_FRAMES_COUNT: u32 = 210;

/// Ideal default sticker render size (`kIdealSize`, 512x512).
pub const IDEAL_SIZE: Size = Size {
    width: 512,
    height: 512,
};

/// 2D integer dimensions in pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Size {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Size {
    /// Creates a new `Size`.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Returns `true` if either dimension is zero.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Scales `self` to fit inside `target` while preserving aspect ratio
    /// (matches `QSize::scaled(target, Qt::KeepAspectRatio)`).
    #[must_use]
    pub fn scaled_keep_aspect(self, target: Size) -> Size {
        if self.is_empty() || target.is_empty() {
            return Size::default();
        }
        let rw = (u64::from(target.height) * u64::from(self.width) + u64::from(self.height) / 2)
            / u64::from(self.height);
        if rw <= u64::from(target.width) {
            Size::new(rw.max(1) as u32, target.height)
        } else {
            let rh = (u64::from(target.width) * u64::from(self.height) + u64::from(self.width) / 2)
                / u64::from(self.width);
            Size::new(target.width, rh.max(1) as u32)
        }
    }

    /// Scales `self` to cover `target` while preserving aspect ratio
    /// (matches `QSize::scaled(target, Qt::KeepAspectRatioByExpanding)`).
    #[must_use]
    pub fn scaled_keep_aspect_expanding(self, target: Size) -> Size {
        if self.is_empty() || target.is_empty() {
            return Size::default();
        }
        let rw = (u64::from(target.height) * u64::from(self.width) + u64::from(self.height) / 2)
            / u64::from(self.height);
        if rw >= u64::from(target.width) {
            Size::new(rw.max(1) as u32, target.height)
        } else {
            let rh = (u64::from(target.width) * u64::from(self.height) + u64::from(self.width) / 2)
                / u64::from(self.width);
            Size::new(target.width, rh.max(1) as u32)
        }
    }
}

/// Straight (non-premultiplied) 8-bit RGBA color.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Color {
    /// Red channel (0..=255).
    pub r: u8,
    /// Green channel (0..=255).
    pub g: u8,
    /// Blue channel (0..=255).
    pub b: u8,
    /// Alpha channel (0..=255).
    pub a: u8,
}

impl Color {
    /// Fully transparent black.
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);
    /// Opaque white.
    pub const WHITE: Self = Self::rgb(255, 255, 255);
    /// Opaque black.
    pub const BLACK: Self = Self::rgb(0, 0, 0);

    /// Creates an opaque RGB color.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Creates an RGBA color.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Creates an opaque color from `0xRRGGBB`.
    #[must_use]
    pub const fn from_rgb_u32(rgb: u32) -> Self {
        Self {
            r: ((rgb >> 16) & 0xFF) as u8,
            g: ((rgb >> 8) & 0xFF) as u8,
            b: (rgb & 0xFF) as u8,
            a: 255,
        }
    }

    /// Encodes the RGB channels as `0x00RRGGBB`.
    #[must_use]
    pub const fn to_rgb_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }
}

/// Metadata describing a loaded Lottie animation (`Lottie::Information`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Information {
    /// Original composition dimensions.
    pub size: Size,
    /// Effective playback frame rate in frames per second.
    pub frame_rate: u32,
    /// Effective number of frames in the animation.
    pub frames_count: u32,
}

/// Frame rendering parameters (`Lottie::FrameRequest`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameRequest {
    /// Target bounding box; if empty, the original composition size is used.
    pub box_size: Size,
    /// Optional color overlay (`a == 0` disables coloring).
    pub colored: Color,
    /// Whether to mirror the rendered frame horizontally.
    pub mirror_horizontal: bool,
}

impl FrameRequest {
    /// Creates a `FrameRequest` with only a bounding box.
    #[must_use]
    pub const fn new(box_size: Size) -> Self {
        Self {
            box_size,
            colored: Color::TRANSPARENT,
            mirror_horizontal: false,
        }
    }

    /// Returns `true` if `box_size` is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.box_size.is_empty()
    }

    /// Computes the rounded target render size for `original`, matching
    /// `Lottie::FrameRequest::size(original, sizeRounding)` in `lottie_common.cpp`.
    #[must_use]
    pub fn size(&self, original: Size, size_rounding: u32) -> Size {
        if self.is_empty() || original.is_empty() {
            return Size::default();
        }
        let rounding = size_rounding.max(1);
        let scaled = original.scaled_keep_aspect(self.box_size);
        let skip_w = scaled.width % rounding;
        let skip_h = scaled.height % rounding;
        Size::new(
            scaled.width.saturating_sub(skip_w).max(rounding),
            scaled.height.saturating_sub(skip_h).max(rounding),
        )
    }
}

/// Playback quality / frame-rate reduction mode (`Lottie::Quality`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Quality {
    /// Halves 60 fps animations to 30 fps (every second frame), matching tdesktop default.
    #[default]
    Default,
    /// Renders at full native frame rate (up to 60 fps).
    High,
    /// Renders synchronously at full native frame rate.
    Synchronous,
}

/// Emoji Fitzpatrick skin tone modifier (`Lottie::SkinModifier`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SkinModifier {
    /// Default (no skin tone override).
    #[default]
    None,
    /// Fitzpatrick Type 1-2 (light skin tone).
    Color1,
    /// Fitzpatrick Type 3 (medium-light skin tone).
    Color2,
    /// Fitzpatrick Type 4 (medium skin tone).
    Color3,
    /// Fitzpatrick Type 5 (medium-dark skin tone).
    Color4,
    /// Fitzpatrick Type 6 (dark skin tone).
    Color5,
}

/// Color replacements applied to a Lottie model at parse time (`Lottie::ColorReplacements`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColorReplacements {
    /// Explicit `(source_0xRRGGBB, target_0xRRGGBB)` color pairs.
    pub replacements: Vec<(u32, u32)>,
    /// Fitzpatrick emoji skin tone modifier.
    pub modifier: SkinModifier,
    /// Optional cache tag identifying this replacement variant.
    pub tag: u8,
}

/// Result status returned by cache frame rendering (`Lottie::FrameRenderResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRenderResult {
    /// Frame was rendered successfully.
    Ok,
    /// Frame is not yet present in cache.
    NotReady,
    /// Cached dimensions do not match the requested frame size.
    BadCacheSize,
    /// Frame decoding or rendering failed.
    Failed,
}

/// Premultiplied RGBA8888 image buffer produced by the rasterizer and cache.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameBuffer {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Premultiplied RGBA pixels (`width * height * 4` bytes, row-major).
    pub data: Vec<u8>,
}

impl FrameBuffer {
    /// Allocates a transparent premultiplied RGBA frame buffer of `size`.
    #[must_use]
    pub fn new(size: Size) -> Self {
        let len = (size.width as usize)
            .saturating_mul(size.height as usize)
            .saturating_mul(4);
        Self {
            width: size.width,
            height: size.height,
            data: vec![0; len],
        }
    }

    /// Creates a `FrameBuffer` from a `tiny_skia::Pixmap`.
    #[must_use]
    pub fn from_pixmap(pixmap: &Pixmap) -> Self {
        Self {
            width: pixmap.width(),
            height: pixmap.height(),
            data: pixmap.data().to_vec(),
        }
    }

    /// Copies the buffer into a `tiny_skia::Pixmap`, if non-empty and valid.
    #[must_use]
    pub fn to_pixmap(&self) -> Option<Pixmap> {
        let mut pm = Pixmap::new(self.width, self.height)?;
        if pm.data().len() != self.data.len() {
            return None;
        }
        pm.data_mut().copy_from_slice(&self.data);
        Some(pm)
    }

    /// Returns the dimensions of the frame buffer.
    #[must_use]
    pub const fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }

    /// Returns `true` if the buffer has zero width, zero height, or no pixel data.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.data.is_empty()
    }

    /// Returns `true` if this buffer already matches `size` with properly sized storage
    /// (`Lottie::GoodStorageForFrame`).
    #[must_use]
    pub fn is_good_storage_for(&self, size: Size) -> bool {
        !size.is_empty()
            && self.width == size.width
            && self.height == size.height
            && self.data.len() == (size.width as usize) * (size.height as usize) * 4
    }

    /// Resizes and clears the buffer to transparent black if it does not match `size`.
    pub fn ensure_storage(&mut self, size: Size) {
        if !self.is_good_storage_for(size) {
            *self = Self::new(size);
        }
    }

    /// Clears all pixels to transparent black.
    pub fn clear(&mut self) {
        self.data.fill(0);
    }

    /// Returns the premultiplied `[R, G, B, A]` pixel at `(x, y)`, or `None` if out of bounds.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let idx = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        if idx + 4 > self.data.len() {
            return None;
        }
        Some([
            self.data[idx],
            self.data[idx + 1],
            self.data[idx + 2],
            self.data[idx + 3],
        ])
    }

    /// Mirrors the image horizontally in place.
    pub fn mirror_horizontal_in_place(&mut self) {
        let w = self.width as usize;
        let h = self.height as usize;
        if w <= 1 || h == 0 {
            return;
        }
        for y in 0..h {
            let row_start = y * w * 4;
            for x in 0..(w / 2) {
                let left = row_start + x * 4;
                let right = row_start + (w - 1 - x) * 4;
                for c in 0..4 {
                    self.data.swap(left + c, right + c);
                }
            }
        }
    }

    /// Applies tdesktop's `Images::Colored` tinting formula in place to premultiplied RGBA pixels.
    ///
    /// Matches `ui/image/image_prepare.cpp` lines 1226-1241: blends each pixel's premultiplied
    /// RGB toward `add` weighted by `add.a` while preserving the pixel's existing alpha.
    pub fn apply_colored_in_place(&mut self, add: Color) {
        if add.a == 0 {
            return;
        }
        let ca = u32::from(add.a);
        let cr = u32::from(add.r) * (ca + 1);
        let cg = u32::from(add.g) * (ca + 1);
        let cb = u32::from(add.b) * (ca + 1);
        let ra = (0x100 - ca) * 0x100;
        for px in self.data.as_chunks_mut::<4>().0 {
            let a = u32::from(px[3]) + 1;
            px[0] = ((ra * u32::from(px[0]) + a * cr) >> 16).min(255) as u8;
            px[1] = ((ra * u32::from(px[1]) + a * cg) >> 16).min(255) as u8;
            px[2] = ((ra * u32::from(px[2]) + a * cb) >> 16).min(255) as u8;
        }
    }

    /// Scales this frame buffer to `target` using bilinear filtering via `tiny-skia`.
    #[must_use]
    pub fn scaled_to(&self, target: Size) -> Self {
        if target.is_empty() || self.is_empty() {
            return Self::new(target);
        }
        if self.size() == target {
            return self.clone();
        }
        let Some(src) = self.to_pixmap() else {
            return Self::new(target);
        };
        let Some(mut dst) = Pixmap::new(target.width, target.height) else {
            return Self::new(target);
        };
        let sx = target.width as f32 / self.width as f32;
        let sy = target.height as f32 / self.height as f32;
        let paint = PixmapPaint {
            opacity: 1.0,
            blend_mode: tiny_skia::BlendMode::Source,
            quality: FilterQuality::Bilinear,
        };
        dst.draw_pixmap(
            0,
            0,
            src.as_ref(),
            &paint,
            Transform::from_scale(sx, sy),
            None,
        );
        Self::from_pixmap(&dst)
    }

    /// Saves the premultiplied RGBA frame buffer as a PNG file.
    pub fn save_png(&self, path: impl AsRef<std::path::Path>) -> Result<(), Error> {
        let pixmap = self
            .to_pixmap()
            .ok_or_else(|| Error::RenderFailed("empty or invalid frame buffer".into()))?;
        pixmap
            .save_png(path)
            .map_err(|e| Error::RenderFailed(format!("failed to save PNG: {e}")))
    }
}

/// Validates that raw content does not exceed `MAX_FILE_SIZE` (`Lottie::ContentError`).
pub fn content_error(content: &[u8]) -> Result<(), Error> {
    if content.len() > MAX_FILE_SIZE {
        return Err(Error::FileTooLarge {
            size: content.len(),
            max: MAX_FILE_SIZE,
        });
    }
    Ok(())
}

/// Returns `true` if `bytes` begins with the standard gzip magic header (`0x1f, 0x8b`).
#[must_use]
pub fn is_gzip(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

/// Unpacks a gzip-compressed `.tgs` payload with the `MAX_GZIP_FILE_SIZE` safety cap.
///
/// Mirrors `Images::UnpackGzip` in `ui/image/image_prepare.cpp`: if `bytes` is not
/// gzip-compressed, it returns a copy of `bytes` unchanged. If it is gzip-compressed
/// and invalid or exceeds `MAX_GZIP_FILE_SIZE`, returns an error.
pub fn unpack_gzip(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    if !is_gzip(bytes) {
        return Ok(bytes.to_vec());
    }
    let decoder = GzDecoder::new(bytes);
    let limit = (MAX_GZIP_FILE_SIZE + 1) as u64;
    let mut out = Vec::new();
    decoder
        .take(limit)
        .read_to_end(&mut out)
        .map_err(|e| Error::DecompressionFailed(e.to_string()))?;
    if out.len() > MAX_GZIP_FILE_SIZE {
        return Err(Error::FileTooLarge {
            size: out.len(),
            max: MAX_GZIP_FILE_SIZE,
        });
    }
    out.shrink_to_fit();
    Ok(out)
}

/// Strips a leading Unicode BOM (UTF-32BE/LE, UTF-16BE/LE, UTF-8) and validates UTF-8.
///
/// Port of `Lottie::ReadUtf8` in `lottie/lottie_common.cpp`.
pub fn read_utf8(data: &[u8]) -> Result<&str, Error> {
    let skip = if data.len() >= 4 {
        let bom = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if bom == 0xFFFE_0000 || bom == 0x0000_FEFF {
            4
        } else if (bom & 0xFFFF) == 0xFFFE || (bom & 0xFFFF) == 0xFEFF {
            2
        } else if (bom & 0x00FF_FFFF) == 0x00BF_BBEF {
            3
        } else {
            0
        }
    } else if data.len() >= 3 && data[0] == 0xEF && data[1] == 0xBB && data[2] == 0xBF {
        3
    } else {
        0
    };
    std::str::from_utf8(&data[skip..])
        .map_err(|e| Error::ParseFailed(format!("invalid UTF-8: {e}")))
}

/// Checks whether a frame's dimensions, frame rate, and frame count satisfy tdesktop's bounds.
pub fn validate_information(info: Information) -> Result<(), Error> {
    if info.size.is_empty() || info.size.width > MAX_SIZE || info.size.height > MAX_SIZE {
        return Err(Error::NotSupported(format!(
            "invalid animation size {}x{}",
            info.size.width, info.size.height
        )));
    }
    if info.frame_rate == 0 || info.frame_rate > MAX_FRAME_RATE {
        return Err(Error::NotSupported(format!(
            "invalid frame rate {}",
            info.frame_rate
        )));
    }
    if info.frames_count == 0 || info.frames_count > MAX_FRAMES_COUNT {
        return Err(Error::NotSupported(format!(
            "invalid frames count {}",
            info.frames_count
        )));
    }
    Ok(())
}
