//! Port of `Telegram/lib_ui/ui/image/image_prepare.cpp` inline thumbnails:
//! `ExpandInlineBytes`, `FromInlineBytes`, `ExpandPathInlineBytes`, and `PathFromInlineBytes`.

use crate::error::{MediaError, Result};
use image::RgbaImage;

/// Fixed JPEG header prepended to Telegram "stripped" thumbnails (`photoStrippedSize`).
///
/// Exact byte sequence from `Telegram/lib_ui/ui/image/image_prepare.cpp` (`ExpandInlineBytes`).
/// Byte offset 164 receives the thumbnail height (`bytes[1]`) and byte offset 166 receives
/// the thumbnail width (`bytes[2]`).
#[rustfmt::skip]
pub const STRIPPED_JPEG_HEADER: [u8; 623] = [
    0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49,
    0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x28, 0x1c,
    0x1e, 0x23, 0x1e, 0x19, 0x28, 0x23, 0x21, 0x23, 0x2d, 0x2b, 0x28, 0x30, 0x3c, 0x64, 0x41, 0x3c, 0x37, 0x37,
    0x3c, 0x7b, 0x58, 0x5d, 0x49, 0x64, 0x91, 0x80, 0x99, 0x96, 0x8f, 0x80, 0x8c, 0x8a, 0xa0, 0xb4, 0xe6, 0xc3,
    0xa0, 0xaa, 0xda, 0xad, 0x8a, 0x8c, 0xc8, 0xff, 0xcb, 0xda, 0xee, 0xf5, 0xff, 0xff, 0xff, 0x9b, 0xc1, 0xff,
    0xff, 0xff, 0xfa, 0xff, 0xe6, 0xfd, 0xff, 0xf8, 0xff, 0xdb, 0x00, 0x43, 0x01, 0x2b, 0x2d, 0x2d, 0x3c, 0x35,
    0x3c, 0x76, 0x41, 0x41, 0x76, 0xf8, 0xa5, 0x8c, 0xa5, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8,
    0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8,
    0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xf8,
    0xf8, 0xf8, 0xf8, 0xf8, 0xf8, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x00, 0x00, 0x00, 0x03, 0x01, 0x22, 0x00,
    0x02, 0x11, 0x01, 0x03, 0x11, 0x01, 0xff, 0xc4, 0x00, 0x1f, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
    0x09, 0x0a, 0x0b, 0xff, 0xc4, 0x00, 0xb5, 0x10, 0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05, 0x05,
    0x04, 0x04, 0x00, 0x00, 0x01, 0x7d, 0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06,
    0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52,
    0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53,
    0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75,
    0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96,
    0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
    0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6,
    0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4,
    0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xff, 0xc4, 0x00, 0x1f, 0x01, 0x00, 0x03, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
    0x09, 0x0a, 0x0b, 0xff, 0xc4, 0x00, 0xb5, 0x11, 0x00, 0x02, 0x01, 0x02, 0x04, 0x04, 0x03, 0x04, 0x07, 0x05,
    0x04, 0x04, 0x00, 0x01, 0x02, 0x77, 0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41,
    0x51, 0x07, 0x61, 0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33,
    0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a,
    0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74,
    0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94,
    0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4,
    0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4,
    0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4,
    0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xff, 0xda, 0x00, 0x0c, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00,
    0x3f, 0x00,
];

/// Fixed JPEG end-of-image footer appended to stripped thumbnails.
pub const STRIPPED_JPEG_FOOTER: [u8; 2] = [0xff, 0xd9];

/// Lookup table for `ExpandPathInlineBytes` (`c >= 192`), exact match to tdesktop/TDLib.
const PATH_LOOKUP_TABLE: &[u8; 64] =
    b"AACAAAAHAAALMAAAQASTAVAAAZaacaaaahaaalmaaaqastava.az0123456789-,";

/// Expands Telegram stripped thumbnail bytes (`photoStrippedSize`) into a complete JPEG buffer.
///
/// Mirrors `Images::ExpandInlineBytes` from `image_prepare.cpp`.
/// Returns `None` if `bytes` has fewer than 3 bytes or does not start with `0x01`.
#[must_use]
pub fn expand_inline_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 3 || bytes[0] != 0x01 {
        return None;
    }
    let mut out = Vec::with_capacity(
        STRIPPED_JPEG_HEADER.len() + (bytes.len() - 3) + STRIPPED_JPEG_FOOTER.len(),
    );
    out.extend_from_slice(&STRIPPED_JPEG_HEADER);
    out[164] = bytes[1];
    out[166] = bytes[2];
    out.extend_from_slice(&bytes[3..]);
    out.extend_from_slice(&STRIPPED_JPEG_FOOTER);
    Some(out)
}

