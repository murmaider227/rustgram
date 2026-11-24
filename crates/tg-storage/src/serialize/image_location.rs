//! `StorageFileLocation`, `DownloadLocation`, and `ImageLocation` serialization.
//!
//! Port of `Telegram/SourceFiles/ui/image/image_location.h` & `.cpp` and
//! `Serialize::writeImageLocation` / `readImageLocation` in `serialize_peer.cpp`.

use crate::error::{Result, StorageError};
use crate::peer_id::{PeerId, deserialize_peer_id};
use crate::qdatastream::{QDataStreamReader, QDataStreamWriter};

/// Tag written before modern `ImageLocation::serialize()` blobs (`kModernImageLocationTag`).
pub const MODERN_IMAGE_LOCATION_TAG: i32 = i32::MIN;

const NON_STORAGE_LOCATION_TOKEN: u8 = 0x10;
const LEGACY_IN_MESSAGE_PEER_ID_FLAG: u8 = 0x08;
const MODERN_LOCATION_FLAG: u8 = 0x20;
const IN_MESSAGE_FIELDS_FLAG: u8 = 0x40;

/// Storage file location kind (`StorageFileLocation::Type`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StorageFileLocationType {
    /// Legacy volume/local/secret location (`0x00`).
    #[default]
    Legacy = 0x00,
    /// Encrypted secret-chat file (`0x01`).
    Encrypted = 0x01,
    /// Document or document thumbnail (`0x02`).
    Document = 0x02,
    /// Telegram Passport secure file (`0x03`).
    Secure = 0x03,
    /// Takeout location (`0x04`).
    Takeout = 0x04,
    /// Photo size (`0x05`).
    Photo = 0x05,
    /// Peer profile photo (`0x06`).
    PeerPhoto = 0x06,
    /// Sticker set thumbnail (`0x07`).
    StickerSetThumb = 0x07,
    /// Group call stream (`0x08`).
    GroupCallStream = 0x08,
}

impl StorageFileLocationType {
    /// Convert a raw byte into a `StorageFileLocationType`.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x00 => Some(Self::Legacy),
            0x01 => Some(Self::Encrypted),
            0x02 => Some(Self::Document),
            0x03 => Some(Self::Secure),
            0x04 => Some(Self::Takeout),
            0x05 => Some(Self::Photo),
            0x06 => Some(Self::PeerPhoto),
            0x07 => Some(Self::StickerSetThumb),
            0x08 => Some(Self::GroupCallStream),
            _ => None,
        }
    }
}

/// Telegram cloud file location (`StorageFileLocation`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StorageFileLocation {
    /// Datacenter ID.
    pub dc_id: u16,
    /// Location type tag.
    pub location_type: StorageFileLocationType,
    /// Thumbnail size letter (e.g. `b's'`, `b'm'`, `b'a'`, `b'c'`, or `0`).
    pub size_letter: u8,
    /// Local ID (or thumb version for sticker set thumbs).
    pub local_id: i32,
    /// Primary object ID (document/photo/peer/stickerset ID).
    pub id: u64,
    /// Access hash or legacy secret.
    pub access_hash: u64,
    /// Volume ID or photo ID (for peer photos).
    pub volume_id: u64,
    /// In-message peer ID for peer photos inside forwarded/channel messages.
    pub in_message_peer_id: PeerId,
    /// In-message ID for peer photos inside messages.
    pub in_message_id: i32,
    /// MTProto file reference bytes.
    pub file_reference: Vec<u8>,
}

