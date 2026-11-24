//! Per-chat emoji and collectible unique-gift theme picker for a single chat.
//!
//! Port of `Telegram/SourceFiles/ui/chat/choose_theme_controller.h`,
//! `choose_theme_controller.cpp`, and `window/themes/window_themes_chat.cpp`.
//!
//! Hydrates available per-chat emoji themes from `account.getChatThemes`, collectible unique-gift
//! chat themes from `account.getUniqueGiftChatThemes`, and syncs per-chat selections via
//! `messages.setChatTheme` (including `inputChatThemeUniqueGift`), while falling back cleanly to
//! offline built-in presets when no live session is attached.

use std::collections::HashMap;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Key, Margin, Pos2, Rect, Role,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo,
};
use tg_lang::tr;
use tg_theme::{CloudThemeSettings, CloudThemeType, Color, WallPaperFlags};
use tg_ui::anim::{SimpleAnimation, Transition, with_alpha};
use tg_ui::tg_style::st;
use tg_ui::theme::{Colors, theme_color_to_egui};

use crate::themes::backend::{
    ChatThemeChoice, CloudThemeRecord, CloudThemesResult, ThemesBackend, UniqueGiftChatThemeRecord,
    UniqueGiftChatThemesResult,
};
use crate::themes::chat_background::{BackgroundConfig, ChatBackground, PatternKind};

/// Page size for `account.getUniqueGiftChatThemes` (`kGiftThemesLimit` in `choose_theme_controller.cpp`).
pub const UNIQUE_GIFT_THEMES_PAGE_LIMIT: i32 = 24;

/// Selection ring transition duration in ms.
const SELECTION_ANIM_DURATION_MS: u32 = 220;

/// One emoji chat theme definition with paired Day and Night visual settings.
#[derive(Debug, Clone)]
pub struct EmojiChatTheme {
    /// Theme token / emoticon (`""` for default / no per-chat theme).
    pub token: String,
    /// Display emoji or badge glyph.
    pub emoji: String,
    /// Human-readable theme name.
    pub name: String,
    /// Outgoing message bubble fill in Day mode.
    pub day_out_bg: Color,
    /// Incoming message bubble fill in Day mode.
    pub day_in_bg: Color,
    /// Background configuration in Day mode.
    pub day_bg: BackgroundConfig,
    /// Outgoing message bubble fill in Night mode.
    pub night_out_bg: Color,
    /// Incoming message bubble fill in Night mode.
    pub night_in_bg: Color,
    /// Background configuration in Night mode.
    pub night_bg: BackgroundConfig,
}

impl EmojiChatTheme {
    /// Returns `(outgoing_bubble, incoming_bubble, background)` for `dark_mode`.
    #[must_use]
    pub fn variant(&self, dark_mode: bool) -> (Color, Color, &BackgroundConfig) {
        if dark_mode {
            (self.night_out_bg, self.night_in_bg, &self.night_bg)
        } else {
            (self.day_out_bg, self.day_in_bg, &self.day_bg)
        }
    }
}

/// One collectible unique Star Gift chat theme (`chatThemeUniqueGift`) with paired Day/Night visuals.
#[derive(Debug, Clone)]
pub struct UniqueGiftChatTheme {
    /// Raw protocol record from `account.getUniqueGiftChatThemes`.
    pub record: UniqueGiftChatThemeRecord,
    /// Outgoing message bubble fill in Day mode.
    pub day_out_bg: Color,
    /// Incoming message bubble fill in Day mode.
    pub day_in_bg: Color,
    /// Background configuration in Day mode (supports `.tgv`/SVG pattern bytes).
    pub day_bg: BackgroundConfig,
    /// Outgoing message bubble fill in Night mode.
    pub night_out_bg: Color,
    /// Incoming message bubble fill in Night mode.
    pub night_in_bg: Color,
    /// Background configuration in Night mode (supports `.tgv`/SVG pattern bytes).
    pub night_bg: BackgroundConfig,
    /// Optional peer title where this unique gift is already assigned as a chat theme.
    pub taken_by_peer_name: Option<String>,
}

impl UniqueGiftChatTheme {
    /// Constructs a renderable [`UniqueGiftChatTheme`] from a WP-58 [`UniqueGiftChatThemeRecord`].
    #[must_use]
    pub fn from_record(record: UniqueGiftChatThemeRecord) -> Self {
        let light = record.theme.settings.get(&CloudThemeType::Light);
        let dark = record.theme.settings.get(&CloudThemeType::Dark);

        let fallback_accent = light
            .and_then(|s| s.paper.as_ref())
            .and_then(|p| p.background_colors.first().copied())
            .or_else(|| light.map(|s| s.accent_color))
            .unwrap_or(Color::rgb(0x40, 0xA7, 0xE3));
        let fallback_edge = light
            .and_then(|s| s.paper.as_ref())
            .and_then(|p| p.background_colors.get(1).copied())
            .or_else(|| dark.map(|s| s.accent_color))
            .unwrap_or(Color::rgb(0x2B, 0x52, 0x78));

        let day_out_bg =
            extract_outgoing_color(light, blend_tint(fallback_accent, Color::WHITE, 0.72));
        let night_out_bg = extract_outgoing_color(dark, fallback_edge);

        let default_day_bg = BackgroundConfig::gradient(
            vec![
                blend_tint(fallback_accent, Color::WHITE, 0.78),
                blend_tint(fallback_edge, Color::WHITE, 0.68),
                blend_tint(fallback_accent, Color::WHITE, 0.84),
            ],
            45,
            PatternKind::Constellation,
            34,
            true,
        );
        let mut default_night_bg = BackgroundConfig::gradient(
            vec![
                darken_color(fallback_edge, 0.42),
                darken_color(fallback_accent, 0.50),
                darken_color(fallback_edge, 0.34),
            ],
            45,
            PatternKind::Constellation,
            -44,
            true,
        );
        default_night_bg.wallpaper.flags =
            default_night_bg.wallpaper.flags.union(WallPaperFlags::DARK);

        let day_bg = bg_from_theme_settings(light, default_day_bg);
        let night_bg = bg_from_theme_settings(dark, default_night_bg);

        Self {
            record,
            day_out_bg,
            day_in_bg: Color::WHITE,
            day_bg,
            night_out_bg,
            night_in_bg: Color::rgb(0x18, 0x25, 0x33),
            night_bg,
            taken_by_peer_name: None,
        }
    }

    /// Returns `(outgoing_bubble, incoming_bubble, background)` for `dark_mode`.
    #[must_use]
    pub fn variant(&self, dark_mode: bool) -> (Color, Color, &BackgroundConfig) {
        if dark_mode {
            (self.night_out_bg, self.night_in_bg, &self.night_bg)
        } else {
            (self.day_out_bg, self.day_in_bg, &self.day_bg)
        }
    }

    /// Display title for cards and preview banners.
    #[must_use]
    pub fn display_title(&self) -> &str {
        if !self.record.title.is_empty() {
            &self.record.title
        } else {
            &self.record.slug
        }
    }
}

