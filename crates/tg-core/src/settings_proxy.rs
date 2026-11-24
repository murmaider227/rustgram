//! Port of `core/core_settings_proxy.h`, `core/core_settings_proxy.cpp`,
//! and `MTP::ProxyData` from `mtproto/mtproto_proxy_data.h`.
//!
//! Binary serialization matches `Core::SettingsProxy::serialize()` and
//! `Core::SettingsProxy::setFromSerialized()` byte-for-byte.

use thiserror::Error;

use tg_storage::qdatastream::{DataStreamError, QDataStreamReader, QDataStreamWriter};

/// Supported proxy rotation timeout presets in seconds (`kProxyRotationTimeouts`).
pub const PROXY_ROTATION_TIMEOUTS: [i32; 5] = [5, 10, 15, 30, 60];

/// Default proxy rotation timeout in seconds (`kDefaultProxyRotationTimeout`).
pub const DEFAULT_PROXY_ROTATION_TIMEOUT: i32 = 10;

/// Errors returned when deserializing proxy settings.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProxySettingsError {
    /// Underlying `QDataStream` decoding error.
    #[error("QDataStream error: {0}")]
    Stream(#[from] DataStreamError),
    /// Unknown proxy mode integer (expected `0..=2`).
    #[error("invalid proxy settings mode: {0}")]
    InvalidSettingsMode(i32),
    /// Negative list count in serialized proxy settings.
    #[error("negative count in proxy settings: {0}")]
    NegativeCount(i32),
}

/// Global proxy connection mode (`MTP::ProxyData::Settings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum ProxySettingsMode {
    /// Use system proxy configuration (`0`).
    #[default]
    System = 0,
    /// Use the explicitly selected proxy (`1`).
    Enabled = 1,
    /// Direct connections only; proxy disabled (`2`).
    Disabled = 2,
}

impl ProxySettingsMode {
    /// Converts to tdesktop's serialized integer representation (`ProxySettingsToInt`).
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        self as i32
    }

    /// Decodes from tdesktop's serialized integer representation (`IntToProxySettings`).
    pub fn from_i32(value: i32) -> Result<Self, ProxySettingsError> {
        match value {
            0 => Ok(Self::System),
            1 => Ok(Self::Enabled),
            2 => Ok(Self::Disabled),
            other => Err(ProxySettingsError::InvalidSettingsMode(other)),
        }
    }
}

/// Proxy protocol type (`MTP::ProxyData::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum ProxyType {
    /// Empty / unset proxy (`0`).
    #[default]
    None = 0,
    /// SOCKS5 proxy (`1`).
    Socks5 = 1,
    /// HTTP CONNECT proxy (`2`).
    Http = 2,
    /// MTProto proxy (`3`).
    Mtproto = 3,
    /// Web bridge proxy (`4`).
    Web = 4,
}

impl ProxyType {
    /// Converts to tdesktop's serialized integer representation.
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        self as i32
    }

    /// Decodes from tdesktop's serialized integer representation (unknown values map to `None`).
    #[must_use]
    pub const fn from_i32(value: i32) -> Self {
        match value {
            1 => Self::Socks5,
            2 => Self::Http,
            3 => Self::Mtproto,
            4 => Self::Web,
            _ => Self::None,
        }
    }
}

/// Single proxy server configuration (`MTP::ProxyData`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProxyData {
    /// Proxy protocol type.
    pub proxy_type: ProxyType,
    /// Hostname or IP address.
    pub host: String,
    /// TCP port number.
    pub port: u32,
    /// Optional username (or web base path for `ProxyType::Web`).
    pub user: String,
    /// Optional password (or MTProto secret for `ProxyType::Mtproto`).
    pub password: String,
}