/// Decodes a Telegram stripped thumbnail (`photoStrippedSize`) into an RGBA image.
///
/// Mirrors `Images::FromInlineBytes` from `image_prepare.cpp`.
pub fn from_inline_bytes(bytes: &[u8]) -> Result<RgbaImage> {
    let expanded = expand_inline_bytes(bytes).ok_or_else(|| {
        MediaError::InvalidInlineData("stripped thumbnail missing 0x01 header".into())
    })?;
    let read = super::read_image(super::ReadArgs {
        content: expanded,
        ..Default::default()
    })?;
    Ok(read.image)
}

/// Offsets of the tables inside [`STRIPPED_JPEG_HEADER`] (segment payload starts).
const HEADER_DQT_LUMA: usize = 25;
const HEADER_DQT_CHROMA: usize = 94;
const HEADER_DHT_DC_LUMA: usize = 182;
const HEADER_DHT_AC_LUMA: usize = 215;
const HEADER_DHT_DC_CHROMA: usize = 398;
const HEADER_DHT_AC_CHROMA: usize = 431;

/// JPEG zig-zag scan order (index = zig-zag position, value = natural row-major index).
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Canonical Huffman code table built from a DHT segment (`code`, `length`) per symbol.
struct HuffTable {
    codes: [(u16, u8); 256],
}

impl HuffTable {
    fn from_header(offset: usize) -> Self {
        let bits = &STRIPPED_JPEG_HEADER[offset..offset + 16];
        let mut values = &STRIPPED_JPEG_HEADER[offset + 16..];
        let mut codes = [(0u16, 0u8); 256];
        let mut code: u16 = 0;
        for (len_minus_one, &count) in bits.iter().enumerate() {
            for _ in 0..count {
                if let Some((&symbol, rest)) = values.split_first() {
                    codes[usize::from(symbol)] = (code, (len_minus_one + 1) as u8);
                    values = rest;
                }
                code = code.wrapping_add(1);
            }
            code <<= 1;
        }
        Self { codes }
    }
}

/// Bit writer with JPEG `0xFF` byte stuffing.
#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    used: u8,
}

