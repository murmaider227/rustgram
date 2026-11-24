//! Document and sticker serialization.
//!
//! Port of `Telegram/SourceFiles/storage/serialize_document.h` and `serialize_document.cpp`.

use crate::error::{Result, StorageError};
use crate::qdatastream::{QDataStreamReader, QDataStreamWriter};
use crate::serialize::image_location::ImageLocation;

/// Version tag marking versioned document streams (`kVersionTag = 0x7FFFFFFF`).
pub const DOCUMENT_VERSION_TAG: i32 = 0x7FFF_FFFF;

/// Current document serialization format version (`kVersion = 6`).
pub const DOCUMENT_SERIALIZE_VERSION: i32 = 6;

/// Document type codes (`DocumentType` in `data/data_types.h`).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocumentType {
    /// Generic file (`0`).
    #[default]
    File = 0,
    /// Video (`1`).
    Video = 1,
    /// Audio/song (`2`).
    Song = 2,
    /// Sticker / mask / custom emoji (`3`).
    Sticker = 3,
    /// Animated GIF/MP4 (`4`).
    Animated = 4,
    /// Voice note (`5`).
    Voice = 5,
    /// Round video note (`6`).
    RoundVideo = 6,
    /// Wallpaper document (`7`).
    WallPaper = 7,
}

impl DocumentType {
    /// Convert raw `i32` to `DocumentType`.
    #[must_use]
    pub const fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::File),
            1 => Some(Self::Video),
            2 => Some(Self::Song),
            3 => Some(Self::Sticker),
            4 => Some(Self::Animated),
            5 => Some(Self::Voice),
            6 => Some(Self::RoundVideo),
            7 => Some(Self::WallPaper),
            _ => None,
        }
    }
}

/// Sticker set reference type stored inside serialized sticker documents (`StickerSetType`).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StickerSetType {
    /// Empty / standalone (`0`).
    #[default]
    Empty = 0,
    /// Standard sticker set referenced by ID (`1`).
    Id = 1,
    /// Legacy short-name reference (`2`).
    ShortName = 2,
    /// Custom emoji set (`3`).
    Emoji = 3,
    /// Masks set (`4`).
    Masks = 4,
}

impl StickerSetType {
    /// Convert raw `i32` to `StickerSetType`.
    #[must_use]
    pub const fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::Id,
            2 => Self::ShortName,
            3 => Self::Emoji,
            4 => Self::Masks,
            _ => Self::Empty,
        }
    }
}

/// Serialized document record (`Serialize::Document`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SerializedDocument {
    /// Document ID.
    pub id: u64,
    /// Access hash.
    pub access_hash: u64,
    /// Creation unix timestamp.
    pub date: i32,
    /// File reference bytes.
    pub file_reference: Vec<u8>,
    /// File name.
    pub filename: String,
    /// MIME type string.
    pub mime_type: String,
    /// Datacenter ID.
    pub dc_id: i32,
    /// File size in bytes.
    pub size: u32,
    /// Media width in pixels.
    pub width: i32,
    /// Media height in pixels.
    pub height: i32,
    /// Document kind.
    pub doc_type: DocumentType,
    /// Sticker alt emoji (only serialized when `doc_type == DocumentType::Sticker`).
    pub sticker_alt: String,
    /// Sticker set type (only serialized when `doc_type == DocumentType::Sticker`).
    pub sticker_set_type: StickerSetType,
    /// Duration in milliseconds (`-1` when absent).
    pub duration_ms: i64,
    /// Whether this sticker/custom emoji requires Telegram Premium (stickers only).
    pub is_premium_sticker: bool,
    /// Whether this custom emoji adapts to text color (stickers only).
    pub emoji_uses_text_color: bool,
    /// Thumbnail image location.
    pub thumbnail_location: ImageLocation,
    /// Thumbnail byte size.
    pub thumbnail_byte_size: i32,
    /// Video thumbnail location.
    pub video_thumbnail_location: ImageLocation,
    /// Video thumbnail byte size.
    pub video_thumbnail_byte_size: i32,
    /// Whether inline thumbnail bytes are an SVG path rather than JPEG bytes.
    pub inline_thumbnail_is_path: bool,
    /// Inline stripped thumbnail bytes.
    pub inline_thumbnail_bytes: Vec<u8>,
}

