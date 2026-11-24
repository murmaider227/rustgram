//! Port of JPEG inspection, progressive detection, and EXIF orientation handling
//! from `Telegram/lib_ui/ui/image/image_prepare.cpp`.

use image::RgbaImage;

/// EXIF Orientation tag (0x0112) values 1..=8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExifOrientation {
    /// 1: Normal (0°).
    #[default]
    Normal = 1,
    /// 2: Mirrored horizontally.
    FlipHorizontal = 2,
    /// 3: Rotated 180°.
    Rotate180 = 3,
    /// 4: Mirrored vertically.
    FlipVertical = 4,
    /// 5: Mirrored horizontally then rotated 270° CW (transposed).
    Transpose = 5,
    /// 6: Rotated 90° CW.
    Rotate90 = 6,
    /// 7: Mirrored horizontally then rotated 90° CW (transversed).
    Transverse = 7,
    /// 8: Rotated 270° CW (90° CCW).
    Rotate270 = 8,
}

impl ExifOrientation {
    /// Converts a raw EXIF orientation tag value (1..=8) to [`ExifOrientation`].
    #[must_use]
    pub const fn from_u16(value: u16) -> Option<Self> {
        match value {
            1 => Some(Self::Normal),
            2 => Some(Self::FlipHorizontal),
            3 => Some(Self::Rotate180),
            4 => Some(Self::FlipVertical),
            5 => Some(Self::Transpose),
            6 => Some(Self::Rotate90),
            7 => Some(Self::Transverse),
            8 => Some(Self::Rotate270),
            _ => None,
        }
    }

    /// Returns `true` if applying this orientation swaps image width and height.
    #[must_use]
    pub const fn swaps_dimensions(self) -> bool {
        matches!(
            self,
            Self::Transpose | Self::Rotate90 | Self::Transverse | Self::Rotate270
        )
    }
}

/// Lightweight metadata extracted from a JPEG header without decoding pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpegFrameInfo {
    /// Coded width in pixels (before EXIF orientation).
    pub width: u16,
    /// Coded height in pixels (before EXIF orientation).
    pub height: u16,
    /// Number of color components (e.g., 1 grayscale, 3 YCbCr).
    pub components: u8,
    /// `true` if the JPEG uses progressive DCT (`SOF2` / `0xC2` or `SOF10` / `0xCA`).
    pub progressive: bool,
    /// Parsed EXIF orientation if an APP1 Exif header with tag `0x0112` was found.
    pub orientation: Option<ExifOrientation>,
}

/// Returns `true` if `bytes` is a progressive JPEG (`SOF2` / `SOF10`).
///
/// Mirrors `Images::IsProgressiveJpeg` from `image_prepare.cpp`.
#[must_use]
pub fn is_progressive_jpeg(bytes: &[u8]) -> bool {
    jpeg_frame_info(bytes).is_some_and(|info| info.progressive)
}

/// Parses JPEG header markers to extract frame dimensions, progressive flag, and EXIF orientation.
#[must_use]
pub fn jpeg_frame_info(bytes: &[u8]) -> Option<JpegFrameInfo> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut pos = 2usize;
    let mut orientation = None;

    while pos < bytes.len() {
        // Skip any fill 0xFF bytes before marker.
        if bytes[pos] != 0xFF {
            return None;
        }
        while pos < bytes.len() && bytes[pos] == 0xFF {
            pos += 1;
        }
        if pos >= bytes.len() {
            return None;
        }
        let marker = bytes[pos];
        pos += 1;

        match marker {
            // Standalone markers without payload.
            0x01 | 0xD0..=0xD8 => continue,
            // EOI or SOS before any SOF means invalid/incomplete header.
            0xD9 | 0xDA => return None,
            _ => {}
        }

        if pos + 2 > bytes.len() {
            return None;
        }
        let seg_len = usize::from(u16::from_be_bytes([bytes[pos], bytes[pos + 1]]));
        if seg_len < 2 || pos + seg_len > bytes.len() {
            return None;
        }
        let seg_data = &bytes[pos + 2..pos + seg_len];

        match marker {
            // APP1: check for EXIF orientation.
            0xE1 => {
                if orientation.is_none() {
                    orientation = parse_exif_app1_orientation(seg_data);
                }
            }
            // Start Of Frame markers (baseline, extended, progressive, lossless, differential, arithmetic).
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                if seg_data.len() < 6 {
                    return None;
                }
                let height = u16::from_be_bytes([seg_data[1], seg_data[2]]);
                let width = u16::from_be_bytes([seg_data[3], seg_data[4]]);
                let components = seg_data[5];
                let progressive = matches!(marker, 0xC2 | 0xCA);
                return Some(JpegFrameInfo {
                    width,
                    height,
                    components,
                    progressive,
                    orientation,
                });
            }
            _ => {}
        }

        pos += seg_len;
    }

    None
}

/// Extracts the EXIF orientation from a JPEG buffer, returning `ExifOrientation::Normal` if absent.
#[must_use]
pub fn read_jpeg_exif_orientation(bytes: &[u8]) -> ExifOrientation {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return ExifOrientation::Normal;
    }
    let mut pos = 2usize;
    while pos < bytes.len() {
        if bytes[pos] != 0xFF {
            break;
        }
        while pos < bytes.len() && bytes[pos] == 0xFF {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        let marker = bytes[pos];
        pos += 1;
        match marker {
            0x01 | 0xD0..=0xD8 => continue,
            0xD9 | 0xDA => break,
            _ => {}
        }
        if pos + 2 > bytes.len() {
            break;
        }
        let seg_len = usize::from(u16::from_be_bytes([bytes[pos], bytes[pos + 1]]));
        if seg_len < 2 || pos + seg_len > bytes.len() {
            break;
        }
        if marker == 0xE1
            && let Some(orient) = parse_exif_app1_orientation(&bytes[pos + 2..pos + seg_len])
        {
            return orient;
        }
        pos += seg_len;
    }
    ExifOrientation::Normal
}