impl StorageFileLocation {
    /// Return whether this location holds valid identifying fields (`StorageFileLocation::valid`).
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        match self.location_type {
            StorageFileLocationType::Legacy => {
                self.dc_id != 0 && self.volume_id != 0 && self.local_id != 0
            }
            StorageFileLocationType::Encrypted
            | StorageFileLocationType::Secure
            | StorageFileLocationType::Document
            | StorageFileLocationType::PeerPhoto
            | StorageFileLocationType::StickerSetThumb => self.dc_id != 0 && self.id != 0,
            StorageFileLocationType::Photo => {
                self.dc_id != 0 && self.id != 0 && self.size_letter != 0
            }
            StorageFileLocationType::Takeout => true,
            StorageFileLocationType::GroupCallStream => {
                self.dc_id != 0 && self.id != 0 && self.volume_id != 0
            }
        }
    }

    /// Serialize to tdesktop's modern `StorageFileLocation::serialize()` byte format.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        if !self.is_valid() {
            return Vec::new();
        }
        let mut type_with_flags = (self.location_type as u8) | MODERN_LOCATION_FLAG;
        let (field1, field2) = if !self.in_message_peer_id.is_zero() {
            type_with_flags |= IN_MESSAGE_FIELDS_FLAG;
            (
                (self.in_message_peer_id.0 >> 32) as u32 as i32,
                (self.in_message_peer_id.0 & 0xFFFF_FFFF) as u32 as i32,
            )
        } else {
            (self.local_id, 0)
        };

        let mut w = QDataStreamWriter::with_capacity(44 + self.file_reference.len());
        w.write_u16(self.dc_id);
        w.write_u8(type_with_flags);
        w.write_u8(self.size_letter);
        w.write_i32(field1);
        w.write_u64(self.id);
        w.write_u64(self.access_hash);
        w.write_u64(self.volume_id);
        w.write_i32(field2);
        w.write_i32(self.in_message_id);
        w.write_byte_array(&self.file_reference);
        w.into_bytes()
    }

    /// Deserialize from `StorageFileLocation::FromSerialized`.
    pub fn deserialize(serialized: &[u8]) -> Result<Self> {
        if serialized.is_empty() {
            return Ok(Self::default());
        }
        let mut r = QDataStreamReader::new(serialized);
        let dc_id = r.read_u16()?;
        let mut type_with_flags = r.read_u8()?;
        if type_with_flags == NON_STORAGE_LOCATION_TOKEN {
            return Err(StorageError::CorruptData(
                "unexpected non-storage location token".into(),
            ));
        }
        let size_letter = r.read_u8()?;
        let field1 = r.read_i32()?;
        let id = r.read_u64()?;
        let access_hash = r.read_u64()?;
        let volume_id = r.read_u64()?;

        let modern = (type_with_flags & MODERN_LOCATION_FLAG) != 0;
        let in_message_fields = (type_with_flags & IN_MESSAGE_FIELDS_FLAG) != 0;
        let mut field2 = 0i32;
        let mut in_message_id = 0i32;

        if modern {
            field2 = r.read_i32()?;
            in_message_id = r.read_i32()?;
            type_with_flags &= !MODERN_LOCATION_FLAG;
            if in_message_fields {
                type_with_flags &= !IN_MESSAGE_FIELDS_FLAG;
            }
        } else if (type_with_flags & LEGACY_IN_MESSAGE_PEER_ID_FLAG) != 0 {
            type_with_flags &= !LEGACY_IN_MESSAGE_PEER_ID_FLAG;
            field2 = r.read_i32()?;
            in_message_id = r.read_i32()?;
        }

        let file_reference = r.read_byte_array()?;
        let location_type = StorageFileLocationType::from_u8(type_with_flags).ok_or_else(|| {
            StorageError::CorruptData(format!(
                "unknown StorageFileLocation type {type_with_flags}"
            ))
        })?;

        let (resolved_id, local_id, in_message_peer_id) = if modern {
            if in_message_fields {
                let raw_peer = ((field1 as u32 as u64) << 32) | (field2 as u32 as u64);
                (id, 0, PeerId(raw_peer))
            } else {
                (id, field1, PeerId::ZERO)
            }
        } else {
            let resolved_id = if location_type == StorageFileLocationType::PeerPhoto {
                deserialize_peer_id(id).0
            } else {
                id
            };
            let local_id = if location_type == StorageFileLocationType::PeerPhoto {
                0
            } else {
                field1
            };
            let in_msg_peer = if field2 != 0 && location_type == StorageFileLocationType::PeerPhoto
            {
                if field2 > 0 {
                    PeerId::from_user(field2 as u64)
                } else {
                    PeerId::from_channel((-field2) as u64)
                }
            } else {
                PeerId::ZERO
            };
            (resolved_id, local_id, in_msg_peer)
        };

        if location_type == StorageFileLocationType::StickerSetThumb && volume_id != 0 {
            return Err(StorageError::CorruptData(
                "legacy StickerSetThumb without thumb_version".into(),
            ));
        }

        let loc = Self {
            dc_id,
            location_type,
            size_letter,
            local_id,
            id: resolved_id,
            access_hash,
            volume_id,
            in_message_peer_id,
            in_message_id,
            file_reference,
        };
        if !loc.is_valid() {
            return Err(StorageError::CorruptData(
                "invalid StorageFileLocation".into(),
            ));
        }
        Ok(loc)
    }
}