/// Output events emitted by [`ChooseChatThemeState::show`].
#[derive(Debug, Clone, Default)]
pub struct ChooseChatThemeOutput {
    /// User committed a chat theme token (`None` means reset to default) for `peer_id`.
    ///
    /// For backward compatibility with existing callers, emoji themes emit `Some(emoticon)`,
    /// unique gift themes emit `Some(slug)`, and default emits `None`. Prefer
    /// [`Self::applied_choice`] to distinguish `Emoticon` vs `UniqueGift`.
    pub applied_token: Option<(i64, Option<String>)>,
    /// Typed `messages.setChatTheme` payload (`Empty`, `Emoticon`, or `UniqueGift`).
    pub applied_choice: Option<(i64, ChatThemeChoice)>,
    /// User clicked "Choose Wallpaper..." to open the background picker scoped to this chat.
    pub open_background_picker_for_peer: Option<(i64, String)>,
    /// Transient toast notification.
    pub toast: Option<String>,
}

/// Per-chat theme picker controller (`Ui::ChooseThemeController`).
pub struct ChooseChatThemeState {
    /// Target chat peer ID.
    pub peer_id: i64,
    /// Target chat display title.
    pub peer_name: String,
    /// Available emoji chat themes (built-in presets or live catalog from `account.getChatThemes`).
    themes: Vec<EmojiChatTheme>,
    /// Collectible unique Star Gift chat themes from `account.getUniqueGiftChatThemes`.
    gift_themes: Vec<UniqueGiftChatTheme>,
    /// Currently previewed/selected choice (`Empty`, `Emoticon`, or `UniqueGift`).
    selected_choice: ChatThemeChoice,
    /// Committed per-chat choices by `peer_id`.
    applied_per_peer: HashMap<i64, ChatThemeChoice>,
    /// Whether live cloud sync (`account.getChatThemes`) has hydrated this picker.
    pub cloud_synced: bool,
    /// Last `account.getChatThemes` hash returned by the server.
    cloud_hash: i64,
    /// Whether `account.getUniqueGiftChatThemes` has been fetched at least once.
    pub gift_themes_synced: bool,
    /// Last `account.getUniqueGiftChatThemes` hash returned by the server.
    gift_hash: i64,
    /// Next pagination offset returned by `account.getUniqueGiftChatThemes`, if any.
    gift_next_offset: Option<String>,
    /// Pending confirmation when applying a collectible gift currently used in another chat.
    pending_gift_replace: Option<(String, String)>,
    /// Interruptible 220ms `easeOutCubic` selection ring transition.
    selection_anim: SimpleAnimation,
    /// Monotonic counter incremented whenever the selected theme preview changes.
    preview_generation: u64,
}

impl ChooseChatThemeState {
    /// Creates a new per-chat theme picker targeting `(peer_id, peer_name)`.
    #[must_use]
    pub fn new(peer_id: i64, peer_name: impl Into<String>) -> Self {
        Self {
            peer_id,
            peer_name: peer_name.into(),
            themes: builtin_emoji_chat_themes(),
            gift_themes: Vec::new(),
            selected_choice: ChatThemeChoice::Empty,
            applied_per_peer: HashMap::new(),
            cloud_synced: false,
            cloud_hash: 0,
            gift_themes_synced: false,
            gift_hash: 0,
            gift_next_offset: None,
            pending_gift_replace: None,
            selection_anim: SimpleAnimation::new(),
            preview_generation: 0,
        }
    }

    /// Returns the last `account.getChatThemes` cache hash observed from the live session.
    #[must_use]
    pub const fn cloud_hash(&self) -> i64 {
        self.cloud_hash
    }

    /// Returns the last `account.getUniqueGiftChatThemes` cache hash observed from the live session.
    #[must_use]
    pub const fn gift_hash(&self) -> i64 {
        self.gift_hash
    }

    /// Returns the next pagination offset for `account.getUniqueGiftChatThemes`, if more pages exist.
    #[must_use]
    pub fn gift_next_offset(&self) -> Option<&str> {
        self.gift_next_offset.as_deref()
    }

    /// Returns the preview generation counter (incremented on every theme selection change).
    #[must_use]
    pub const fn preview_generation(&self) -> u64 {
        self.preview_generation
    }

    /// Hydrates the per-chat emoji theme catalog directly from a live [`ThemesBackend`] via
    /// `account.getChatThemes(hash)`.
    pub fn hydrate_from_backend(&mut self, backend: &dyn ThemesBackend) -> Result<usize, String> {
        match backend.get_chat_themes(self.cloud_hash)? {
            CloudThemesResult::Themes { hash, themes } => {
                self.cloud_hash = hash;
                self.apply_cloud_chat_themes(&themes);
                self.cloud_synced = true;
                Ok(self.themes.len())
            }
            CloudThemesResult::NotModified => {
                self.cloud_synced = true;
                Ok(self.themes.len())
            }
        }
    }

    /// Hydrates collectible unique-gift chat themes from a live [`ThemesBackend`] via
    /// `account.getUniqueGiftChatThemes(offset, 24, hash)`.
    pub fn hydrate_unique_gift_themes(
        &mut self,
        backend: &dyn ThemesBackend,
        load_more: bool,
    ) -> Result<usize, String> {
        let (offset, hash) = if load_more {
            let Some(next) = self.gift_next_offset.as_deref() else {
                return Ok(self.gift_themes.len());
            };
            (next, 0)
        } else {
            ("", self.gift_hash)
        };

        match backend.get_unique_gift_chat_themes(offset, UNIQUE_GIFT_THEMES_PAGE_LIMIT, hash)? {
            UniqueGiftChatThemesResult::Themes {
                hash: new_hash,
                themes,
                next_offset,
            } => {
                if !load_more {
                    self.gift_hash = new_hash;
                }
                self.gift_next_offset = next_offset.filter(|s| !s.is_empty());
                self.apply_unique_gift_chat_themes(&themes, load_more);
                self.gift_themes_synced = true;
                Ok(self.gift_themes.len())
            }
            UniqueGiftChatThemesResult::NotModified => {
                self.gift_themes_synced = true;
                Ok(self.gift_themes.len())
            }
        }
    }

    /// Resets live cloud hydration state and restores offline fallback presets (e.g. on logout).
    pub fn reset_cloud_state(&mut self) {
        self.cloud_hash = 0;
        self.cloud_synced = false;
        self.gift_hash = 0;
        self.gift_themes_synced = false;
        self.gift_next_offset = None;
        self.gift_themes.clear();
        self.pending_gift_replace = None;
        self.themes = builtin_emoji_chat_themes();
    }

