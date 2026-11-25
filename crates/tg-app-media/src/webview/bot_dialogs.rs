//! Mini App per-bot local storage, file download manager + confirmation dialog,
//! and prepared inline message confirmation dialog.
//!
//! Port of:
//! - `Telegram/SourceFiles/inline_bots/inline_bot_storage.{h,cpp}`
//! - `Telegram/SourceFiles/inline_bots/inline_bot_downloads.{h,cpp}`
//! - `Telegram/SourceFiles/inline_bots/inline_bot_confirm_prepared.{h,cpp}`

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use egui::{CornerRadius, Stroke, StrokeKind, Ui};
use serde::{Deserialize, Serialize};
use tg_storage::StorageEncryptionKey;
use tg_storage::cache::media_cache::{
    lookup_cache_key_for_path, read_encrypted_blob_bound, write_encrypted_blob_bound,
};
use tg_theme::Palette as TgPalette;
use tg_ui::fonts::font_id_from_style;
use tg_ui::tg_style::st;

use crate::webview::theme::color_to_egui;
use tg_app_core::stand_in::types::PeerId;

/// Maximum per-bot local storage footprint in bytes (`kMaxStorageSize = 5 << 20` = 5 MiB).
pub const MAX_BOT_STORAGE_BYTES: usize = 5 << 20;

/// Format version for serialized bot download history (`kDownloadsVersion = 1`).
pub const BOT_DOWNLOADS_VERSION: u32 = 1;

/// Maximum distinct bots stored in download history (`kMaxDownloadsBots = 4096`).
pub const MAX_DOWNLOADS_BOTS: usize = 4096;

/// Maximum download entries per bot (`kMaxDownloadsPerBot = 16384`).
pub const MAX_DOWNLOADS_PER_BOT: usize = 16_384;

pub use tg_app_core::core_types::misc::account_bot_storage_dir;

/// MAC context of an encrypted per-bot storage file; the bot id is appended, so one bot's file
/// is not accepted as another bot's.
const BOT_STORAGE_CONTEXT: &[u8] = b"rustgram.bot_storage.v1\0";

/// MAC context of the marker written once the plaintext files of older builds are migrated.
const BOT_STORAGE_MIGRATED_CONTEXT: &[u8] = b"rustgram.bot_storage.migrated.v1\0";

/// Marker file name inside the account's bot storage directory.
const BOT_STORAGE_MIGRATED_MARKER: &str = "plaintext_migrated.tdef";

fn bot_storage_context(bot_id: u64) -> Vec<u8> {
    let mut context = BOT_STORAGE_CONTEXT.to_vec();
    context.extend_from_slice(&bot_id.to_le_bytes());
    context
}

/// Path of `bot_id`'s encrypted storage file (`<dir>/bot_storage_<bot_id>.tdef`).
#[must_use]
pub fn bot_storage_file_path(dir: &Path, bot_id: u64) -> PathBuf {
    dir.join(format!("bot_storage_{bot_id}.tdef"))
}

/// Path of the plaintext JSON file older builds wrote for `bot_id`.
fn legacy_plaintext_path(dir: &Path, bot_id: u64) -> PathBuf {
    dir.join(format!("bot_storage_{bot_id}.json"))
}

/// The account storage key registered for `dir` (the account's bot storage directory, see
/// `LiveMediaCache::register_account_dir`). `None` when no account key is known: nothing is then
/// written to or read from disk.
fn bot_storage_key(dir: &Path) -> Option<StorageEncryptionKey> {
    lookup_cache_key_for_path(dir)
}

/// Bot id of a legacy plaintext file name (`bot_storage_<bot_id>.json`).
fn legacy_plaintext_bot_id(file_name: &str) -> Option<u64> {
    file_name
        .strip_prefix("bot_storage_")?
        .strip_suffix(".json")?
        .parse()
        .ok()
}

fn write_encrypted_bot_file(
    dir: &Path,
    bot_id: u64,
    key: &StorageEncryptionKey,
    data: &[u8],
) -> Result<PathBuf, String> {
    let path = bot_storage_file_path(dir, bot_id);
    let tmp = dir.join(format!("bot_storage_{bot_id}.tdef.tmp"));
    write_encrypted_blob_bound(&tmp, key, &bot_storage_context(bot_id), data).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("failed to write {}: {e}", tmp.display())
    })?;
    if let Err(rename_err) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&path);
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("failed to finalize {} ({rename_err}): {e}", path.display())
        })?;
    }
    Ok(path)
}

