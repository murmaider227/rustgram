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
    /// Whether the vertical chat folders sidebar is enabled.
    pub folders_sidebar_enabled: bool,
    /// Optional local passcode configured in Settings (stand-in plain mock secret).
    pub local_passcode: Option<String>,
    /// Whether to use the OS native window frame (`CoreSettings::native_window_frame`).
    /// `false` enables the custom frameless titlebar (`tg-platform::frameless`).
    #[serde(default)]
    pub native_window_frame: bool,
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            window_position: WindowPosition::default(),
            columns: SavedWindowColumns::default(),
            theme_mode: ThemeMode::Day,
            embedded_theme: None,
            accent_rgb: None,
            folders_sidebar_enabled: true,
            local_passcode: None,
            native_window_frame: false,
        }
    }
}

impl ShellSettings {
    /// Resolves the persisted `tg-theme` [`tg_theme::EmbeddedType`].
    #[must_use]
    pub fn resolved_embedded_type(&self) -> tg_theme::EmbeddedType {
        match self.embedded_theme.as_deref() {
            Some("DayBlue") => tg_theme::EmbeddedType::DayBlue,
            Some("Default") => tg_theme::EmbeddedType::Default,
            Some("Night") => tg_theme::EmbeddedType::Night,
            Some("NightGreen") => tg_theme::EmbeddedType::NightGreen,
            _ => self.theme_mode.to_embedded(),
        }
    }

    /// Resolves the persisted optional accent color override.
    #[must_use]
    pub fn resolved_accent_color(&self) -> Option<tg_theme::Color> {
        self.accent_rgb
            .map(|[r, g, b]| tg_theme::Color::rgb(r, g, b))
    }

    /// Updates persisted embedded theme and accent color from [`tg_ui::theme::ThemeManager`].
    pub fn record_theme(&mut self, kind: tg_theme::EmbeddedType, accent: Option<tg_theme::Color>) {
        self.theme_mode = ThemeMode::from_embedded(kind);
        self.embedded_theme = Some(
            match kind {
                tg_theme::EmbeddedType::DayBlue => "DayBlue",
                tg_theme::EmbeddedType::Default => "Default",
                tg_theme::EmbeddedType::Night => "Night",
                tg_theme::EmbeddedType::NightGreen => "NightGreen",
            }
            .to_owned(),
        );
        self.accent_rgb = accent.map(|c| [c.r, c.g, c.b]);
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
}
