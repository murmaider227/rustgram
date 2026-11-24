//! Recent Settings searches list, LRU bumping, cap (`32`), binary wire serialization, and
//! [`CoreSettings`] persistence.
//!
//! Port origin:
//! - `Telegram/SourceFiles/settings/settings_recent_searches.h`
//! - `Telegram/SourceFiles/settings/settings_recent_searches.cpp`

use tg_core::CoreSettings;
use tg_core::version::TDESKTOP_COMPAT_VERSION;

/// Maximum number of recent settings searches retained (`kLimit = 32` in `settings_recent_searches.cpp`).
pub const MAX_RECENT_SETTINGS_SEARCHES: usize = 32;

/// Persistent LRU list of recently clicked Settings search items (`Settings::RecentSearches`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecentSettingsSearches {
    list: Vec<String>,
}

impl RecentSettingsSearches {
    /// Creates an empty [`RecentSettingsSearches`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads initial entries from [`CoreSettings::recent_settings_searches`].
    #[must_use]
    pub fn from_core_settings(core: &CoreSettings) -> Self {
        let mut list = Vec::with_capacity(
            core.recent_settings_searches
                .len()
                .min(MAX_RECENT_SETTINGS_SEARCHES),
        );
        for entry in &core.recent_settings_searches {
            if !entry.is_empty()
                && !list.contains(entry)
                && list.len() < MAX_RECENT_SETTINGS_SEARCHES
            {
                list.push(entry.clone());
            }
        }
        Self { list }
    }

    /// Writes the current list into [`CoreSettings::recent_settings_searches`].
    pub fn sync_to_core_settings(&self, core: &mut CoreSettings) {
        core.recent_settings_searches = self.list.clone();
    }

    /// Returns the ordered recent search entry IDs (most recent first).
    #[must_use]
    pub fn list(&self) -> &[String] {
        &self.list
    }

    /// Returns `true` when no recent searches are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Moves `entry_id` to the front of the list, deduplicating and capping at
    /// [`MAX_RECENT_SETTINGS_SEARCHES`] (`RecentSearches::bump`).
    pub fn bump(&mut self, entry_id: &str) {
        let trimmed = entry_id.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.list.first().map(String::as_str) == Some(trimmed) {
            return;
        }
        if let Some(pos) = self.list.iter().position(|e| e == trimmed) {
            self.list[..=pos].rotate_right(1);
        } else {
            if self.list.len() >= MAX_RECENT_SETTINGS_SEARCHES {
                self.list.pop();
            }
            self.list.insert(0, trimmed.to_owned());
        }
    }

    /// Removes `entry_id` from the list (`RecentSearches::remove`).
    pub fn remove(&mut self, entry_id: &str) {
        self.list.retain(|e| e != entry_id);
    }

    /// Clears all recent search entries.
    pub fn clear(&mut self) {
        self.list.clear();
    }

    /// Serializes matching `RecentSearches::serialize()` (`u32 version`, `u32 count`, length-prefixed strings).
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        if self.list.is_empty() {
            return Vec::new();
        }
        let count = self.list.len().min(MAX_RECENT_SETTINGS_SEARCHES);
        let mut out = Vec::with_capacity(8 + count * 16);
        out.extend_from_slice(&(TDESKTOP_COMPAT_VERSION as u32).to_be_bytes());
        out.extend_from_slice(&(count as u32).to_be_bytes());
        for entry in self.list.iter().take(count) {
            let bytes = entry.as_bytes();
            out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(bytes);
        }
        out
    }

    /// Deserializes a blob produced by [`Self::serialize`] (`RecentSearches::applyLocal`).
    pub fn apply_local(&mut self, serialized: &[u8]) {
        self.list.clear();
        if serialized.len() < 8 {
            return;
        }
        let _version =
            u32::from_be_bytes([serialized[0], serialized[1], serialized[2], serialized[3]]);
        let count = u32::from_be_bytes([serialized[4], serialized[5], serialized[6], serialized[7]])
            as usize;
        if count > MAX_RECENT_SETTINGS_SEARCHES {
            return;
        }
        let mut pos = 8usize;
        let mut parsed = Vec::with_capacity(count);
        for _ in 0..count {
            if pos + 4 > serialized.len() {
                return;
            }
            let len = u32::from_be_bytes([
                serialized[pos],
                serialized[pos + 1],
                serialized[pos + 2],
                serialized[pos + 3],
            ]) as usize;
            pos += 4;
            if pos + len > serialized.len() {
                return;
            }
            let Ok(s) = std::str::from_utf8(&serialized[pos..pos + len]) else {
                return;
            };
            pos += len;
            if !s.is_empty() {
                parsed.push(s.to_owned());
            }
        }
        self.list = parsed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_remove_clear_and_limit() {
        let mut recent = RecentSettingsSearches::new();
        for i in 0..40 {
            recent.bump(&format!("entry_{i}"));
        }
        assert_eq!(recent.list().len(), MAX_RECENT_SETTINGS_SEARCHES);
        assert_eq!(recent.list()[0], "entry_39");

        // Bumping an existing item rotates it to the front without duplicating.
        recent.bump("entry_35");
        assert_eq!(recent.list().len(), MAX_RECENT_SETTINGS_SEARCHES);
        assert_eq!(recent.list()[0], "entry_35");

        recent.remove("entry_35");
        assert!(!recent.list().contains(&"entry_35".to_owned()));

        let blob = recent.serialize();
        let mut restored = RecentSettingsSearches::new();
        restored.apply_local(&blob);
        assert_eq!(restored, recent);

        // Round-trip through CoreSettings in tempdir.
        let mut core = CoreSettings::new();
        recent.sync_to_core_settings(&mut core);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.bin");
        std::fs::write(&path, core.serialize()).expect("write");

        let mut reloaded_core = CoreSettings::new();
        reloaded_core
            .add_from_serialized(
                &std::fs::read(&path).expect("read"),
                tg_core::version::TDESKTOP_COMPAT_VERSION,
            )
            .expect("deserialize");
        let reloaded_recent = RecentSettingsSearches::from_core_settings(&reloaded_core);
        assert_eq!(reloaded_recent, recent);

        recent.clear();
        assert!(recent.is_empty());
    }
}