/// Encrypts the plaintext `bot_storage_<bot_id>.json` files older builds left in `dir`, deletes
/// the plaintext, and writes a marker authenticated with `key`. Once the marker is present,
/// plaintext files are only deleted, never read: a `.json` planted later cannot feed values to a
/// Mini App. A bot that already has an encrypted file keeps it.
fn migrate_legacy_plaintext(dir: &Path, key: &StorageEncryptionKey) {
    let marker = dir.join(BOT_STORAGE_MIGRATED_MARKER);
    let already_migrated =
        read_encrypted_blob_bound(&marker, key, BOT_STORAGE_MIGRATED_CONTEXT).is_ok();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut all_removed = true;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let path = entry.path();
        if name.starts_with("bot_storage_") && name.ends_with(".json.tmp") {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let Some(bot_id) = legacy_plaintext_bot_id(name) else {
            continue;
        };
        if !already_migrated
            && !bot_storage_file_path(dir, bot_id).exists()
            && let Ok(bytes) = std::fs::read(&path)
        {
            let mut parsed = BotLocalStorage::new();
            if parsed.deserialize_bot(bot_id, &bytes).is_ok()
                && !parsed.keys(bot_id).is_empty()
                && let Ok(data) = parsed.serialize_bot(bot_id)
                && let Err(err) = write_encrypted_bot_file(dir, bot_id, key, &data)
            {
                // Keep the plaintext for the next attempt rather than lose the data.
                tracing::warn!(bot_id, %err, "Failed to migrate plaintext BotLocalStorage");
                all_removed = false;
                continue;
            }
        }
        if std::fs::remove_file(&path).is_err() && path.exists() {
            all_removed = false;
        }
    }
    if !already_migrated && all_removed {
        let _ = write_encrypted_blob_bound(&marker, key, BOT_STORAGE_MIGRATED_CONTEXT, b"1");
    }
}

/// Per-bot key-value storage enforcing tdesktop's 5 MiB quota (`InlineBots::Storage`).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BotLocalStorage {
    bots: BTreeMap<u64, BTreeMap<String, String>>,
}

impl BotLocalStorage {
    /// Creates an empty [`BotLocalStorage`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads `key` for `bot_id`.
    #[must_use]
    pub fn read(&self, bot_id: u64, key: &str) -> Option<&str> {
        self.bots
            .get(&bot_id)
            .and_then(|map| map.get(key))
            .map(String::as_str)
    }

    /// Returns total bytes currently used by `bot_id` (`sum(key.len() + value.len())`).
    #[must_use]
    pub fn total_bytes(&self, bot_id: u64) -> usize {
        self.bots
            .get(&bot_id)
            .map_or(0, |map| map.iter().map(|(k, v)| k.len() + v.len()).sum())
    }

    /// Writes `value` (or removes `key` when `None`) for `bot_id`, enforcing [`MAX_BOT_STORAGE_BYTES`].
    ///
    /// Returns `false` when the write would exceed the 5 MiB per-bot quota.
    pub fn write(&mut self, bot_id: u64, key: &str, value: Option<String>) -> bool {
        if let Some(ref val) = value
            && val.len() > MAX_BOT_STORAGE_BYTES
        {
            return false;
        }
        let current_total = self.total_bytes(bot_id);
        let existing_len = self
            .bots
            .get(&bot_id)
            .and_then(|map| map.get(key))
            .map_or(0, |v| key.len() + v.len());

        let new_entry_len = value.as_ref().map_or(0, |v| key.len() + v.len());
        let next_total = current_total
            .saturating_sub(existing_len)
            .saturating_add(new_entry_len);
        if next_total > MAX_BOT_STORAGE_BYTES {
            return false;
        }

        match value {
            Some(val) => {
                self.bots
                    .entry(bot_id)
                    .or_default()
                    .insert(key.to_owned(), val);
            }
            None => {
                if let Some(map) = self.bots.get_mut(&bot_id) {
                    map.remove(key);
                    if map.is_empty() {
                        self.bots.remove(&bot_id);
                    }
                }
            }
        }
        true
    }

    /// Clears all keys stored for `bot_id`.
    pub fn clear(&mut self, bot_id: u64) {
        self.bots.remove(&bot_id);
    }

