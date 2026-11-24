//! Peer serialization (`UserData`, `ChatData`, `ChannelData`).
//!
//! Port of `Telegram/SourceFiles/storage/serialize_peer.h` and `serialize_peer.cpp`.

use crate::error::{Result, StorageError};
use crate::peer_id::{PeerId, deserialize_peer_id, serialize_peer_id};
use crate::qdatastream::{QDataStreamReader, QDataStreamWriter};
use crate::serialize::image_location::ImageLocation;

/// Version tag marking modern serialized peers (`kVersionTag` in `serialize_peer.cpp`).
pub const PEER_SERIALIZE_VERSION_TAG: u64 = 0x77FF_FFFF_FFFF_FFFF;

/// Current peer serialization version (`kVersion = 3`).
pub const PEER_SERIALIZE_VERSION: i32 = 3;

/// User-specific fields serialized by `Serialize::writePeer`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SerializedUser {
    /// First name.
    pub first_name: String,
    /// Last name.
    pub last_name: String,
    /// Phone number string.
    pub phone: String,
    /// Username without `@`.
    pub username: String,
    /// MTProto access hash.
    pub access_hash: u64,
    /// Raw `UserDataFlags` bitmask.
    pub flags: i32,
    /// Inline bot placeholder string (empty if not an inline bot).
    pub bot_inline_placeholder: String,
    /// Packed `Data::LastseenStatus`.
    pub lastseen: u32,
    /// Contact flag (`1` if contact, `0` otherwise).
    pub is_contact: bool,
    /// Bot info version (`-1` if not a bot).
    pub bot_info_version: i32,
    /// Whether bot supports guest chat (`version > 2`).
    pub supports_guest_chat: bool,
}

/// Basic group chat fields serialized by `Serialize::writePeer`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SerializedChat {
    /// Chat title.
    pub name: String,
    /// Participant count.
    pub count: i32,
    /// Creation date unix timestamp.
    pub date: i32,
    /// Chat version.
    pub version: i32,
    /// Creator bare user ID (48-bit split across `field1`/`field2`).
    pub creator_id: u64,
    /// Raw `ChatDataFlags` bitmask.
    pub flags: u32,
    /// Invite link URL.
    pub invite_link: String,
}

/// Channel/supergroup fields serialized by `Serialize::writePeer`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SerializedChannel {
    /// Channel title.
    pub name: String,
    /// MTProto access hash.
    pub access_hash: u64,
    /// Creation/join date unix timestamp.
    pub date: i32,
    /// Raw `ChannelDataFlags` bitmask.
    pub flags: u32,
    /// Invite link URL.
    pub invite_link: String,
}

/// Kind-specific peer payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerializedPeerKind {
    /// User peer.
    User(SerializedUser),
    /// Basic group chat peer.
    Chat(SerializedChat),
    /// Channel or supergroup peer.
    Channel(SerializedChannel),
}

/// Full serialized peer record (`Serialize::writePeer` / `Serialize::readPeer`).
#[derive(Debug, Clone, PartialEq)]
pub struct SerializedPeer {
    /// Peer ID (encodes user/chat/channel kind + bare ID).
    pub id: PeerId,
    /// Profile photo ID (`0` if none).
    pub userpic_photo_id: u64,
    /// Profile photo location.
    pub userpic_location: ImageLocation,
    /// Whether the profile photo has an animated video avatar.
    pub userpic_has_video: bool,
    /// Type-specific fields.
    pub kind: SerializedPeerKind,
}