impl BitWriter {
    fn put(&mut self, value: u16, len: u8) {
        for i in (0..len).rev() {
            self.acc = (self.acc << 1) | u32::from((value >> i) & 1);
            self.used += 1;
            if self.used == 8 {
                let byte = (self.acc & 0xFF) as u8;
                self.out.push(byte);
                if byte == 0xFF {
                    self.out.push(0x00);
                }
                self.acc = 0;
                self.used = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        while self.used != 0 {
            self.put(1, 1);
        }
        self.out
    }
}

/// Magnitude category and the low bits of a coefficient (JPEG "SSSS" + additional bits).
fn coefficient_bits(value: i32) -> (u8, u16) {
    let magnitude = value.unsigned_abs();
    let size = (32 - magnitude.leading_zeros()) as u8;
    let bits = if value < 0 {
        (value - 1) as u32 & ((1u32 << size) - 1)
    } else {
        value as u32
    };
    (size, bits as u16)
}

fn encode_block(
    writer: &mut BitWriter,
    block: &[f32; 64],
    quant_offset: usize,
    dc: &HuffTable,
    ac: &HuffTable,
    prev_dc: &mut i32,
) {
    let mut coeffs = [0i32; 64];
    for v in 0..8 {
        for u in 0..8 {
            let cu = if u == 0 {
                std::f32::consts::FRAC_1_SQRT_2
            } else {
                1.0
            };
            let cv = if v == 0 {
                std::f32::consts::FRAC_1_SQRT_2
            } else {
                1.0
            };
            let mut sum = 0.0f32;
            for y in 0..8 {
                for x in 0..8 {
                    sum += block[y * 8 + x]
                        * (((2 * x + 1) as f32 * u as f32 * std::f32::consts::PI) / 16.0).cos()
                        * (((2 * y + 1) as f32 * v as f32 * std::f32::consts::PI) / 16.0).cos();
                }
            }
            coeffs[v * 8 + u] = (0.25 * cu * cv * sum) as i32;
        }
    }
    let mut zz = [0i32; 64];
    for (pos, &natural) in ZIGZAG.iter().enumerate() {
        let q = f32::from(STRIPPED_JPEG_HEADER[quant_offset + pos]);
        zz[pos] = (coeffs[natural] as f32 / q).round() as i32;
    }
    let diff = zz[0] - *prev_dc;
    *prev_dc = zz[0];
    let (size, bits) = coefficient_bits(diff);
    let (code, len) = dc.codes[usize::from(size)];
    writer.put(code, len);
    writer.put(bits, size);
    let mut run = 0u8;
    for &value in &zz[1..] {
        if value == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            let (code, len) = ac.codes[0xF0];
            writer.put(code, len);
            run -= 16;
        }
        let (size, bits) = coefficient_bits(value);
        let (code, len) = ac.codes[usize::from((run << 4) | size)];
        writer.put(code, len);
        writer.put(bits, size);
        run = 0;
    }
    if run > 0 {
        let (code, len) = ac.codes[0x00];
        writer.put(code, len);
    }
}

/// Encodes `image` as Telegram stripped thumbnail bytes (`photoStrippedSize`), the inverse of
/// [`from_inline_bytes`]: a baseline JPEG scan using the quantization and Huffman tables of
/// [`STRIPPED_JPEG_HEADER`] (Y 2x2, Cb/Cr 1x1), prefixed with `[0x01, height, width]`.
///
/// Telegram servers produce these bytes; the client only decodes them. This encoder exists for
/// demo and test data. Images larger than 255 px are cropped to 255 px per side.
#[must_use]
pub fn encode_stripped_thumb(image: &RgbaImage) -> Vec<u8> {
    let w = image.width().clamp(1, 255);
    let h = image.height().clamp(1, 255);
    let pixel = |x: u32, y: u32| -> [f32; 3] {
        let p = image.get_pixel(x.min(w - 1), y.min(h - 1)).0;
        let (r, g, b) = (f32::from(p[0]), f32::from(p[1]), f32::from(p[2]));
        [
            0.299 * r + 0.587 * g + 0.114 * b,
            -0.168_736 * r - 0.331_264 * g + 0.5 * b + 128.0,
            0.5 * r - 0.418_688 * g - 0.081_312 * b + 128.0,
        ]
    };
    let dc_luma = HuffTable::from_header(HEADER_DHT_DC_LUMA);
    let ac_luma = HuffTable::from_header(HEADER_DHT_AC_LUMA);
    let dc_chroma = HuffTable::from_header(HEADER_DHT_DC_CHROMA);
    let ac_chroma = HuffTable::from_header(HEADER_DHT_AC_CHROMA);
    let mut writer = BitWriter::default();
    let mut prev = [0i32; 3];
    for mcu_y in (0..h).step_by(16) {
        for mcu_x in (0..w).step_by(16) {
            for (bx, by) in [(0, 0), (8, 0), (0, 8), (8, 8)] {
                let mut block = [0.0f32; 64];
                for y in 0..8 {
                    for x in 0..8 {
                        block[y * 8 + x] =
                            pixel(mcu_x + bx + x as u32, mcu_y + by + y as u32)[0] - 128.0;
                    }
                }
                encode_block(
                    &mut writer,
                    &block,
                    HEADER_DQT_LUMA,
                    &dc_luma,
                    &ac_luma,
                    &mut prev[0],
                );
            }
            for (component, prev_dc) in prev.iter_mut().enumerate().skip(1) {
                let mut block = [0.0f32; 64];
                for y in 0..8u32 {
                    for x in 0..8u32 {
                        let mut sum = 0.0f32;
                        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                            sum += pixel(mcu_x + x * 2 + dx, mcu_y + y * 2 + dy)[component];
                        }
                        block[(y * 8 + x) as usize] = sum / 4.0 - 128.0;
                    }
                }
                encode_block(
                    &mut writer,
                    &block,
                    HEADER_DQT_CHROMA,
                    &dc_chroma,
                    &ac_chroma,
                    prev_dc,
                );
            }
        }
    }
    let mut out = vec![0x01, h as u8, w as u8];
    out.extend(writer.finish());
    out
}