impl ProxyData {
    /// Returns `true` if the proxy has a configured type, non-empty host, and valid port.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.proxy_type != ProxyType::None && !self.host.is_empty() && self.port > 0
    }

    /// Returns `true` if this proxy type can relay VoIP calls (SOCKS5 only in tdesktop).
    #[must_use]
    pub fn supports_calls(&self) -> bool {
        self.proxy_type == ProxyType::Socks5
    }

    /// Serializes a single `ProxyData` entry matching `SerializeProxyData()` in
    /// `core_settings_proxy.cpp`.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut w = QDataStreamWriter::new();
        w.write_i32(self.proxy_type.to_i32());
        w.write_qstring(&self.host);
        w.write_i32(self.port as i32);
        w.write_qstring(&self.user);
        w.write_qstring(&self.password);
        w.into_bytes()
    }

    /// Deserializes a single `ProxyData` entry matching `DeserializeProxyData()`.
    pub fn deserialize(data: &[u8]) -> Result<Self, DataStreamError> {
        if data.is_empty() {
            return Ok(Self::default());
        }
        let mut r = QDataStreamReader::new(data);
        let raw_type = r.read_i32()?;
        let host = r.read_qstring_lossy()?;
        let port = r.read_i32()?;
        let user = r.read_qstring_lossy()?;
        let password = r.read_qstring_lossy()?;
        Ok(Self {
            proxy_type: ProxyType::from_i32(raw_type),
            host,
            port: port as u32,
            user,
            password,
        })
    }
}

/// Application proxy configuration (`Core::SettingsProxy`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsProxy {
    try_ipv6: bool,
    use_proxy_for_calls: bool,
    proxy_rotation_enabled: bool,
    check_ip_warning_shown: bool,
    proxy_rotation_timeout: i32,
    settings: ProxySettingsMode,
    selected: ProxyData,
    list: Vec<ProxyData>,
    proxy_rotation_preferred_indices: Vec<i32>,
}

