//! MTProto authorization serialization (user ID, main DC, DC auth keys, keys to destroy).
//!
//! Port of `Main::Account::serializeMtpAuthorization` and `Main::Account::setMtpAuthorization`
//! from `Telegram/SourceFiles/main/main_account.cpp`, and `MTP::AuthKey::write` from
//! `Telegram/SourceFiles/mtproto/mtproto_auth_key.cpp`.

use tg_crypto::{AUTH_KEY_SIZE, AuthKey};

use crate::error::{Result, StorageError};
use crate::qdatastream::{QDataStreamReader, QDataStreamWriter};

/// 64-bit marker indicating the modern wide-ID format (`kWideIdsTag = ~uint64(0)`).
pub const WIDE_IDS_TAG: u64 = u64::MAX;

/// A DC ID paired with its 256-byte MTProto authorization key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MtpAuthKeyEntry {
    /// Datacenter ID (may include tdesktop shift flags such as media/CDN shifts).
    pub dc_id: i32,
    /// 2048-bit MTProto auth key.
    pub key: AuthKey,
}

/// Serialized MTProto authorization state stored under `dbiMtpAuthorization` (`0x4b`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MtpAuthorization {
    /// Authorized user ID (`0` if not signed in).
    pub user_id: u64,
    /// Main datacenter ID (e.g., `2`).
    pub main_dc_id: i32,
    /// Active DC authorization keys.
    pub keys: Vec<MtpAuthKeyEntry>,
    /// Keys scheduled for server-side destruction (`auth.logOut` / stale key cleanup).
    pub keys_to_destroy: Vec<MtpAuthKeyEntry>,
}

impl MtpAuthorization {
    /// Serialize in tdesktop's modern wide-ID format (`Main::Account::serializeMtpAuthorization`).
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let keys_size = |list: &[MtpAuthKeyEntry]| 4 + list.len() * (4 + AUTH_KEY_SIZE);
        let cap = 16 + 4 + keys_size(&self.keys) + keys_size(&self.keys_to_destroy);
        let mut w = QDataStreamWriter::with_capacity(cap);

        w.write_u64(WIDE_IDS_TAG);
        w.write_u64(self.user_id);
        w.write_i32(self.main_dc_id);

        write_keys(&mut w, &self.keys);
        write_keys(&mut w, &self.keys_to_destroy);

        w.into_bytes()
    }

    /// Serialize in tdesktop's legacy 32-bit `user_id` + `main_dc_id` format (pre-64-bit user IDs).
    #[must_use]
    pub fn serialize_legacy(&self) -> Vec<u8> {
        let mut w = QDataStreamWriter::new();
        w.write_i32(self.user_id as i32);
        w.write_i32(self.main_dc_id);
        write_keys(&mut w, &self.keys);
        write_keys(&mut w, &self.keys_to_destroy);
        w.into_bytes()
    }

    /// Deserialize from either the modern wide-ID format or the legacy 32-bit format
    /// (`Main::Account::setMtpAuthorization`).
    pub fn deserialize(bytes: &[u8]) -> Result<Self> {
        let mut r = QDataStreamReader::new(bytes);

        let legacy_user_id = r.read_i32()?;
        let legacy_main_dc_id = r.read_i32()?;

        // Matches C++: ((uint64(legacyUserId) << 32) | uint64(legacyMainDcId)) == kWideIdsTag
        // Note: in C++ `uint64(qint32(-1))` sign-extends to `0xFFFF_FFFF_FFFF_FFFF`, so both
        // `-1` values combine to `WIDE_IDS_TAG`.
        let combined = ((legacy_user_id as u32 as u64) << 32) | (legacy_main_dc_id as u32 as u64);
        let (user_id, main_dc_id) = if combined == WIDE_IDS_TAG {
            let uid = r.read_u64()?;
            let dc = r.read_i32()?;
            (uid, dc)
        } else {
            (legacy_user_id as i64 as u64, legacy_main_dc_id)
        };

        let keys = read_keys(&mut r)?;
        let keys_to_destroy = if r.at_end() {
            Vec::new()
        } else {
            read_keys(&mut r)?
        };

        Ok(Self {
            user_id,
            main_dc_id,
            keys,
            keys_to_destroy,
        })
    }
}

fn write_keys(w: &mut QDataStreamWriter, keys: &[MtpAuthKeyEntry]) {
    w.write_i32(keys.len() as i32);
    for entry in keys {
        w.write_i32(entry.dc_id);
        w.write_raw(entry.key.data());
    }
}

fn read_keys(r: &mut QDataStreamReader<'_>) -> Result<Vec<MtpAuthKeyEntry>> {
    let count = r.read_i32()?;
    if !(0..=10_000).contains(&count) {
        return Err(StorageError::CorruptData(format!(
            "invalid MTP key count: {count}"
        )));
    }
    let mut keys = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let dc_id = r.read_i32()?;
        let key_data = r.read_exact_array::<AUTH_KEY_SIZE>()?;
        keys.push(MtpAuthKeyEntry {
            dc_id,
            key: AuthKey::new(key_data),
        });
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mtp_auth_wide_format_roundtrip_and_layout() {
        let auth = MtpAuthorization {
            user_id: 5_123_456_789,
            main_dc_id: 2,
            keys: vec![
                MtpAuthKeyEntry {
                    dc_id: 2,
                    key: AuthKey::new([0x11u8; 256]),
                },
                MtpAuthKeyEntry {
                    dc_id: 4,
                    key: AuthKey::new([0x22u8; 256]),
                },
            ],
            keys_to_destroy: vec![MtpAuthKeyEntry {
                dc_id: 1,
                key: AuthKey::new([0x33u8; 256]),
            }],
        };

        let bytes = auth.serialize();
        // 8 (wide tag) + 8 (user_id) + 4 (dc) + 4 (count=2) + 2*260 + 4 (count=1) + 1*260 = 808
        assert_eq!(bytes.len(), 8 + 8 + 4 + 4 + 2 * 260 + 4 + 260);
        assert_eq!(&bytes[0..8], &[0xFF; 8]);
        assert_eq!(&bytes[8..16], &5_123_456_789u64.to_be_bytes());
        assert_eq!(&bytes[16..20], &2i32.to_be_bytes());

        let parsed = MtpAuthorization::deserialize(&bytes).expect("deserialize");
        assert_eq!(parsed, auth);
    }

    #[test]
    fn mtp_auth_legacy_format_roundtrip() {
        let auth = MtpAuthorization {
            user_id: 123_456,
            main_dc_id: 4,
            keys: vec![MtpAuthKeyEntry {
                dc_id: 4,
                key: AuthKey::new([0xABu8; 256]),
            }],
            keys_to_destroy: vec![],
        };
        let bytes = auth.serialize_legacy();
        assert_eq!(&bytes[0..4], &123_456i32.to_be_bytes());
        assert_eq!(&bytes[4..8], &4i32.to_be_bytes());

        let parsed = MtpAuthorization::deserialize(&bytes).expect("deserialize legacy");
        assert_eq!(parsed, auth);
    }
}