impl SerializedPeer {
    /// Write this peer in tdesktop's v3 binary layout (`Serialize::writePeer`).
    pub fn write_to_stream(&self, w: &mut QDataStreamWriter) {
        w.write_u64(serialize_peer_id(self.id));
        w.write_u64(PEER_SERIALIZE_VERSION_TAG);
        w.write_i32(PEER_SERIALIZE_VERSION);
        w.write_u64(self.userpic_photo_id);
        self.userpic_location.write_to_stream(w);
        w.write_i32(if self.userpic_has_video { 1 } else { 0 });

        match &self.kind {
            SerializedPeerKind::User(user) => {
                w.write_qstring(&user.first_name);
                w.write_qstring(&user.last_name);
                w.write_qstring(&user.phone);
                w.write_qstring(&user.username);
                w.write_u64(user.access_hash);
                w.write_i32(user.flags);
                w.write_qstring(&user.bot_inline_placeholder);
                w.write_u32(user.lastseen);
                w.write_i32(if user.is_contact { 1 } else { 0 });
                w.write_i32(user.bot_info_version);
                w.write_i32(if user.supports_guest_chat { 1 } else { 0 });
            }
            SerializedPeerKind::Chat(chat) => {
                let field1 = (chat.creator_id & 0xFFFF_FFFF) as u32 as i32;
                let field2 = (((chat.creator_id >> 32) as u32) << 8) as i32;
                w.write_qstring(&chat.name);
                w.write_i32(chat.count);
                w.write_i32(chat.date);
                w.write_i32(chat.version);
                w.write_i32(field1);
                w.write_i32(field2);
                w.write_u32(chat.flags);
                w.write_qstring(&chat.invite_link);
            }
            SerializedPeerKind::Channel(channel) => {
                w.write_qstring(&channel.name);
                w.write_u64(channel.access_hash);
                w.write_i32(channel.date);
                w.write_i32(0); // legacy version
                w.write_i32(0); // old forbidden
                w.write_u32(channel.flags);
                w.write_qstring(&channel.invite_link);
            }
        }
    }

    /// Read a peer from a stream (`Serialize::readPeer`).
    pub fn read_from_stream(
        stream_app_version: i32,
        r: &mut QDataStreamReader<'_>,
    ) -> Result<Self> {
        let peer_id_serialized = r.read_u64()?;
        let version_tag = r.read_u64()?;
        let id = deserialize_peer_id(peer_id_serialized);
        if id.is_zero() {
            return Err(StorageError::CorruptData("zero peer id in stream".into()));
        }

        let (version, userpic_photo_id) = if version_tag == PEER_SERIALIZE_VERSION_TAG {
            let v = r.read_i32()?;
            let pid = r.read_u64()?;
            (v, pid)
        } else {
            (0, version_tag)
        };

        let userpic_location = ImageLocation::read_from_stream(stream_app_version, r)?;
        let userpic_has_video = if version > 0 {
            r.read_i32()? == 1
        } else {
            false
        };

        let kind = if id.is_user() {
            let first_name = r.read_qstring()?;
            let last_name = r.read_qstring()?;
            let phone = r.read_qstring()?;
            let username = r.read_qstring()?;
            let access_hash = r.read_u64()?;
            let flags = if stream_app_version >= 9012 {
                r.read_i32()?
            } else {
                0
            };
            let bot_inline_placeholder = if stream_app_version >= 9016 {
                r.read_qstring()?
            } else {
                String::new()
            };
            let lastseen = r.read_u32()?;
            let contact = r.read_i32()?;
            let bot_info_version = r.read_i32()?;
            let supports_guest_chat = if version > 2 {
                r.read_i32()? == 1
            } else {
                false
            };

            SerializedPeerKind::User(SerializedUser {
                first_name,
                last_name,
                phone,
                username,
                access_hash,
                flags,
                bot_inline_placeholder,
                lastseen,
                is_contact: contact == 1,
                bot_info_version,
                supports_guest_chat,
            })
        } else if id.is_chat() {
            let name = r.read_qstring()?;
            let count = r.read_i32()?;
            let date = r.read_i32()?;
            let chat_version = r.read_i32()?;
            let field1 = r.read_i32()?;
            let field2 = r.read_i32()?;
            let flags = r.read_u32()?;
            let invite_link = r.read_qstring()?;

            let creator_id = (field1 as u32 as u64) | (((field2 as u32 >> 8) as u64) << 32);
            SerializedPeerKind::Chat(SerializedChat {
                name,
                count,
                date,
                version: chat_version,
                creator_id,
                flags,
                invite_link,
            })
        } else if id.is_channel() {
            let name = r.read_qstring()?;
            let access_hash = r.read_u64()?;
            let date = r.read_i32()?;
            let _legacy_version = r.read_i32()?;
            let _old_forbidden = r.read_i32()?;
            let flags = r.read_u32()?;
            let invite_link = r.read_qstring()?;

            SerializedPeerKind::Channel(SerializedChannel {
                name,
                access_hash,
                date,
                flags,
                invite_link,
            })
        } else {
            return Err(StorageError::CorruptData(format!(
                "unsupported peer kind shift {}",
                id.kind_shift()
            )));
        };

        Ok(Self {
            id,
            userpic_photo_id,
            userpic_location,
            userpic_has_video,
            kind,
        })
    }
}