impl Default for SettingsProxy {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsProxy {
    /// Creates default proxy settings (`try_ipv6 = !Platform::IsWindows()`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            try_ipv6: !cfg!(target_os = "windows"),
            use_proxy_for_calls: false,
            proxy_rotation_enabled: false,
            check_ip_warning_shown: false,
            proxy_rotation_timeout: DEFAULT_PROXY_ROTATION_TIMEOUT,
            settings: ProxySettingsMode::System,
            selected: ProxyData::default(),
            list: Vec::new(),
            proxy_rotation_preferred_indices: Vec::new(),
        }
    }

    /// Returns `true` if a custom proxy is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.settings == ProxySettingsMode::Enabled
    }

    /// Returns `true` if system proxy settings are used.
    #[must_use]
    pub fn is_system(&self) -> bool {
        self.settings == ProxySettingsMode::System
    }

    /// Returns `true` if proxies are explicitly disabled.
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.settings == ProxySettingsMode::Disabled
    }

    /// Whether IPv6 connections should be attempted.
    #[must_use]
    pub fn try_ipv6(&self) -> bool {
        self.try_ipv6
    }

    /// Sets whether IPv6 connections should be attempted.
    pub fn set_try_ipv6(&mut self, value: bool) {
        self.try_ipv6 = value;
    }

    /// Whether calls should route through the active proxy.
    #[must_use]
    pub fn use_proxy_for_calls(&self) -> bool {
        self.use_proxy_for_calls
    }

    /// Sets whether calls should route through the active proxy.
    pub fn set_use_proxy_for_calls(&mut self, value: bool) {
        self.use_proxy_for_calls = value;
    }

    /// Whether automatic proxy rotation is enabled.
    #[must_use]
    pub fn proxy_rotation_enabled(&self) -> bool {
        self.proxy_rotation_enabled
    }

    /// Sets whether automatic proxy rotation is enabled.
    pub fn set_proxy_rotation_enabled(&mut self, value: bool) {
        self.proxy_rotation_enabled = value;
    }

    /// Current proxy rotation timeout in seconds.
    #[must_use]
    pub fn proxy_rotation_timeout(&self) -> i32 {
        self.proxy_rotation_timeout
    }

    /// Sets the proxy rotation timeout (non-positive values fall back to `DEFAULT_PROXY_ROTATION_TIMEOUT`).
    pub fn set_proxy_rotation_timeout(&mut self, value: i32) {
        self.proxy_rotation_timeout = if value > 0 {
            value
        } else {
            DEFAULT_PROXY_ROTATION_TIMEOUT
        };
    }

    /// Whether the IP-check warning has been shown.
    #[must_use]
    pub fn check_ip_warning_shown(&self) -> bool {
        self.check_ip_warning_shown
    }

    /// Sets whether the IP-check warning has been shown.
    pub fn set_check_ip_warning_shown(&mut self, value: bool) {
        self.check_ip_warning_shown = value;
    }

    /// Current proxy settings mode (`System`, `Enabled`, `Disabled`).
    #[must_use]
    pub fn settings(&self) -> ProxySettingsMode {
        self.settings
    }

    /// Sets the proxy settings mode.
    pub fn set_settings(&mut self, value: ProxySettingsMode) {
        self.settings = value;
    }

    /// Currently selected proxy entry.
    #[must_use]
    pub fn selected(&self) -> &ProxyData {
        &self.selected
    }

    /// Sets the currently selected proxy entry.
    pub fn set_selected(&mut self, value: ProxyData) {
        self.selected = value;
    }

    /// Returns the preferred rotation indices into [`Self::list`].
    #[must_use]
    pub fn proxy_rotation_preferred_indices(&self) -> &[i32] {
        &self.proxy_rotation_preferred_indices
    }

    /// Sets and normalizes preferred rotation indices (deduplicating and dropping out-of-range entries).
    pub fn set_proxy_rotation_preferred_indices(&mut self, indices: Vec<i32>) {
        self.proxy_rotation_preferred_indices =
            normalize_preferred_indices(indices, self.list.len());
    }

    /// Moves `index` to the front of the preferred rotation order; returns `true` if modified.
    pub fn promote_proxy_rotation_preferred_index(&mut self, index: i32) -> bool {
        if index < 0 || (index as usize) >= self.list.len() {
            return false;
        }
        if let Some(pos) = self
            .proxy_rotation_preferred_indices
            .iter()
            .position(|&x| x == index)
        {
            if pos == 0 {
                return false;
            }
            self.proxy_rotation_preferred_indices[..=pos].rotate_right(1);
        } else {
            self.proxy_rotation_preferred_indices.insert(0, index);
        }
        true
    }

    /// Returns the saved proxy list.
    #[must_use]
    pub fn list(&self) -> &[ProxyData] {
        &self.list
    }

    /// Returns a mutable slice of the saved proxy list.
    pub fn list_mut(&mut self) -> &mut Vec<ProxyData> {
        &mut self.list
    }

    /// Replaces the saved proxy list and clears preferred rotation indices.
    pub fn set_list(&mut self, value: Vec<ProxyData>) {
        self.list = value;
        self.proxy_rotation_preferred_indices.clear();
    }

    /// Appends a proxy to the list.
    pub fn add_to_list(&mut self, value: ProxyData) {
        self.list.push(value);
    }

    /// Inserts a proxy at `index` (clamped to `0..=list.len()`), shifting preferred indices.
    pub fn insert_to_list(&mut self, index: usize, value: ProxyData) {
        let clamped = index.min(self.list.len());
        let clamped_i32 = clamped as i32;
        for existing in &mut self.proxy_rotation_preferred_indices {
            if *existing >= clamped_i32 {
                *existing += 1;
            }
        }
        self.list.insert(clamped, value);
    }

    /// Removes the first matching proxy from the list and adjusts preferred indices.
    pub fn remove_from_list(&mut self, value: &ProxyData) -> bool {
        let Some(pos) = self.list.iter().position(|p| p == value) else {
            return false;
        };
        let pos_i32 = pos as i32;
        self.list.remove(pos);
        self.proxy_rotation_preferred_indices
            .retain(|&existing| existing != pos_i32);
        for existing in &mut self.proxy_rotation_preferred_indices {
            if *existing > pos_i32 {
                *existing -= 1;
            }
        }
        true
    }

    /// Replaces the first matching proxy in the list.
    pub fn replace_in_list(&mut self, was: &ProxyData, value: ProxyData) -> bool {
        let Some(slot) = self.list.iter_mut().find(|p| *p == was) else {
            return false;
        };
        *slot = value;
        true
    }

    /// Returns the index of `value` in the list, or `None` if not found.
    #[must_use]
    pub fn index_in_list(&self, value: &ProxyData) -> Option<usize> {
        self.list.iter().position(|p| p == value)
    }

    /// Serializes proxy settings in tdesktop's exact `QDataStream` wire format.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let serialized_selected = self.selected.serialize();
        let serialized_list: Vec<Vec<u8>> = self.list.iter().map(ProxyData::serialize).collect();

        let mut w = QDataStreamWriter::new();
        w.write_bool_i32(self.try_ipv6);
        w.write_bool_i32(self.use_proxy_for_calls);
        w.write_i32(self.settings.to_i32());
        w.write_byte_array(&serialized_selected);
        w.write_i32(serialized_list.len() as i32);
        for entry in &serialized_list {
            w.write_byte_array(entry);
        }
        w.write_bool_i32(self.check_ip_warning_shown);
        w.write_bool_i32(self.proxy_rotation_enabled);
        w.write_i32(self.proxy_rotation_timeout);
        w.write_i32(self.proxy_rotation_preferred_indices.len() as i32);
        for &idx in &self.proxy_rotation_preferred_indices {
            w.write_i32(idx);
        }
        w.into_bytes()
    }

    /// Deserializes proxy settings from tdesktop's `QDataStream` wire format.
    ///
    /// Empty slices succeed as a no-op (matching `Core::SettingsProxy::setFromSerialized`).
    pub fn set_from_serialized(&mut self, serialized: &[u8]) -> Result<(), ProxySettingsError> {
        if serialized.is_empty() {
            return Ok(());
        }

        let mut r = QDataStreamReader::new(serialized);

        let mut try_ipv6 = i32::from(self.try_ipv6);
        let mut use_proxy_for_calls = i32::from(self.use_proxy_for_calls);
        let mut settings_raw = self.settings.to_i32();
        let mut selected_bytes = Vec::new();
        let mut list = Vec::new();
        let mut skipped_indices = Vec::new();

        if !r.at_end() {
            try_ipv6 = r.read_i32()?;
            use_proxy_for_calls = r.read_i32()?;
            settings_raw = r.read_i32()?;
            selected_bytes = r.read_byte_array()?;
            let list_count = r.read_i32()?;
            if list_count < 0 {
                return Err(ProxySettingsError::NegativeCount(list_count));
            }
            list.reserve(list_count as usize);
            for i in 0..list_count {
                let entry_bytes = r.read_byte_array()?;
                let proxy = ProxyData::deserialize(&entry_bytes)?;
                if proxy.proxy_type == ProxyType::None {
                    skipped_indices.push(i);
                } else {
                    list.push(proxy);
                }
            }
        }

        let check_ip_warning_shown = if !r.at_end() { r.read_i32()? } else { 0 };
        let proxy_rotation_enabled = if !r.at_end() {
            r.read_i32()?
        } else {
            i32::from(self.proxy_rotation_enabled)
        };
        let proxy_rotation_timeout = if !r.at_end() {
            r.read_i32()?
        } else {
            self.proxy_rotation_timeout
        };

        let mut preferred_indices = Vec::new();
        if !r.at_end() {
            let preferred_count = r.read_i32()?;
            if preferred_count < 0 {
                return Err(ProxySettingsError::NegativeCount(preferred_count));
            }
            preferred_indices.reserve(preferred_count as usize);
            for _ in 0..preferred_count {
                preferred_indices.push(r.read_i32()?);
            }
        }

        let settings = ProxySettingsMode::from_i32(settings_raw)?;
        let mut selected = ProxyData::deserialize(&selected_bytes)?;
        if selected.proxy_type == ProxyType::None {
            selected = ProxyData::default();
        }

        self.try_ipv6 = try_ipv6 == 1;
        self.use_proxy_for_calls = use_proxy_for_calls == 1;
        self.check_ip_warning_shown = check_ip_warning_shown == 1;
        self.proxy_rotation_enabled = proxy_rotation_enabled == 1;
        self.set_proxy_rotation_timeout(proxy_rotation_timeout);
        self.settings = settings;
        self.selected = selected;
        self.list = list;

        if !skipped_indices.is_empty() {
            for index in &mut preferred_indices {
                if skipped_indices.contains(index) {
                    *index = -1;
                } else {
                    let shift = skipped_indices.iter().filter(|&&s| s < *index).count() as i32;
                    *index -= shift;
                }
            }
        }
        self.set_proxy_rotation_preferred_indices(preferred_indices);
        Ok(())
    }
}