    /// Replaces or updates the emoji chat theme catalog from live `account.getChatThemes` records.
    pub fn apply_cloud_chat_themes(&mut self, records: &[CloudThemeRecord]) {
        let mut parsed = Vec::new();
        for rec in records {
            let token = if !rec.theme.emoticon.is_empty() {
                rec.theme.emoticon.clone()
            } else if !rec.theme.slug.is_empty() {
                rec.theme.slug.clone()
            } else {
                continue;
            };
            let title = if !rec.theme.title.is_empty() {
                rec.theme.title.clone()
            } else {
                token.clone()
            };
            let light = rec.theme.settings.get(&CloudThemeType::Light);
            let dark = rec.theme.settings.get(&CloudThemeType::Dark);

            let day_out_bg = extract_outgoing_color(light, Color::rgb(0xEF, 0xFD, 0xDE));
            let night_out_bg = extract_outgoing_color(dark, Color::rgb(0x2B, 0x52, 0x78));

            let day_bg = bg_from_theme_settings(light, BackgroundConfig::classic_meadow());
            let night_bg = bg_from_theme_settings(dark, BackgroundConfig::night_aurora());

            parsed.push(EmojiChatTheme {
                token: token.clone(),
                emoji: token,
                name: title,
                day_out_bg,
                day_in_bg: Color::WHITE,
                day_bg,
                night_out_bg,
                night_in_bg: Color::rgb(0x18, 0x25, 0x33),
                night_bg,
            });
        }
        if !parsed.is_empty() {
            self.themes = parsed;
            self.cloud_synced = true;
        }
    }

    /// Replaces or appends collectible unique-gift chat themes from
    /// `account.getUniqueGiftChatThemes` records.
    pub fn apply_unique_gift_chat_themes(
        &mut self,
        records: &[UniqueGiftChatThemeRecord],
        append: bool,
    ) {
        if !append {
            self.gift_themes.clear();
        }
        for rec in records {
            if rec.slug.is_empty() {
                continue;
            }
            if let Some(existing) = self
                .gift_themes
                .iter_mut()
                .find(|g| g.record.slug == rec.slug)
            {
                let taken = existing.taken_by_peer_name.clone();
                let mut updated = UniqueGiftChatTheme::from_record(rec.clone());
                updated.taken_by_peer_name = taken;
                *existing = updated;
            } else {
                self.gift_themes
                    .push(UniqueGiftChatTheme::from_record(rec.clone()));
            }
        }
        self.gift_themes_synced = true;
    }

    /// Marks a collectible unique-gift theme as already used in another chat (`taken_by`),
    /// so clicking Apply prompts the tdesktop reassignment confirmation first.
    pub fn set_gift_taken_by(&mut self, slug: &str, peer_name: Option<String>) -> bool {
        if let Some(gift) = self.gift_themes.iter_mut().find(|g| g.record.slug == slug) {
            gift.taken_by_peer_name = peer_name;
            true
        } else {
            false
        }
    }

    /// Attaches raw SVG or gzipped `.tgv` pattern wallpaper bytes to a collectible gift theme's
    /// Day or Night background so `ChatBackground` rasterizes it via `tg_theme::svg_pattern`.
    pub fn set_gift_pattern_bytes(&mut self, slug: &str, dark: bool, bytes: Vec<u8>) -> bool {
        if let Some(gift) = self.gift_themes.iter_mut().find(|g| g.record.slug == slug) {
            if dark {
                gift.night_bg = gift.night_bg.clone().with_pattern_bytes(bytes);
            } else {
                gift.day_bg = gift.day_bg.clone().with_pattern_bytes(bytes);
            }
            self.preview_generation = self.preview_generation.wrapping_add(1);
            true
        } else {
            false
        }
    }

    /// Returns all available emoji chat themes (live `account.getChatThemes` when hydrated,
    /// or offline fallback presets).
    #[must_use]
    pub fn themes(&self) -> &[EmojiChatTheme] {
        &self.themes
    }

    /// Returns all hydrated collectible unique-gift chat themes.
    #[must_use]
    pub fn gift_themes(&self) -> &[UniqueGiftChatTheme] {
        &self.gift_themes
    }

    /// Returns raw [`UniqueGiftChatThemeRecord`] clones (for sharing with `ShareThemePickerState`).
    #[must_use]
    pub fn gift_theme_records(&self) -> Vec<UniqueGiftChatThemeRecord> {
        self.gift_themes.iter().map(|g| g.record.clone()).collect()
    }

    /// Returns the currently selected (previewed) typed [`ChatThemeChoice`].
    #[must_use]
    pub fn selected_choice(&self) -> &ChatThemeChoice {
        &self.selected_choice
    }

    /// Returns the currently selected (previewed) theme token (`""` = None, emoticon for emoji
    /// themes, or slug for unique gift themes).
    #[must_use]
    pub fn selected_token(&self) -> &str {
        match &self.selected_choice {
            ChatThemeChoice::Empty => "",
            ChatThemeChoice::Emoticon(tok) => tok.as_str(),
            ChatThemeChoice::UniqueGift(slug) => slug.as_str(),
        }
    }

    /// Returns the committed typed [`ChatThemeChoice`] for `peer_id`, if any.
    #[must_use]
    pub fn applied_choice(&self, peer_id: i64) -> Option<&ChatThemeChoice> {
        self.applied_per_peer
            .get(&peer_id)
            .filter(|c| !matches!(c, ChatThemeChoice::Empty))
    }

    /// Returns the committed theme token for `peer_id`, if any.
    #[must_use]
    pub fn applied_token(&self, peer_id: i64) -> Option<&str> {
        match self.applied_per_peer.get(&peer_id) {
            Some(ChatThemeChoice::Emoticon(tok)) if !tok.is_empty() => Some(tok.as_str()),
            Some(ChatThemeChoice::UniqueGift(slug)) if !slug.is_empty() => Some(slug.as_str()),
            _ => None,
        }
    }

    /// Returns `true` when the previewed choice differs from the committed choice for `peer_id`.
    #[must_use]
    pub fn has_unapplied_change(&self) -> bool {
        let current = self
            .applied_per_peer
            .get(&self.peer_id)
            .cloned()
            .unwrap_or(ChatThemeChoice::Empty);
        self.selected_choice != current
    }

    /// Selects an emoji theme token (or `""` for None, or a known gift slug) for live preview.
    pub fn select_token(&mut self, token: &str) -> bool {
        let next = if token.is_empty() {
            ChatThemeChoice::Empty
        } else if self.themes.iter().any(|t| t.token == token) {
            ChatThemeChoice::Emoticon(token.to_owned())
        } else if self.gift_themes.iter().any(|g| g.record.slug == token) {
            ChatThemeChoice::UniqueGift(token.to_owned())
        } else {
            return false;
        };
        self.set_selected_choice(next, None);
        true
    }

    /// Selects a collectible unique-gift theme by its `slug` for live preview.
    pub fn select_gift_slug(&mut self, slug: &str) -> bool {
        if self.gift_themes.iter().any(|g| g.record.slug == slug) {
            self.set_selected_choice(ChatThemeChoice::UniqueGift(slug.to_owned()), None);
            true
        } else {
            false
        }
    }

    fn set_selected_choice(&mut self, next: ChatThemeChoice, now_sec: Option<f64>) {
        if self.selected_choice != next {
            self.selected_choice = next;
            self.pending_gift_replace = None;
            self.preview_generation = self.preview_generation.wrapping_add(1);
            if let Some(t) = now_sec {
                self.selection_anim.start(
                    t,
                    0.0,
                    1.0,
                    SELECTION_ANIM_DURATION_MS,
                    Transition::EaseOutCubic,
                );
            }
        }
    }