/// Variant download source (`DownloadLocation`).
#[derive(Debug, Clone, PartialEq)]
pub enum DownloadLocation {
    /// Cloud storage file location.
    Storage(StorageFileLocation),
    /// Telegram web document (`NonStorageLocationType::Web = 0`).
    Web {
        /// Web document URL bytes.
        url: Vec<u8>,
        /// Access hash.
        access_hash: u64,
    },
    /// Geo point map thumbnail (`NonStorageLocationType::Geo = 1`).
    Geo {
        /// Latitude.
        lat: f64,
        /// Longitude.
        lon: f64,
        /// Access hash.
        access: u64,
        /// Width.
        width: i32,
        /// Height.
        height: i32,
        /// Zoom level.
        zoom: i32,
        /// Scale factor.
        scale: i32,
    },
    /// Plain HTTP/HTTPS URL (`NonStorageLocationType::Url = 2`).
    Url(String),
    /// In-memory raw bytes (`NonStorageLocationType::Memory = 3`).
    Memory(Vec<u8>),
    /// Audio album cover thumb (`NonStorageLocationType::AudioAlbumThumb = 4`).
    AudioAlbumThumb {
        /// Document ID.
        document_id: u64,
    },
}

impl Default for DownloadLocation {
    fn default() -> Self {
        Self::Storage(StorageFileLocation::default())
    }
}

impl DownloadLocation {
    /// Return whether this download location is valid/non-empty.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Storage(s) => s.is_valid(),
            Self::Web { url, .. } => !url.is_empty(),
            Self::Geo { lat, lon, .. } => {
                lat.is_finite()
                    && lon.is_finite()
                    && (-90.0..=90.0).contains(lat)
                    && (-180.0..=180.0).contains(lon)
            }
            Self::Url(u) => !u.is_empty(),
            Self::Memory(b) => !b.is_empty(),
            Self::AudioAlbumThumb { document_id } => *document_id != 0,
        }
    }

    /// Serialize matching `DownloadLocation::serialize()`.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        if !self.is_valid() {
            return Vec::new();
        }
        if let Self::Storage(s) = self {
            return s.serialize();
        }
        let mut w = QDataStreamWriter::new();
        w.write_u16(0);
        w.write_u8(NON_STORAGE_LOCATION_TOKEN);
        match self {
            Self::Storage(_) => unreachable!(),
            Self::Web { url, access_hash } => {
                w.write_u8(0);
                w.write_byte_array(url);
                w.write_u64(*access_hash);
            }
            Self::Geo {
                lat,
                lon,
                access,
                width,
                height,
                zoom,
                scale,
            } => {
                w.write_u8(1);
                w.write_f64(*lat);
                w.write_f64(*lon);
                w.write_u64(*access);
                w.write_i32(*width);
                w.write_i32(*height);
                w.write_i32(*zoom);
                w.write_i32(*scale);
            }
            Self::Url(url) => {
                w.write_u8(2);
                w.write_byte_array(url.as_bytes());
            }
            Self::Memory(bytes) => {
                w.write_u8(3);
                w.write_byte_array(bytes);
            }
            Self::AudioAlbumThumb { document_id } => {
                w.write_u8(4);
                w.write_u64(*document_id);
            }
        }
        w.into_bytes()
    }

    /// Deserialize matching `DownloadLocation::FromSerialized()`.
    pub fn deserialize(serialized: &[u8]) -> Result<Self> {
        if serialized.is_empty() {
            return Ok(Self::default());
        }
        let mut r = QDataStreamReader::new(serialized);
        let dc_id = r.read_u16()?;
        let token = r.read_u8()?;
        if dc_id != 0 || token != NON_STORAGE_LOCATION_TOKEN {
            return Ok(Self::Storage(StorageFileLocation::deserialize(serialized)?));
        }
        let kind = r.read_u8()?;
        match kind {
            0 => {
                let url = r.read_byte_array()?;
                let access_hash = r.read_u64()?;
                Ok(Self::Web { url, access_hash })
            }
            1 => {
                let lat = r.read_f64()?;
                let lon = r.read_f64()?;
                let access = r.read_u64()?;
                let width = r.read_i32()?;
                let height = r.read_i32()?;
                let zoom = r.read_i32()?;
                let scale = r.read_i32()?;
                Ok(Self::Geo {
                    lat,
                    lon,
                    access,
                    width,
                    height,
                    zoom,
                    scale,
                })
            }
            2 => {
                let utf = r.read_byte_array()?;
                let url = String::from_utf8_lossy(&utf).into_owned();
                Ok(Self::Url(url))
            }
            3 => {
                let bytes = r.read_byte_array()?;
                Ok(Self::Memory(bytes))
            }
            4 => {
                let document_id = r.read_u64()?;
                Ok(Self::AudioAlbumThumb { document_id })
            }
            other => Err(StorageError::CorruptData(format!(
                "unknown NonStorageLocationType {other}"
            ))),
        }
    }
}