    /// Lists all keys stored for `bot_id` in sorted order.
    #[must_use]
    pub fn keys(&self, bot_id: u64) -> Vec<String> {
        self.bots
            .get(&bot_id)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Serializes a single bot's key-value table into JSON bytes.
    pub fn serialize_bot(&self, bot_id: u64) -> Result<Vec<u8>, String> {
        let empty = BTreeMap::new();
        let map = self.bots.get(&bot_id).unwrap_or(&empty);
        serde_json::to_vec(map).map_err(|e| format!("failed to serialize bot storage: {e}"))
    }

    /// Restores a single bot's key-value table from serialized JSON bytes, discarding entries if
    /// the quota is exceeded.
    pub fn deserialize_bot(&mut self, bot_id: u64, bytes: &[u8]) -> Result<(), String> {
        let map: BTreeMap<String, String> = serde_json::from_slice(bytes)
            .map_err(|e| format!("failed to deserialize bot storage: {e}"))?;
        let total: usize = map.iter().map(|(k, v)| k.len() + v.len()).sum();
        if total > MAX_BOT_STORAGE_BYTES {
            return Err("bot storage payload exceeds 5 MiB quota".to_owned());
        }
        if map.is_empty() {
            self.bots.remove(&bot_id);
        } else {
            self.bots.insert(bot_id, map);
        }
        Ok(())
    }

    /// Persists `bot_id`'s storage under `<dir>/bot_storage_<bot_id>.tdef` (or removes the file
    /// when `bot_id` has no stored keys), encrypted and authenticated with the account storage
    /// key registered for `dir`, like tdesktop's `Storage::Account::writeBotStorage`
    /// (`inline_bots/inline_bot_storage.cpp:117-124`). Mini Apps keep login and session tokens
    /// here, so nothing is written when no account key is known: the call fails and the values
    /// stay in memory only.
    pub fn save_bot_to_dir(&self, bot_id: u64, dir: &Path) -> Result<PathBuf, String> {
        let path = bot_storage_file_path(dir, bot_id);
        let is_empty = self.bots.get(&bot_id).is_none_or(BTreeMap::is_empty);
        if is_empty {
            Self::delete_bot_from_dir(bot_id, dir)?;
            return Ok(path);
        }
        let key = bot_storage_key(dir).ok_or_else(|| {
            format!(
                "no account storage key for {}: bot storage is kept in memory only",
                dir.display()
            )
        })?;
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
        let data = self.serialize_bot(bot_id)?;
        let path = write_encrypted_bot_file(dir, bot_id, &key, &data)?;
        let _ = std::fs::remove_file(legacy_plaintext_path(dir, bot_id));
        Ok(path)
    }

    /// Removes `bot_id`'s persisted storage file from `dir` if present (and the plaintext file
    /// an older build may have left).
    pub fn delete_bot_from_dir(bot_id: u64, dir: &Path) -> Result<(), String> {
        for path in [
            bot_storage_file_path(dir, bot_id),
            legacy_plaintext_path(dir, bot_id),
        ] {
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("failed to remove {}: {e}", path.display()))?;
            }
        }
        Ok(())
    }

    /// Loads `bot_id`'s storage from `<dir>/bot_storage_<bot_id>.tdef` if it exists.
    ///
    /// The first load with an account key migrates the plaintext files of older builds in `dir`
    /// (all bots) to encrypted files and deletes the plaintext. A file that fails authentication
    /// (altered, another bot's, another account's key) is deleted, not trusted. Without an
    /// account key nothing is read.
    pub fn load_bot_from_dir(&mut self, bot_id: u64, dir: &Path) -> Result<(), String> {
        let Some(key) = bot_storage_key(dir) else {
            return Ok(());
        };
        migrate_legacy_plaintext(dir, &key);
        let path = bot_storage_file_path(dir, bot_id);
        if !path.exists() {
            return Ok(());
        }
        match read_encrypted_blob_bound(&path, &key, &bot_storage_context(bot_id)) {
            Ok(bytes) => self.deserialize_bot(bot_id, &bytes),
            Err(err) => {
                let _ = std::fs::remove_file(&path);
                Err(format!(
                    "dropped {}: it failed authentication ({err})",
                    path.display()
                ))
            }
        }
    }
}

/// Download item action matching `Ui::BotWebView::DownloadsAction` (`inline_bot_downloads.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotDownloadAction {
    /// Reveal downloaded file in system file manager.
    Open,
    /// Cancel an active download.
    Cancel,
    /// Retry a failed download.
    Retry,
}