    /// Applies the currently selected emoji or unique-gift theme to `peer_id`, updating `chat_bg`
    /// with the theme's wallpaper (or clearing the per-peer override when `"None"` is chosen).
    pub fn apply_selected(
        &mut self,
        chat_bg: &mut ChatBackground,
        dark_mode: bool,
    ) -> Option<String> {
        let choice = self.apply_selected_choice(chat_bg, dark_mode);
        match choice {
            ChatThemeChoice::Empty => None,
            ChatThemeChoice::Emoticon(tok) => Some(tok),
            ChatThemeChoice::UniqueGift(slug) => Some(slug),
        }
    }

    /// Typed variant of [`Self::apply_selected`] returning [`ChatThemeChoice`].
    pub fn apply_selected_choice(
        &mut self,
        chat_bg: &mut ChatBackground,
        dark_mode: bool,
    ) -> ChatThemeChoice {
        self.pending_gift_replace = None;
        match self.selected_choice.clone() {
            ChatThemeChoice::Empty => {
                self.applied_per_peer.remove(&self.peer_id);
                chat_bg.clear_peer(self.peer_id);
                ChatThemeChoice::Empty
            }
            ChatThemeChoice::Emoticon(tok) => {
                if let Some(theme) = self.themes.iter().find(|t| t.token == tok) {
                    let (_, _, bg) = theme.variant(dark_mode);
                    chat_bg.set_peer(self.peer_id, bg.clone());
                    let choice = ChatThemeChoice::Emoticon(tok);
                    self.applied_per_peer.insert(self.peer_id, choice.clone());
                    return choice;
                }
                ChatThemeChoice::Empty
            }
            ChatThemeChoice::UniqueGift(slug) => {
                if let Some(gift) = self.gift_themes.iter().find(|g| g.record.slug == slug) {
                    let (_, _, bg) = gift.variant(dark_mode);
                    chat_bg.set_peer(self.peer_id, bg.clone());
                    let choice = ChatThemeChoice::UniqueGift(slug);
                    self.applied_per_peer.insert(self.peer_id, choice.clone());
                    return choice;
                }
                ChatThemeChoice::Empty
            }
        }
    }

    /// Re-syncs the active per-chat background when Day/Night mode toggles.
    pub fn sync_dark_mode(&self, chat_bg: &mut ChatBackground, dark_mode: bool) {
        for (&peer_id, choice) in &self.applied_per_peer {
            match choice {
                ChatThemeChoice::Empty => {}
                ChatThemeChoice::Emoticon(token) => {
                    if let Some(theme) = self.themes.iter().find(|t| t.token == *token) {
                        let (_, _, bg) = theme.variant(dark_mode);
                        chat_bg.set_peer(peer_id, bg.clone());
                    }
                }
                ChatThemeChoice::UniqueGift(slug) => {
                    if let Some(gift) = self.gift_themes.iter().find(|g| g.record.slug == *slug) {
                        let (_, _, bg) = gift.variant(dark_mode);
                        chat_bg.set_peer(peer_id, bg.clone());
                    }
                }
            }
        }
    }

    /// Renders the single-chat theme picker panel (`ChooseThemeController`), hydrating from
    /// `backend` via `account.getChatThemes` and `account.getUniqueGiftChatThemes` on first render.
    pub fn show_with_backend(
        &mut self,
        ui: &mut Ui,
        chat_bg: &mut ChatBackground,
        dark_mode: bool,
        colors: &dyn Colors,
        backend: Option<&dyn ThemesBackend>,
    ) -> ChooseChatThemeOutput {
        if let Some(be) = backend {
            if !self.cloud_synced {
                let _ = self.hydrate_from_backend(be);
            }
            if !self.gift_themes_synced {
                let _ = self.hydrate_unique_gift_themes(be, false);
            }
        }
        self.show_inner(ui, chat_bg, dark_mode, colors, backend)
    }

