//! Stand-in local settings persistence replacing `tg-core` (`Core::Settings`) until integration.
//!
//! Port origin: `Telegram/SourceFiles/core/core_settings.h` (`WindowColumns`, `WindowPosition`)
//! and `Telegram/SourceFiles/window/main_window.cpp` (geometry save/restore).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core_types::window_types::{
    DEFAULT_DIALOGS_WIDTH_RATIO, DEFAULT_THIRD_COLUMN_WIDTH, WINDOW_BIG_DEFAULT_HEIGHT,
    WINDOW_BIG_DEFAULT_WIDTH, WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH,
};
use crate::stand_in::theme::ThemeMode;

/// Saved desktop window position and size.
///
/// Mirrors `Core::WindowPosition` in `core/core_settings.h`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowPosition {
    /// Outer X position in logical points, if previously recorded.
    pub x: Option<f32>,
    /// Outer Y position in logical points, if previously recorded.
    pub y: Option<f32>,
    /// Inner width in logical points.
    pub width: f32,
    /// Inner height in logical points.
    pub height: f32,
    /// Whether the window was maximized.
    pub maximized: bool,
}

impl Default for WindowPosition {
    fn default() -> Self {
        Self {
            x: None,
            y: None,
            width: WINDOW_BIG_DEFAULT_WIDTH as f32,
            height: WINDOW_BIG_DEFAULT_HEIGHT as f32,
            maximized: false,
        }
    }
}

impl WindowPosition {
    /// Clamps dimensions to tdesktop's minimum window bounds.
    #[must_use]
    pub fn clamped(mut self) -> Self {
        self.width = self.width.max(WINDOW_MIN_WIDTH as f32);
        self.height = self.height.max(WINDOW_MIN_HEIGHT as f32);
        self
    }
}

/// Persisted column widths and third-column toggles.
///
/// Mirrors `Core::WindowColumns` in `core/core_settings.h`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedWindowColumns {
    /// Ratio of dialogs column width to body width when a chat/section is open.
    /// `0.0` selects compact avatar-only mode (`DIALOGS_SMALL_COLUMN_WIDTH`).
    pub dialogs_with_chat_width_ratio: f64,
    /// Ratio of dialogs column width to body width when no chat is open.
    pub dialogs_no_chat_width_ratio: f64,
    /// Stored width of the third column (clamped between min and max).
    pub third_column_width: i32,
    /// Pixels by which the window was extended when opening the third section (`-1` = auto).
    pub third_section_extended_by: i32,
    /// Whether the tabbed emoji/sticker selector occupies the third column.
    pub tabbed_selector_section_enabled: bool,
    /// Whether the peer info panel occupies the third column when space permits.
    pub third_section_info_enabled: bool,
}

impl Default for SavedWindowColumns {
    fn default() -> Self {
        Self {
            dialogs_with_chat_width_ratio: DEFAULT_DIALOGS_WIDTH_RATIO,
            dialogs_no_chat_width_ratio: DEFAULT_DIALOGS_WIDTH_RATIO,
            third_column_width: DEFAULT_THIRD_COLUMN_WIDTH,
            third_section_extended_by: -1,
            tabbed_selector_section_enabled: false,
            third_section_info_enabled: false,
        }
    }
}

/// A built-in theme and its accent as saved for day mode or for night mode (tdesktop keeps
/// one saved theme per mode, `themeKeyDay` / `themeKeyNight`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedThemeChoice {
    /// `tg-theme` embedded theme variant (`"Default"`, `"DayBlue"`, `"Night"`, `"NightGreen"`).
    pub embedded_theme: String,
    /// Optional custom accent color RGB override.
    #[serde(default)]
    pub accent_rgb: Option<[u8; 3]>,
}

impl SavedThemeChoice {
    fn new(kind: tg_theme::EmbeddedType, accent: Option<tg_theme::Color>) -> Self {
        Self {
            embedded_theme: embedded_name(kind).to_owned(),
            accent_rgb: accent.map(|c| [c.r, c.g, c.b]),
        }
    }

    /// The saved theme, or `None` when the stored name is unknown or belongs to the other
    /// mode (`night`).
    fn resolve(&self, night: bool) -> Option<tg_ui::theme::SavedEmbedded> {
        let kind = embedded_from_name(&self.embedded_theme)?;
        (kind.is_dark() == night).then(|| tg_ui::theme::SavedEmbedded {
            kind,
            accent: self
                .accent_rgb
                .map(|[r, g, b]| tg_theme::Color::rgb(r, g, b)),
        })
    }
}