/// Aggregated download progress for a bot's active Mini App session (`DownloadsProgress`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BotDownloadsProgress {
    /// Total expected bytes across active downloads.
    pub total: i64,
    /// Downloaded bytes so far.
    pub ready: i64,
    /// Whether any download is currently active.
    pub loading: bool,
}

/// Single Mini App file download record (`DownloadsEntry`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotDownloadEntry {
    /// Monotonic local download ID.
    pub id: u32,
    /// Source HTTP(S) URL requested by the Mini App.
    pub url: String,
    /// Local destination path chosen by the user.
    pub path: String,
    /// Total file size in bytes.
    pub total: i64,
    /// Bytes downloaded so far.
    pub ready: i64,
    /// Whether transfer is currently in progress.
    pub loading: bool,
    /// Whether transfer ended in error.
    pub failed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct SerializedDownloads {
    version: u32,
    bots: BTreeMap<u64, Vec<BotDownloadEntry>>,
}

/// Per-bot Mini App download tracker (`InlineBots::Downloads`).
#[derive(Debug, Default, Clone)]
pub struct BotDownloadsManager {
    next_id: u32,
    lists: BTreeMap<u64, Vec<BotDownloadEntry>>,
}

impl BotDownloadsManager {
    /// Creates an empty [`BotDownloadsManager`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts tracking a new Mini App download and returns its assigned ID.
    pub fn start(&mut self, bot_id: u64, url: impl Into<String>, path: impl Into<String>) -> u32 {
        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        let list = self.lists.entry(bot_id).or_default();
        list.push(BotDownloadEntry {
            id,
            url: url.into(),
            path: path.into(),
            total: 0,
            ready: 0,
            loading: true,
            failed: false,
        });
        id
    }

    /// Updates byte progress for `id`.
    pub fn update_progress(&mut self, bot_id: u64, id: u32, total: i64, ready: i64) {
        if let Some(entry) = self.find_mut(bot_id, id) {
            if total < 0 || ready < 0 || ready > total {
                entry.loading = false;
                entry.failed = true;
                return;
            }
            entry.total = total;
            entry.ready = ready;
            entry.loading = ready < total;
            entry.failed = false;
        }
    }

    /// Marks download `id` as finished with final byte size `total`.
    pub fn complete(&mut self, bot_id: u64, id: u32, total: i64) {
        if let Some(entry) = self.find_mut(bot_id, id) {
            if total <= 0 {
                entry.loading = false;
                entry.failed = true;
            } else {
                entry.total = total;
                entry.ready = total;
                entry.loading = false;
                entry.failed = false;
            }
        }
    }

    /// Marks download `id` as failed; when `cancel == true`, removes the entry from the list.
    pub fn fail(&mut self, bot_id: u64, id: u32, cancel: bool) {
        let Some(list) = self.lists.get_mut(&bot_id) else {
            return;
        };
        if cancel {
            list.retain(|e| e.id != id);
        } else if let Some(entry) = list.iter_mut().find(|e| e.id == id) {
            entry.loading = false;
            entry.failed = true;
        }
    }

    /// Applies a [`BotDownloadAction`] to `id` (`Retry` resets loading state, `Cancel` removes).
    pub fn apply_action(&mut self, bot_id: u64, id: u32, action: BotDownloadAction) {
        match action {
            BotDownloadAction::Cancel => self.fail(bot_id, id, true),
            BotDownloadAction::Retry => {
                if let Some(entry) = self.find_mut(bot_id, id) {
                    entry.loading = true;
                    entry.failed = false;
                    entry.ready = 0;
                }
            }
            BotDownloadAction::Open => {}
        }
    }

    /// Returns all download entries for `bot_id`.
    #[must_use]
    pub fn list(&self, bot_id: u64) -> &[BotDownloadEntry] {
        self.lists.get(&bot_id).map_or(&[], Vec::as_slice)
    }

    /// Computes aggregate [`BotDownloadsProgress`] across active items for `bot_id`.
    #[must_use]
    pub fn progress(&self, bot_id: u64) -> BotDownloadsProgress {
        let mut out = BotDownloadsProgress::default();
        for entry in self.list(bot_id) {
            if entry.loading {
                out.loading = true;
                out.total = out.total.saturating_add(entry.total);
                out.ready = out.ready.saturating_add(entry.ready);
            }
        }
        out
    }