    /// Renders the single-chat emoji & collectible gift theme picker panel (`ChooseThemeController`).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        chat_bg: &mut ChatBackground,
        dark_mode: bool,
        colors: &dyn Colors,
    ) -> ChooseChatThemeOutput {
        self.show_inner(ui, chat_bg, dark_mode, colors, None)
    }

    fn show_inner(
        &mut self,
        ui: &mut Ui,
        chat_bg: &mut ChatBackground,
        dark_mode: bool,
        colors: &dyn Colors,
        backend: Option<&dyn ThemesBackend>,
    ) -> ChooseChatThemeOutput {
        let mut out = ChooseChatThemeOutput::default();
        let bold_fg = colors.color("windowBoldFg");
        let sub_fg = colors.color("windowSubTextFg");
        let active_bg = colors.color("windowBgActive");
        let accent_fg = colors.color("activeButtonFg");
        let divider = colors.color("shadowFg");
        let bg_over = colors.color("windowBgOver");

        let now_sec = ui.ctx().input(|i| i.time);
        let anim_progress = self.selection_anim.animate_egui(ui.ctx(), 1.0) as f32;

        // Dimensions derived from `settings.style` (`chatTheme*` tokens)
        let card_w = (st::chat_theme_preview_size.width.scaled() as f32).max(88.0);
        let preview_h = ((st::chat_theme_preview_size.height.scaled() as f32) - 20.0).max(86.0);
        let footer_h = 28.0;
        let card_h = preview_h + footer_h;
        let bubble_size = Vec2::new(
            ((st::chat_theme_bubble_size.width.scaled() as f32) - 8.0).max(38.0),
            ((st::chat_theme_bubble_size.height.scaled() as f32) - 4.0).max(16.0),
        );
        let bubble_radius =
            CornerRadius::same(st::chat_theme_bubble_radius.scaled().clamp(6, 10) as u8);
        let entry_skip = (st::chat_theme_entry_skip.scaled() as f32).max(10.0);
        let bubble_pos_x = st::chat_theme_bubble_position.x.scaled() as f32;
        let bubble_pos_y = st::chat_theme_bubble_position.y.scaled() as f32;
        let bubble_skip = st::chat_theme_bubble_skip.scaled() as f32;

        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} for {}",
                    tr::lng_chat_theme_title(),
                    self.peer_name
                ))
                .size(15.0)
                .strong()
                .color(bold_fg),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let badge = if self.cloud_synced {
                    "Cloud synced"
                } else {
                    "Local preview"
                };
                ui.label(egui::RichText::new(badge).size(11.0).color(sub_fg));
            });
        });
        ui.label(
            egui::RichText::new(
                "Pick an emoji theme or collectible gift theme for this chat, or choose a custom wallpaper.",
            )
            .size(12.0)
            .color(sub_fg),
        );
        ui.add_space(10.0);

        let mut newly_selected: Option<ChatThemeChoice> = None;

        // 1. Standard Emoji Chat Themes horizontal strip
        ScrollArea::horizontal()
            .id_salt("tg_choose_chat_theme_strip")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = entry_skip;

                    // "No Theme" card (`GenerateEmptyPreview` in `choose_theme_controller.cpp`)
                    let none_selected = matches!(self.selected_choice, ChatThemeChoice::Empty);
                    let (none_rect, none_resp) =
                        ui.allocate_exact_size(Vec2::new(card_w, card_h), Sense::click());
                    none_resp.widget_info(|| {
                        WidgetInfo::selected(
                            Role::Button,
                            ui.is_enabled(),
                            none_selected,
                            "No Theme",
                        )
                    });
                    if none_resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    ui.painter()
                        .rect_filled(none_rect, CornerRadius::same(10), bg_over);
                    paint_selection_border(
                        ui,
                        none_rect,
                        none_selected,
                        none_resp.has_focus(),
                        anim_progress,
                        active_bg,
                        divider,
                    );
                    let none_label = tr::lng_chat_theme_none().replace('\n', " ");
                    ui.painter().text(
                        Pos2::new(none_rect.center().x, none_rect.top() + 34.0),
                        Align2::CENTER_CENTER,
                        &none_label,
                        FontId::proportional(12.0),
                        if none_selected { bold_fg } else { sub_fg },
                    );
                    let xc = Pos2::new(none_rect.center().x, none_rect.bottom() - 28.0);
                    ui.painter()
                        .circle_stroke(xc, 11.0, Stroke::new(1.5, sub_fg));
                    ui.painter().line_segment(
                        [
                            Pos2::new(xc.x - 4.5, xc.y - 4.5),
                            Pos2::new(xc.x + 4.5, xc.y + 4.5),
                        ],
                        Stroke::new(1.6, sub_fg),
                    );
                    ui.painter().line_segment(
                        [
                            Pos2::new(xc.x + 4.5, xc.y - 4.5),
                            Pos2::new(xc.x - 4.5, xc.y + 4.5),
                        ],
                        Stroke::new(1.6, sub_fg),
                    );
                    if card_activated(ui, &none_resp) {
                        newly_selected = Some(ChatThemeChoice::Empty);
                    }

                    // Emoji chat theme cards (`GeneratePreview`)
                    for theme in &self.themes {
                        let is_selected = matches!(
                            &self.selected_choice,
                            ChatThemeChoice::Emoticon(tok) if tok == &theme.token
                        );
                        let (out_c, in_c, bg_cfg) = theme.variant(dark_mode);
                        let (card_rect, resp) =
                            ui.allocate_exact_size(Vec2::new(card_w, card_h), Sense::click());
                        resp.widget_info(|| {
                            WidgetInfo::selected(
                                Role::Button,
                                ui.is_enabled(),
                                is_selected,
                                format!("{} chat theme", theme.name),
                            )
                        });
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        let preview_rect =
                            Rect::from_min_size(card_rect.min, Vec2::new(card_w, preview_h));
                        chat_bg.paint_config(ui, preview_rect, bg_cfg, dark_mode);

                        // Mini outgoing + incoming bubbles matching `ChooseThemeController::GeneratePreview`
                        let sent_rect = Rect::from_min_size(
                            Pos2::new(
                                preview_rect.right() - bubble_size.x - bubble_pos_x,
                                preview_rect.top() + bubble_pos_y,
                            ),
                            bubble_size,
                        );
                        let recv_rect = Rect::from_min_size(
                            Pos2::new(
                                preview_rect.left() + bubble_pos_x,
                                sent_rect.bottom() + bubble_skip,
                            ),
                            bubble_size,
                        );
                        ui.painter().rect_filled(
                            sent_rect,
                            bubble_radius,
                            theme_color_to_egui(out_c),
                        );
                        ui.painter().rect_filled(
                            recv_rect,
                            bubble_radius,
                            theme_color_to_egui(in_c),
                        );

                        let footer_rect = Rect::from_min_max(
                            Pos2::new(card_rect.left(), preview_rect.bottom()),
                            card_rect.max,
                        );
                        ui.painter()
                            .rect_filled(footer_rect, CornerRadius::ZERO, bg_over);
                        ui.painter().circle_filled(
                            Pos2::new(footer_rect.left() + 13.0, footer_rect.center().y),
                            4.5,
                            theme_color_to_egui(out_c),
                        );
                        ui.painter().circle_stroke(
                            Pos2::new(footer_rect.left() + 13.0, footer_rect.center().y),
                            4.5,
                            Stroke::new(1.0, divider),
                        );
                        paint_clamped_label(
                            ui,
                            Pos2::new(footer_rect.left() + 23.0, footer_rect.center().y),
                            (footer_rect.width() - 27.0).max(24.0),
                            &theme.name,
                            11.5,
                            if is_selected { bold_fg } else { sub_fg },
                        );
                        paint_selection_border(
                            ui,
                            card_rect,
                            is_selected,
                            resp.has_focus(),
                            anim_progress,
                            active_bg,
                            divider,
                        );

                        if card_activated(ui, &resp) {
                            newly_selected = Some(ChatThemeChoice::Emoticon(theme.token.clone()));
                        }
                    }
                });
            });

        ui.add_space(12.0);

        // 2. Collectible Unique Gift Chat Themes section (`account.getUniqueGiftChatThemes`)
        let mut load_more_clicked = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new("Collectible Gift Themes")
                    .size(13.5)
                    .strong()
                    .color(bold_fg),
            );
            let count_label = if self.gift_themes.is_empty() {
                "None loaded".to_owned()
            } else {
                format!("{} available", self.gift_themes.len())
            };
            ui.label(egui::RichText::new(count_label).size(11.5).color(sub_fg));
            if self.gift_next_offset.is_some()
                && backend.is_some()
                && ui.small_button("Load More").clicked()
            {
                load_more_clicked = true;
            }
        });
        ui.add_space(6.0);

        if self.gift_themes.is_empty() {
            Frame::NONE
                .fill(bg_over)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(
                            "Collectible gift themes from your profile will appear here once synced.",
                        )
                        .size(12.0)
                        .color(sub_fg),
                    );
                });
        } else {
            ScrollArea::horizontal()
                .id_salt("tg_choose_unique_gift_theme_strip")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = entry_skip;

                        for gift in &self.gift_themes {
                            let is_selected = matches!(
                                &self.selected_choice,
                                ChatThemeChoice::UniqueGift(slug) if slug == &gift.record.slug
                            );
                            let (out_c, in_c, bg_cfg) = gift.variant(dark_mode);
                            let (card_rect, resp) = ui.allocate_exact_size(
                                Vec2::new(card_w + 18.0, card_h),
                                Sense::click(),
                            );
                            let title = gift.display_title();
                            resp.widget_info(|| {
                                WidgetInfo::selected(
                                    Role::Button,
                                    ui.is_enabled(),
                                    is_selected,
                                    format!("{} gift theme #{}", title, gift.record.num),
                                )
                            });
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }

                            let preview_rect = Rect::from_min_size(
                                card_rect.min,
                                Vec2::new(card_rect.width(), preview_h),
                            );
                            chat_bg.paint_config(ui, preview_rect, bg_cfg, dark_mode);

                            // Mini bubbles
                            let sent_rect = Rect::from_min_size(
                                Pos2::new(
                                    preview_rect.right() - bubble_size.x - 6.0,
                                    preview_rect.top() + 26.0,
                                ),
                                bubble_size,
                            );
                            let recv_rect = Rect::from_min_size(
                                Pos2::new(preview_rect.left() + 6.0, sent_rect.bottom() + 6.0),
                                bubble_size,
                            );
                            ui.painter().rect_filled(
                                sent_rect,
                                bubble_radius,
                                theme_color_to_egui(out_c),
                            );
                            ui.painter().rect_filled(
                                recv_rect,
                                bubble_radius,
                                theme_color_to_egui(in_c),
                            );

                            // Collectible `#num` badge pill
                            let num_text = if gift.record.num > 0 {
                                format!("#{}", gift.record.num)
                            } else {
                                "Gift".to_owned()
                            };
                            let badge_rect = Rect::from_min_size(
                                Pos2::new(preview_rect.left() + 6.0, preview_rect.top() + 5.0),
                                Vec2::new((num_text.len() as f32 * 6.5 + 10.0).min(56.0), 16.0),
                            );
                            ui.painter().rect_filled(
                                badge_rect,
                                CornerRadius::same(8),
                                Color32::from_rgba_unmultiplied(15, 20, 28, 178),
                            );
                            ui.painter().text(
                                badge_rect.center(),
                                Align2::CENTER_CENTER,
                                &num_text,
                                FontId::proportional(10.0),
                                Color32::WHITE,
                            );

                            // "In use" pill when assigned in another chat
                            if gift.taken_by_peer_name.is_some() {
                                let used_rect = Rect::from_min_size(
                                    Pos2::new(
                                        preview_rect.right() - 40.0,
                                        preview_rect.top() + 5.0,
                                    ),
                                    Vec2::new(34.0, 16.0),
                                );
                                ui.painter().rect_filled(
                                    used_rect,
                                    CornerRadius::same(8),
                                    with_alpha(active_bg, 0.90),
                                );
                                ui.painter().text(
                                    used_rect.center(),
                                    Align2::CENTER_CENTER,
                                    "Used",
                                    FontId::proportional(9.5),
                                    accent_fg,
                                );
                            }

                            let footer_rect = Rect::from_min_max(
                                Pos2::new(card_rect.left(), preview_rect.bottom()),
                                card_rect.max,
                            );
                            ui.painter()
                                .rect_filled(footer_rect, CornerRadius::ZERO, bg_over);
                            let dot_color = gift
                                .record
                                .theme
                                .settings
                                .get(&CloudThemeType::Light)
                                .map(|s| s.accent_color)
                                .unwrap_or(out_c);
                            ui.painter().circle_filled(
                                Pos2::new(footer_rect.left() + 12.0, footer_rect.center().y),
                                4.5,
                                theme_color_to_egui(dot_color),
                            );
                            ui.painter().circle_stroke(
                                Pos2::new(footer_rect.left() + 12.0, footer_rect.center().y),
                                4.5,
                                Stroke::new(1.0, divider),
                            );
                            paint_clamped_label(
                                ui,
                                Pos2::new(footer_rect.left() + 21.0, footer_rect.center().y),
                                (footer_rect.width() - 28.0).max(24.0),
                                title,
                                11.5,
                                if is_selected { bold_fg } else { sub_fg },
                            );
                            paint_selection_border(
                                ui,
                                card_rect,
                                is_selected,
                                resp.has_focus(),
                                anim_progress,
                                active_bg,
                                divider,
                            );

                            if card_activated(ui, &resp) {
                                newly_selected =
                                    Some(ChatThemeChoice::UniqueGift(gift.record.slug.clone()));
                            }
                        }
                    });
                });
        }

        if load_more_clicked
            && let Some(be) = backend
            && let Err(err) = self.hydrate_unique_gift_themes(be, true)
        {
            out.toast = Some(format!("Load gift themes failed: {err}"));
        }

        if let Some(choice) = newly_selected {
            self.set_selected_choice(choice, Some(now_sec));
        }

        ui.add_space(12.0);

        // 3. Live single-chat preview strip of selected emoji OR unique gift theme
        if let Some((preview_label, out_c, in_c, bg_cfg)) = self.selected_preview_variant(dark_mode)
        {
            let strip_w = ui.available_width().max(220.0);
            let (strip_rect, _) = ui.allocate_exact_size(Vec2::new(strip_w, 92.0), Sense::hover());
            chat_bg.paint_config(ui, strip_rect, bg_cfg, dark_mode);

            let bubble_w = (strip_rect.width() * 0.60).clamp(140.0, 220.0);
            let in_b = Rect::from_min_size(
                Pos2::new(strip_rect.left() + 12.0, strip_rect.top() + 12.0),
                Vec2::new(bubble_w, 30.0),
            );
            let out_b = Rect::from_min_size(
                Pos2::new(strip_rect.right() - bubble_w - 12.0, in_b.bottom() + 8.0),
                Vec2::new(bubble_w, 30.0),
            );
            ui.painter()
                .rect_filled(in_b, CornerRadius::same(10), theme_color_to_egui(in_c));
            ui.painter()
                .rect_filled(out_b, CornerRadius::same(10), theme_color_to_egui(out_c));
            let text_col_in = contrasting_text_color(theme_color_to_egui(in_c));
            let text_col_out = contrasting_text_color(theme_color_to_egui(out_c));
            paint_clamped_label(
                ui,
                Pos2::new(in_b.left() + 10.0, in_b.center().y),
                (in_b.width() - 18.0).max(40.0),
                &format!("{preview_label} theme preview"),
                12.0,
                text_col_in,
            );
            paint_clamped_label(
                ui,
                Pos2::new(out_b.left() + 10.0, out_b.center().y),
                (out_b.width() - 18.0).max(40.0),
                "Applied only in this chat",
                12.0,
                text_col_out,
            );
            ui.painter().rect_stroke(
                strip_rect,
                CornerRadius::same(8),
                Stroke::new(1.0, divider),
                StrokeKind::Inside,
            );
            ui.add_space(10.0);
        }

        // 4. Reassignment confirmation prompt (`lng_chat_theme_gift_replace`) when applying a taken gift
        if let Some((ref slug, ref taken_peer)) = self.pending_gift_replace.clone() {
            Frame::NONE
                .fill(bg_over)
                .stroke(Stroke::new(1.0, active_bg))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(tr::lng_chat_theme_gift_replace(taken_peer))
                            .size(12.0)
                            .color(bold_fg),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let confirm_btn = egui::Button::new(
                            egui::RichText::new("Use Here").color(accent_fg).strong(),
                        )
                        .fill(active_bg);
                        if ui.add(confirm_btn).clicked() {
                            if let Some(gift) =
                                self.gift_themes.iter_mut().find(|g| g.record.slug == *slug)
                            {
                                gift.taken_by_peer_name = None;
                            }
                            let choice = self.apply_selected_choice(chat_bg, dark_mode);
                            out.applied_token = Some((self.peer_id, choice_to_token(&choice)));
                            out.applied_choice = Some((self.peer_id, choice));
                            out.toast = Some(format!(
                                "Applied collectible gift theme for {}",
                                self.peer_name
                            ));
                        }
                        if ui.button("Cancel").clicked() {
                            self.pending_gift_replace = None;
                        }
                    });
                });
            ui.add_space(8.0);
        }

        // 5. Bottom controls (`ChooseThemeController::initButtons`)
        Frame::NONE.inner_margin(Margin::ZERO).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let changed = self.has_unapplied_change();
                let apply_btn = egui::Button::new(
                    egui::RichText::new(tr::lng_chat_theme_apply())
                        .color(accent_fg)
                        .strong(),
                )
                .fill(active_bg)
                .min_size(Vec2::new(124.0, 32.0));
                if ui.add_enabled(changed, apply_btn).clicked() {
                    if let ChatThemeChoice::UniqueGift(ref slug) = self.selected_choice
                        && let Some(taken_peer) = self
                            .gift_themes
                            .iter()
                            .find(|g| g.record.slug == *slug)
                            .and_then(|g| g.taken_by_peer_name.clone())
                    {
                        self.pending_gift_replace = Some((slug.clone(), taken_peer));
                        ui.ctx().request_repaint();
                    } else {
                        let choice = self.apply_selected_choice(chat_bg, dark_mode);
                        let tok = choice_to_token(&choice);
                        out.applied_token = Some((self.peer_id, tok.clone()));
                        out.applied_choice = Some((self.peer_id, choice.clone()));
                        out.toast = Some(match choice {
                            ChatThemeChoice::Empty => {
                                format!("Reset chat theme for {}", self.peer_name)
                            }
                            ChatThemeChoice::Emoticon(t) => {
                                format!("Applied '{t}' chat theme for {}", self.peer_name)
                            }
                            ChatThemeChoice::UniqueGift(slug) => {
                                format!("Applied gift theme '{slug}' for {}", self.peer_name)
                            }
                        });
                    }
                }

                if ui
                    .add(
                        egui::Button::new(tr::lng_chat_theme_change_wallpaper())
                            .min_size(Vec2::new(146.0, 32.0)),
                    )
                    .clicked()
                {
                    out.open_background_picker_for_peer =
                        Some((self.peer_id, self.peer_name.clone()));
                }
            });
        });

        out
    }

    fn selected_preview_variant(
        &self,
        dark_mode: bool,
    ) -> Option<(String, Color, Color, &BackgroundConfig)> {
        match &self.selected_choice {
            ChatThemeChoice::Empty => None,
            ChatThemeChoice::Emoticon(tok) => {
                let theme = self.themes.iter().find(|t| t.token == *tok)?;
                let (out_c, in_c, bg) = theme.variant(dark_mode);
                Some((theme.name.clone(), out_c, in_c, bg))
            }
            ChatThemeChoice::UniqueGift(slug) => {
                let gift = self.gift_themes.iter().find(|g| g.record.slug == *slug)?;
                let (out_c, in_c, bg) = gift.variant(dark_mode);
                Some((gift.display_title().to_owned(), out_c, in_c, bg))
            }
        }
    }
}

