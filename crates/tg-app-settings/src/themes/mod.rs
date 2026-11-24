//! Telegram Desktop theme editor, theme previews, chat background picker/preview,
//! single-chat emoji theme picker, night-mode schedule, and `ChatBackground` rendering API.
//!
//! Port origin:
//! - `Telegram/SourceFiles/window/themes/window_theme.cpp` ([`chat_background`], [`night_schedule`])
//! - `Telegram/SourceFiles/window/themes/window_theme_editor.cpp`,
//!   `window_theme_editor_block.cpp`, `window_theme_editor_box.cpp` ([`editor`])
//! - `Telegram/SourceFiles/ui/widgets/color_editor.cpp` ([`color_editor`])
//! - `Telegram/SourceFiles/window/themes/window_theme_preview.cpp` ([`preview`])
//! - `Telegram/SourceFiles/boxes/background_box.cpp`,
//!   `background_preview_box.cpp` ([`background_picker`])
//! - `Telegram/SourceFiles/ui/chat/choose_theme_controller.cpp` ([`chat_theme_picker`])
//!
//! # Using [`ChatBackground`] from `history/`
//!
//! Other modules (such as `crates/tg-app/src/history/`) can paint the active chat background
//! without depending on theme-editor internals:
//!
//! ```ignore
//! if let Ok(mut bg) = crate::themes::active_chat_background(ui.ctx()).lock() {
//!     bg.paint(ui, canvas_rect, Some(peer_id));
//! }
//! ```

pub use tg_app_core::themes::backend;
pub mod background_picker;
pub mod chat_background;
pub mod chat_theme_picker;
pub mod color_editor;
pub mod editor;
pub mod night_schedule;
pub mod preview;
pub mod share_theme_picker;
#[cfg(test)]
mod tests_wp62;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{
    Align2, Color32, Context, CornerRadius, CursorIcon, FontId, Frame, Id, Key, Margin, Modifiers,
    Order, Pos2, Rect, Role, ScrollArea, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo,
};
use tg_theme::{EmbeddedType, ThemeInstance, embedded_themes, prepare_embedded_palette};
use tg_ui::theme::{Colors, ThemeManager};

pub use backend::{
    ChatThemeChoice, CloudBaseTheme, CloudDocumentRef, CloudThemeCreateRequest,
    CloudThemeInstallRequest, CloudThemeRecord, CloudThemeSettingsInput, CloudThemeUpdateRequest,
    CloudThemesResult, CloudWallPaperRecord, CloudWallPapersResult, PeerColorOptionRecord,
    PeerColorsResult, PeerProfileColorOptionRecord, PeerProfileColorSet, PeerProfileColorsResult,
    ShareThemeBackdropOption, ShareThemeBackdropsResult, ShareThemeChoice, ShareThemeReaction,
    ThemesBackend, UniqueGiftChatThemeRecord, UniqueGiftChatThemesResult, WallPaperLookup,
};

pub use background_picker::{
    BackgroundApplyScope, BackgroundCategory, BackgroundGalleryItem, BackgroundPickerOutput,
    BackgroundPickerState, BackgroundPreviewOutput, BackgroundPreviewState, builtin_gallery_items,
};
pub use chat_background::{
    BackgroundConfig, ChatBackground, PatternKind, active_chat_background, aspect_fill_uv,
    generate_pattern_mask, rgba_to_color_image,
};
pub use chat_theme_picker::{
    ChooseChatThemeOutput, ChooseChatThemeState, EmojiChatTheme, UNIQUE_GIFT_THEMES_PAGE_LIMIT,
    UniqueGiftChatTheme, builtin_emoji_chat_themes,
};
pub use color_editor::{ColorEditorMode, ColorEditorState, draw_checkerboard};
pub use editor::{
    ActiveColorEdit, EditorBlockKind, EditorRow, SaveCloudThemeBoxState, ThemeEditorOutput,
    ThemeEditorState, split_search_words,
};
pub use night_schedule::{
    NightModeScheduler, NightScheduleEvent, NightScheduleMode, format_minutes_hhmm,
};
pub use preview::{
    PreviewType, ThemePreviewCache, fill_initials, generate_compact_theme_card,
    generate_pattern_wallpaper_preview, generate_theme_preview,
};
pub use share_theme_picker::{
    DEFAULT_SHARE_CENTER_COLOR, DEFAULT_SHARE_EDGE_COLOR, MAX_SHARE_THEME_EMOJIS,
    ShareThemePickerOutput, ShareThemePickerState, ShareThemeTab, builtin_share_theme_backdrops,
};

/// Active tab inside [`ThemesStudio`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemesTab {
    /// Live palette editor (`window_theme_editor.cpp`).
    #[default]
    Editor,
    /// Chat background gallery + interactive message preview (`background_box.cpp` + `background_preview_box.cpp`).
    Backgrounds,
    /// Per-chat emoji theme picker + night-mode schedule (`choose_theme_controller.cpp`).
    ChatThemes,
    /// Rendered desktop previews of built-in and edited themes (`window_theme_preview.cpp`).
    Previews,
}

impl ThemesTab {
    /// Ordered tabs shown in the Theme Studio header.
    pub const ALL: &'static [Self] = &[
        Self::Editor,
        Self::Backgrounds,
        Self::ChatThemes,
        Self::Previews,
    ];

    /// Human-readable tab label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Editor => "Theme Editor",
            Self::Backgrounds => "Backgrounds",
            Self::ChatThemes => "Chat Themes & Night Mode",
            Self::Previews => "Theme Previews",
        }
    }
}

/// Complete Theme Studio panel uniting the live theme editor, background picker & preview,
/// single-chat theme picker, theme preview gallery, and night-mode schedule.
pub struct ThemesStudio {
    /// Active UI tab.
    pub active_tab: ThemesTab,
    /// Live `tg-ui` theme manager driving the whole app when edited.
    pub theme_manager: ThemeManager,
    /// Live `ChatBackground` renderer shared with `history/`.
    pub chat_background: ChatBackground,
    /// Night-mode auto-switch scheduler (`tg-platform` + time window).
    pub night_scheduler: NightModeScheduler,
    /// Live palette editor panel state.
    pub editor: ThemeEditorState,
    /// Chat background picker + preview state.
    pub bg_picker: BackgroundPickerState,
    /// Per-chat emoji and unique-gift theme picker state.
    pub chat_theme_picker: ChooseChatThemeState,
    /// Shareable QR/profile theme backdrop selector state (`info_profile_share_theme.cpp`).
    pub share_theme_picker: ShareThemePickerState,
    /// Selected embedded theme inside the Theme Previews tab.
    pub preview_embedded: EmbeddedType,
    /// Whether the Theme Previews tab shows the live custom edited theme.
    pub preview_show_custom: bool,
    /// Cached rendered theme preview textures.
    pub preview_cache: ThemePreviewCache,
    /// Optional live cloud themes/wallpapers RPC backend (`LiveSession`).
    backend: Option<Arc<dyn ThemesBackend>>,
    /// Cloud themes fetched via `account.getThemes`.
    cloud_themes: Vec<CloudThemeRecord>,
    /// Cloud theme IDs currently saved to the user's account (`account.saveTheme`).
    saved_cloud_theme_ids: HashSet<u64>,
    /// Last `account.getThemes` cache hash.
    cloud_themes_hash: i64,
    /// Last `account.getChatThemes` cache hash.
    cloud_chat_themes_hash: i64,
    /// Last `account.getWallPapers` cache hash.
    cloud_wallpapers_hash: i64,
    /// Whether initial cloud sync ran after attaching a backend.
    cloud_loaded: bool,
    /// Transient toast notification `(text, expires_at_ms)`.
    toast: Option<(String, i64)>,
    started_at: Instant,
    /// Last applied palette generation synced to `egui::Context`.
    applied_generation: u64,
}

impl Default for ThemesStudio {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemesStudio {
    /// Creates a new `ThemesStudio` initialized with Telegram's Classic theme and default sample peer.
    #[must_use]
    pub fn new() -> Self {
        let theme_manager = ThemeManager::new();
        let editor = ThemeEditorState::from_theme_manager(&theme_manager);
        let bg_picker = BackgroundPickerState::new(false, Some((1001, "Eva Summer".to_owned())));
        let chat_theme_picker = ChooseChatThemeState::new(1001, "Eva Summer");
        let share_theme_picker = ShareThemePickerState::new(1001, "Eva Summer");
        Self {
            active_tab: ThemesTab::Editor,
            theme_manager,
            chat_background: ChatBackground::new(),
            night_scheduler: NightModeScheduler::default(),
            editor,
            bg_picker,
            chat_theme_picker,
            share_theme_picker,
            preview_embedded: EmbeddedType::Default,
            preview_show_custom: false,
            preview_cache: ThemePreviewCache::default(),
            backend: None,
            cloud_themes: Vec::new(),
            saved_cloud_theme_ids: HashSet::new(),
            cloud_themes_hash: 0,
            cloud_chat_themes_hash: 0,
            cloud_wallpapers_hash: 0,
            cloud_loaded: false,
            toast: None,
            started_at: Instant::now(),
            applied_generation: 0,
        }
    }

    /// Attaches a live [`ThemesBackend`] (e.g. `live.themes_backend()`), enabling cloud themes,
    /// chat themes, share themes, and cloud wallpapers across all studio tabs.
    pub fn set_backend(&mut self, backend: Arc<dyn ThemesBackend>) {
        self.backend = Some(backend);
        self.cloud_loaded = false;
        self.cloud_chat_themes_hash = 0;
        self.editor.cloud_save.cloud_connected = true;
        self.bg_picker.preview.cloud_synced = true;
        self.chat_theme_picker.reset_cloud_state();
        self.share_theme_picker.reset_cloud_state();
    }

    /// Detaches any live [`ThemesBackend`] and returns to offline local-only mode.
    pub fn clear_backend(&mut self) {
        self.backend = None;
        self.cloud_loaded = false;
        self.cloud_themes.clear();
        self.saved_cloud_theme_ids.clear();
        self.cloud_themes_hash = 0;
        self.cloud_chat_themes_hash = 0;
        self.cloud_wallpapers_hash = 0;
        self.editor.cloud_save.cloud_connected = false;
        self.bg_picker.preview.cloud_synced = false;
        self.chat_theme_picker.reset_cloud_state();
        self.share_theme_picker.reset_cloud_state();
    }

    /// Returns `true` when connected to a live [`ThemesBackend`].
    #[must_use]
    pub fn has_backend(&self) -> bool {
        self.backend.is_some()
    }

    /// Updates the target chat for single-chat backgrounds (`messages.setChatWallPaper`),
    /// per-chat themes (`messages.setChatTheme`), and shareable themes (`messages.setShareTheme`).
    pub fn set_target_peer(&mut self, peer_id: i64, peer_name: impl Into<String>) {
        let name = peer_name.into();
        self.bg_picker.preview.target_peer = Some((peer_id, name.clone()));
        self.chat_theme_picker.peer_id = peer_id;
        self.chat_theme_picker.peer_name = name.clone();
        self.share_theme_picker.peer_id = peer_id;
        self.share_theme_picker.peer_name = name;
    }

    /// Returns the cloud themes currently loaded from `account.getThemes`.
    #[must_use]
    pub fn cloud_themes(&self) -> &[CloudThemeRecord] {
        &self.cloud_themes
    }