    /// Serializes completed downloads (version 1) to JSON bytes.
    pub fn serialize(&self) -> Result<Vec<u8>, String> {
        let mut completed: BTreeMap<u64, Vec<BotDownloadEntry>> = BTreeMap::new();
        for (&bot_id, list) in &self.lists {
            let done: Vec<BotDownloadEntry> = list
                .iter()
                .filter(|e| !e.loading && !e.failed && e.total > 0 && e.ready == e.total)
                .cloned()
                .collect();
            if !done.is_empty() {
                completed.insert(bot_id, done);
            }
        }
        serde_json::to_vec(&SerializedDownloads {
            version: BOT_DOWNLOADS_VERSION,
            bots: completed,
        })
        .map_err(|e| format!("failed to serialize bot downloads: {e}"))
    }

    /// Deserializes completed downloads from JSON bytes, validating version and bounds.
    pub fn deserialize(bytes: &[u8]) -> Result<Self, String> {
        let parsed: SerializedDownloads = serde_json::from_slice(bytes)
            .map_err(|e| format!("failed to deserialize bot downloads: {e}"))?;
        if parsed.version != BOT_DOWNLOADS_VERSION {
            return Err(format!("unsupported downloads version {}", parsed.version));
        }
        if parsed.bots.len() > MAX_DOWNLOADS_BOTS {
            return Err("downloads bot count exceeds limit".to_owned());
        }
        let mut mgr = Self::new();
        for (bot_id, entries) in parsed.bots {
            if entries.len() > MAX_DOWNLOADS_PER_BOT {
                return Err("downloads per-bot count exceeds limit".to_owned());
            }
            let mut assigned = Vec::with_capacity(entries.len());
            for mut entry in entries {
                mgr.next_id = mgr.next_id.saturating_add(1);
                entry.id = mgr.next_id;
                entry.loading = false;
                entry.failed = false;
                assigned.push(entry);
            }
            mgr.lists.insert(bot_id, assigned);
        }
        Ok(mgr)
    }

    fn find_mut(&mut self, bot_id: u64, id: u32) -> Option<&mut BotDownloadEntry> {
        self.lists
            .get_mut(&bot_id)
            .and_then(|list| list.iter_mut().find(|e| e.id == id))
    }
}

/// Request arguments for the Mini App `web_app_request_file_download` confirmation box
/// (`InlineBots::DownloadBoxArgs` in `inline_bot_downloads.cpp:381-427`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadFilePrompt {
    /// Bot display name shown in the confirmation prompt.
    pub bot_name: String,
    /// Suggested file name from the Mini App.
    pub file_name: String,
    /// Download source URL.
    pub url: String,
}

/// Preview state for `web_app_send_prepared_message` confirmation dialog
/// (`InlineBots::PreparedPreviewBox` in `inline_bot_confirm_prepared.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedMessagePrompt {
    /// Bot user ID.
    pub bot_id: u64,
    /// Bot display name.
    pub bot_name: String,
    /// Prepared message identifier passed to `messages.getPreparedInlineMessage`.
    pub prepared_id: String,
    /// Inline query ID returned by `messages.getPreparedInlineMessage`.
    pub query_id: i64,
    /// Inline result identifier returned by `messages.getPreparedInlineMessage`.
    pub result_id: String,
    /// Inline result title / preview heading.
    pub preview_title: String,
    /// Rendered message text preview shown inside the confirmation bubble.
    pub preview_text: String,
    /// Target chat types permitted by the bot for this prepared inline message.
    pub allowed_peer_types: Vec<tg_webview::InlineChatType>,
    /// Chats offered in the recipient chooser, already filtered by `allowed_peer_types`.
    pub candidates: Vec<(PeerId, String)>,
    /// Recipient chosen in the chooser; "Send" is refused until one is set.
    pub recipient: Option<(PeerId, String)>,
}

/// Renders the body of the Mini App file download confirmation modal (`DownloadFileBox`).
pub fn render_download_file_confirm_body(
    ui: &mut Ui,
    palette: &TgPalette,
    prompt: &DownloadFilePrompt,
) {
    let fg = color_to_egui(palette.window_fg);
    let sub = color_to_egui(palette.window_sub_text_fg);
    let card_bg = color_to_egui(palette.window_bg_over);

    ui.label(
        egui::RichText::new(format!(
            "Do you want to download this file from {}?",
            prompt.bot_name
        ))
        .font(font_id_from_style(st::normal_font))
        .color(fg),
    );
    ui.add_space(8.0);
    egui::Frame::NONE
        .fill(card_bg)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(&prompt.file_name)
                    .font(font_id_from_style(st::semibold_font))
                    .color(fg),
            );
            ui.label(
                egui::RichText::new(&prompt.url)
                    .font(font_id_from_style(st::normal_font))
                    .color(sub),
            );
        });
}