/// Parses an APP1 payload starting with `Exif\0\0` and extracts TIFF IFD0 tag `0x0112`.
#[must_use]
pub fn parse_exif_app1_orientation(app1: &[u8]) -> Option<ExifOrientation> {
    if app1.len() < 14 || &app1[..6] != b"Exif\0\0" {
        return None;
    }
    parse_tiff_orientation(&app1[6..])
}

/// Parses a TIFF header (`II` or `MM`) and scans IFD0 for the Orientation tag (`0x0112`).
#[must_use]
pub fn parse_tiff_orientation(tiff: &[u8]) -> Option<ExifOrientation> {
    if tiff.len() < 8 {
        return None;
    }
    let little_endian = match &tiff[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };

    let read_u16 = |offset: usize| -> Option<u16> {
        let slice = tiff.get(offset..offset + 2)?;
        let arr = [slice[0], slice[1]];
        Some(if little_endian {
            u16::from_le_bytes(arr)
        } else {
            u16::from_be_bytes(arr)
        })
    };
    let read_u32 = |offset: usize| -> Option<u32> {
        let slice = tiff.get(offset..offset + 4)?;
        let arr = [slice[0], slice[1], slice[2], slice[3]];
        Some(if little_endian {
            u32::from_le_bytes(arr)
        } else {
            u32::from_be_bytes(arr)
        })
    };

    if read_u16(2)? != 0x002A {
        return None;
    }
    let ifd0_offset = usize::try_from(read_u32(4)?).ok()?;
    let entry_count = usize::from(read_u16(ifd0_offset)?);

    for i in 0..entry_count {
        let entry_off = ifd0_offset
            .checked_add(2)?
            .checked_add(i.checked_mul(12)?)?;
        let tag = read_u16(entry_off)?;
        if tag == 0x0112 {
            // Type 3 = SHORT (u16), count = 1; value is stored inline in the first 2 bytes of the 4-byte value field.
            let field_type = read_u16(entry_off + 2)?;
            let count = read_u32(entry_off + 4)?;
            if field_type == 3 && count >= 1 {
                let value = read_u16(entry_off + 8)?;
                return ExifOrientation::from_u16(value);
            }
            return None;
        }
    }

    None
}

/// Applies an [`ExifOrientation`] transform to an RGBA image.
#[must_use]
pub fn apply_exif_orientation(image: RgbaImage, orientation: ExifOrientation) -> RgbaImage {
    match orientation {
        ExifOrientation::Normal => image,
        ExifOrientation::FlipHorizontal => image::imageops::flip_horizontal(&image),
        ExifOrientation::Rotate180 => image::imageops::rotate180(&image),
        ExifOrientation::FlipVertical => image::imageops::flip_vertical(&image),
        ExifOrientation::Transpose => {
            image::imageops::flip_horizontal(&image::imageops::rotate90(&image))
        }
        ExifOrientation::Rotate90 => image::imageops::rotate90(&image),
        ExifOrientation::Transverse => {
            image::imageops::flip_horizontal(&image::imageops::rotate270(&image))
        }
        ExifOrientation::Rotate270 => image::imageops::rotate270(&image),
    }
}

/// Counts the number of Start-Of-Scan (`SOS` / `0xFF 0xDA`) markers present in a JPEG buffer.
///
/// Useful for verifying progressive JPEG partial scans (a baseline JPEG has 1 scan, while a
/// progressive JPEG has multiple scans).
#[must_use]
pub fn count_jpeg_scans(bytes: &[u8]) -> usize {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return 0;
    }
    let mut scans = 0usize;
    let mut pos = 2usize;
    while pos + 1 < bytes.len() {
        if bytes[pos] != 0xFF {
            pos += 1;
            continue;
        }
        while pos < bytes.len() && bytes[pos] == 0xFF {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        let marker = bytes[pos];
        pos += 1;
        match marker {
            // Stuffed zero byte inside entropy-coded data.
            0x00 | 0x01 | 0xD0..=0xD8 => continue,
            0xD9 => break,
            0xDA => {
                scans += 1;
                if pos + 2 <= bytes.len() {
                    let len = usize::from(u16::from_be_bytes([bytes[pos], bytes[pos + 1]]));
                    if len >= 2 && pos + len <= bytes.len() {
                        pos += len;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            _ => {
                if pos + 2 <= bytes.len() {
                    let len = usize::from(u16::from_be_bytes([bytes[pos], bytes[pos + 1]]));
                    if len >= 2 && pos + len <= bytes.len() {
                        pos += len;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
    }
    scans
}

/// Ensures a truncated progressive JPEG prefix is terminated with an EOI (`0xFF 0xD9`) marker
/// so standard decoders can render the completed progressive scans.
#[must_use]
pub fn finalize_progressive_prefix(prefix: &[u8]) -> Vec<u8> {
    if prefix.len() >= 2 && prefix[prefix.len() - 2] == 0xFF && prefix[prefix.len() - 1] == 0xD9 {
        return prefix.to_vec();
    }
    let mut out = Vec::with_capacity(prefix.len() + 2);
    out.extend_from_slice(prefix);
    // Avoid trailing dangling 0xFF before EOI.
    if out.last() == Some(&0xFF) {
        out.push(0xD9);
    } else {
        out.extend_from_slice(&[0xFF, 0xD9]);
    }
    out
}