    /// Sets cloud themes directly (useful in offline previews and UI tests).
    pub fn set_cloud_themes(&mut self, themes: Vec<CloudThemeRecord>) {
        self.saved_cloud_theme_ids = themes.iter().map(|t| t.theme.id).collect();
        self.cloud_themes = themes;
    }

    /// Refreshes cloud themes (`account.getThemes`), per-chat emoji themes (`account.getChatThemes`),
    /// and cloud wallpapers (`account.getWallPapers`) from the attached [`ThemesBackend`], and
    /// best-effort hydrates unique gift themes, share-theme backdrops/peer colors, and SVG/`.tgv`
    /// pattern bytes for cloud pattern wallpapers.
    pub fn refresh_cloud_catalogs(&mut self) -> Result<(), String> {
        let Some(backend) = self.backend.clone() else {
            return Err("No live ThemesBackend attached.".to_owned());
        };
        let mut errors = Vec::new();

        match backend.get_themes(self.cloud_themes_hash) {
            Ok(CloudThemesResult::Themes { hash, themes }) => {
                self.cloud_themes_hash = hash;
                self.saved_cloud_theme_ids = themes.iter().map(|t| t.theme.id).collect();
                self.cloud_themes = themes;
            }
            Ok(CloudThemesResult::NotModified) => {}
            Err(err) => errors.push(format!("getThemes: {err}")),
        }

        match self
            .chat_theme_picker
            .hydrate_from_backend(backend.as_ref())
        {
            Ok(_) => {
                self.cloud_chat_themes_hash = self.chat_theme_picker.cloud_hash();
            }
            Err(err) => errors.push(format!("getChatThemes: {err}")),
        }

        match backend.get_wallpapers(self.cloud_wallpapers_hash) {
            Ok(CloudWallPapersResult::WallPapers { hash, wallpapers }) => {
                self.cloud_wallpapers_hash = hash;
                self.bg_picker.merge_cloud_wallpapers(&wallpapers);
                let _ = self.bg_picker.hydrate_cloud_pattern_bytes(backend.as_ref());
            }
            Ok(CloudWallPapersResult::NotModified) => {}
            Err(err) => errors.push(format!("getWallPapers: {err}")),
        }

        // Best-effort hydration of WP-58 / WP-62 extended catalogs so older tests that only
        // enqueue getThemes/getChatThemes/getWallPapers continue passing without errors.
        let _ = self
            .chat_theme_picker
            .hydrate_unique_gift_themes(backend.as_ref(), false);
        let _ = self
            .share_theme_picker
            .hydrate_from_backend(backend.as_ref());

        self.cloud_loaded = true;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    /// Explicitly refreshes collectible unique-gift chat themes (`account.getUniqueGiftChatThemes`).
    pub fn refresh_unique_gift_themes(&mut self, load_more: bool) -> Result<usize, String> {
        let Some(backend) = self.backend.clone() else {
            return Err("No live ThemesBackend attached.".to_owned());
        };
        self.chat_theme_picker
            .hydrate_unique_gift_themes(backend.as_ref(), load_more)
    }

    /// Explicitly refreshes share-theme backdrops (`messages.getShareThemeBackdrops`) and
    /// peer colors (`help.getPeerColors`, `help.getPeerProfileColors`).
    pub fn refresh_share_theme_catalogs(&mut self) -> Result<usize, String> {
        let Some(backend) = self.backend.clone() else {
            return Err("No live ThemesBackend attached.".to_owned());
        };
        self.share_theme_picker
            .hydrate_from_backend(backend.as_ref())
    }

    /// Saves (`unsave = false`) or removes (`unsave = true`) a cloud theme in the user's account
    /// (`account.saveTheme`).
    pub fn save_cloud_theme_live(
        &mut self,
        theme_id: u64,
        access_hash: u64,
        unsave: bool,
    ) -> Result<(), String> {
        let Some(backend) = self.backend.clone() else {
            return Err("No live ThemesBackend attached.".to_owned());
        };
        backend.save_theme(theme_id, access_hash, unsave)?;
        if unsave {
            self.saved_cloud_theme_ids.remove(&theme_id);
        } else {
            self.saved_cloud_theme_ids.insert(theme_id);
        }
        Ok(())
    }

    /// Uploads and saves the current edited theme to Telegram (`account.uploadTheme` +
    /// `account.createTheme` or `account.updateTheme`).
    pub fn save_current_cloud_theme(&mut self) -> Result<CloudThemeRecord, String> {
        let Some(backend) = self.backend.clone() else {
            return Err("No live ThemesBackend attached.".to_owned());
        };
        let saved = self.editor.submit_cloud_save(
            &self.theme_manager,
            &mut self.chat_background,
            backend.as_ref(),
        )?;
        if let Some(slot) = self
            .cloud_themes
            .iter_mut()
            .find(|t| t.theme.id == saved.theme.id)
        {
            *slot = saved.clone();
        } else {
            self.cloud_themes.insert(0, saved.clone());
        }
        Ok(saved)
    }

    /// Installs a cloud theme (`account.installTheme`), downloads its `.tdesktop-theme` file
    /// when a document is attached, and applies it to `ThemeManager` & `ChatBackground`.
    pub fn install_cloud_theme(&mut self, record: &CloudThemeRecord) -> Result<(), String> {
        let Some(backend) = self.backend.clone() else {
            return Err("No live ThemesBackend attached.".to_owned());
        };
        let is_dark = self.theme_manager.is_dark();
        backend.install_theme(&CloudThemeInstallRequest {
            dark: is_dark,
            theme_id: (record.theme.id != 0).then_some((record.theme.id, record.theme.access_hash)),
            slug: (!record.theme.slug.is_empty()).then(|| record.theme.slug.clone()),
            base_theme: None,
        })?;
        if let Some(ref doc) = record.document {
            let bytes = backend.download_theme_bytes(doc)?;
            self.editor
                .import_from_bytes(&bytes, &mut self.theme_manager, &mut self.chat_background)
                .map_err(|e| e.to_string())?;
        }
        self.editor.set_cloud(record.theme.clone());
        self.preview_cache.invalidate("custom_live");
        Ok(())
    }

    /// Installs a built-in embedded theme locally and notifies `account.installTheme` when live.
    pub fn install_embedded_theme(&mut self, kind: EmbeddedType) -> Result<(), String> {
        self.theme_manager.set_embedded(kind, None);
        self.chat_background.set_dark_mode(kind.is_dark());
        self.night_scheduler
            .sync_from_theme_manager(&self.theme_manager);
        self.editor = ThemeEditorState::from_theme_manager(&self.theme_manager);
        if let Some(backend) = self.backend.clone() {
            self.editor.cloud_save.cloud_connected = true;
            let base = match kind {
                EmbeddedType::Default => CloudBaseTheme::Classic,
                EmbeddedType::DayBlue => CloudBaseTheme::Day,
                EmbeddedType::Night => CloudBaseTheme::Tinted,
                EmbeddedType::NightGreen => CloudBaseTheme::Night,
            };
            backend.install_theme(&CloudThemeInstallRequest {
                dark: kind.is_dark(),
                theme_id: None,
                slug: None,
                base_theme: Some(base),
            })?;
        }
        Ok(())
    }

    /// Syncs the currently previewed background to Telegram (`account.uploadWallPaper` if it's
    /// a local photo, then `account.installWallPaper` for global or `messages.setChatWallPaper`
    /// for a single chat).
    pub fn apply_preview_background_live(
        &mut self,
        scope: BackgroundApplyScope,
    ) -> Result<(), String> {
        let Some(backend) = self.backend.clone() else {
            return Ok(());
        };
        let mut record = self.bg_picker.preview.to_cloud_record();
        let for_chat = matches!(scope, BackgroundApplyScope::SingleChat { .. });

        if record.document.is_none()
            && !record.is_no_file
            && record.wallpaper.id == tg_theme::CUSTOM_BACKGROUND_ID
            && let Some(jpeg) = self.bg_picker.preview.encode_custom_image_jpeg()
        {
            record = backend.upload_wallpaper(&jpeg, "image/jpeg", for_chat, &record)?;
            self.bg_picker.preview.cloud_record = Some(record.clone());
        }

        match scope {
            BackgroundApplyScope::AllChats => backend.install_wallpaper(&record),
            BackgroundApplyScope::SingleChat { peer_id, for_both } => {
                backend.set_chat_wallpaper(peer_id, Some(&record), for_both, false, None)
            }
        }
    }

    /// Saves (`unsave = false`) or removes (`unsave = true`) the previewed wallpaper in cloud
    /// (`account.saveWallPaper`).
    pub fn save_preview_wallpaper_live(&mut self, unsave: bool) -> Result<(), String> {
        let Some(backend) = self.backend.clone() else {
            return Err("No live ThemesBackend attached.".to_owned());
        };
        let record = self.bg_picker.preview.to_cloud_record();
        backend.save_wallpaper(&record, unsave)
    }

    /// Reverts a per-chat wallpaper override (`messages.setChatWallPaper(revert: true)`).
    pub fn reset_chat_wallpaper_live(&mut self, peer_id: i64) -> Result<(), String> {
        self.chat_background.clear_peer(peer_id);
        let Some(backend) = self.backend.clone() else {
            return Ok(());
        };
        backend.set_chat_wallpaper(peer_id, None, false, true, None)
    }

    /// Resolves multiple wallpapers via `account.getMultiWallPapers` and merges them into the picker.
    pub fn fetch_multi_wallpapers_live(
        &mut self,
        lookups: &[WallPaperLookup],
    ) -> Result<Vec<CloudWallPaperRecord>, String> {
        let Some(backend) = self.backend.clone() else {
            return Err("No live ThemesBackend attached.".to_owned());
        };
        let list = backend.get_multi_wallpapers(lookups)?;
        self.bg_picker.merge_cloud_wallpapers(&list);
        Ok(list)
    }

    /// Syncs a per-chat emoji theme selection to Telegram (`messages.setChatTheme`).
    pub fn apply_chat_theme_live(
        &mut self,
        peer_id: i64,
        token: Option<&str>,
    ) -> Result<(), String> {
        let choice = match token {
            Some(tok) if !tok.is_empty() => ChatThemeChoice::Emoticon(tok.to_owned()),
            _ => ChatThemeChoice::Empty,
        };
        self.apply_chat_theme_choice_live(peer_id, &choice)
    }

    /// Syncs a typed per-chat theme selection (`Empty`, `Emoticon`, or `UniqueGift`) to Telegram
    /// (`messages.setChatTheme`).
    pub fn apply_chat_theme_choice_live(
        &mut self,
        peer_id: i64,
        choice: &ChatThemeChoice,
    ) -> Result<(), String> {
        let Some(backend) = self.backend.clone() else {
            return Ok(());
        };
        backend.set_chat_theme(peer_id, choice)
    }

    /// Syncs a shareable QR/profile card theme choice to Telegram (`messages.setShareTheme`).
    pub fn apply_share_theme_live(
        &mut self,
        peer_id: i64,
        choice: &ShareThemeChoice,
    ) -> Result<(), String> {
        let Some(backend) = self.backend.clone() else {
            return Ok(());
        };
        backend.set_share_theme(peer_id, choice)
    }

    /// Shows a transient toast notification for 3 seconds.
    pub fn show_toast(&mut self, msg: String, now_ms: i64) {
        self.toast = Some((msg, now_ms + 3_000));
    }

    /// Elapsed milliseconds since construction.
    #[must_use]
    pub fn elapsed_ms(&self) -> i64 {
        self.started_at.elapsed().as_millis().min(i64::MAX as u128) as i64
    }

    /// Applies the active `ThemeManager` palette to `ctx` if its generation changed.
    pub fn sync_egui_visuals(&mut self, ctx: &Context) {
        if self.applied_generation != self.theme_manager.generation() {
            self.theme_manager.apply_to_egui(ctx);
            self.applied_generation = self.theme_manager.generation();
        }
    }

    /// Renders the complete Theme Studio panel into `ui`. Returns `true` if Close was clicked.
    pub fn show(&mut self, ui: &mut Ui, show_close: bool) -> bool {
        let now_ms = self.elapsed_ms();
        self.show_at(ui, show_close, now_ms)
    }

    /// Deterministic timestamp variant of [`Self::show`] (used in unit tests & screenshots).
    pub fn show_at(&mut self, ui: &mut Ui, show_close: bool, now_ms: i64) -> bool {
        if self.backend.is_some() && !self.cloud_loaded {
            let _ = self.refresh_cloud_catalogs();
        }
        self.sync_egui_visuals(ui.ctx());
        let mut closed = false;
        let palette_snap = self.theme_manager.palette().clone();
        let colors: &dyn Colors = &palette_snap;

        let panel_bg = colors.color("windowBg");
        let bold_fg = colors.color("windowBoldFg");
        let sub_fg = colors.color("windowSubTextFg");
        let active_text = colors.color("windowActiveTextFg");
        let divider = colors.color("shadowFg");

        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, CornerRadius::ZERO, panel_bg);

        // Top header bar (46px)
        let header_h = 46.0;
        let narrow = rect.width() < 520.0;
        let header_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), header_h));
        ui.scope_builder(egui::UiBuilder::new().max_rect(header_rect), |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(if narrow { 10 } else { 16 }, 8))
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        let header_title = if narrow {
                            "Themes"
                        } else {
                            "Appearance & Theme Studio"
                        };
                        ui.label(
                            egui::RichText::new(header_title)
                                .size(if narrow { 14.5 } else { 15.5 })
                                .strong()
                                .color(bold_fg),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if show_close && ui.button("Close").clicked() {
                                closed = true;
                            }
                            if self.backend.is_some()
                                && ui
                                    .button(if narrow { "Sync" } else { "Refresh Cloud" })
                                    .clicked()
                            {
                                match self.refresh_cloud_catalogs() {
                                    Ok(()) => self.show_toast(
                                        "Refreshed cloud themes & wallpapers".to_owned(),
                                        now_ms,
                                    ),
                                    Err(err) => {
                                        self.show_toast(format!("Cloud sync error: {err}"), now_ms)
                                    }
                                }
                            }
                            let dark_label = if narrow {
                                if self.theme_manager.is_dark() {
                                    "Night: ON"
                                } else {
                                    "Night: OFF"
                                }
                            } else if self.theme_manager.is_dark() {
                                "Night Mode: ON"
                            } else {
                                "Night Mode: OFF"
                            };
                            if ui.button(dark_label).clicked() {
                                match self.night_scheduler.request_manual_toggle() {
                                    NightScheduleEvent::Switched { night_mode } => {
                                        self.theme_manager.set_night_mode(night_mode);
                                        self.chat_background.set_dark_mode(night_mode);
                                        self.chat_theme_picker
                                            .sync_dark_mode(&mut self.chat_background, night_mode);
                                        self.bg_picker.preview.preview_dark_mode = night_mode;
                                        self.editor = ThemeEditorState::from_theme_manager(
                                            &self.theme_manager,
                                        );
                                        self.editor.cloud_save.cloud_connected =
                                            self.backend.is_some();
                                        self.sync_egui_visuals(ui.ctx());
                                    }
                                    NightScheduleEvent::NeedsConfirmDisableSystem => {
                                        self.show_toast(
                                            "System dark mode following is active. Confirm in Chat Themes & Night Mode to switch manually.".to_owned(),
                                            now_ms,
                                        );
                                        self.active_tab = ThemesTab::ChatThemes;
                                    }
                                    NightScheduleEvent::Unchanged => {}
                                }
                            }
                        });
                    });
                });
            ui.painter().line_segment(
                [
                    Pos2::new(header_rect.left(), header_rect.bottom()),
                    Pos2::new(header_rect.right(), header_rect.bottom()),
                ],
                Stroke::new(1.0, divider),
            );
        });

        // Tab strip (36px, horizontally scrollable on narrow viewports)
        let tab_h = 36.0;
        let tab_rect = Rect::from_min_size(
            Pos2::new(rect.left(), header_rect.bottom()),
            Vec2::new(rect.width(), tab_h),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(tab_rect), |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(if narrow { 10 } else { 14 }, 0))
                .show(ui, |ui| {
                    ScrollArea::horizontal()
                        .id_salt("tg_themes_studio_tab_strip")
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .show(ui, |ui| {
                            ui.horizontal_centered(|ui| {
                                ui.spacing_mut().item_spacing.x = if narrow { 12.0 } else { 16.0 };
                                for &tab in ThemesTab::ALL {
                                    let selected = self.active_tab == tab;
                                    let col = if selected { active_text } else { sub_fg };
                                    let tab_label = if narrow {
                                        match tab {
                                            ThemesTab::Editor => "Editor",
                                            ThemesTab::Backgrounds => "Backgrounds",
                                            ThemesTab::ChatThemes => "Chat Themes",
                                            ThemesTab::Previews => "Previews",
                                        }
                                    } else {
                                        tab.label()
                                    };
                                    let galley = ui.fonts_mut(|f| {
                                        f.layout_no_wrap(
                                            tab_label.to_owned(),
                                            FontId::proportional(12.5),
                                            col,
                                        )
                                    });
                                    let (r, resp) = ui.allocate_exact_size(
                                        Vec2::new(galley.size().x + 6.0, tab_h),
                                        Sense::click(),
                                    );
                                    resp.widget_info(|| {
                                        WidgetInfo::selected(
                                            Role::Tab,
                                            ui.is_enabled(),
                                            selected,
                                            tab.label(),
                                        )
                                    });
                                    if resp.hovered() {
                                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                    }
                                    if resp.clicked() {
                                        self.active_tab = tab;
                                        self.night_scheduler
                                            .set_editing_theme(tab == ThemesTab::Editor);
                                    }
                                    ui.painter().galley(
                                        Pos2::new(
                                            r.left() + 3.0,
                                            r.center().y - galley.size().y / 2.0,
                                        ),
                                        galley,
                                        col,
                                    );
                                    if selected {
                                        ui.painter().line_segment(
                                            [
                                                Pos2::new(r.left(), r.bottom() - 2.0),
                                                Pos2::new(r.right(), r.bottom() - 2.0),
                                            ],
                                            Stroke::new(2.5, active_text),
                                        );
                                    }
                                }
                            });
                        });
                });
            ui.painter().line_segment(
                [
                    Pos2::new(tab_rect.left(), tab_rect.bottom()),
                    Pos2::new(tab_rect.right(), tab_rect.bottom()),
                ],
                Stroke::new(1.0, divider),
            );
        });

        // Body
        let body_rect = Rect::from_min_max(Pos2::new(rect.left(), tab_rect.bottom()), rect.max);
        let mut pending_toast: Option<String> = None;
        let mut needs_visual_sync = false;

        ui.scope_builder(
            egui::UiBuilder::new().max_rect(body_rect),
            |ui| match self.active_tab {
                ThemesTab::Editor => {
                    let ed_out = self.editor.show_with_backend(
                        ui,
                        &mut self.theme_manager,
                        &mut self.chat_background,
                        self.backend.as_deref(),
                    );
                    if ed_out.palette_changed {
                        needs_visual_sync = true;
                        self.preview_cache.invalidate("custom_live");
                    }
                    if let Some(saved) = ed_out.saved_cloud_theme {
                        if let Some(slot) = self
                            .cloud_themes
                            .iter_mut()
                            .find(|t| t.theme.id == saved.theme.id)
                        {
                            *slot = saved.clone();
                        } else {
                            self.cloud_themes.insert(0, saved.clone());
                        }
                        pending_toast = ed_out.toast.or_else(|| {
                            Some(format!(
                                "Saved cloud theme '{}' (t.me/addtheme/{})",
                                saved.theme.title, saved.theme.slug
                            ))
                        });
                    } else if ed_out.save_cloud_requested {
                        match self.save_current_cloud_theme() {
                            Ok(saved) => {
                                pending_toast = Some(format!(
                                    "Saved cloud theme '{}' (t.me/addtheme/{})",
                                    saved.theme.title, saved.theme.slug
                                ));
                            }
                            Err(err) => {
                                pending_toast = Some(format!("Cloud theme save failed: {err}"));
                            }
                        }
                    } else if ed_out.toast.is_some() {
                        pending_toast = ed_out.toast;
                    }
                    if ed_out.close_requested {
                        closed = true;
                    }
                }
                ThemesTab::Backgrounds => {
                    ScrollArea::vertical()
                        .id_salt("tg_backgrounds_studio_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            Frame::NONE.inner_margin(Margin::same(14)).show(ui, |ui| {
                                let bg_out =
                                    self.bg_picker.show(ui, &mut self.chat_background, colors);
                                if let Some(scope) = bg_out.applied {
                                    self.preview_cache.invalidate("custom_live");
                                    if let Err(err) = self.apply_preview_background_live(scope) {
                                        pending_toast =
                                            Some(format!("Wallpaper sync failed: {err}"));
                                    }
                                }
                                if bg_out.save_cloud_requested {
                                    match self.save_preview_wallpaper_live(false) {
                                        Ok(()) => {
                                            pending_toast =
                                                Some("Saved wallpaper to cloud".to_owned());
                                        }
                                        Err(err) => {
                                            pending_toast =
                                                Some(format!("Save wallpaper failed: {err}"));
                                        }
                                    }
                                }
                                if let Some(peer_id) = bg_out.reset_peer_requested
                                    && let Err(err) = self.reset_chat_wallpaper_live(peer_id)
                                {
                                    pending_toast = Some(format!("Reset wallpaper failed: {err}"));
                                }
                                if let Some(removed) = bg_out.removed_cloud
                                    && let Some(backend) = self.backend.clone()
                                    && let Err(err) = backend.save_wallpaper(&removed, true)
                                {
                                    pending_toast =
                                        Some(format!("Remove cloud wallpaper failed: {err}"));
                                }
                                if pending_toast.is_none() && bg_out.toast.is_some() {
                                    pending_toast = bg_out.toast;
                                }
                            });
                        });
                }
                ThemesTab::ChatThemes => {
                    ScrollArea::vertical()
                        .id_salt("tg_chat_themes_and_schedule_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            Frame::NONE.inner_margin(Margin::same(14)).show(ui, |ui| {
                                let dark = self.theme_manager.is_dark();
                                let ct_out = self.chat_theme_picker.show_with_backend(
                                    ui,
                                    &mut self.chat_background,
                                    dark,
                                    colors,
                                    self.backend.as_deref(),
                                );
                                if let Some((peer_id, ref choice)) = ct_out.applied_choice {
                                    if let Err(err) =
                                        self.apply_chat_theme_choice_live(peer_id, choice)
                                    {
                                        pending_toast =
                                            Some(format!("Chat theme sync failed: {err}"));
                                    }
                                } else if let Some((peer_id, ref token)) = ct_out.applied_token
                                    && let Err(err) =
                                        self.apply_chat_theme_live(peer_id, token.as_deref())
                                {
                                    pending_toast = Some(format!("Chat theme sync failed: {err}"));
                                }
                                if let Some(peer) = ct_out.open_background_picker_for_peer {
                                    self.bg_picker.preview.target_peer = Some(peer);
                                    self.active_tab = ThemesTab::Backgrounds;
                                }
                                if pending_toast.is_none() && ct_out.toast.is_some() {
                                    pending_toast = ct_out.toast;
                                }

                                ui.add_space(16.0);
                                ui.separator();
                                ui.add_space(12.0);

                                let gift_records = self.chat_theme_picker.gift_theme_records();
                                let share_out = self.share_theme_picker.show(
                                    ui,
                                    colors,
                                    &gift_records,
                                    self.backend.as_deref(),
                                );
                                if pending_toast.is_none() && share_out.toast.is_some() {
                                    pending_toast = share_out.toast;
                                }

                                ui.add_space(16.0);
                                ui.separator();
                                ui.add_space(12.0);

                                if let Some(msg) = self.show_night_schedule_section(ui, colors) {
                                    needs_visual_sync = true;
                                    pending_toast = Some(msg);
                                }
                            });
                        });
                }
                ThemesTab::Previews => {
                    ScrollArea::vertical()
                        .id_salt("tg_theme_previews_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            Frame::NONE.inner_margin(Margin::same(14)).show(ui, |ui| {
                                if let Some(msg) = self.show_theme_previews_tab(ui, colors) {
                                    needs_visual_sync = true;
                                    pending_toast = Some(msg);
                                }
                            });
                        });
                }
            },
        );

        if needs_visual_sync {
            self.sync_egui_visuals(ui.ctx());
        }
        if let Some(msg) = pending_toast {
            self.show_toast(msg, now_ms);
        }

        // Transient bottom toast
        if let Some((ref text, expires_at)) = self.toast {
            if now_ms >= expires_at {
                self.toast = None;
            } else {
                let toast_w = (rect.width() - 40.0).min(460.0);
                let toast_rect = Rect::from_min_size(
                    Pos2::new(rect.center().x - toast_w / 2.0, rect.bottom() - 48.0),
                    Vec2::new(toast_w, 34.0),
                );
                ui.painter().rect_filled(
                    toast_rect,
                    CornerRadius::same(8),
                    Color32::from_rgba_unmultiplied(0x1E, 0x24, 0x2B, 232),
                );
                ui.painter().text(
                    toast_rect.center(),
                    Align2::CENTER_CENTER,
                    text,
                    FontId::proportional(12.0),
                    Color32::WHITE,
                );
            }
        }

        closed
    }

    fn show_night_schedule_section(&mut self, ui: &mut Ui, colors: &dyn Colors) -> Option<String> {
        let mut toast = None;
        let bold_fg = colors.color("windowBoldFg");
        let sub_fg = colors.color("windowSubTextFg");
        let bg_over = colors.color("windowBgOver");
        let active_text = colors.color("windowActiveTextFg");

        ui.label(
            egui::RichText::new("Night Mode Schedule")
                .size(15.0)
                .strong()
                .color(bold_fg),
        );
        let os_status = match tg_platform::system_info::is_dark_mode() {
            Some(true) => "Windows system theme: Dark",
            Some(false) => "Windows system theme: Light",
            None => "Windows system theme: Unavailable",
        };
        ui.label(egui::RichText::new(os_status).size(12.0).color(sub_fg));
        ui.add_space(6.0);

        let cur_mode = self.night_scheduler.mode();
        let (mut start_min, mut end_min) = self.night_scheduler.schedule_window();

        ui.horizontal_wrapped(|ui| {
            let sys_dark = tg_platform::system_info::is_dark_mode();
            let now_min = night_schedule::current_local_minute_of_day();
            if ui
                .selectable_label(matches!(cur_mode, NightScheduleMode::Manual), "Manual")
                .clicked()
            {
                self.night_scheduler
                    .set_mode(NightScheduleMode::Manual, sys_dark, now_min);
                toast = Some("Night mode set to Manual".to_owned());
            }
            if ui
                .selectable_label(
                    matches!(cur_mode, NightScheduleMode::System),
                    "Follow System (Windows)",
                )
                .clicked()
            {
                if let NightScheduleEvent::Switched { night_mode } =
                    self.night_scheduler
                        .set_mode(NightScheduleMode::System, sys_dark, now_min)
                {
                    self.theme_manager.set_night_mode(night_mode);
                    self.chat_background.set_dark_mode(night_mode);
                    self.bg_picker.preview.preview_dark_mode = night_mode;
                }
                toast = Some("Auto-switching with Windows dark mode".to_owned());
            }
            if ui
                .selectable_label(
                    matches!(cur_mode, NightScheduleMode::Scheduled { .. }),
                    "Scheduled Hours",
                )
                .clicked()
            {
                let sched = NightScheduleMode::Scheduled {
                    start_minutes: start_min,
                    end_minutes: end_min,
                };
                if let NightScheduleEvent::Switched { night_mode } =
                    self.night_scheduler.set_mode(sched, sys_dark, now_min)
                {
                    self.theme_manager.set_night_mode(night_mode);
                    self.chat_background.set_dark_mode(night_mode);
                    self.bg_picker.preview.preview_dark_mode = night_mode;
                }
                toast = Some(format!(
                    "Night mode scheduled {} – {}",
                    format_minutes_hhmm(start_min),
                    format_minutes_hhmm(end_min)
                ));
            }
        });

        if matches!(
            self.night_scheduler.mode(),
            NightScheduleMode::Scheduled { .. }
        ) {
            ui.add_space(6.0);
            Frame::NONE
                .fill(bg_over)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    let mut start_hr = (start_min / 60) as i32;
                    let mut end_hr = (end_min / 60) as i32;
                    let mut changed = false;
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("From:").size(12.0).color(sub_fg));
                        changed |= ui
                            .add(egui::Slider::new(&mut start_hr, 0..=23).suffix(":00"))
                            .changed();
                        ui.add_space(10.0);
                        ui.label(egui::RichText::new("To:").size(12.0).color(sub_fg));
                        changed |= ui
                            .add(egui::Slider::new(&mut end_hr, 0..=23).suffix(":00"))
                            .changed();
                    });
                    if changed {
                        start_min = (start_hr.clamp(0, 23) as u16) * 60;
                        end_min = (end_hr.clamp(0, 23) as u16) * 60;
                        let sched = NightScheduleMode::Scheduled {
                            start_minutes: start_min,
                            end_minutes: end_min,
                        };
                        let now_min = night_schedule::current_local_minute_of_day();
                        if let NightScheduleEvent::Switched { night_mode } =
                            self.night_scheduler.set_mode(sched, None, now_min)
                        {
                            self.theme_manager.set_night_mode(night_mode);
                            self.chat_background.set_dark_mode(night_mode);
                        }
                    }
                });
        }

        if self.night_scheduler.pending_disable_system_confirm {
            ui.add_space(8.0);
            Frame::NONE
                .fill(bg_over)
                .stroke(Stroke::new(1.0, active_text))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(
                            "Automatic system dark mode following is active. Disable it and switch manually?",
                        )
                        .size(12.0)
                        .color(bold_fg),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("Disable System Follow & Switch").clicked()
                            && let NightScheduleEvent::Switched { night_mode } =
                                self.night_scheduler.confirm_disable_system_and_toggle()
                        {
                            self.theme_manager.set_night_mode(night_mode);
                            self.chat_background.set_dark_mode(night_mode);
                            self.bg_picker.preview.preview_dark_mode = night_mode;
                            toast = Some("Switched manually; system follow disabled".to_owned());
                        }
                        if ui.button("Cancel").clicked() {
                            self.night_scheduler.pending_disable_system_confirm = false;
                        }
                    });
                });
        }

        toast
    }

    fn show_theme_previews_tab(&mut self, ui: &mut Ui, colors: &dyn Colors) -> Option<String> {
        let mut toast = None;
        let bold_fg = colors.color("windowBoldFg");
        let sub_fg = colors.color("windowSubTextFg");
        let active_bg = colors.color("windowBgActive");
        let divider = colors.color("shadowFg");

        ui.label(
            egui::RichText::new("Theme Preview Gallery")
                .size(15.0)
                .strong()
                .color(bold_fg),
        );
        ui.label(
            egui::RichText::new(
                "Select any built-in, custom, or cloud theme to preview and apply across chats.",
            )
            .size(12.0)
            .color(sub_fg),
        );
        ui.add_space(10.0);

        // Horizontal compact card strip (`generate_compact_theme_card`)
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(10.0, 10.0);
            for scheme in embedded_themes() {
                let pal = prepare_embedded_palette(scheme.kind, None).unwrap_or_default();
                let inst = ThemeInstance {
                    palette: pal,
                    background: None,
                    cached: Default::default(),
                    tiled: false,
                };
                let key = format!("card_{:?}", scheme.kind);
                let tex = self
                    .preview_cache
                    .get_or_generate_card(ui.ctx(), &key, &inst, 136, 94);
                let is_sel = !self.preview_show_custom && self.preview_embedded == scheme.kind;
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(136.0, 114.0), Sense::click());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                let img_rect = Rect::from_min_size(rect.min, Vec2::new(136.0, 94.0));
                ui.painter().image(
                    tex.id(),
                    img_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                ui.painter().rect_stroke(
                    img_rect,
                    CornerRadius::same(8),
                    Stroke::new(
                        if is_sel { 2.5 } else { 1.0 },
                        if is_sel { active_bg } else { divider },
                    ),
                    StrokeKind::Inside,
                );
                ui.painter().text(
                    Pos2::new(rect.center().x, img_rect.bottom() + 4.0),
                    Align2::CENTER_TOP,
                    scheme.name,
                    FontId::proportional(12.0),
                    if is_sel { bold_fg } else { sub_fg },
                );
                if resp.clicked() {
                    self.preview_show_custom = false;
                    self.preview_embedded = scheme.kind;
                }
            }

            // Live Edited Theme card
            let custom_inst = self
                .editor
                .snapshot_instance(&self.theme_manager, &mut self.chat_background);
            let custom_tex = self.preview_cache.get_or_generate_card(
                ui.ctx(),
                "custom_live_card",
                &custom_inst,
                136,
                94,
            );
            let is_sel = self.preview_show_custom;
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(136.0, 114.0), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let img_rect = Rect::from_min_size(rect.min, Vec2::new(136.0, 94.0));
            ui.painter().image(
                custom_tex.id(),
                img_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            ui.painter().rect_stroke(
                img_rect,
                CornerRadius::same(8),
                Stroke::new(
                    if is_sel { 2.5 } else { 1.0 },
                    if is_sel { active_bg } else { divider },
                ),
                StrokeKind::Inside,
            );
            ui.painter().text(
                Pos2::new(rect.center().x, img_rect.bottom() + 4.0),
                Align2::CENTER_TOP,
                "Current Live",
                FontId::proportional(12.0),
                if is_sel { bold_fg } else { sub_fg },
            );
            if resp.clicked() {
                self.preview_show_custom = true;
            }
        });

        ui.add_space(12.0);

        // Full extended desktop mockup preview (`generate_theme_preview(Extended)`)
        let (preview_key, preview_inst, title) = if self.preview_show_custom {
            (
                "custom_live".to_owned(),
                self.editor
                    .snapshot_instance(&self.theme_manager, &mut self.chat_background),
                "Current Live Edited Theme".to_owned(),
            )
        } else {
            let pal = prepare_embedded_palette(self.preview_embedded, None).unwrap_or_default();
            let inst = ThemeInstance {
                palette: pal,
                background: None,
                cached: Default::default(),
                tiled: false,
            };
            let display_name = embedded_themes()
                .iter()
                .find(|s| s.kind == self.preview_embedded)
                .map_or("Built-in", |s| s.name);
            (
                format!("full_{:?}", self.preview_embedded),
                inst,
                format!("Built-in Theme: {display_name}"),
            )
        };

        let full_tex = self.preview_cache.get_or_generate(
            ui.ctx(),
            &preview_key,
            &preview_inst,
            PreviewType::Extended,
        );
        let [tw, th] = full_tex.size();
        let avail_w = ui.available_width().min(600.0);
        let scale = avail_w / (tw.max(1) as f32);
        let draw_size = Vec2::new(avail_w, (th as f32) * scale);

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(title)
                    .size(13.5)
                    .strong()
                    .color(bold_fg),
            );
            if !self.preview_show_custom && ui.button("Apply This Theme").clicked() {
                match self.install_embedded_theme(self.preview_embedded) {
                    Ok(()) => toast = Some(format!("Applied {:?} theme", self.preview_embedded)),
                    Err(err) => toast = Some(format!("Theme sync failed: {err}")),
                }
            }
        });
        ui.add_space(6.0);
        let (img_rect, _) = ui.allocate_exact_size(draw_size, Sense::hover());
        ui.painter().image(
            full_tex.id(),
            img_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        ui.painter().rect_stroke(
            img_rect,
            CornerRadius::same(8),
            Stroke::new(1.0, divider),
            StrokeKind::Inside,
        );

        // Cloud themes catalog fetched via `account.getThemes`
        if !self.cloud_themes.is_empty() {
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Cloud Themes")
                    .size(14.5)
                    .strong()
                    .color(bold_fg),
            );
            ui.add_space(6.0);
            let snapshot = self.cloud_themes.clone();
            let mut install_candidate: Option<CloudThemeRecord> = None;
            let mut edit_candidate: Option<CloudThemeRecord> = None;
            let mut toggle_save_candidate: Option<(CloudThemeRecord, bool)> = None;
            for rec in &snapshot {
                let is_saved = self.saved_cloud_theme_ids.contains(&rec.theme.id);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(&rec.theme.title)
                            .size(13.0)
                            .strong()
                            .color(bold_fg),
                    );
                    if !rec.theme.slug.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("t.me/addtheme/{}", rec.theme.slug))
                                .size(11.5)
                                .color(sub_fg),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Install").clicked() {
                            install_candidate = Some(rec.clone());
                        }
                        if self.backend.is_some() {
                            let save_label = if is_saved { "Remove" } else { "Save" };
                            if ui.small_button(save_label).clicked() {
                                toggle_save_candidate = Some((rec.clone(), is_saved));
                            }
                        }
                        if rec.is_creator && ui.small_button("Edit").clicked() {
                            edit_candidate = Some(rec.clone());
                        }
                    });
                });
                ui.add_space(4.0);
            }
            if let Some(rec) = install_candidate {
                match self.install_cloud_theme(&rec) {
                    Ok(()) => {
                        toast = Some(format!("Installed cloud theme '{}'", rec.theme.title));
                    }
                    Err(err) => {
                        toast = Some(format!("Install cloud theme failed: {err}"));
                    }
                }
            } else if let Some((rec, unsave)) = toggle_save_candidate {
                match self.save_cloud_theme_live(rec.theme.id, rec.theme.access_hash, unsave) {
                    Ok(()) => {
                        toast = Some(if unsave {
                            format!("Removed cloud theme '{}'", rec.theme.title)
                        } else {
                            format!("Saved cloud theme '{}'", rec.theme.title)
                        });
                    }
                    Err(err) => {
                        toast = Some(format!("Cloud theme save failed: {err}"));
                    }
                }
            } else if let Some(rec) = edit_candidate {
                let _ = self.install_cloud_theme(&rec);
                self.editor
                    .open_cloud_save_dialog(&self.theme_manager, rec.theme.id);
                self.active_tab = ThemesTab::Editor;
                toast = Some(format!("Editing cloud theme '{}'", rec.theme.title));
            }
        }

        toast
    }
}