const fn embedded_name(kind: tg_theme::EmbeddedType) -> &'static str {
    match kind {
        tg_theme::EmbeddedType::DayBlue => "DayBlue",
        tg_theme::EmbeddedType::Default => "Default",
        tg_theme::EmbeddedType::Night => "Night",
        tg_theme::EmbeddedType::NightGreen => "NightGreen",
    }
}

fn embedded_from_name(name: &str) -> Option<tg_theme::EmbeddedType> {
    match name {
        "DayBlue" => Some(tg_theme::EmbeddedType::DayBlue),
        "Default" => Some(tg_theme::EmbeddedType::Default),
        "Night" => Some(tg_theme::EmbeddedType::Night),
        "NightGreen" => Some(tg_theme::EmbeddedType::NightGreen),
        _ => None,
    }
}

/// Persisted shell settings stored as JSON on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellSettings {
    /// Window geometry (size, position, maximized).
    pub window_position: WindowPosition,
    /// Saved column layout widths and toggles.
    pub columns: SavedWindowColumns,
    /// Active Day/Night theme mode.
    pub theme_mode: ThemeMode,
    /// Active `tg-theme` embedded theme variant (`"Default"`, `"DayBlue"`, `"Night"`, `"NightGreen"`).
    #[serde(default)]
    pub embedded_theme: Option<String>,
    /// Optional custom accent color RGB override.
    #[serde(default)]
    pub accent_rgb: Option<[u8; 3]>,
    /// Theme the night-mode toggle returns to in day mode (absent: Classic).
    #[serde(default)]
    pub day_theme: Option<SavedThemeChoice>,
    /// Theme the night-mode toggle switches to in night mode (absent: Tinted).
    #[serde(default)]
    pub night_theme: Option<SavedThemeChoice>,
    /// Whether the vertical chat folders sidebar is enabled.
    pub folders_sidebar_enabled: bool,
    /// Optional local passcode configured in Settings (stand-in plain mock secret).
    pub local_passcode: Option<String>,
    /// Whether to use the OS native window frame (`CoreSettings::native_window_frame`).
    /// `false` enables the custom frameless titlebar (`tg-platform::frameless`).
    #[serde(default)]
    pub native_window_frame: bool,
    /// The chat-list search field's "Add Story" button was hidden from its menu
    /// (tdesktop's `dialogs-add-story-hidden` pref).
    #[serde(default)]
    pub add_story_button_hidden: bool,
    /// The stories strip's first-run "Click here to view updates" tooltip was dismissed
    /// (`CoreSettings::storiesClickTooltipHidden`).
    #[serde(default)]
    pub stories_click_tooltip_hidden: bool,
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            window_position: WindowPosition::default(),
            columns: SavedWindowColumns::default(),
            theme_mode: ThemeMode::Day,
            embedded_theme: None,
            accent_rgb: None,
            day_theme: None,
            night_theme: None,
            folders_sidebar_enabled: true,
            local_passcode: None,
            native_window_frame: false,
            add_story_button_hidden: false,
            stories_click_tooltip_hidden: false,
        }
    }
}

impl ShellSettings {
    /// Resolves the persisted `tg-theme` [`tg_theme::EmbeddedType`].
    #[must_use]
    pub fn resolved_embedded_type(&self) -> tg_theme::EmbeddedType {
        self.embedded_theme
            .as_deref()
            .and_then(embedded_from_name)
            .unwrap_or_else(|| self.theme_mode.to_embedded())
    }

    /// Builds the [`tg_ui::theme::ThemeManager`] these settings describe: the applied theme
    /// with its accent, plus the themes saved for day mode and for night mode.
    #[must_use]
    pub fn restored_theme_manager(&self) -> tg_ui::theme::ThemeManager {
        let embedded = self.resolved_embedded_type();
        let mut manager = tg_ui::theme::ThemeManager::with_embedded(embedded);
        for (saved, night) in [(&self.day_theme, false), (&self.night_theme, true)] {
            if let Some(saved) = saved.as_ref().and_then(|s| s.resolve(night)) {
                manager.remember(saved);
            }
        }
        if let Some(accent) = self.resolved_accent_color() {
            manager.set_embedded(embedded, Some(accent));
        }
        manager
    }

    /// Resolves the persisted optional accent color override.
    #[must_use]
    pub fn resolved_accent_color(&self) -> Option<tg_theme::Color> {
        self.accent_rgb
            .map(|[r, g, b]| tg_theme::Color::rgb(r, g, b))
    }

