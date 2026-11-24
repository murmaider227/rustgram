//! Serialization helpers for peers, documents, image locations, and text tags.
//!
//! Ports:
//! - `Telegram/SourceFiles/storage/serialize_common.h` & `.cpp`
//! - `Telegram/SourceFiles/storage/serialize_peer.h` & `.cpp`
//! - `Telegram/SourceFiles/storage/serialize_document.h` & `.cpp`
//! - `Telegram/SourceFiles/ui/image/image_location.h` & `.cpp`
//! - `Telegram/lib_ui/ui/text/text_entity.cpp` (`SerializeTags` / `DeserializeTags`)

pub mod document;
pub mod image_location;
pub mod peer;

use crate::qdatastream::{QDataStreamReader, QDataStreamWriter};

/// A formatting tag attached to a draft's text (`TextWithTags::Tag`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextTag {
    /// Start offset in UTF-16 code units.
    pub offset: i32,
    /// Tag span length in UTF-16 code units.
    pub length: i32,
    /// Tag identifier string (e.g., `"bold"`, `"italic"`, `"mention://..."`).
    pub id: String,
}

/// Serialize draft text tags (`TextUtilities::SerializeTags`).
#[must_use]
pub fn serialize_tags(tags: &[TextTag]) -> Vec<u8> {
    if tags.is_empty() {
        return Vec::new();
    }
    let mut w = QDataStreamWriter::new();
    w.write_i32(tags.len() as i32);
    for tag in tags {
        w.write_i32(tag.offset);
        w.write_i32(tag.length);
        w.write_qstring(&tag.id);
    }
    w.into_bytes()
}

/// Deserialize draft text tags (`TextUtilities::DeserializeTags`).
///
/// `text_utf16_len` is the length of the associated draft text in UTF-16 code units (`QString::size()`).
#[must_use]
pub fn deserialize_tags(data: &[u8], text_utf16_len: usize) -> Vec<TextTag> {
    if data.is_empty() {
        return Vec::new();
    }
    let mut r = QDataStreamReader::new(data);
    let Ok(count) = r.read_i32() else {
        return Vec::new();
    };
    let text_len_i32 = text_utf16_len as i32;
    if count <= 0 || count > text_len_i32 {
        return Vec::new();
    }
    let mut result = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let (Ok(offset), Ok(length), Ok(id)) = (r.read_i32(), r.read_i32(), r.read_qstring())
        else {
            return result;
        };
        if offset < 0 || length <= 0 || offset.saturating_add(length) > text_len_i32 {
            return result;
        }
        result.push(TextTag { offset, length, id });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_tags_roundtrip() {
        let tags = vec![
            TextTag {
                offset: 0,
                length: 5,
                id: "bold".into(),
            },
            TextTag {
                offset: 6,
                length: 4,
                id: "italic".into(),
            },
        ];
        let encoded = serialize_tags(&tags);
        let decoded = deserialize_tags(&encoded, 12);
        assert_eq!(decoded, tags);
        assert!(serialize_tags(&[]).is_empty());
        assert!(deserialize_tags(&[], 10).is_empty());
    }
}
