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
        }
    }

    Ok(VectorPath { commands })
}