fn choice_to_token(choice: &ChatThemeChoice) -> Option<String> {
    match choice {
        ChatThemeChoice::Empty => None,
        ChatThemeChoice::Emoticon(tok) => Some(tok.clone()),
        ChatThemeChoice::UniqueGift(slug) => Some(slug.clone()),
    }
}

fn card_activated(ui: &Ui, resp: &egui::Response) -> bool {
    if resp.clicked() {
        return true;
    }
    resp.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Space))
}

fn paint_selection_border(
    ui: &Ui,
    rect: Rect,
    selected: bool,
    focused: bool,
    anim_progress: f32,
    active_bg: Color32,
    divider: Color32,
) {
    let stroke = if selected {
        let width = 1.5 + 1.0 * anim_progress.clamp(0.0, 1.0);
        Stroke::new(width, active_bg)
    } else if focused {
        Stroke::new(2.0, with_alpha(active_bg, 0.72))
    } else {
        Stroke::new(1.0, divider)
    };
    ui.painter()
        .rect_stroke(rect, CornerRadius::same(10), stroke, StrokeKind::Inside);
}

fn paint_clamped_label(
    ui: &Ui,
    left_center: Pos2,
    max_width: f32,
    text: &str,
    size: f32,
    color: Color32,
) {
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.to_owned(),
        FontId::proportional(size),
        color,
    );
    job.wrap = egui::text::TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter().galley(
        Pos2::new(left_center.x, left_center.y - galley.size().y / 2.0),
        galley,
        color,
    );
}