    /// Updates persisted embedded theme and accent color from [`tg_ui::theme::ThemeManager`].
    /// The applied theme is also the one saved for its own mode (day or night), as
    /// tdesktop's `KeepApplied` writes it under `themeKeyDay` / `themeKeyNight`.
    pub fn record_theme(&mut self, kind: tg_theme::EmbeddedType, accent: Option<tg_theme::Color>) {
        self.theme_mode = ThemeMode::from_embedded(kind);
        self.embedded_theme = Some(embedded_name(kind).to_owned());
        self.accent_rgb = accent.map(|c| [c.r, c.g, c.b]);
        let saved = Some(SavedThemeChoice::new(kind, accent));
        if kind.is_dark() {
            self.night_theme = saved;
        } else {
            self.day_theme = saved;
        }
    }

    /// Resolves the default persistence path under `%LOCALAPPDATA%\Rustgram\shell_state.json`
    /// (falling back to the OS temp dir if `LOCALAPPDATA` is unavailable).
    #[must_use]
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        base.join("Rustgram").join("shell_state.json")
    }

    /// Loads settings from `path`, returning defaults if missing or malformed.
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&raw) {
            Ok(mut loaded) => {
                loaded.window_position = loaded.window_position.clamped();
                loaded
            }
            Err(err) => {
                tracing::warn!(%err, path = %path.display(), "Failed to parse shell settings, using defaults");
                Self::default()
            }
        }
    }

    /// Persists settings atomically-enough to `path`, creating parent directories as needed.
    pub fn save_to(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        fs::write(path, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_and_min_size_clamping() {
        let tmp = std::env::temp_dir().join(format!(
            "rustgram_shell_settings_test_{}.json",
            std::process::id()
        ));
        let mut s = ShellSettings {
            window_position: WindowPosition {
                x: Some(120.0),
                y: Some(80.0),
                width: 200.0, // below minimum -> clamped on load
                height: 200.0,
                maximized: false,
            },
            theme_mode: ThemeMode::Night,
            ..ShellSettings::default()
        };
        s.columns.third_column_width = 340;
        s.save_to(&tmp).expect("save settings");

        let loaded = ShellSettings::load_from(&tmp);
        let _ = fs::remove_file(&tmp);

        assert_eq!(loaded.theme_mode, ThemeMode::Night);
        assert_eq!(loaded.columns.third_column_width, 340);
        assert_eq!(loaded.window_position.width, WINDOW_MIN_WIDTH as f32);
        assert_eq!(loaded.window_position.height, WINDOW_MIN_HEIGHT as f32);
        assert_eq!(loaded.window_position.x, Some(120.0));
    }

    /// TGB-2955: the chosen theme, its accent and the theme saved for the other mode all
    /// survive a restart; a settings file written before the per-mode themes still loads.
    #[test]
    fn theme_choice_and_per_mode_themes_survive_a_restart() {
        let path = std::env::temp_dir().join(format!(
            "rustgram_shell_settings_theme_test_{}.json",
            std::process::id()
        ));
        let accent = tg_theme::Color::rgb(0xd0, 0x40, 0x40);

        let mut s = ShellSettings::default();
        s.record_theme(tg_theme::EmbeddedType::DayBlue, Some(accent));
        s.record_theme(tg_theme::EmbeddedType::NightGreen, None);
        s.save_to(&path).expect("save settings");

        let loaded = ShellSettings::load_from(&path);
        let mut manager = loaded.restored_theme_manager();
        assert_eq!(manager.embedded_type(), tg_theme::EmbeddedType::NightGreen);
        assert!(manager.is_dark());
        manager.set_night_mode(false);
        assert_eq!(manager.embedded_type(), tg_theme::EmbeddedType::DayBlue);
        assert_eq!(manager.accent(), Some(accent));
        manager.set_night_mode(true);
        assert_eq!(manager.embedded_type(), tg_theme::EmbeddedType::NightGreen);

        // A file written before the per-mode themes existed.
        let mut old_json = serde_json::to_value(ShellSettings::default()).expect("to json");
        let fields = old_json.as_object_mut().expect("object");
        fields.remove("day_theme");
        fields.remove("night_theme");
        fields.insert("theme_mode".to_owned(), serde_json::json!("Night"));
        fields.insert("embedded_theme".to_owned(), serde_json::json!("NightGreen"));
        fs::write(&path, old_json.to_string()).expect("write old settings");
        let old = ShellSettings::load_from(&path);
        let _ = fs::remove_file(&path);
        assert_eq!(old.day_theme, None);
        let manager = old.restored_theme_manager();
        assert_eq!(manager.embedded_type(), tg_theme::EmbeddedType::NightGreen);
        assert_eq!(
            manager.saved_embedded(false),
            tg_ui::theme::SavedEmbedded::default_for(false)
        );
    }
}