/// Self-contained overlay & system dark-mode host for `crates/tg-app/src/app.rs`
/// (single-line hook inside `TgforkApp::ui`).
///
/// Press `Ctrl+Shift+Y` in the desktop app to open or close the Appearance & Theme Studio
/// overlay; live edits immediately update `egui::Context` visuals and the shared
/// [`active_chat_background`].
pub struct AppThemesHost {
    open: bool,
    studio: ThemesStudio,
    bound_session_key: Option<usize>,
    last_deep_link_theme_slug: Option<String>,
    last_deep_link_background: Option<String>,
}

impl AppThemesHost {
    fn get_or_init(ctx: &Context) -> Arc<Mutex<Self>> {
        ctx.data_mut(|d| {
            d.get_temp_mut_or_insert_with(Id::unique("tg_app_themes_host"), || {
                Arc::new(Mutex::new(Self {
                    open: false,
                    studio: ThemesStudio::new(),
                    bound_session_key: None,
                    last_deep_link_theme_slug: None,
                    last_deep_link_background: None,
                }))
            })
            .clone()
        })
    }

    /// Attaches a live [`ThemesBackend`] (e.g. `live.themes_backend()`) to the context-scoped [`ThemesStudio`].
    pub fn set_backend(ctx: &Context, backend: Arc<dyn ThemesBackend>) {
        let host = Self::get_or_init(ctx);
        if let Ok(mut guard) = host.lock() {
            guard.studio.set_backend(backend);
        }
    }