fn contrasting_text_color(bg: Color32) -> Color32 {
    let luma = 0.299 * f32::from(bg.r()) + 0.587 * f32::from(bg.g()) + 0.114 * f32::from(bg.b());
    if luma < 140.0 {
        Color32::WHITE
    } else {
        Color32::from_rgb(0x16, 0x1B, 0x22)
    }
}

fn extract_outgoing_color(settings: Option<&CloudThemeSettings>, fallback: Color) -> Color {
    settings
        .and_then(|s| {
            s.outgoing_accent_color
                .or_else(|| s.outgoing_messages_colors.first().copied())
        })
        .or_else(|| settings.map(|s| s.accent_color))
        .unwrap_or(fallback)
}

fn bg_from_theme_settings(
    settings: Option<&CloudThemeSettings>,
    fallback: BackgroundConfig,
) -> BackgroundConfig {
    let Some(paper) = settings.and_then(|v| v.paper.as_ref()) else {
        return fallback;
    };
    let pattern = if paper.is_pattern() {
        PatternKind::from_slug(&paper.slug)
    } else {
        PatternKind::None
    };
    BackgroundConfig {
        wallpaper: paper.clone(),
        pattern,
        pattern_bytes: None,
        custom_image: None,
        tiled: paper.is_pattern(),
        motion: paper.background_colors.len() >= 3,
        dark_dimming: 0,
    }
}

fn blend_tint(a: Color, b: Color, t: f32) -> Color {
    let inv = 1.0 - t;
    Color::rgb(
        (f32::from(a.r) * inv + f32::from(b.r) * t).round() as u8,
        (f32::from(a.g) * inv + f32::from(b.g) * t).round() as u8,
        (f32::from(a.b) * inv + f32::from(b.b) * t).round() as u8,
    )
}

fn darken_color(c: Color, factor: f32) -> Color {
    Color::rgb(
        (f32::from(c.r) * factor).round() as u8,
        (f32::from(c.g) * factor).round() as u8,
        (f32::from(c.b) * factor).round() as u8,
    )
}