fn normalize_preferred_indices(indices: Vec<i32>, list_len: usize) -> Vec<i32> {
    let mut filtered = Vec::with_capacity(indices.len());
    for index in indices {
        if index < 0 || (index as usize) >= list_len || filtered.contains(&index) {
            continue;
        }
        filtered.push(index);
    }
    filtered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_settings_round_trip_and_byte_layout() {
        let mut proxy = SettingsProxy::new();
        proxy.set_try_ipv6(true);
        proxy.set_use_proxy_for_calls(true);
        proxy.set_settings(ProxySettingsMode::Enabled);
        let socks = ProxyData {
            proxy_type: ProxyType::Socks5,
            host: "127.0.0.1".to_owned(),
            port: 1080,
            user: "alice".to_owned(),
            password: "secret".to_owned(),
        };
        let mtp = ProxyData {
            proxy_type: ProxyType::Mtproto,
            host: "proxy.example.com".to_owned(),
            port: 443,
            user: String::new(),
            password: "ee0123456789abcdef0123456789abcdef".to_owned(),
        };
        proxy.set_selected(socks.clone());
        proxy.add_to_list(socks.clone());
        proxy.add_to_list(mtp.clone());
        proxy.set_check_ip_warning_shown(true);
        proxy.set_proxy_rotation_enabled(true);
        proxy.set_proxy_rotation_timeout(30);
        proxy.set_proxy_rotation_preferred_indices(vec![1, 0, 1, 99]);

        assert_eq!(proxy.proxy_rotation_preferred_indices(), &[1, 0]);

        let bytes = proxy.serialize();
        // Verify first 3 big-endian i32s: try_ipv6=1, use_proxy_for_calls=1, settings=1 (Enabled)
        assert_eq!(&bytes[0..4], &1i32.to_be_bytes());
        assert_eq!(&bytes[4..8], &1i32.to_be_bytes());
        assert_eq!(&bytes[8..12], &1i32.to_be_bytes());

        let mut decoded = SettingsProxy::new();
        decoded.set_from_serialized(&bytes).unwrap();
        assert_eq!(decoded, proxy);
    }

    #[test]
    fn skipped_none_proxy_remaps_preferred_indices() {
        let mut w = QDataStreamWriter::new();
        w.write_i32(0); // try_ipv6
        w.write_i32(0); // use_proxy_for_calls
        w.write_i32(0); // System
        w.write_byte_array(&ProxyData::default().serialize());
        w.write_i32(3); // 3 entries, middle one is Type::None
        w.write_byte_array(
            &ProxyData {
                proxy_type: ProxyType::Http,
                host: "a.test".to_owned(),
                port: 8080,
                user: String::new(),
                password: String::new(),
            }
            .serialize(),
        );
        w.write_byte_array(&ProxyData::default().serialize());
        w.write_byte_array(
            &ProxyData {
                proxy_type: ProxyType::Socks5,
                host: "b.test".to_owned(),
                port: 1080,
                user: String::new(),
                password: String::new(),
            }
            .serialize(),
        );
        w.write_i32(0); // check_ip_warning_shown
        w.write_i32(1); // rotation enabled
        w.write_i32(15); // rotation timeout
        w.write_i32(3); // preferred count
        w.write_i32(2); // old index 2 -> new index 1
        w.write_i32(1); // old index 1 (skipped!) -> dropped
        w.write_i32(0); // old index 0 -> stays 0

        let mut decoded = SettingsProxy::new();
        decoded.set_from_serialized(&w.into_bytes()).unwrap();
        assert_eq!(decoded.list().len(), 2);
        assert_eq!(decoded.proxy_rotation_preferred_indices(), &[1, 0]);
    }

    #[test]
    fn list_mutations_adjust_preferred_indices() {
        let mut p = SettingsProxy::new();
        let a = ProxyData {
            proxy_type: ProxyType::Socks5,
            host: "a".to_owned(),
            port: 1,
            ..Default::default()
        };
        let b = ProxyData {
            proxy_type: ProxyType::Http,
            host: "b".to_owned(),
            port: 2,
            ..Default::default()
        };
        let c = ProxyData {
            proxy_type: ProxyType::Mtproto,
            host: "c".to_owned(),
            port: 3,
            ..Default::default()
        };
        p.add_to_list(a.clone());
        p.add_to_list(b.clone());
        p.set_proxy_rotation_preferred_indices(vec![1, 0]);
        p.insert_to_list(1, c);
        assert_eq!(p.proxy_rotation_preferred_indices(), &[2, 0]);
        assert!(p.promote_proxy_rotation_preferred_index(0));
        assert_eq!(p.proxy_rotation_preferred_indices(), &[0, 2]);
        assert!(p.remove_from_list(&a));
        assert_eq!(p.proxy_rotation_preferred_indices(), &[1]);
    }
}