    /// Attaches a live [`ThemesBackend`] and records the owning session token so account switches
    /// can cleanly detect and rebind.
    pub fn set_backend_for_session(
        ctx: &Context,
        session_key: usize,
        backend: Arc<dyn ThemesBackend>,
    ) {
        let host = Self::get_or_init(ctx);
        if let Ok(mut guard) = host.lock() {
            guard.studio.set_backend(backend);
            guard.bound_session_key = Some(session_key);
        }
    }

    /// Returns the currently bound live session token, if any.
    #[must_use]
    pub fn bound_session_key(ctx: &Context) -> Option<usize> {
        let host = Self::get_or_init(ctx);
        host.lock().ok().and_then(|guard| guard.bound_session_key)
    }

    /// Returns `true` when a live [`ThemesBackend`] is currently attached.
    #[must_use]
    pub fn has_backend(ctx: &Context) -> bool {
        let host = Self::get_or_init(ctx);
        host.lock()
            .map(|guard| guard.studio.has_backend())
            .unwrap_or(false)
    }

    /// Returns `true` when the Theme Studio overlay is currently open.
    #[must_use]
    pub fn is_open(ctx: &Context) -> bool {
        let host = Self::get_or_init(ctx);
        host.lock().map(|guard| guard.open).unwrap_or(false)
    }