impl SerializedDocument {
    /// Write document in tdesktop v6 format (`Serialize::Document::writeToStream`).
    pub fn write_to_stream(&self, w: &mut QDataStreamWriter) {
        w.write_u64(self.id);
        w.write_u64(self.access_hash);
        w.write_i32(self.date);
        w.write_byte_array(&self.file_reference);
        w.write_i32(DOCUMENT_VERSION_TAG);
        w.write_i32(DOCUMENT_SERIALIZE_VERSION);
        w.write_qstring(&self.filename);
        w.write_qstring(&self.mime_type);
        w.write_i32(self.dc_id);
        w.write_i32(self.size as i32);
        w.write_i32(self.width);
        w.write_i32(self.height);
        w.write_i32(self.doc_type as i32);

        if self.doc_type == DocumentType::Sticker {
            w.write_qstring(&self.sticker_alt);
            w.write_i32(self.sticker_set_type as i32);
        }

        w.write_i64(self.duration_ms);

        if self.doc_type == DocumentType::Sticker {
            w.write_i32(if self.is_premium_sticker { 1 } else { 0 });
            w.write_i32(if self.emoji_uses_text_color { 1 } else { 0 });
        }

        self.thumbnail_location.write_to_stream(w);
        w.write_i32(self.thumbnail_byte_size);
        self.video_thumbnail_location.write_to_stream(w);
        w.write_i32(self.video_thumbnail_byte_size);
        w.write_i32(if self.inline_thumbnail_is_path { 1 } else { 0 });
        w.write_byte_array(&self.inline_thumbnail_bytes);
    }