/// Renders the body of the Mini App prepared inline message confirmation modal
/// (`PreparedPreviewBox` in `inline_bot_confirm_prepared.cpp:177-238`) together with the
/// recipient chooser (`Window::PrepareChooseRecipientBox`). Returns the chat the user clicked.
pub fn render_prepared_message_confirm_body(
    ui: &mut Ui,
    palette: &TgPalette,
    prompt: &PreparedMessagePrompt,
) -> Option<(PeerId, String)> {
    let fg = color_to_egui(palette.window_fg);
    let sub = color_to_egui(palette.window_sub_text_fg);
    let bubble_bg = color_to_egui(palette.msg_in_bg);
    let accent = color_to_egui(palette.window_active_text_fg);

    egui::Frame::NONE
        .fill(bubble_bg)
        .corner_radius(CornerRadius::same(10))
        .stroke(Stroke::new(1.0, color_to_egui(palette.shadow_fg)))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(format!("via @{}", prompt.bot_name))
                    .font(font_id_from_style(st::semibold_font))
                    .color(accent),
            );
            if !prompt.preview_title.is_empty() {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(&prompt.preview_title)
                        .font(font_id_from_style(st::semibold_font))
                        .color(fg),
                );
            }
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(&prompt.preview_text)
                    .font(font_id_from_style(st::normal_font))
                    .color(fg),
            );
        });

    ui.add_space(10.0);
    if let Some((_, recipient_name)) = &prompt.recipient {
        let (rect, _) = ui.allocate_exact_size(
            egui::Vec2::new(ui.available_width().max(180.0), 34.0),
            egui::Sense::hover(),
        );
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(6),
            color_to_egui(palette.window_bg_over),
        );
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(6),
            Stroke::new(1.0, color_to_egui(palette.shadow_fg)),
            StrokeKind::Inside,
        );
        ui.painter().text(
            egui::Pos2::new(rect.left() + 10.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            format!("Recipient: {recipient_name}"),
            font_id_from_style(st::semibold_font),
            fg,
        );
    } else {
        ui.label(
            egui::RichText::new(format!(
                "{} prepared this message for you to share. Choose a chat to send it to.",
                prompt.bot_name
            ))
            .font(font_id_from_style(st::normal_font))
            .color(sub),
        );
    }
    render_prepared_recipient_list(ui, palette, prompt)
}