/// Image location with pixel dimensions (`ImageLocation`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImageLocation {
    /// Underlying file/download location.
    pub file: DownloadLocation,
    /// Image width in pixels.
    pub width: i32,
    /// Image height in pixels.
    pub height: i32,
}

impl ImageLocation {
    /// Serialize inner bytes matching `ImageLocation::serialize()`.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = self.file.serialize();
        if !bytes.is_empty() || self.width > 0 || self.height > 0 {
            bytes.extend_from_slice(&self.width.to_be_bytes());
            bytes.extend_from_slice(&self.height.to_be_bytes());
        }
        bytes
    }

    /// Deserialize inner bytes matching `ImageLocation::FromSerialized()`.
    pub fn deserialize(serialized: &[u8]) -> Result<Self> {
        if serialized.is_empty() {
            return Ok(Self::default());
        }
        let file = DownloadLocation::deserialize(serialized)?;
        if serialized.len() < 8 {
            return Err(StorageError::UnexpectedEof);
        }
        let tail = &serialized[serialized.len() - 8..];
        let width = i32::from_be_bytes([tail[0], tail[1], tail[2], tail[3]]);
        let height = i32::from_be_bytes([tail[4], tail[5], tail[6], tail[7]]);
        Ok(Self {
            file,
            width,
            height,
        })
    }

    /// Write to a stream with `kModernImageLocationTag` (`Serialize::writeImageLocation`).
    pub fn write_to_stream(&self, w: &mut QDataStreamWriter) {
        w.write_i32(MODERN_IMAGE_LOCATION_TAG);
        w.write_byte_array(&self.serialize());
    }

    /// Read from a stream supporting both modern and legacy formats (`Serialize::readImageLocation`).
    pub fn read_from_stream(
        stream_app_version: i32,
        r: &mut QDataStreamReader<'_>,
    ) -> Result<Self> {
        let width = r.read_i32()?;
        if width == MODERN_IMAGE_LOCATION_TAG {
            let serialized = r.read_byte_array()?;
            return Self::deserialize(&serialized);
        }
        // Legacy StorageImageLocation
        let height = r.read_i32()?;
        let dc = r.read_i32()?;
        let volume = r.read_u64()?;
        let local = r.read_i32()?;
        let secret = r.read_u64()?;
        let file_reference = if stream_app_version >= 1_003_013 {
            r.read_byte_array()?
        } else {
            Vec::new()
        };
        Ok(Self {
            file: DownloadLocation::Storage(StorageFileLocation {
                dc_id: dc as u16,
                location_type: StorageFileLocationType::Legacy,
                size_letter: 0,
                local_id: local,
                id: 0,
                access_hash: secret,
                volume_id: volume,
                in_message_peer_id: PeerId::ZERO,
                in_message_id: 0,
                file_reference,
            }),
            width,
            height,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tdf::APP_VERSION;

    #[test]
    fn image_location_storage_and_web_roundtrip() {
        let loc = ImageLocation {
            file: DownloadLocation::Storage(StorageFileLocation {
                dc_id: 2,
                location_type: StorageFileLocationType::Document,
                size_letter: b's',
                local_id: 0,
                id: 999_888_777,
                access_hash: 0xDEAD_BEEF_CAFE_BABE,
                volume_id: 0,
                in_message_peer_id: PeerId::ZERO,
                in_message_id: 0,
                file_reference: vec![1, 2, 3, 4],
            }),
            width: 128,
            height: 128,
        };

        let mut w = QDataStreamWriter::new();
        loc.write_to_stream(&mut w);
        let bytes = w.into_bytes();

        let mut r = QDataStreamReader::new(&bytes);
        let decoded = ImageLocation::read_from_stream(APP_VERSION, &mut r).expect("read");
        assert_eq!(decoded, loc);
        assert!(r.at_end());

        // Empty location roundtrip
        let empty = ImageLocation::default();
        let mut w2 = QDataStreamWriter::new();
        empty.write_to_stream(&mut w2);
        let mut r2 = QDataStreamReader::new(w2.as_bytes());
        assert_eq!(
            ImageLocation::read_from_stream(APP_VERSION, &mut r2).expect("read empty"),
            empty
        );
    }
}