    /// Returns the currently selected [`ThemesTab`] in the Theme Studio overlay.
    #[must_use]
    pub fn active_tab(ctx: &Context) -> ThemesTab {
        let host = Self::get_or_init(ctx);
        host.lock()
            .map(|guard| guard.studio.active_tab)
            .unwrap_or(ThemesTab::Editor)
    }

    /// Returns the last cloud theme slug routed via `tg://addtheme`, if any.
    #[must_use]
    pub fn last_deep_link_theme_slug(ctx: &Context) -> Option<String> {
        let host = Self::get_or_init(ctx);
        host.lock()
            .ok()
            .and_then(|guard| guard.last_deep_link_theme_slug.clone())
    }

    /// Returns the last background slug/color routed via `tg://bg`, if any.
    #[must_use]
    pub fn last_deep_link_background(ctx: &Context) -> Option<String> {
        let host = Self::get_or_init(ctx);
        host.lock()
            .ok()
            .and_then(|guard| guard.last_deep_link_background.clone())
    }

    /// Clears any attached [`ThemesBackend`] (e.g. on logout or account switch).
    pub fn clear_backend(ctx: &Context) {
        let host = Self::get_or_init(ctx);
        if let Ok(mut guard) = host.lock() {
            guard.studio.clear_backend();
            guard.bound_session_key = None;
        }
    }

    /// Opens the Theme Studio overlay on [`ThemesTab::Previews`] for a `tg://addtheme?slug=...`
    /// deep link, refreshing cloud themes when live.
    pub fn open_theme_deep_link(ctx: &Context, slug: &str, title: Option<&str>) {
        let host = Self::get_or_init(ctx);
        if let Ok(mut guard) = host.lock() {
            guard.last_deep_link_theme_slug = Some(slug.to_owned());
            guard.studio.active_tab = ThemesTab::Previews;
            guard.open = true;
            guard.studio.night_scheduler.set_editing_theme(false);
            if guard.studio.has_backend() && !guard.studio.cloud_loaded {
                let _ = guard.studio.refresh_cloud_catalogs();
            }
            let display = title.filter(|t| !t.trim().is_empty()).unwrap_or(slug);
            let now_ms = guard.studio.elapsed_ms();
            guard.studio.show_toast(
                format!("Cloud theme: {display} (t.me/addtheme/{slug})"),
                now_ms,
            );
        }
    }

    /// Opens the Theme Studio overlay on [`ThemesTab::Backgrounds`] for a `tg://bg?...` deep link,
    /// previewing parsed hex colors/gradients or resolving a cloud wallpaper slug when live.
    #[allow(clippy::too_many_arguments)]
    pub fn open_background_deep_link(
        ctx: &Context,
        slug_or_color: &str,
        is_color_or_gradient: bool,
        blur: bool,
        motion: bool,
        intensity: Option<i32>,
        rotation: Option<i32>,
    ) {
        let host = Self::get_or_init(ctx);
        if let Ok(mut guard) = host.lock() {
            guard.last_deep_link_background = Some(slug_or_color.to_owned());
            guard.studio.active_tab = ThemesTab::Backgrounds;
            guard.open = true;
            guard.studio.night_scheduler.set_editing_theme(false);

            if is_color_or_gradient {
                let colors = parse_bg_link_colors(slug_or_color);
                if !colors.is_empty() {
                    let mut wp = tg_theme::WallPaper::new(0);
                    wp.background_colors = colors;
                    wp.blurred = blur;
                    if let Some(rot) = rotation {
                        wp.rotation = (rot.rem_euclid(360) / 45) * 45;
                    }
                    if let Some(inten) = intensity {
                        wp = wp.with_pattern_intensity(inten.clamp(-100, 100));
                    }
                    let cfg = BackgroundConfig {
                        wallpaper: wp,
                        motion,
                        ..BackgroundConfig::default()
                    };
                    guard.studio.bg_picker.preview.set_config(cfg);
                }
            } else if !slug_or_color.is_empty() {
                if guard.studio.has_backend()
                    && let Ok(list) =
                        guard
                            .studio
                            .fetch_multi_wallpapers_live(&[WallPaperLookup::Slug(
                                slug_or_color.to_owned(),
                            )])
                    && let Some(first) = list.into_iter().next()
                {
                    let mut wp = first.wallpaper.clone();
                    if blur {
                        wp.blurred = true;
                    }
                    if let Some(rot) = rotation {
                        wp.rotation = (rot.rem_euclid(360) / 45) * 45;
                    }
                    if let Some(inten) = intensity {
                        wp = wp.with_pattern_intensity(inten.clamp(-100, 100));
                    }
                    let cfg = BackgroundConfig {
                        wallpaper: wp,
                        motion: motion || first.motion,
                        ..BackgroundConfig::default()
                    };
                    guard
                        .studio
                        .bg_picker
                        .preview
                        .set_config_with_cloud(cfg, Some(first));
                } else {
                    let mut wp = tg_theme::WallPaper::new(0);
                    wp.slug = slug_or_color.to_owned();
                    wp.blurred = blur;
                    if let Some(rot) = rotation {
                        wp.rotation = (rot.rem_euclid(360) / 45) * 45;
                    }
                    if let Some(inten) = intensity {
                        wp = wp.with_pattern_intensity(inten.clamp(-100, 100));
                    }
                    let cfg = BackgroundConfig {
                        wallpaper: wp,
                        motion,
                        ..BackgroundConfig::default()
                    };
                    guard.studio.bg_picker.preview.set_config(cfg);
                }
            }

            let now_ms = guard.studio.elapsed_ms();
            let label = if slug_or_color.is_empty() {
                "Opened Cloud Wallpapers".to_owned()
            } else {
                format!("Background preview: {slug_or_color}")
            };
            guard.studio.show_toast(label, now_ms);
        }
    }