/// Expands Telegram compressed vector path bytes (`photoPathSize`) into an ASCII SVG path string.
///
/// Mirrors `Images::ExpandPathInlineBytes` from `image_prepare.cpp`.
#[must_use]
pub fn expand_path_inline_bytes(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(3 * (bytes.len() + 1));
    result.push('M');
    for &c in bytes {
        if c >= 128 + 64 {
            let idx = usize::from(c - 128 - 64);
            result.push(PATH_LOOKUP_TABLE[idx] as char);
        } else {
            if c >= 128 {
                result.push(',');
            } else if c >= 64 {
                result.push('-');
            }
            let num = c & 63;
            // Format 0..=63 without allocation.
            if num >= 10 {
                result.push((b'0' + (num / 10)) as char);
            }
            result.push((b'0' + (num % 10)) as char);
        }
    }
    result.push('z');
    result
}

/// A single drawing command emitted by [`path_from_inline_bytes`], matching `QPainterPath` calls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathCommand {
    /// Move current point to `(x, y)` (starts a new subpath).
    MoveTo {
        /// Target X coordinate.
        x: f64,
        /// Target Y coordinate.
        y: f64,
    },
    /// Straight line from current point to `(x, y)`.
    LineTo {
        /// Target X coordinate.
        x: f64,
        /// Target Y coordinate.
        y: f64,
    },
    /// Cubic Bézier curve from current point to `(x, y)` via control points `(cx1, cy1)` and `(cx2, cy2)`.
    CubicTo {
        /// First control point X.
        cx1: f64,
        /// First control point Y.
        cy1: f64,
        /// Second control point X.
        cx2: f64,
        /// Second control point Y.
        cy2: f64,
        /// End point X.
        x: f64,
        /// End point Y.
        y: f64,
    },
}

/// Decoded vector outline path for sticker thumbnails (`photoPathSize`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VectorPath {
    /// Sequence of absolute path commands matching `QPainterPath` operations in tdesktop.
    pub commands: Vec<PathCommand>,
}

impl VectorPath {
    /// Returns `true` if the path contains no commands.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Formats the decoded commands as a standard SVG `d` attribute string.
    #[must_use]
    pub fn to_svg_d(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for (idx, cmd) in self.commands.iter().enumerate() {
            if idx > 0 {
                out.push(' ');
            }
            match *cmd {
                PathCommand::MoveTo { x, y } => {
                    let _ = write!(out, "M{x},{y}");
                }
                PathCommand::LineTo { x, y } => {
                    let _ = write!(out, "L{x},{y}");
                }
                PathCommand::CubicTo {
                    cx1,
                    cy1,
                    cx2,
                    cy2,
                    x,
                    y,
                } => {
                    let _ = write!(out, "C{cx1},{cy1},{cx2},{cy2},{x},{y}");
                }
            }
        }
        out
    }
}

/// Parses Telegram compressed vector path bytes (`photoPathSize`) into drawing commands.
///
/// Mirrors `Images::PathFromInlineBytes` from `image_prepare.cpp` exactly, including
/// `m`/`M`, `l`/`L`/`h`/`H`/`v`/`V`, `c`/`C`/`s`/`S`, `z`/`Z`, implicit close-line emission,
/// and returning an empty path / error on unclosed or invalid commands.
pub fn path_from_inline_bytes(bytes: &[u8]) -> Result<VectorPath> {
    if bytes.is_empty() {
        return Ok(VectorPath::default());
    }
    let expanded = expand_path_inline_bytes(bytes);
    parse_svg_path(&expanded)
}