/// Built-in offline fallback emoji chat themes modeled after Telegram's classic chat themes,
/// replaced by live `account.getChatThemes` records once hydrated.
#[must_use]
pub fn builtin_emoji_chat_themes() -> Vec<EmojiChatTheme> {
    let dark_cfg = |colors: Vec<Color>, pattern: PatternKind| {
        let mut cfg = BackgroundConfig::gradient(colors, 45, pattern, -45, true);
        cfg.wallpaper.flags = cfg.wallpaper.flags.union(WallPaperFlags::DARK);
        cfg
    };

    vec![
        EmojiChatTheme {
            token: "classic".to_owned(),
            emoji: "🏡".to_owned(),
            name: "Classic".to_owned(),
            day_out_bg: Color::rgb(0xEF, 0xFD, 0xDE),
            day_in_bg: Color::WHITE,
            day_bg: BackgroundConfig::classic_meadow(),
            night_out_bg: Color::rgb(0x2B, 0x52, 0x78),
            night_in_bg: Color::rgb(0x18, 0x25, 0x33),
            night_bg: BackgroundConfig::night_aurora(),
        },
        EmojiChatTheme {
            token: "duck".to_owned(),
            emoji: "🦆".to_owned(),
            name: "Duck".to_owned(),
            day_out_bg: Color::rgb(0xDF, 0xF5, 0xC8),
            day_in_bg: Color::WHITE,
            day_bg: BackgroundConfig::gradient(
                vec![
                    Color::rgb(0xF6, 0xE7, 0xA8),
                    Color::rgb(0xB8, 0xDF, 0xB8),
                    Color::rgb(0xE8, 0xF0, 0xB5),
                ],
                45,
                PatternKind::Doodles,
                40,
                true,
            ),
            night_out_bg: Color::rgb(0x3B, 0x5C, 0x3B),
            night_in_bg: Color::rgb(0x1E, 0x29, 0x22),
            night_bg: dark_cfg(
                vec![
                    Color::rgb(0x18, 0x2B, 0x20),
                    Color::rgb(0x2A, 0x45, 0x30),
                    Color::rgb(0x1D, 0x2F, 0x25),
                ],
                PatternKind::Doodles,
            ),
        },
        EmojiChatTheme {
            token: "snowman".to_owned(),
            emoji: "⛄".to_owned(),
            name: "Frost".to_owned(),
            day_out_bg: Color::rgb(0xD7, 0xF0, 0xFF),
            day_in_bg: Color::WHITE,
            day_bg: BackgroundConfig::gradient(
                vec![
                    Color::rgb(0xC6, 0xE6, 0xFA),
                    Color::rgb(0x8E, 0xCA, 0xF0),
                    Color::rgb(0xDF, 0xF2, 0xFE),
                ],
                90,
                PatternKind::Waves,
                38,
                true,
            ),
            night_out_bg: Color::rgb(0x26, 0x4E, 0x73),
            night_in_bg: Color::rgb(0x16, 0x24, 0x33),
            night_bg: dark_cfg(
                vec![
                    Color::rgb(0x12, 0x25, 0x3B),
                    Color::rgb(0x1F, 0x42, 0x63),
                    Color::rgb(0x15, 0x2C, 0x44),
                ],
                PatternKind::Waves,
            ),
        },
        EmojiChatTheme {
            token: "diamond".to_owned(),
            emoji: "💎".to_owned(),
            name: "Diamond".to_owned(),
            day_out_bg: Color::rgb(0xE3, 0xDC, 0xFF),
            day_in_bg: Color::WHITE,
            day_bg: BackgroundConfig::gradient(
                vec![
                    Color::rgb(0xD6, 0xCB, 0xF8),
                    Color::rgb(0xA6, 0x9C, 0xEC),
                    Color::rgb(0xE8, 0xE2, 0xFC),
                ],
                135,
                PatternKind::Hexagons,
                36,
                true,
            ),
            night_out_bg: Color::rgb(0x4A, 0x3B, 0x7A),
            night_in_bg: Color::rgb(0x1F, 0x1A, 0x30),
            night_bg: dark_cfg(
                vec![
                    Color::rgb(0x1E, 0x17, 0x36),
                    Color::rgb(0x38, 0x2A, 0x63),
                    Color::rgb(0x26, 0x1D, 0x45),
                ],
                PatternKind::Hexagons,
            ),
        },
        EmojiChatTheme {
            token: "astronaut".to_owned(),
            emoji: "🚀".to_owned(),
            name: "Space".to_owned(),
            day_out_bg: Color::rgb(0xDC, 0xEB, 0xFF),
            day_in_bg: Color::WHITE,
            day_bg: BackgroundConfig::gradient(
                vec![
                    Color::rgb(0xBD, 0xD3, 0xF2),
                    Color::rgb(0x8A, 0xA8, 0xDC),
                    Color::rgb(0xD8, 0xE5, 0xF8),
                ],
                0,
                PatternKind::Constellation,
                42,
                true,
            ),
            night_out_bg: Color::rgb(0x2A, 0x42, 0x73),
            night_in_bg: Color::rgb(0x12, 0x19, 0x29),
            night_bg: dark_cfg(
                vec![
                    Color::rgb(0x0D, 0x14, 0x26),
                    Color::rgb(0x1E, 0x2C, 0x52),
                    Color::rgb(0x16, 0x20, 0x3D),
                ],
                PatternKind::Constellation,
            ),
        },
        EmojiChatTheme {
            token: "heart".to_owned(),
            emoji: "💘".to_owned(),
            name: "Rose".to_owned(),
            day_out_bg: Color::rgb(0xFF, 0xE1, 0xEB),
            day_in_bg: Color::WHITE,
            day_bg: BackgroundConfig::gradient(
                vec![
                    Color::rgb(0xFA, 0xD2, 0xE1),
                    Color::rgb(0xE8, 0x99, 0xB8),
                    Color::rgb(0xFD, 0xE6, 0xEE),
                ],
                45,
                PatternKind::Botanical,
                38,
                true,
            ),
            night_out_bg: Color::rgb(0x70, 0x2F, 0x4A),
            night_in_bg: Color::rgb(0x2B, 0x18, 0x22),
            night_bg: dark_cfg(
                vec![
                    Color::rgb(0x2B, 0x15, 0x21),
                    Color::rgb(0x52, 0x24, 0x3B),
                    Color::rgb(0x3B, 0x1A, 0x2C),
                ],
                PatternKind::Botanical,
            ),
        },
        EmojiChatTheme {
            token: "magic".to_owned(),
            emoji: "🔮".to_owned(),
            name: "Magic".to_owned(),
            day_out_bg: Color::rgb(0xFF, 0xEB, 0xD2),
            day_in_bg: Color::WHITE,
            day_bg: BackgroundConfig::gradient(
                vec![
                    Color::rgb(0xFD, 0xDC, 0xB8),
                    Color::rgb(0xE6, 0x9F, 0x7A),
                    Color::rgb(0xF5, 0xC6, 0xAA),
                ],
                135,
                PatternKind::Doodles,
                40,
                true,
            ),
            night_out_bg: Color::rgb(0x6E, 0x44, 0x28),
            night_in_bg: Color::rgb(0x29, 0x1E, 0x18),
            night_bg: dark_cfg(
                vec![
                    Color::rgb(0x29, 0x1B, 0x14),
                    Color::rgb(0x4F, 0x30, 0x22),
                    Color::rgb(0x38, 0x22, 0x18),
                ],
                PatternKind::Doodles,
            ),
        },
    ]
}