/// Peek only the phone number from a serialized self-user blob (`Serialize::peekUserPhone`).
pub fn peek_user_phone(stream_app_version: i32, data: &[u8]) -> Result<Option<String>> {
    let mut r = QDataStreamReader::new(data);
    let peer_id_serialized = r.read_u64()?;
    let version_tag = r.read_u64()?;
    let id = deserialize_peer_id(peer_id_serialized);
    if id.is_zero() || !id.is_user() {
        return Ok(None);
    }
    let version = if version_tag == PEER_SERIALIZE_VERSION_TAG {
        let v = r.read_i32()?;
        let _photo_id = r.read_u64()?;
        v
    } else {
        0
    };
    let _userpic = ImageLocation::read_from_stream(stream_app_version, &mut r)?;
    if version > 0 {
        let _has_video = r.read_i32()?;
    }
    let _first = r.read_qstring()?;
    let _last = r.read_qstring()?;
    let phone = r.read_qstring()?;
    Ok(Some(phone))
}

/// Serialize a self-user record plus bio (`about`) as stored under `lskSelfSerialized`.
#[must_use]
pub fn serialize_self_user(peer: &SerializedPeer, about: &str) -> Vec<u8> {
    let mut w = QDataStreamWriter::new();
    peer.write_to_stream(&mut w);
    w.write_qstring(about);
    w.into_bytes()
}

/// Deserialize a `lskSelfSerialized` blob into `(SerializedPeer, about)`.
pub fn deserialize_self_user(
    stream_app_version: i32,
    data: &[u8],
) -> Result<(SerializedPeer, String)> {
    let mut r = QDataStreamReader::new(data);
    let peer = SerializedPeer::read_from_stream(stream_app_version, &mut r)?;
    let about = if r.at_end() {
        String::new()
    } else {
        r.read_qstring()?
    };
    Ok((peer, about))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tdf::APP_VERSION;

    #[test]
    fn peer_user_chat_channel_roundtrip_and_peek() {
        let user = SerializedPeer {
            id: PeerId::from_user(42_000_000_001),
            userpic_photo_id: 987_654,
            userpic_location: ImageLocation::default(),
            userpic_has_video: true,
            kind: SerializedPeerKind::User(SerializedUser {
                first_name: "Alice".into(),
                last_name: "Test".into(),
                phone: "0001234567".into(),
                username: "alice_test".into(),
                access_hash: 0x1234_5678_9ABC_DEF0,
                flags: 0x05,
                bot_inline_placeholder: String::new(),
                lastseen: 12345,
                is_contact: true,
                bot_info_version: -1,
                supports_guest_chat: false,
            }),
        };

        let self_blob = serialize_self_user(&user, "Bio text");
        assert_eq!(
            peek_user_phone(APP_VERSION, &self_blob).expect("peek"),
            Some("0001234567".into())
        );
        let (decoded_user, about) =
            deserialize_self_user(APP_VERSION, &self_blob).expect("deserialize self");
        assert_eq!(decoded_user, user);
        assert_eq!(about, "Bio text");

        let chat = SerializedPeer {
            id: PeerId::from_chat(555_111),
            userpic_photo_id: 0,
            userpic_location: ImageLocation::default(),
            userpic_has_video: false,
            kind: SerializedPeerKind::Chat(SerializedChat {
                name: "Test Group".into(),
                count: 10,
                date: 1_700_000_000,
                version: 3,
                creator_id: 0x0001_0000_1234,
                flags: 2,
                invite_link: "https://t.me/+test".into(),
            }),
        };
        let mut w = QDataStreamWriter::new();
        chat.write_to_stream(&mut w);
        let mut r = QDataStreamReader::new(w.as_bytes());
        assert_eq!(
            SerializedPeer::read_from_stream(APP_VERSION, &mut r).expect("chat"),
            chat
        );

        let chan = SerializedPeer {
            id: PeerId::from_channel(999_222),
            userpic_photo_id: 1,
            userpic_location: ImageLocation::default(),
            userpic_has_video: false,
            kind: SerializedPeerKind::Channel(SerializedChannel {
                name: "News".into(),
                access_hash: 0xABCD,
                date: 1_700_000_100,
                flags: 0x10,
                invite_link: String::new(),
            }),
        };
        let mut wc = QDataStreamWriter::new();
        chan.write_to_stream(&mut wc);
        let mut rc = QDataStreamReader::new(wc.as_bytes());
        assert_eq!(
            SerializedPeer::read_from_stream(APP_VERSION, &mut rc).expect("channel"),
            chan
        );
    }
}