/// Parses an expanded SVG-like path string using tdesktop's exact state machine.
pub fn parse_svg_path(expanded: &str) -> Result<VectorPath> {
    let path = expanded.as_bytes();
    let mut pos: usize = 0;

    let peek = |p: usize| -> u8 { path.get(p).copied().unwrap_or(0) };
    let is_alpha = |c: u8| -> bool {
        let low = c | 0x20;
        low.is_ascii_lowercase()
    };
    let is_digit = |c: u8| -> bool { c.is_ascii_digit() };

    let skip_commas = |pos: &mut usize| {
        while peek(*pos) == b',' {
            *pos += 1;
        }
    };

    let get_number = |pos: &mut usize| -> f64 {
        skip_commas(pos);
        let mut sign = 1.0_f64;
        if peek(*pos) == b'-' {
            sign = -1.0;
            *pos += 1;
        }
        let mut res = 0.0_f64;
        while is_digit(peek(*pos)) {
            res = res * 10.0 + f64::from(peek(*pos) - b'0');
            *pos += 1;
        }
        if peek(*pos) == b'.' {
            *pos += 1;
            let mut mul = 0.1_f64;
            while is_digit(peek(*pos)) {
                res += f64::from(peek(*pos) - b'0') * mul;
                mul *= 0.1;
                *pos += 1;
            }
        }
        sign * res
    };

    let mut commands = Vec::new();
    let mut x = 0.0_f64;
    let mut y = 0.0_f64;

    while peek(pos) != 0 {
        skip_commas(&mut pos);
        if peek(pos) == 0 {
            break;
        }

        while peek(pos) == b'm' || peek(pos) == b'M' {
            let cmd = peek(pos);
            pos += 1;
            loop {
                let coords_start = pos;
                if cmd == b'm' {
                    x += get_number(&mut pos);
                    y += get_number(&mut pos);
                } else {
                    x = get_number(&mut pos);
                    y = get_number(&mut pos);
                }
                skip_commas(&mut pos);
                let next = peek(pos);
                if next == 0 || is_alpha(next) {
                    break;
                }
                if pos == coords_start {
                    return Err(stuck_at(expanded, pos));
                }
            }
        }

        let x_start = x;
        let y_start = y;
        commands.push(PathCommand::MoveTo {
            x: x_start,
            y: y_start,
        });
        let mut have_last_end_control_point = false;
        let mut x_last_end_control_point = 0.0_f64;
        let mut y_last_end_control_point = 0.0_f64;
        let mut is_closed = false;
        let mut command = b'-';

        while !is_closed {
            skip_commas(&mut pos);
            let iteration_start = pos;
            let ch = peek(pos);
            if ch == 0 {
                return Err(MediaError::InvalidInlineData(format!(
                    "unclosed SVG path: {expanded}"
                )));
            }
            if is_alpha(ch) {
                command = ch;
                pos += 1;
            }
            match command {
                b'l' | b'L' | b'h' | b'H' | b'v' | b'V' => {
                    if command == b'l' || command == b'h' {
                        x += get_number(&mut pos);
                    } else if command == b'L' || command == b'H' {
                        x = get_number(&mut pos);
                    }
                    if command == b'l' || command == b'v' {
                        y += get_number(&mut pos);
                    } else if command == b'L' || command == b'V' {
                        y = get_number(&mut pos);
                    }
                    commands.push(PathCommand::LineTo { x, y });
                    have_last_end_control_point = false;
                }
                b'C' | b'c' | b'S' | b's' => {
                    let (mut cx1, mut cy1);
                    if command == b'S' || command == b's' {
                        if have_last_end_control_point {
                            cx1 = 2.0 * x - x_last_end_control_point;
                            cy1 = 2.0 * y - y_last_end_control_point;
                        } else {
                            cx1 = x;
                            cy1 = y;
                        }
                    } else {
                        cx1 = get_number(&mut pos);
                        cy1 = get_number(&mut pos);
                        if command == b'c' {
                            cx1 += x;
                            cy1 += y;
                        }
                    }

                    x_last_end_control_point = get_number(&mut pos);
                    y_last_end_control_point = get_number(&mut pos);
                    if command == b'c' || command == b's' {
                        x_last_end_control_point += x;
                        y_last_end_control_point += y;
                    }
                    have_last_end_control_point = true;

                    if command == b'c' || command == b's' {
                        x += get_number(&mut pos);
                        y += get_number(&mut pos);
                    } else {
                        x = get_number(&mut pos);
                        y = get_number(&mut pos);
                    }
                    commands.push(PathCommand::CubicTo {
                        cx1,
                        cy1,
                        cx2: x_last_end_control_point,
                        cy2: y_last_end_control_point,
                        x,
                        y,
                    });
                }
                b'm' | b'M' => {
                    pos = pos.saturating_sub(1);
                    if x != x_start || y != y_start {
                        x = x_start;
                        y = y_start;
                        commands.push(PathCommand::LineTo { x, y });
                    }
                    is_closed = true;
                }
                b'z' | b'Z' => {
                    if x != x_start || y != y_start {
                        x = x_start;
                        y = y_start;
                        commands.push(PathCommand::LineTo { x, y });
                    }
                    is_closed = true;
                }
                other => {
                    return Err(MediaError::InvalidInlineData(format!(
                        "invalid SVG path command '{}' at pos {}: {}",
                        other as char, pos, expanded
                    )));
                }
            }
            // A character outside the path alphabet repeats the last command without consuming
            // anything; without this check the loop pushed commands forever (TGB-3085).
            if !is_closed && pos == iteration_start {
                return Err(stuck_at(expanded, pos));
            }
        }
    }

    Ok(VectorPath { commands })
}