    /// Read document from stream supporting versions 0..=6 (`Serialize::Document::readFromStreamHelper`).
    pub fn read_from_stream(
        stream_app_version: i32,
        r: &mut QDataStreamReader<'_>,
    ) -> Result<Self> {
        let id = r.read_u64()?;
        let access_hash = r.read_u64()?;
        let date = r.read_i32()?;

        let mut file_reference = Vec::new();
        let mut version = 0i32;
        if stream_app_version >= 9061 {
            if stream_app_version >= 1_003_013 {
                file_reference = r.read_byte_array()?;
            }
            let version_tag = r.read_i32()?;
            if version_tag == DOCUMENT_VERSION_TAG {
                version = r.read_i32()?;
            }
        }

        let filename = r.read_qstring()?;
        let mime_type = r.read_qstring()?;
        let dc_id = r.read_i32()?;
        let size = r.read_i32()? as u32;
        let width = r.read_i32()?;
        let height = r.read_i32()?;
        let raw_type = r.read_i32()?;
        let doc_type = DocumentType::from_i32(raw_type)
            .ok_or_else(|| StorageError::CorruptData(format!("unknown DocumentType {raw_type}")))?;

        let mut sticker_alt = String::new();
        let mut sticker_set_type = StickerSetType::Empty;
        let mut duration_ms = -1i64;
        let mut is_premium_sticker = false;
        let mut emoji_uses_text_color = false;

        if doc_type == DocumentType::Sticker {
            sticker_alt = r.read_qstring()?;
            sticker_set_type = StickerSetType::from_i32(r.read_i32()?);
            if version >= 6 {
                duration_ms = r.read_i64()?;
                is_premium_sticker = r.read_i32()? == 1;
                emoji_uses_text_color = r.read_i32()? == 1;
            } else if version >= 3 {
                let old_dur = r.read_i32()?;
                duration_ms = if old_dur < 0 {
                    i64::from(old_dur)
                } else {
                    i64::from(old_dur) * 1000
                };
                if version >= 4 {
                    is_premium_sticker = r.read_i32()? == 1;
                    if version >= 5 {
                        emoji_uses_text_color = r.read_i32()? == 1;
                    }
                }
            }
            if sticker_set_type == StickerSetType::Emoji && version < 5 {
                return Err(StorageError::CorruptData(
                    "custom emoji sticker serialized without useTextColor".into(),
                ));
            }
        } else if version >= 6 {
            duration_ms = r.read_i64()?;
        } else {
            let old_dur = r.read_i32()?;
            duration_ms = if old_dur < 0 {
                i64::from(old_dur)
            } else {
                i64::from(old_dur) * 1000
            };
        }

        let thumbnail_location = ImageLocation::read_from_stream(stream_app_version, r)?;
        let (
            thumbnail_byte_size,
            video_thumbnail_location,
            video_thumbnail_byte_size,
            inline_thumbnail_is_path,
            inline_thumbnail_bytes,
        ) = if version >= 1 {
            let thumb_bytes = r.read_i32()?;
            let video_thumb = ImageLocation::read_from_stream(stream_app_version, r)?;
            let video_bytes = r.read_i32()?;
            let (is_path, inline_bytes) = if version >= 2 {
                (r.read_i32()? == 1, r.read_byte_array()?)
            } else {
                (false, Vec::new())
            };
            (thumb_bytes, video_thumb, video_bytes, is_path, inline_bytes)
        } else {
            (0, ImageLocation::default(), 0, false, Vec::new())
        };

        if dc_id == 0 && access_hash == 0 {
            return Err(StorageError::CorruptData(
                "document has zero dc_id and zero access_hash".into(),
            ));
        }

        Ok(Self {
            id,
            access_hash,
            date,
            file_reference,
            filename,
            mime_type,
            dc_id,
            size,
            width,
            height,
            doc_type,
            sticker_alt,
            sticker_set_type,
            duration_ms,
            is_premium_sticker,
            emoji_uses_text_color,
            thumbnail_location,
            thumbnail_byte_size,
            video_thumbnail_location,
            video_thumbnail_byte_size,
            inline_thumbnail_is_path,
            inline_thumbnail_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serialize::image_location::{
        DownloadLocation, StorageFileLocation, StorageFileLocationType,
    };
    use crate::tdf::APP_VERSION;

    #[test]
    fn sticker_and_gif_document_roundtrip() {
        let sticker = SerializedDocument {
            id: 555_000_111,
            access_hash: 0x0102_0304_0506_0708,
            date: 1_700_000_000,
            file_reference: vec![0xAA, 0xBB],
            filename: "sticker.webp".into(),
            mime_type: "image/webp".into(),
            dc_id: 2,
            size: 32_768,
            width: 512,
            height: 512,
            doc_type: DocumentType::Sticker,
            sticker_alt: "🔥".into(),
            sticker_set_type: StickerSetType::Id,
            duration_ms: -1,
            is_premium_sticker: false,
            emoji_uses_text_color: false,
            thumbnail_location: ImageLocation {
                file: DownloadLocation::Storage(StorageFileLocation {
                    dc_id: 2,
                    location_type: StorageFileLocationType::Document,
                    size_letter: b's',
                    id: 555_000_111,
                    access_hash: 0x0102_0304_0506_0708,
                    ..Default::default()
                }),
                width: 128,
                height: 128,
            },
            thumbnail_byte_size: 2048,
            video_thumbnail_location: ImageLocation::default(),
            video_thumbnail_byte_size: 0,
            inline_thumbnail_is_path: true,
            inline_thumbnail_bytes: vec![0x01, 0x02, 0x03],
        };

        let mut w = QDataStreamWriter::new();
        sticker.write_to_stream(&mut w);
        let mut r = QDataStreamReader::new(w.as_bytes());
        let decoded = SerializedDocument::read_from_stream(APP_VERSION, &mut r).expect("read");
        assert_eq!(decoded, sticker);
        assert!(r.at_end());

        let gif = SerializedDocument {
            id: 777_000_222,
            access_hash: 0x9988,
            date: 1_700_000_500,
            file_reference: vec![9, 8, 7],
            filename: "clip.mp4".into(),
            mime_type: "video/mp4".into(),
            dc_id: 4,
            size: 120_000,
            width: 320,
            height: 240,
            doc_type: DocumentType::Animated,
            duration_ms: 3_500,
            ..Default::default()
        };
        let mut wg = QDataStreamWriter::new();
        gif.write_to_stream(&mut wg);
        let mut rg = QDataStreamReader::new(wg.as_bytes());
        assert_eq!(
            SerializedDocument::read_from_stream(APP_VERSION, &mut rg).expect("read gif"),
            gif
        );
    }
}