    /// Opens the Theme Studio overlay on an optional [`ThemesTab`].
    pub fn open(ctx: &Context, tab: Option<ThemesTab>) {
        let host = Self::get_or_init(ctx);
        if let Ok(mut guard) = host.lock() {
            if let Some(t) = tab {
                guard.studio.active_tab = t;
            }
            guard.open = true;
            let is_editing = guard.studio.active_tab == ThemesTab::Editor;
            guard.studio.night_scheduler.set_editing_theme(is_editing);
            if guard.studio.has_backend() && !guard.studio.cloud_loaded {
                let _ = guard.studio.refresh_cloud_catalogs();
            }
        }
    }

    /// Opens the Theme Studio overlay targeting a specific chat (`peer_id`, `peer_name`) on `tab`
    /// (e.g. `ThemesTab::ChatThemes` or `ThemesTab::Backgrounds` from chat menu / Info).
    pub fn open_for_peer(
        ctx: &Context,
        peer_id: i64,
        peer_name: impl Into<String>,
        tab: ThemesTab,
    ) {
        let host = Self::get_or_init(ctx);
        if let Ok(mut guard) = host.lock() {
            guard.studio.set_target_peer(peer_id, peer_name);
            guard.studio.active_tab = tab;
            guard.open = true;
            guard
                .studio
                .night_scheduler
                .set_editing_theme(tab == ThemesTab::Editor);
            if guard.studio.has_backend() && !guard.studio.cloud_loaded {
                let _ = guard.studio.refresh_cloud_catalogs();
            }
        }
    }

    /// Opens the Shareable Theme Backdrop picker ([`ShareThemePickerState`], tdesktop
    /// `info/profile/info_profile_share_theme.cpp`) inside the Theme Studio overlay for
    /// `(peer_id, peer_name)`, hydrating backdrop catalogs when connected to a live backend.
    pub fn open_share_theme_for_peer(ctx: &Context, peer_id: i64, peer_name: impl Into<String>) {
        let host = Self::get_or_init(ctx);
        if let Ok(mut guard) = host.lock() {
            guard.studio.set_target_peer(peer_id, peer_name);
            guard.studio.active_tab = ThemesTab::ChatThemes;
            guard.open = true;
            guard.studio.night_scheduler.set_editing_theme(false);
            if guard.studio.has_backend() {
                if !guard.studio.cloud_loaded {
                    let _ = guard.studio.refresh_cloud_catalogs();
                } else {
                    let _ = guard.studio.refresh_share_theme_catalogs();
                }
            }
        }
    }

    /// Returns the currently configured `(peer_id, peer_name)` on [`ThemesStudio::share_theme_picker`].
    #[must_use]
    pub fn share_theme_target_peer(ctx: &Context) -> Option<(i64, String)> {
        let host = Self::get_or_init(ctx);
        host.lock().ok().map(|guard| {
            (
                guard.studio.share_theme_picker.peer_id,
                guard.studio.share_theme_picker.peer_name.clone(),
            )
        })
    }

    /// Drives keyboard toggle (`Ctrl+Shift+Y`), polls OS dark-mode schedule via `tg-platform`,
    /// syncs the shared [`active_chat_background`], and renders the floating Theme Studio overlay.
    pub fn update_and_render(ctx: &Context, backdrop: Color32, divider: Color32) {
        let toggle_pressed = ctx.input(|i| {
            i.modifiers
                .matches_logically(Modifiers::COMMAND | Modifiers::SHIFT)
                && i.key_pressed(Key::Y)
        });

        let state_arc = Self::get_or_init(ctx);

        let Ok(mut guard) = state_arc.lock() else {
            return;
        };

        // Poll automatic system dark mode / scheduled night mode periodically
        let polled = {
            let studio = &mut guard.studio;
            studio
                .night_scheduler
                .poll_platform(&mut studio.theme_manager)
        };
        if let NightScheduleEvent::Switched { night_mode } = polled {
            guard.studio.chat_background.set_dark_mode(night_mode);
            guard.studio.sync_egui_visuals(ctx);
        }

        if toggle_pressed {
            guard.open = !guard.open;
            let is_editing = guard.open && guard.studio.active_tab == ThemesTab::Editor;
            guard.studio.night_scheduler.set_editing_theme(is_editing);
        }

        // Keep shared `active_chat_background(ctx)` in sync for `history/` consumers
        let studio_gen = guard.studio.chat_background.generation();
        if let Ok(mut shared_bg) = active_chat_background(ctx).lock()
            && shared_bg.generation() != studio_gen
        {
            shared_bg.set_global(guard.studio.chat_background.global_config().clone());
            shared_bg.set_dark_mode(guard.studio.chat_background.is_dark_mode());
        }

        if !guard.open {
            return;
        }

        let screen_rect = ctx.content_rect();
        let panel_w = (screen_rect.width() * 0.78).clamp(520.0, 960.0);
        let panel_h = (screen_rect.height() - 40.0).clamp(440.0, 780.0);
        let panel_pos = Pos2::new(
            screen_rect.center().x - panel_w / 2.0,
            screen_rect.center().y - panel_h / 2.0,
        );

        let mut close_requested = false;
        egui::Area::new(Id::unique("tg_themes_overlay_backdrop"))
            .order(Order::Middle)
            .fixed_pos(screen_rect.min)
            .show(ctx, |ui| {
                let (_, resp) = ui.allocate_exact_size(screen_rect.size(), Sense::click());
                ui.painter()
                    .rect_filled(screen_rect, CornerRadius::ZERO, backdrop);
                if resp.clicked() {
                    close_requested = true;
                }
            });

        egui::Area::new(Id::unique("tg_themes_overlay_panel"))
            .order(Order::Foreground)
            .fixed_pos(panel_pos)
            .show(ctx, |ui| {
                let panel_rect = Rect::from_min_size(panel_pos, Vec2::new(panel_w, panel_h));
                ui.painter()
                    .rect_filled(panel_rect.expand(1.0), CornerRadius::same(10), divider);
                ui.scope_builder(egui::UiBuilder::new().max_rect(panel_rect), |ui| {
                    if guard.studio.show(ui, true) {
                        close_requested = true;
                    }
                });
            });

        if close_requested {
            guard.open = false;
            guard.studio.night_scheduler.set_editing_theme(false);
        }
    }
}

