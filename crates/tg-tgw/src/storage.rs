//! Per-message widget local storage, quota enforcement, and binary record serialization.
//!
//! Port origin: `Telegram/SourceFiles/tgw/tgw_storage.h/.cpp` and `tgw_storage_disk.h/.cpp`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Result, TgwError};

/// Per-widget local storage quota in bytes (`twidget::sdk::STORAGE_QUOTA_BYTES = 4096`).
pub const STORAGE_QUOTA_BYTES: usize = 4 * 1024;

/// Legacy storage quota accepted when reading older records (`kMaxStoredBytes` in `tgw_storage_disk.cpp:24`).
pub const LEGACY_MAX_STORED_BYTES: usize = 16 * 1024;

/// Storage index format version (`kIndexVersion = 2` in `tgw_storage_disk.cpp:19`).
pub const STORAGE_INDEX_VERSION: u32 = 2;

/// Storage record format version (`kRecordVersion = 1` in `tgw_storage_disk.cpp:20`).
pub const STORAGE_RECORD_VERSION: u32 = 1;

/// Sentinel length denoting empty byte string (`kNullBytes = 0xFFFFFFFF`).
const NULL_BYTES: u32 = 0xFFFF_FFFF;

/// Message key for widget local storage (`FullMsgId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WidgetMsgKey {
    /// Peer ID.
    pub peer_id: i64,
    /// Server message ID (must be `> 0`).
    pub msg_id: i32,
}

/// Message metadata tracked for storage pruning/expiry (`Tgw::StorageMeta` in `tgw_storage_disk.h:19-29`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageMeta {
    /// Message timestamp (Unix seconds).
    pub date: i32,
    /// Optional TTL expiry timestamp (Unix seconds, `0` = no expiry).
    pub expires: i32,
    /// Optional forum topic root message ID.
    pub topic_root_id: i32,
    /// Optional Saved Messages sublist peer ID.
    pub sublist_peer_id: i64,
    /// Sender peer ID.
    pub from_peer_id: i64,
}

/// Persisted auto-height record (`Tgw::HeightRecord` in `tgw_storage.h:36-41`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct HeightRecord {
    /// Last rendered height in CSS pixels.
    pub height: f64,
    /// Whether bottom-anchored.
    pub bottom: bool,
}

/// Key-value local storage area for a single widget message.
pub type StorageArea = BTreeMap<String, String>;

/// Computes total UTF-8 byte footprint of a [`StorageArea`].
#[must_use]
pub fn storage_area_bytes(area: &StorageArea) -> usize {
    area.iter().map(|(k, v)| k.len() + v.len()).sum()
}

/// Returns `true` if `area` fits within [`STORAGE_QUOTA_BYTES`].
#[must_use]
pub fn storage_area_fits(area: &StorageArea) -> bool {
    storage_area_bytes(area) <= STORAGE_QUOTA_BYTES
}

/// Encodes a [`StorageArea`] into tdesktop's binary record format (`tgw_storage_disk.cpp`).
pub fn encode_storage_record(area: &StorageArea) -> Result<Vec<u8>> {
    if !storage_area_fits(area) {
        return Err(TgwError::Storage(format!(
            "storage area ({} bytes) exceeds quota ({STORAGE_QUOTA_BYTES} bytes)",
            storage_area_bytes(area)
        )));
    }
    let mut buf = Vec::new();
    buf.extend_from_slice(&STORAGE_RECORD_VERSION.to_be_bytes());
    buf.extend_from_slice(&(area.len() as u32).to_be_bytes());
    for (k, v) in area {
        write_be_bytes(&mut buf, k.as_bytes());
        write_be_bytes(&mut buf, v.as_bytes());
    }
    Ok(buf)
}

/// Decodes a binary storage record, enforcing version, count, duplicate-key, and byte-quota checks
/// (`ReadRecord` in `tgw_storage_disk.cpp:83-107`).
pub fn decode_storage_record(data: &[u8]) -> Result<StorageArea> {
    if data.len() < 8 {
        return Err(TgwError::Storage(
            "truncated storage record header".to_owned(),
        ));
    }
    let version = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
    if version == 0 || version > STORAGE_RECORD_VERSION {
        return Err(TgwError::Storage(format!(
            "unsupported storage record version {version}"
        )));
    }
    let max_pairs = LEGACY_MAX_STORED_BYTES + 1;
    if count > max_pairs || count.saturating_mul(8) > data.len() - 8 {
        return Err(TgwError::Storage(format!(
            "invalid storage record pair count {count}"
        )));
    }
    let mut cursor = &data[8..];
    let mut left = LEGACY_MAX_STORED_BYTES;
    let mut area = StorageArea::new();
    for _ in 0..count {
        let key = read_be_string(&mut cursor, &mut left)?;
        let val = read_be_string(&mut cursor, &mut left)?;
        if area.insert(key, val).is_some() {
            return Err(TgwError::Storage(
                "duplicate key in widget storage record".to_owned(),
            ));
        }
    }
    Ok(area)
}