fn render_prepared_recipient_list(
    ui: &mut Ui,
    palette: &TgPalette,
    prompt: &PreparedMessagePrompt,
) -> Option<(PeerId, String)> {
    let fg = color_to_egui(palette.window_fg);
    let sub = color_to_egui(palette.window_sub_text_fg);
    ui.add_space(8.0);
    if prompt.candidates.is_empty() {
        ui.label(
            egui::RichText::new("No chats can receive this message.")
                .font(font_id_from_style(st::normal_font))
                .color(sub),
        );
        return None;
    }
    let selected = prompt.recipient.as_ref().map(|(peer, _)| *peer);
    let mut clicked = None;
    egui::ScrollArea::vertical()
        .id_salt("tg_prepared_message_recipients")
        .max_height(180.0)
        .show(ui, |ui| {
            for (peer, title) in &prompt.candidates {
                let text = egui::RichText::new(title)
                    .font(font_id_from_style(st::normal_font))
                    .color(fg);
                if ui.selectable_label(selected == Some(*peer), text).clicked() {
                    clicked = Some((*peer, title.clone()));
                }
            }
        });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key(seed: u8) -> StorageEncryptionKey {
        tg_storage::cache::media_cache::derive_media_cache_key(
            &tg_storage::AuthKey::new([seed; 256]),
            "data",
        )
    }

    fn dir_contains(dir: &Path, needle: &[u8]) -> bool {
        std::fs::read_dir(dir)
            .expect("read dir")
            .flatten()
            .filter(|e| e.path().is_file())
            .any(|e| {
                std::fs::read(e.path())
                    .expect("read file")
                    .windows(needle.len())
                    .any(|w| w == needle)
            })
    }

    #[test]
    fn sec3_b2_bot_storage_is_encrypted_with_the_account_key() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("bot_storage").join("user_1");
        let key = test_key(0x21);
        tg_storage::cache::media_cache::register_cache_key(&dir, key.clone());

        let mut storage = BotLocalStorage::new();
        assert!(storage.write(
            1001,
            "session_token",
            Some("SECRET_TOKEN_abc123".to_owned())
        ));
        let path = storage.save_bot_to_dir(1001, &dir).expect("save");
        assert_eq!(path, bot_storage_file_path(&dir, 1001));
        assert!(path.is_file());
        assert!(
            !legacy_plaintext_path(&dir, 1001).exists(),
            "no plaintext JSON file may be written"
        );
        let raw = std::fs::read(&path).expect("read raw");
        assert!(raw.starts_with(b"TDEF"));
        assert!(!dir_contains(&dir, b"SECRET_TOKEN"));
        assert!(!dir_contains(&dir, b"session_token"));

        let mut loaded = BotLocalStorage::new();
        loaded.load_bot_from_dir(1001, &dir).expect("load");
        assert_eq!(
            loaded.read(1001, "session_token"),
            Some("SECRET_TOKEN_abc123")
        );

        // One bot's file is not accepted as another bot's.
        std::fs::copy(&path, bot_storage_file_path(&dir, 2002)).expect("copy to other bot");
        let mut other = BotLocalStorage::new();
        assert!(other.load_bot_from_dir(2002, &dir).is_err());
        assert_eq!(other.read(2002, "session_token"), None);
        assert!(
            !bot_storage_file_path(&dir, 2002).exists(),
            "a file that fails authentication is deleted"
        );

        // An altered file is dropped, not trusted.
        let mut altered = raw.clone();
        let last = altered.len() - 20;
        altered[last] ^= 0x01;
        std::fs::write(&path, &altered).expect("write altered");
        let mut reloaded = BotLocalStorage::new();
        assert!(reloaded.load_bot_from_dir(1001, &dir).is_err());
        assert_eq!(reloaded.read(1001, "session_token"), None);
        assert!(!path.exists());

        // Another account's key cannot read the file either.
        std::fs::write(&path, &raw).expect("restore");
        tg_storage::cache::media_cache::register_cache_key(&dir, test_key(0x22));
        let mut wrong_key = BotLocalStorage::new();
        assert!(wrong_key.load_bot_from_dir(1001, &dir).is_err());
        assert_eq!(wrong_key.read(1001, "session_token"), None);
        tg_storage::cache::media_cache::unregister_cache_key(&dir);
    }

    #[test]
    fn sec3_b2_plaintext_bot_storage_is_migrated_and_deleted() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("bot_storage").join("user_2");
        std::fs::create_dir_all(&dir).expect("mkdir");
        // Files written by an older build: two bots, plus a stale temp file.
        std::fs::write(
            legacy_plaintext_path(&dir, 1001),
            br#"{"session_token":"LEGACY_TOKEN_1001","theme":"dark"}"#,
        )
        .expect("write legacy 1001");
        std::fs::write(
            legacy_plaintext_path(&dir, 3003),
            br#"{"auth":"LEGACY_TOKEN_3003"}"#,
        )
        .expect("write legacy 3003");
        std::fs::write(
            dir.join("bot_storage_1001.json.tmp"),
            br#"{"session_token":"LEGACY_TOKEN_TMP"}"#,
        )
        .expect("write legacy tmp");
        tg_storage::cache::media_cache::register_cache_key(&dir, test_key(0x23));

        // Opening bot 1001 migrates every bot's plaintext file, not only its own.
        let mut storage = BotLocalStorage::new();
        storage
            .load_bot_from_dir(1001, &dir)
            .expect("load migrates");
        assert_eq!(
            storage.read(1001, "session_token"),
            Some("LEGACY_TOKEN_1001")
        );
        assert_eq!(storage.read(1001, "theme"), Some("dark"));
        assert!(!legacy_plaintext_path(&dir, 1001).exists());
        assert!(!legacy_plaintext_path(&dir, 3003).exists());
        assert!(!dir.join("bot_storage_1001.json.tmp").exists());
        assert!(bot_storage_file_path(&dir, 1001).is_file());
        assert!(bot_storage_file_path(&dir, 3003).is_file());
        assert!(
            !dir_contains(&dir, b"LEGACY_TOKEN"),
            "no plaintext token may remain on disk"
        );
        let mut other = BotLocalStorage::new();
        other
            .load_bot_from_dir(3003, &dir)
            .expect("load migrated 3003");
        assert_eq!(other.read(3003, "auth"), Some("LEGACY_TOKEN_3003"));

        // After the migration a planted plaintext file is deleted, never read.
        std::fs::write(
            legacy_plaintext_path(&dir, 4004),
            br#"{"auth":"PLANTED_BY_ATTACKER"}"#,
        )
        .expect("plant");
        let mut planted = BotLocalStorage::new();
        planted.load_bot_from_dir(4004, &dir).expect("load planted");
        assert_eq!(planted.read(4004, "auth"), None);
        assert!(!legacy_plaintext_path(&dir, 4004).exists());
        assert!(!bot_storage_file_path(&dir, 4004).exists());
        tg_storage::cache::media_cache::unregister_cache_key(&dir);
    }

    #[test]
    fn sec3_b2_bot_storage_without_account_key_stays_in_memory() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("bot_storage").join("user_3");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let mut storage = BotLocalStorage::new();
        assert!(storage.write(1001, "token", Some("MEMORY_ONLY_TOKEN".to_owned())));
        assert!(
            storage.save_bot_to_dir(1001, &dir).is_err(),
            "without an account key nothing may be written"
        );
        assert_eq!(std::fs::read_dir(&dir).expect("read dir").count(), 0);
        assert_eq!(storage.read(1001, "token"), Some("MEMORY_ONLY_TOKEN"));

        // A plaintext file is neither read nor migrated without a key.
        std::fs::write(legacy_plaintext_path(&dir, 1001), br#"{"token":"OLD"}"#).expect("write");
        let mut loaded = BotLocalStorage::new();
        loaded
            .load_bot_from_dir(1001, &dir)
            .expect("load without key");
        assert_eq!(loaded.read(1001, "token"), None);
    }

    #[test]
    fn bot_local_storage_quota_and_persistence() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("bot_storage").join("user_4");
        tg_storage::cache::media_cache::register_cache_key(&dir, test_key(0x24));
        let dir = dir.as_path();
        let mut storage = BotLocalStorage::new();

        assert!(storage.write(1001, "theme", Some("dark".to_owned())));
        assert!(storage.write(1001, "token", Some("abc123".to_owned())));
        assert_eq!(storage.read(1001, "theme"), Some("dark"));
        assert_eq!(storage.keys(1001), vec!["theme", "token"]);

        // Reject values exceeding 5 MiB quota.
        let huge = "x".repeat(MAX_BOT_STORAGE_BYTES + 1);
        assert!(!storage.write(1001, "huge", Some(huge)));
        assert_eq!(storage.read(1001, "huge"), None);

        // Save and reload from disk.
        storage
            .save_bot_to_dir(1001, dir)
            .expect("save bot storage");
        let mut loaded = BotLocalStorage::new();
        loaded
            .load_bot_from_dir(1001, dir)
            .expect("load bot storage");
        assert_eq!(loaded.read(1001, "theme"), Some("dark"));
        assert_eq!(loaded.read(1001, "token"), Some("abc123"));

        // Removing a key updates total bytes cleanly.
        assert!(loaded.write(1001, "token", None));
        assert_eq!(loaded.read(1001, "token"), None);
        loaded.clear(1001);
        assert_eq!(loaded.total_bytes(1001), 0);
        // Saving an empty table removes the file.
        loaded.save_bot_to_dir(1001, dir).expect("save empty");
        assert!(!bot_storage_file_path(dir, 1001).exists());
        tg_storage::cache::media_cache::unregister_cache_key(dir);
    }

    #[test]
    fn bot_downloads_lifecycle_and_serialization() {
        let mut mgr = BotDownloadsManager::new();
        let id1 = mgr.start(500, "https://bot.example/report.pdf", "C:\\tmp\\report.pdf");
        let id2 = mgr.start(500, "https://bot.example/temp.bin", "C:\\tmp\\temp.bin");

        mgr.update_progress(500, id1, 1000, 400);
        let prog = mgr.progress(500);
        assert!(prog.loading);
        assert_eq!(prog.total, 1000);
        assert_eq!(prog.ready, 400);

        mgr.complete(500, id1, 1000);
        mgr.apply_action(500, id2, BotDownloadAction::Cancel);
        assert_eq!(mgr.list(500).len(), 1);
        assert!(!mgr.progress(500).loading);

        let bytes = mgr.serialize().expect("serialize downloads");
        let restored = BotDownloadsManager::deserialize(&bytes).expect("deserialize downloads");
        assert_eq!(restored.list(500).len(), 1);
        assert_eq!(restored.list(500)[0].url, "https://bot.example/report.pdf");
        assert_eq!(restored.list(500)[0].total, 1000);
    }
}