/// Parses hex color or gradient tokens (`1a2b3c`, `112233-445566`, `112233~445566~778899`)
/// from a `tg://bg` / `t.me/bg/...` deep link into `tg_theme::Color` entries.
fn parse_bg_link_colors(spec: &str) -> Vec<tg_theme::Color> {
    spec.split(['-', '~', ','])
        .filter_map(|part| {
            let trimmed = part.trim().trim_start_matches('#');
            if trimmed.len() == 6 || trimmed.len() == 8 {
                tg_theme::Color::from_hex(trimmed)
            } else {
                None
            }
        })
        .take(4)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use egui::epaint::{Primitive, TextureId};
    use egui::{RawInput, Rect, Vec2};
    use tg_theme::{Color, EmbeddedType, Palette, ThemeInstance, prepare_embedded_palette};
    use tg_ui::theme::ThemeManager;

    use super::*;
    use crate::themes::color_editor::{sanitize_digits, sanitize_hex_input};

    #[test]
    fn color_editor_hsv_rgb_hex_alpha_and_hsl_limits() {
        let initial = Color::rgba(0x40, 0xA7, 0xE3, 0xCC);
        let mut ed = ColorEditorState::new(ColorEditorMode::Rgba, initial);
        assert_eq!(ed.color(), initial);
        assert_eq!(ed.original_color(), initial);

        // Apply 8-char RGBA hex
        assert!(ed.apply_hex("#ff880080"));
        assert_eq!(ed.color(), Color::rgba(0xFF, 0x88, 0x00, 0x80));

        // Apply 6-char RGB hex preserves current alpha
        assert!(ed.apply_hex("112233"));
        assert_eq!(ed.color(), Color::rgba(0x11, 0x22, 0x33, 0x80));

        // Invalid hex rejected
        assert!(!ed.apply_hex("xyz"));

        // Alpha slider updates alpha channel
        ed.set_alpha_slider(0.5);
        assert!((i16::from(ed.color().a) - 128).abs() <= 1);

        // HSV percent round-trip
        ed.set_hsv_percent(120, 100, 100);
        assert_eq!(ed.color().r, 0);
        assert_eq!(ed.color().g, 255);
        assert_eq!(ed.color().b, 0);

        // Revert restores original snapshot
        ed.revert_to_original();
        assert_eq!(ed.color(), initial);

        // HSL mode clamps lightness
        let mut hsl_ed = ColorEditorState::new(ColorEditorMode::Hsl, Color::rgb(250, 250, 250));
        hsl_ed.set_lightness_limits(64, 160);
        assert!(hsl_ed.color().lightness() <= 160);
        assert!(hsl_ed.color().lightness() >= 64);

        // Input sanitizers match ColorEditor::Field & ResultField
        assert_eq!(sanitize_digits("999", 255), "255");
        assert_eq!(sanitize_digits("12a3", 360), "123");
        assert_eq!(sanitize_hex_input("#GG11aa22ff99", true), "11aa22ff");
        assert_eq!(sanitize_hex_input("#11aa22ff", false), "11aa22");
    }

    #[test]
    fn theme_editor_loads_all_palette_rows_searches_links_cascades_and_roundtrips() {
        let mut mgr = ThemeManager::new();
        let mut bg = ChatBackground::new();
        let mut editor = ThemeEditorState::from_theme_manager(&mgr);

        // Every palette entry from colors.palette is represented with descriptions
        assert!(editor.existing_rows().len() + editor.new_rows().len() >= Palette::ENTRY_COUNT);
        let window_bg_row = editor
            .existing_rows()
            .iter()
            .find(|r| r.name == "windowBg")
            .expect("windowBg row present");
        assert!(!window_bg_row.description.is_empty());

        // Search matches key name, alias, description, and hex
        editor.search_query = "windowBgActive".to_owned();
        let (ex, _) = editor.filtered_indices();
        assert!(!ex.is_empty());
        assert!(
            ex.iter()
                .any(|&i| editor.existing_rows()[i].name == "windowBgActive")
        );

        editor.search_query = "#40a7e3".to_owned();
        let (ex_hex, _) = editor.filtered_indices();
        assert!(!ex_hex.is_empty());
        editor.search_query.clear();

        // Edit windowBg -> live ThemeManager updates AND alias menuBg (= windowBg) cascades!
        let custom_bg = Color::rgb(0xFE, 0xF8, 0xEC);
        editor
            .apply_color_edit("windowBg", custom_bg, None, &mut mgr)
            .unwrap();
        assert_eq!(mgr.palette().window_bg, custom_bg);
        assert_eq!(editor.find_color("windowBg"), Some(custom_bg));
        assert_eq!(editor.find_color("menuBg"), Some(custom_bg));
        assert!(editor.is_dirty());
        assert!(editor.can_undo());

        // Linking "= same as" while editing an existing row
        assert!(editor.activate_key("windowBgOver"));
        assert!(editor.can_copy_from("windowBgOver", EditorBlockKind::Existing, "windowBg"));
        assert!(editor.activate_key("windowBg")); // clicks earlier row -> sets possible_copy_of
        assert_eq!(
            editor
                .active_edit()
                .and_then(|e| e.possible_copy_of.as_deref()),
            Some("windowBg")
        );
        assert!(editor.commit_active_edit(&mut mgr).unwrap());
        let over_row = editor
            .existing_rows()
            .iter()
            .find(|r| r.name == "windowBgOver")
            .unwrap();
        assert_eq!(over_row.copy_of, "windowBg");
        assert_eq!(over_row.value, custom_bg);

        // Export to .tdesktop-theme ZIP with background, then re-import into a fresh editor
        let parsed = editor
            .collect_for_export(&mgr, &mut bg, true, false)
            .unwrap();
        let packed = tg_theme::pack_theme(&parsed).unwrap();
        assert!(!packed.is_empty());

        let mut mgr2 = ThemeManager::new();
        let mut bg2 = ChatBackground::new();
        let mut editor2 = ThemeEditorState::from_theme_manager(&mgr2);
        let loaded_bg = editor2
            .import_from_bytes(&packed, &mut mgr2, &mut bg2)
            .unwrap();
        assert!(loaded_bg);
        assert_eq!(mgr2.palette().window_bg, custom_bg);
        assert!(bg2.global_config().custom_image.is_some());

        // Cloud save packaging validates slug and prepares a cloud-headered archive
        editor.open_cloud_save_dialog(&mgr, 7);
        assert!(!editor.cloud_save.title.is_empty());
        editor.cloud_save.slug = "bad".to_owned();
        assert!(editor.build_cloud_theme_package(&mgr, &mut bg).is_err());
        editor.cloud_save.slug = "valid_theme_slug".to_owned();
        let (_title, slug, cloud_zip) = editor.build_cloud_theme_package(&mgr, &mut bg).unwrap();
        assert_eq!(slug, "valid_theme_slug");
        assert!(!cloud_zip.is_empty());

        // Undo and Revert work cleanly
        assert!(editor.undo(&mut mgr));
        editor.revert_all(&mut mgr);
        assert!(!editor.is_dirty());
        assert_eq!(mgr.palette().window_bg, Palette::default().window_bg);
    }

    #[test]
    fn chat_background_renders_gradients_patterns_blur_motion_and_per_peer_overrides() {
        let mut bg = ChatBackground::new();

        // All procedural GPL-safe patterns generate non-empty alpha masks
        for &kind in PatternKind::ALL {
            let mask = bg.pattern_mask(kind).expect("non-empty pattern mask");
            assert_eq!(mask.width(), 256);
            assert!(mask.pixels().any(|p| p.0[3] > 100));
            assert!(mask.pixels().any(|p| p.0[3] == 0));
        }

        // Render default patterned 4-color gradient
        let img0 = bg.create_current_image(None, 128, 128);
        assert_eq!(img0.dimensions(), (128, 128));

        // Motion step changes rendered complex gradient image
        let cfg_motion = BackgroundConfig::night_aurora();
        let m0 = bg.render_image(&cfg_motion, 96, 96, 0);
        bg.trigger_motion_step();
        assert_eq!(bg.motion_rotation_add(), 45);
        let m1 = bg.render_image(&cfg_motion, 96, 96, bg.motion_rotation_add());
        assert_ne!(m0.as_raw(), m1.as_raw());

        // Per-peer override resolves independently of global
        let peer_cfg = BackgroundConfig::solid(Color::rgb(0x11, 0x33, 0x55));
        bg.set_peer(42, peer_cfg);
        assert!(bg.has_peer_override(42));
        assert_eq!(
            bg.resolve_for_peer(Some(42)).wallpaper.background_colors,
            vec![Color::rgb(0x11, 0x33, 0x55)]
        );
        assert_ne!(
            bg.resolve_for_peer(Some(99)).wallpaper.background_colors,
            vec![Color::rgb(0x11, 0x33, 0x55)]
        );
        assert!(bg.clear_peer(42));
        assert!(!bg.has_peer_override(42));

        // Adjustable colors produce all 6 keys harmonized with background
        let base_pal = Palette::default();
        let adj = bg.adjustable_colors(&base_pal, None);
        assert_eq!(adj.len(), tg_theme::ADJUSTABLE_COLOR_KEYS.len());

        // Rejects SVG masquerading as photo background
        assert!(ChatBackground::load_config_from_bytes(b"<svg></svg>").is_err());

        // Valid PNG loads and blurs cleanly
        let sample_photo = image::RgbaImage::from_fn(64, 64, |x, y| {
            image::Rgba([(x * 4) as u8, (y * 4) as u8, 128, 255])
        });
        let mut png_buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(sample_photo)
            .write_to(&mut png_buf, image::ImageFormat::Png)
            .unwrap();
        let mut loaded = ChatBackground::load_config_from_bytes(&png_buf.into_inner()).unwrap();
        assert!(loaded.supports_blur());
        let sharp = bg.render_image(&loaded, 64, 64, 0);
        loaded.wallpaper = loaded.wallpaper.with_blurred(true);
        let blurred = bg.render_image(&loaded, 64, 64, 0);
        assert_ne!(sharp.as_raw(), blurred.as_raw());
    }

    #[test]
    fn background_picker_and_chat_theme_picker_apply_for_all_or_single_chat() {
        let mut bg = ChatBackground::new();
        let mut picker = BackgroundPickerState::new(false, Some((77, "Alex".to_owned())));
        assert!(!picker.items().is_empty());

        picker.category = BackgroundCategory::Solid;
        assert!(
            picker
                .visible_items()
                .iter()
                .all(|i| i.category == BackgroundCategory::Solid)
        );

        picker.category = BackgroundCategory::All;
        assert!(picker.select_by_id("sunset_bloom"));
        picker.preview.set_pattern_kind(PatternKind::Waves);
        picker.preview.set_pattern_intensity(65);
        assert_eq!(picker.preview.config.wallpaper.intensity, 65);
        assert!(picker.preview.config.wallpaper.is_pattern());

        // Apply to single chat only
        picker.preview.apply_to(
            &mut bg,
            BackgroundApplyScope::SingleChat {
                peer_id: 77,
                for_both: false,
            },
        );
        assert!(bg.has_peer_override(77));
        assert_eq!(bg.resolve_for_peer(Some(77)).pattern, PatternKind::Waves);
        assert_eq!(bg.resolve_for_peer(None).pattern, PatternKind::Doodles);

        // Apply to all chats
        picker
            .preview
            .apply_to(&mut bg, BackgroundApplyScope::AllChats);
        assert_eq!(bg.resolve_for_peer(None).pattern, PatternKind::Waves);

        // Single-chat emoji theme picker
        let mut ct = ChooseChatThemeState::new(88, "Design Group");
        assert!(!ct.has_unapplied_change());
        assert!(ct.select_token("astronaut"));
        assert!(ct.has_unapplied_change());
        assert_eq!(
            ct.apply_selected(&mut bg, true).as_deref(),
            Some("astronaut")
        );
        assert_eq!(ct.applied_token(88), Some("astronaut"));
        assert!(bg.has_peer_override(88));
        assert!(!ct.has_unapplied_change());

        // Reset back to default clears peer override
        assert!(ct.select_token(""));
        assert_eq!(ct.apply_selected(&mut bg, true), None);
        assert!(!bg.has_peer_override(88));
    }

    #[test]
    fn night_mode_scheduler_handles_system_follow_confirmation_and_midnight_schedule() {
        // 1. System mode follows OS transitions & requires confirmation on manual override
        let mut sched = NightModeScheduler::new(NightScheduleMode::System, false);
        assert_eq!(
            sched.evaluate(Some(true), 600),
            NightScheduleEvent::Switched { night_mode: true }
        );
        assert!(sched.is_night_mode());

        // Manual toggle while synced to OS dark mode asks for confirmation
        assert_eq!(
            sched.request_manual_toggle(),
            NightScheduleEvent::NeedsConfirmDisableSystem
        );
        assert!(sched.pending_disable_system_confirm);
        assert_eq!(
            sched.confirm_disable_system_and_toggle(),
            NightScheduleEvent::Switched { night_mode: false }
        );
        assert_eq!(sched.mode(), NightScheduleMode::Manual);
        assert!(!sched.is_night_mode());

        // 2. Entering theme editor disables system dark mode following
        let mut sched_ed = NightModeScheduler::new(NightScheduleMode::System, false);
        sched_ed.set_editing_theme(true);
        assert_eq!(sched_ed.mode(), NightScheduleMode::Manual);
        assert_eq!(
            sched_ed.evaluate(Some(true), 600),
            NightScheduleEvent::Unchanged
        );

        // 3. Scheduled window wrapping midnight (22:00 .. 07:00)
        assert!(NightScheduleMode::is_night_at_minute(
            22 * 60,
            7 * 60,
            23 * 60 + 30
        ));
        assert!(NightScheduleMode::is_night_at_minute(
            22 * 60,
            7 * 60,
            2 * 60
        ));
        assert!(!NightScheduleMode::is_night_at_minute(
            22 * 60,
            7 * 60,
            12 * 60
        ));
        assert_eq!(format_minutes_hhmm(22 * 60 + 5), "22:05");

        let mut sched_time = NightModeScheduler::new(NightScheduleMode::DEFAULT_SCHEDULE, false);
        assert_eq!(
            sched_time.evaluate(None, 23 * 60),
            NightScheduleEvent::Switched { night_mode: true }
        );
        assert_eq!(
            sched_time.evaluate(None, 10 * 60),
            NightScheduleEvent::Switched { night_mode: false }
        );
    }

    #[test]
    fn theme_preview_generator_produces_distinct_day_and_night_images() {
        assert_eq!(fill_initials("Eva Summer"), "ES");
        assert_eq!(fill_initials("Jean-Luc Picard"), "JP");
        assert_eq!(fill_initials("Alice"), "A");

        let day = ThemeInstance {
            palette: prepare_embedded_palette(EmbeddedType::Default, None).unwrap(),
            background: None,
            cached: Default::default(),
            tiled: false,
        };
        let night = ThemeInstance {
            palette: prepare_embedded_palette(EmbeddedType::Night, None).unwrap(),
            background: None,
            cached: Default::default(),
            tiled: false,
        };

        let day_img = generate_theme_preview(&day, PreviewType::Extended, None);
        let night_img = generate_theme_preview(&night, PreviewType::Extended, None);
        assert_eq!(day_img.dimensions(), (600, 446));
        assert_ne!(day_img.as_raw(), night_img.as_raw());

        let normal_img = generate_theme_preview(&day, PreviewType::Normal, None);
        assert_eq!(normal_img.dimensions(), (560, 390));

        let card_img = generate_compact_theme_card(&night, 136, 94);
        assert_eq!(card_img.dimensions(), (136, 94));
    }

    // --- Headless egui software rasterizer for generating crates/tg-app/docs/themes_*.png ---

    struct SoftwareTexture {
        width: usize,
        height: usize,
        pixels: Vec<[u8; 4]>,
    }

    fn render_egui_to_png(
        width_pt: u32,
        height_pt: u32,
        ppp: f32,
        mut draw: impl FnMut(&mut egui::Ui),
    ) -> Vec<u8> {
        let ctx = egui::Context::default();
        let mut textures: HashMap<TextureId, SoftwareTexture> = HashMap::new();
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(width_pt as f32, height_pt as f32),
            )),
            ..Default::default()
        };
        let mut last_output = None;
        for _ in 0..3 {
            let mut output = ctx.run_ui(raw_input.clone(), |ui| {
                ui.ctx().set_pixels_per_point(ppp);
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| draw(ui));
            });
            for (id, deltas) in &output.textures_delta.set {
                for delta in deltas {
                    apply_texture_delta(&mut textures, *id, delta);
                }
            }
            output.textures_delta.clear();
            last_output = Some(output);
        }
        let output = last_output.expect("at least one pass");
        let primitives = ctx.tessellate(output.shapes, ppp);
        let px_w = ((width_pt as f32) * ppp).round() as usize;
        let px_h = ((height_pt as f32) * ppp).round() as usize;
        let mut fb = vec![[255u8, 255, 255, 255]; px_w * px_h];

        for clipped in &primitives {
            let clip_min_x = (clipped.clip_rect.min.x * ppp).floor().max(0.0) as usize;
            let clip_min_y = (clipped.clip_rect.min.y * ppp).floor().max(0.0) as usize;
            let clip_max_x = (clipped.clip_rect.max.x * ppp).ceil().min(px_w as f32) as usize;
            let clip_max_y = (clipped.clip_rect.max.y * ppp).ceil().min(px_h as f32) as usize;
            if clip_min_x >= clip_max_x || clip_min_y >= clip_max_y {
                continue;
            }
            if let Primitive::Mesh(mesh) = &clipped.primitive {
                let tex = textures.get(&mesh.texture_id);
                for tri in mesh.indices.as_chunks::<3>().0 {
                    let v0 = mesh.vertices[tri[0] as usize];
                    let v1 = mesh.vertices[tri[1] as usize];
                    let v2 = mesh.vertices[tri[2] as usize];
                    rasterize_triangle(
                        &mut fb,
                        px_w,
                        (clip_min_x, clip_min_y, clip_max_x, clip_max_y),
                        ppp,
                        v0,
                        v1,
                        v2,
                        tex,
                    );
                }
            }
        }

        let mut img = image::RgbaImage::new(px_w as u32, px_h as u32);
        for (idx, &[r, g, b, a]) in fb.iter().enumerate() {
            img.put_pixel(
                (idx % px_w) as u32,
                (idx / px_w) as u32,
                image::Rgba([r, g, b, a]),
            );
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .expect("encode png");
        buf.into_inner()
    }

    fn apply_texture_delta(
        textures: &mut HashMap<TextureId, SoftwareTexture>,
        id: TextureId,
        delta: &egui::epaint::ImageDelta,
    ) {
        let (w, h, src_pixels): (usize, usize, Vec<[u8; 4]>) = match &delta.image {
            egui::ImageData::Color(c) => (
                c.size[0],
                c.size[1],
                c.pixels.iter().map(|p| p.to_srgba_unmultiplied()).collect(),
            ),
        };
        if let Some(pos) = delta.pos {
            if let Some(existing) = textures.get_mut(&id) {
                for y in 0..h {
                    for x in 0..w {
                        let dx = pos[0] + x;
                        let dy = pos[1] + y;
                        if dx < existing.width && dy < existing.height {
                            existing.pixels[dy * existing.width + dx] = src_pixels[y * w + x];
                        }
                    }
                }
            }
        } else {
            textures.insert(
                id,
                SoftwareTexture {
                    width: w,
                    height: h,
                    pixels: src_pixels,
                },
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rasterize_triangle(
        fb: &mut [[u8; 4]],
        fb_w: usize,
        clip: (usize, usize, usize, usize),
        ppp: f32,
        v0: egui::epaint::Vertex,
        v1: egui::epaint::Vertex,
        v2: egui::epaint::Vertex,
        tex: Option<&SoftwareTexture>,
    ) {
        let p0 = [v0.pos.x * ppp, v0.pos.y * ppp];
        let p1 = [v1.pos.x * ppp, v1.pos.y * ppp];
        let p2 = [v2.pos.x * ppp, v2.pos.y * ppp];

        let min_x = p0[0].min(p1[0]).min(p2[0]).floor().max(clip.0 as f32) as usize;
        let max_x = p0[0].max(p1[0]).max(p2[0]).ceil().min(clip.2 as f32) as usize;
        let min_y = p0[1].min(p1[1]).min(p2[1]).floor().max(clip.1 as f32) as usize;
        let max_y = p0[1].max(p1[1]).max(p2[1]).ceil().min(clip.3 as f32) as usize;

        let edge = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| -> f32 {
            (c[0] - a[0]) * (b[1] - a[1]) - (c[1] - a[1]) * (b[0] - a[0])
        };
        let area = edge(p0, p1, p2);
        if area.abs() < 1e-6 {
            return;
        }
        let inv_area = 1.0 / area;

        let c0 = v0.color.to_srgba_unmultiplied();
        let c1 = v1.color.to_srgba_unmultiplied();
        let c2 = v2.color.to_srgba_unmultiplied();

        for y in min_y..max_y {
            let py = y as f32 + 0.5;
            for x in min_x..max_x {
                let px = x as f32 + 0.5;
                let w0 = edge(p1, p2, [px, py]) * inv_area;
                let w1 = edge(p2, p0, [px, py]) * inv_area;
                let w2 = 1.0 - w0 - w1;
                if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                    continue;
                }
                let vr = f32::from(c0[0]) * w0 + f32::from(c1[0]) * w1 + f32::from(c2[0]) * w2;
                let vg = f32::from(c0[1]) * w0 + f32::from(c1[1]) * w1 + f32::from(c2[1]) * w2;
                let vb = f32::from(c0[2]) * w0 + f32::from(c1[2]) * w1 + f32::from(c2[2]) * w2;
                let va = f32::from(c0[3]) * w0 + f32::from(c1[3]) * w1 + f32::from(c2[3]) * w2;

                let (sr, sg, sb, sa) = if let Some(t) = tex {
                    let u = v0.uv.x * w0 + v1.uv.x * w1 + v2.uv.x * w2;
                    let v = v0.uv.y * w0 + v1.uv.y * w1 + v2.uv.y * w2;
                    let tp = sample_bilinear(t, u, v);
                    (
                        (vr * tp[0] / 255.0).clamp(0.0, 255.0),
                        (vg * tp[1] / 255.0).clamp(0.0, 255.0),
                        (vb * tp[2] / 255.0).clamp(0.0, 255.0),
                        (va * tp[3] / 255.0).clamp(0.0, 255.0),
                    )
                } else {
                    (vr, vg, vb, va)
                };

                let alpha = (sa / 255.0).clamp(0.0, 1.0);
                if alpha <= 0.0 {
                    continue;
                }
                let dst = &mut fb[y * fb_w + x];
                let inv = 1.0 - alpha;
                dst[0] = (sr * alpha + f32::from(dst[0]) * inv)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                dst[1] = (sg * alpha + f32::from(dst[1]) * inv)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                dst[2] = (sb * alpha + f32::from(dst[2]) * inv)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                dst[3] = 255;
            }
        }
    }

    fn sample_bilinear(tex: &SoftwareTexture, u: f32, v: f32) -> [f32; 4] {
        if tex.width == 0 || tex.height == 0 {
            return [255.0; 4];
        }
        let fx = (u * tex.width as f32 - 0.5).clamp(0.0, (tex.width - 1) as f32);
        let fy = (v * tex.height as f32 - 0.5).clamp(0.0, (tex.height - 1) as f32);
        let x0 = fx.floor() as usize;
        let y0 = fy.floor() as usize;
        let x1 = (x0 + 1).min(tex.width - 1);
        let y1 = (y0 + 1).min(tex.height - 1);
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let p00 = tex.pixels[y0 * tex.width + x0];
        let p10 = tex.pixels[y0 * tex.width + x1];
        let p01 = tex.pixels[y1 * tex.width + x0];
        let p11 = tex.pixels[y1 * tex.width + x1];
        let mut out = [0.0f32; 4];
        for i in 0..4 {
            let top = f32::from(p00[i]) * (1.0 - tx) + f32::from(p10[i]) * tx;
            let bot = f32::from(p01[i]) * (1.0 - tx) + f32::from(p11[i]) * tx;
            out[i] = top * (1.0 - ty) + bot * ty;
        }
        out
    }

    #[test]
    fn renders_theme_editor_background_picker_and_preview_screenshots() {
        let write_files = std::env::var("RUSTGRAM_UPDATE_SCREENSHOTS").as_deref() == Ok("1")
            || std::env::var("TGFORK_UPDATE_SCREENSHOTS").as_deref() == Ok("1");
        let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
        if write_files {
            let _ = std::fs::create_dir_all(&docs_dir);
        }

        // 1. Live Theme Editor screenshot (with active ColorEditor open on `windowBgActive`)
        let mut studio_editor = ThemesStudio::new();
        studio_editor.active_tab = ThemesTab::Editor;
        studio_editor.editor.activate_key("windowBgActive");
        let editor_png = render_egui_to_png(760, 720, 1.5, |ui| {
            studio_editor.show_at(ui, true, 1_000);
        });
        assert!(editor_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("themes_editor.png"), &editor_png)
                .expect("write themes_editor.png");
        }

        // 2. Background Picker + Interactive Message Preview screenshot
        let mut studio_bg = ThemesStudio::new();
        studio_bg.active_tab = ThemesTab::Backgrounds;
        studio_bg.bg_picker.select_by_id("arctic_waves");
        let bg_png = render_egui_to_png(860, 620, 1.5, |ui| {
            studio_bg.show_at(ui, true, 1_000);
        });
        assert!(bg_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("themes_backgrounds.png"), &bg_png)
                .expect("write themes_backgrounds.png");
        }

        // 3. Theme Previews Gallery + Chat Theme Picker screenshot
        let mut studio_prev = ThemesStudio::new();
        studio_prev.active_tab = ThemesTab::Previews;
        studio_prev.preview_embedded = EmbeddedType::Night;
        let prev_png = render_egui_to_png(760, 740, 1.5, |ui| {
            studio_prev.show_at(ui, true, 1_000);
        });
        assert!(prev_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("themes_preview.png"), &prev_png)
                .expect("write themes_preview.png");
        }

        // 4. Single-chat Emoji Theme Picker + Night Mode Schedule screenshot
        let mut studio_chat = ThemesStudio::new();
        studio_chat.active_tab = ThemesTab::ChatThemes;
        studio_chat.chat_theme_picker.select_token("astronaut");
        let chat_png = render_egui_to_png(760, 560, 1.5, |ui| {
            studio_chat.show_at(ui, true, 1_000);
        });
        assert!(chat_png.starts_with(&[137, 80, 78, 71]));
        if write_files {
            std::fs::write(docs_dir.join("themes_chat_picker.png"), &chat_png)
                .expect("write themes_chat_picker.png");
        }
    }
}