fn write_be_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.is_empty() {
        buf.extend_from_slice(&NULL_BYTES.to_be_bytes());
    } else {
        buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(bytes);
    }
}

fn read_be_string(cursor: &mut &[u8], left: &mut usize) -> Result<String> {
    if cursor.len() < 4 {
        return Err(TgwError::Storage(
            "truncated storage entry length".to_owned(),
        ));
    }
    let raw_len = u32::from_be_bytes([cursor[0], cursor[1], cursor[2], cursor[3]]);
    *cursor = &cursor[4..];
    if raw_len == NULL_BYTES {
        return Ok(String::new());
    }
    let len = raw_len as usize;
    if len > *left || len > cursor.len() {
        return Err(TgwError::Storage(format!(
            "storage entry length {len} exceeds remaining budget"
        )));
    }
    let slice = &cursor[..len];
    *cursor = &cursor[len..];
    *left -= len;
    String::from_utf8(slice.to_vec())
        .map_err(|e| TgwError::Storage(format!("invalid UTF-8 in storage entry: {e}")))
}

#[derive(Debug, Clone, Default)]
struct StoredEntry {
    meta: StorageMeta,
    area: StorageArea,
    height: Option<HeightRecord>,
}

/// In-memory widget storage index supporting height caching, quota enforcement, expiry, and
/// chat/topic/date/sender purge operations (`Tgw::StorageDisk`).
#[derive(Debug, Default)]
pub struct WidgetStorageStore {
    entries: BTreeMap<WidgetMsgKey, StoredEntry>,
}

impl WidgetStorageStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes `area` for `key` if `key.msg_id > 0` and `area` fits within [`STORAGE_QUOTA_BYTES`].
    pub fn write(
        &mut self,
        key: WidgetMsgKey,
        meta: StorageMeta,
        area: StorageArea,
    ) -> Result<bool> {
        if key.msg_id <= 0 {
            return Ok(false);
        }
        if !storage_area_fits(&area) {
            return Err(TgwError::Storage(format!(
                "storage area exceeds {STORAGE_QUOTA_BYTES} bytes"
            )));
        }
        if !self.entries.contains_key(&key) && area.is_empty() {
            return Ok(false);
        }
        let entry = self.entries.entry(key).or_default();
        entry.meta = meta;
        entry.area = area;
        Ok(true)
    }

    /// Reads the stored local key-value map for `key`, if present.
    #[must_use]
    pub fn read(&self, key: WidgetMsgKey) -> Option<&StorageArea> {
        self.entries.get(&key).map(|e| &e.area)
    }

    /// Stores a positive finite [`HeightRecord`] for `key`.
    pub fn set_height(
        &mut self,
        key: WidgetMsgKey,
        meta: StorageMeta,
        height: HeightRecord,
    ) -> bool {
        if key.msg_id <= 0 || !height.height.is_finite() || height.height <= 0.0 {
            return false;
        }
        let entry = self.entries.entry(key).or_default();
        if entry.meta == meta && entry.height == Some(height) {
            return false;
        }
        entry.meta = meta;
        entry.height = Some(height);
        true
    }

    /// Reads the stored [`HeightRecord`] for `key`.
    #[must_use]
    pub fn height(&self, key: WidgetMsgKey) -> Option<HeightRecord> {
        self.entries.get(&key).and_then(|e| e.height)
    }

    /// Removes all entries belonging to `peer_id` (`ClearChatStorage` / `DeleteChatStorage`).
    pub fn clear_chat(&mut self, peer_id: i64) -> usize {
        let before = self.entries.len();
        self.entries.retain(|k, _| k.peer_id != peer_id);
        before - self.entries.len()
    }

    /// Removes all entries whose `meta.expires > 0 && meta.expires <= now_unix`.
    pub fn prune_expired(&mut self, now_unix: i32) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|_, v| v.meta.expires <= 0 || v.meta.expires > now_unix);
        before - self.entries.len()
    }

    /// Moves storage from `from` to `to` (e.g. when a pending message receives a server ID).
    pub fn move_entry(&mut self, from: WidgetMsgKey, to: WidgetMsgKey) -> bool {
        if let Some(entry) = self.entries.remove(&from) {
            self.entries.insert(to, entry);
            true
        } else {
            false
        }
    }

    /// Returns the number of tracked messages.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if no entries are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