fn stuck_at(expanded: &str, pos: usize) -> MediaError {
    MediaError::InvalidInlineData(format!(
        "unexpected character at pos {pos} in SVG path: {expanded}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quadrant_image() -> RgbaImage {
        // Left half red, right half blue; bottom quarter white (orientation + colour probe).
        RgbaImage::from_fn(40, 30, |x, y| {
            if y >= 22 {
                image::Rgba([255, 255, 255, 255])
            } else if x < 20 {
                image::Rgba([220, 30, 30, 255])
            } else {
                image::Rgba([30, 60, 220, 255])
            }
        })
    }

    #[test]
    fn stripped_round_trip_keeps_orientation_and_colours() {
        let source = quadrant_image();
        let bytes = encode_stripped_thumb(&source);
        assert_eq!(&bytes[..3], &[0x01, 30, 40]);
        let decoded = from_inline_bytes(&bytes).expect("stripped thumbnail decodes");
        assert_eq!(decoded.dimensions(), (40, 30));
        let left = decoded.get_pixel(6, 8).0;
        let right = decoded.get_pixel(33, 8).0;
        let bottom = decoded.get_pixel(20, 27).0;
        assert!(
            left[0] > 160 && left[1] < 90 && left[2] < 90,
            "left must stay red (no flip, no green cast): {left:?}"
        );
        assert!(
            right[2] > 160 && right[0] < 90 && right[1] < 120,
            "right must stay blue: {right:?}"
        );
        assert!(
            bottom.iter().take(3).all(|&c| c > 200),
            "bottom band must stay white (no vertical flip): {bottom:?}"
        );
    }

    /// TGB-3085: a character outside the path alphabet used to repeat the last command without
    /// consuming input, so `"M0l%"` pushed `LineTo` until the allocator aborted and `"M0%"`
    /// spun in the move-to loop.
    #[test]
    fn svg_path_with_a_foreign_character_is_an_error_not_a_hang() {
        for input in [
            "M0l%",
            "M0%",
            "M1,2l3,4%",
            "M0,0c1,1,2,2,3,3#",
            "M0,0l,,,%",
            "M0l-%",
        ] {
            assert!(
                matches!(parse_svg_path(input), Err(MediaError::InvalidInlineData(_))),
                "{input:?} must be rejected"
            );
        }
    }

    #[test]
    fn svg_path_parses_valid_paths() {
        let path = parse_svg_path("M1,2l3,4h1v-1z").expect("valid path");
        assert_eq!(
            path.commands,
            vec![
                PathCommand::MoveTo { x: 1.0, y: 2.0 },
                PathCommand::LineTo { x: 4.0, y: 6.0 },
                PathCommand::LineTo { x: 5.0, y: 6.0 },
                PathCommand::LineTo { x: 5.0, y: 5.0 },
                PathCommand::LineTo { x: 1.0, y: 2.0 },
            ]
        );
        let two = parse_svg_path("M0,0l1,0zM5,5l0,1z").expect("two subpaths");
        assert_eq!(
            two.commands
                .iter()
                .filter(|c| matches!(c, PathCommand::MoveTo { .. }))
                .count(),
            2
        );
    }

    /// Every path the inline-bytes expansion can produce still parses (it emits only letters,
    /// digits, `.`, `-` and `,`).
    #[test]
    fn expanded_inline_bytes_still_parse() {
        let bytes: Vec<u8> = (0u8..=255).collect();
        let expanded = expand_path_inline_bytes(&bytes);
        let _ = parse_svg_path(&expanded);
        let sample = [0x80u8 + 12, 0x03, 0x05, 0xC0 + 3, 0x80 + 1, 0x2a];
        let _ = parse_svg_path(&expand_path_inline_bytes(&sample));
    }
}
