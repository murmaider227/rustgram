//! Port of `core/core_settings.h` and `core/core_settings.cpp`.
//!
//! Full `Core::Settings` structure and exact `QDataStream` big-endian binary serialization
//! (`serialize()` / `add_from_serialized()`) matching Telegram Desktop (commit `c2d0ad9`, layer 233)
//! so settings blobs stored in `tdata` round-trip cleanly with tdesktop.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::settings_proxy::{ProxySettingsError, SettingsProxy};
use tg_storage::qdatastream::{DataStreamError, QDataStreamReader, QDataStreamWriter, QtColor};

/// Default audio/video playback volume (`Settings::kDefaultVolume`).
pub const DEFAULT_VOLUME: f64 = 0.9;
/// Minimum playback speed (`Media::kSpeedMin`).
pub const SPEED_MIN: f64 = 0.5;
/// Maximum playback speed (`Media::kSpeedMax`).
pub const SPEED_MAX: f64 = 2.5;
/// Default sped-up playback rate (`Media::kSpedUpDefault`).
pub const SPED_UP_DEFAULT: f64 = 1.7;
/// Initial default video quality height (`kInitialVideoQuality`).
pub const INITIAL_VIDEO_QUALITY_HEIGHT: u32 = 480;
/// Minimum Instant View zoom percentage (`kMinIvZoom`).
pub const MIN_IV_ZOOM: i32 = 25;
/// Maximum Instant View zoom percentage (`kMaxIvZoom`).
pub const MAX_IV_ZOOM: i32 = 400;
/// Maximum accent color overrides (`kMaxAccentColors`).
pub const MAX_ACCENT_COLORS: i32 = 4;
/// Maximum recent emoji entries (`kRecentEmojiLimit`).
pub const RECENT_EMOJI_LIMIT: usize = 54;
/// Default dialogs column width ratio (`kDefaultDialogsWidthRatio`).
pub const DEFAULT_DIALOGS_WIDTH_RATIO: f64 = 5.0 / 14.0;
/// Default dialogs width ratio when 3-column mode is enabled by default (`kDefaultBigDialogsWidthRatio`).
pub const DEFAULT_BIG_DIALOGS_WIDTH_RATIO: f64 = 0.275;
/// Language-codes tail format tag (`kLanguageCodesFormat`).
pub const LANGUAGE_CODES_FORMAT: i32 = 1;
/// Preference key for screen reader mode disabled (`kScreenReaderModeDisabledKey`).
pub const SCREEN_READER_MODE_DISABLED_KEY: &str = "screen-reader-mode-disabled";

/// Errors returned when deserializing `Settings`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SettingsError {
    /// Underlying `QDataStream` decoding error.
    #[error("QDataStream error: {0}")]
    Stream(#[from] DataStreamError),
    /// Embedded `AccentColors` blob was invalid.
    #[error("invalid accent colors payload")]
    InvalidAccentColors,
    /// Embedded `SettingsProxy` blob was invalid.
    #[error("invalid proxy settings payload: {0}")]
    InvalidProxy(#[from] ProxySettingsError),
    /// Negative or out-of-bounds item count in serialized settings.
    #[error("invalid count in settings stream: {0}")]
    InvalidCount(i32),
}

/// Embedded theme identifier (`Window::Theme::EmbeddedType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i32)]
pub enum EmbeddedThemeType {
    /// Day Blue embedded theme (`0`).
    DayBlue = 0,
    /// Default light theme (`1`).
    Default = 1,
    /// Night dark theme (`2`).
    Night = 2,
    /// Night Green tinted dark theme (`3`).
    NightGreen = 3,
}

impl EmbeddedThemeType {
    /// Decodes an `EmbeddedThemeType` from its serialized `i32`.
    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::DayBlue),
            1 => Some(Self::Default),
            2 => Some(Self::Night),
            3 => Some(Self::NightGreen),
            _ => None,
        }
    }
}

/// Per-embedded-theme custom accent colors (`Window::Theme::AccentColors`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccentColors {
    data: BTreeMap<EmbeddedThemeType, QtColor>,
}

impl AccentColors {
    /// Sets the custom accent color for `theme`.
    pub fn set(&mut self, theme: EmbeddedThemeType, color: QtColor) {
        self.data.insert(theme, color);
    }

    /// Clears the custom accent color for `theme`.
    pub fn clear(&mut self, theme: EmbeddedThemeType) {
        self.data.remove(&theme);
    }

    /// Gets the custom accent color for `theme`, if any.
    #[must_use]
    pub fn get(&self, theme: EmbeddedThemeType) -> Option<QtColor> {
        self.data.get(&theme).copied()
    }

    /// Returns `true` if no accent overrides are set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Serializes matching `Window::Theme::AccentColors::serialize()`.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        if self.data.is_empty() {
            return Vec::new();
        }
        let mut w = QDataStreamWriter::new();
        w.write_i32(self.data.len() as i32);
        for (&theme, &color) in &self.data {
            w.write_i32(theme as i32);
            w.write_color(color);
        }
        w.into_bytes()
    }

    /// Deserializes matching `Window::Theme::AccentColors::setFromSerialized()`.
    pub fn set_from_serialized(&mut self, serialized: &[u8]) -> Result<(), SettingsError> {
        if serialized.is_empty() {
            self.data.clear();
            return Ok(());
        }
        let mut r = QDataStreamReader::new(serialized);
        let count = r.read_i32()?;
        if count <= 0 || count > MAX_ACCENT_COLORS {
            return Err(SettingsError::InvalidAccentColors);
        }
        let mut parsed = BTreeMap::new();
        for _ in 0..count {
            let raw_type = r.read_i32()?;
            let color = r.read_color()?;
            let Some(theme) = EmbeddedThemeType::from_i32(raw_type) else {
                return Err(SettingsError::InvalidAccentColors);
            };
            parsed.insert(theme, color);
        }
        self.data = parsed;
        Ok(())
    }
}

/// Saved window geometry (`Core::WindowPosition`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowPosition {
    /// Monitor name CRC32.
    pub moncrc: i32,
    /// Maximized state (`0` normal, `1` maximized, `2` media viewer default maximized).
    pub maximized: i32,
    /// Interface scale percentage when saved.
    pub scale: i32,
    /// X coordinate.
    pub x: i32,
    /// Y coordinate.
    pub y: i32,
    /// Width in pixels.
    pub w: i32,
    /// Height in pixels.
    pub h: i32,
}

impl WindowPosition {
    /// Serializes in tdesktop's exact 28-byte `QDataStream` layout (`x, y, w, h, moncrc, maximized, scale`).
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut w = QDataStreamWriter::with_capacity(7 * 4);
        w.write_i32(self.x);
        w.write_i32(self.y);
        w.write_i32(self.w);
        w.write_i32(self.h);
        w.write_i32(self.moncrc);
        w.write_i32(self.maximized);
        w.write_i32(self.scale);
        w.into_bytes()
    }

    /// Deserializes from a 28-byte `QDataStream` payload.
    pub fn deserialize(data: &[u8]) -> Result<Self, DataStreamError> {
        let mut r = QDataStreamReader::new(data);
        let x = r.read_i32()?;
        let y = r.read_i32()?;
        let w = r.read_i32()?;
        let h = r.read_i32()?;
        let moncrc = r.read_i32()?;
        let maximized = r.read_i32()?;
        let scale = r.read_i32()?;
        Ok(Self {
            moncrc,
            maximized,
            scale,
            x,
            y,
            w,
            h,
        })
    }

    /// Adjusts window position from its saved scale to `current_scale` (`Core::AdjustToScale`).
    #[must_use]
    pub fn adjust_to_scale(mut self, current_scale: i32) -> Self {
        if self.scale == 0 {
            return self;
        }
        let scale_factor = f64::from(current_scale) / f64::from(self.scale);
        if (scale_factor - 1.0).abs() > f64::EPSILON {
            self.x += self.w / 2;
            self.y += self.h / 2;
            self.w = (f64::from(self.w) * scale_factor) as i32;
            self.h = (f64::from(self.h) * scale_factor) as i32;
            self.x -= self.w / 2;
            self.y -= self.h / 2;
        }
        self
    }
}

/// Window title visibility toggles (`Core::WindowTitleContent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowTitleContent {
    /// Hide active chat name from the window title bar.
    pub hide_chat_name: bool,
    /// Hide active account name from the window title bar.
    pub hide_account_name: bool,
    /// Hide total unread counter from the window title bar.
    pub hide_total_unread: bool,
}

/// Notification toast screen corner (`Core::Settings::ScreenCorner`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum ScreenCorner {
    /// Top-left corner (`0`).
    TopLeft = 0,
    /// Top-right corner (`1`).
    TopRight = 1,
    /// Bottom-right corner (`2`, default).
    #[default]
    BottomRight = 2,
    /// Bottom-left corner (`3`).
    BottomLeft = 3,
}

impl ScreenCorner {
    /// Decodes from serialized `i32`.
    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::TopLeft),
            1 => Some(Self::TopRight),
            2 => Some(Self::BottomRight),
            3 => Some(Self::BottomLeft),
            _ => None,
        }
    }

    /// Returns `true` if the corner is on the left edge.
    #[must_use]
    pub const fn is_left(self) -> bool {
        matches!(self, Self::TopLeft | Self::BottomLeft)
    }

    /// Returns `true` if the corner is on the top edge.
    #[must_use]
    pub const fn is_top(self) -> bool {
        matches!(self, Self::TopLeft | Self::TopRight)
    }
}

/// Notification content detail level (`Core::Settings::NotifyView`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum NotifyView {
    /// Show sender name and message preview (`0`, default).
    #[default]
    ShowPreview = 0,
    /// Show sender name only (`1`).
    ShowName = 1,
    /// Show generic notification only (`2`).
    ShowNothing = 2,
}

impl NotifyView {
    /// Decodes from serialized `i32`.
    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::ShowPreview),
            1 => Some(Self::ShowName),
            2 => Some(Self::ShowNothing),
            _ => None,
        }
    }
}

/// Window / tray icon visibility mode (`Core::Settings::WorkMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum WorkMode {
    /// Show both taskbar window and tray icon (`0`, default).
    #[default]
    WindowAndTray = 0,
    /// Show tray icon only (`1`).
    TrayOnly = 1,
    /// Show taskbar window only (`2`).
    WindowOnly = 2,
}

impl WorkMode {
    /// Decodes from serialized `i32`.
    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::WindowAndTray),
            1 => Some(Self::TrayOnly),
            2 => Some(Self::WindowOnly),
            _ => None,
        }
    }
}

/// Window close button behavior (`Core::Settings::CloseBehavior`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum CloseBehavior {
    /// Quit the application (`0`, default).
    #[default]
    Quit = 0,
    /// Minimize to taskbar (`1`).
    CloseToTaskbar = 1,
    /// Hide window and keep running in background (`2`).
    RunInBackground = 2,
}

impl CloseBehavior {
    /// Decodes from serialized `i32`.
    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Quit),
            1 => Some(Self::CloseToTaskbar),
            2 => Some(Self::RunInBackground),
            _ => None,
        }
    }
}

/// Message submit shortcut (`Ui::InputSubmitSettings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum InputSubmitSettings {
    /// Send with Enter (`0`, default).
    #[default]
    Enter = 0,
    /// Send with Ctrl+Enter (`1`).
    CtrlEnter = 1,
    /// Send with either (`2`).
    Both = 2,
    /// Disabled (`3`).
    None = 3,
}

/// File attachment grouping and compression flags (`Ui::SendFilesWay`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendFilesWay {
    /// Group multiple media items into an album.
    pub group_files: bool,
    /// Compress images and send as photos.
    pub send_images_as_photos: bool,
    /// Send high-resolution (HD / large) photos.
    pub send_large_photos: bool,
}

impl Default for SendFilesWay {
    fn default() -> Self {
        Self {
            group_files: true,
            send_images_as_photos: true,
            send_large_photos: false,
        }
    }
}

impl SendFilesWay {
    const SERIALIZED_SEND_LARGE_PHOTOS: i32 = 4;

    /// Serializes matching `Ui::SendFilesWay::serialize()`.
    #[must_use]
    pub const fn serialize(self) -> i32 {
        let mut result = if self.send_images_as_photos && self.group_files {
            0
        } else if self.send_images_as_photos {
            1
        } else if self.group_files {
            3
        } else {
            2
        };
        if self.send_large_photos {
            result |= Self::SERIALIZED_SEND_LARGE_PHOTOS;
        }
        result
    }

    /// Deserializes matching `Ui::SendFilesWay::FromSerialized()`.
    #[must_use]
    pub const fn from_serialized(value: i32) -> Option<Self> {
        if value < 0 || value > 7 {
            return None;
        }
        let send_large_photos = (value & Self::SERIALIZED_SEND_LARGE_PHOTOS) != 0;
        let base = value & !Self::SERIALIZED_SEND_LARGE_PHOTOS;
        Some(Self {
            group_files: base == 0 || base == 3,
            send_images_as_photos: base == 0 || base == 1,
            send_large_photos,
        })
    }
}

/// Window column index (`Window::Column`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum WindowColumn {
    /// Leftmost dialogs column (`0`).
    First = 0,
    /// Main chat column (`1`, default for float player).
    #[default]
    Second = 1,
    /// Right info/selector column (`2`).
    Third = 2,
}

impl WindowColumn {
    /// Decodes from serialized `i32`.
    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::First),
            1 => Some(Self::Second),
            2 => Some(Self::Third),
            _ => None,
        }
    }
}

/// Corner bitmask value (`RectPart` from `lib_ui/ui/rect_part.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum RectPartCorner {
    /// Top-left (`1 << 0 = 1`).
    TopLeft = 1,
    /// Top-right (`1 << 2 = 4`, default).
    #[default]
    TopRight = 4,
    /// Bottom-left (`1 << 6 = 64`).
    BottomLeft = 64,
    /// Bottom-right (`1 << 8 = 256`).
    BottomRight = 256,
}

impl RectPartCorner {
    /// Decodes from serialized `i32`.
    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::TopLeft),
            4 => Some(Self::TopRight),
            64 => Some(Self::BottomLeft),
            256 => Some(Self::BottomRight),
            _ => None,
        }
    }
}

/// Audio/video player repeat mode (`Media::RepeatMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum RepeatMode {
    /// No repeat (`0`).
    #[default]
    None = 0,
    /// Repeat single track (`1`).
    One = 1,
    /// Repeat playlist (`2`).
    All = 2,
}

impl RepeatMode {
    /// Decodes from serialized `i32`.
    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::One),
            2 => Some(Self::All),
            _ => None,
        }
    }
}

/// Audio/video player track ordering (`Media::OrderMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum OrderMode {
    /// Chronological order (`0`).
    #[default]
    Default = 0,
    /// Reverse order (`1`).
    Reverse = 1,
    /// Shuffle order (`2`).
    Shuffle = 2,
}

impl OrderMode {
    /// Decodes from serialized `i32`.
    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Default),
            1 => Some(Self::Reverse),
            2 => Some(Self::Shuffle),
            _ => None,
        }
    }
}

/// Double-click message quick action (`HistoryView::DoubleClickQuickAction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum DoubleClickQuickAction {
    /// Reply to message (`0`, default).
    #[default]
    Reply = 0,
    /// React to message (`1`).
    React = 1,
    /// Do nothing (`2`).
    None = 2,
}

impl DoubleClickQuickAction {
    /// Decodes from serialized `i32`.
    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Reply),
            1 => Some(Self::React),
            2 => Some(Self::None),
            _ => None,
        }
    }
}

/// Chat-list swipe/quick dialog action (`Dialogs::Ui::QuickDialogAction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum QuickDialogAction {
    /// Mute chat (`0`).
    Mute = 0,
    /// Pin chat (`1`).
    Pin = 1,
    /// Mark read/unread (`2`).
    Read = 2,
    /// Archive chat (`3`).
    Archive = 3,
    /// Delete chat (`4`).
    Delete = 4,
    /// Disabled (`5`, default).
    #[default]
    Disabled = 5,
}

impl QuickDialogAction {
    /// Decodes from serialized `i32`.
    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Mute),
            1 => Some(Self::Pin),
            2 => Some(Self::Read),
            3 => Some(Self::Archive),
            4 => Some(Self::Delete),
            5 => Some(Self::Disabled),
            _ => None,
        }
    }
}

/// Folder tab rendering mode (`Ui::ChatsFiltersTabsMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum ChatsFiltersTabsMode {
    /// Platform/layout default (`0`).
    #[default]
    Default = 0,
    /// Show both icon and label (`1`).
    TextAndIcons = 1,
    /// Show icons only (`2`).
    IconsOnly = 2,
    /// Show text labels only (`3`).
    TextOnly = 3,
}

impl ChatsFiltersTabsMode {
    /// Decodes from serialized `i32`.
    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Default),
            1 => Some(Self::TextAndIcons),
            2 => Some(Self::IconsOnly),
            3 => Some(Self::TextOnly),
            _ => None,
        }
    }
}

/// Preferred video playback quality (`Media::VideoQuality` bitfield).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoQuality {
    /// User manually chose a fixed quality.
    pub manual: bool,
    /// Target vertical resolution in pixels (e.g. `480`, `720`, `1080`).
    pub height: u32,
    /// Play original uncompressed stream.
    pub original: bool,
}

impl Default for VideoQuality {
    fn default() -> Self {
        Self {
            manual: false,
            height: INITIAL_VIDEO_QUALITY_HEIGHT,
            original: false,
        }
    }
}

impl VideoQuality {
    /// Packs into tdesktop's 32-bit C bitfield layout (`manual:1 | height:30 | original:1`, little-endian bit order).
    #[must_use]
    pub const fn to_u32(self) -> u32 {
        let manual = if self.manual { 1u32 } else { 0u32 };
        let height = (self.height & 0x3FFF_FFFF) << 1;
        let original = if self.original { 1u32 << 31 } else { 0u32 };
        manual | height | original
    }

    /// Unpacks from tdesktop's 32-bit bitfield, resetting to default if `height > 4320`.
    #[must_use]
    pub const fn from_u32(raw: u32) -> Self {
        let manual = (raw & 1) != 0;
        let height = (raw >> 1) & 0x3FFF_FFFF;
        let original = (raw >> 31) != 0;
        if height <= 4320 {
            Self {
                manual,
                height,
                original,
            }
        } else {
            Self {
                manual: false,
                height: 0,
                original: false,
            }
        }
    }
}

/// Saved playback speed (`Core::Settings::PlaybackSpeed`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackSpeed {
    /// Speed multiplier when enabled (`0.5..=2.5`, default `1.7`).
    pub value: f64,
    /// Whether non-1.0x speed is active.
    pub enabled: bool,
}

impl Default for PlaybackSpeed {
    fn default() -> Self {
        Self {
            value: SPED_UP_DEFAULT,
            enabled: false,
        }
    }
}

impl PlaybackSpeed {
    /// Serializes matching `Settings::SerializePlaybackSpeed()`.
    #[must_use]
    pub fn serialize(self) -> i32 {
        let value = (self.value.clamp(SPEED_MIN, SPEED_MAX) * 100.0).round() as i32;
        if self.enabled { value } else { -value }
    }

    /// Deserializes matching `Settings::DeserializePlaybackSpeed()`.
    #[must_use]
    pub fn deserialize(mut speed: i32) -> Self {
        let mut enabled = true;
        let validate = |result: f64, is_enabled: bool| -> Self {
            let is_one = (result - 1.0).abs() < f64::EPSILON;
            Self {
                value: if is_one { SPED_UP_DEFAULT } else { result },
                enabled: is_enabled && !is_one,
            }
        };
        if (0..10).contains(&speed) {
            // Legacy enum values in older settings.
            return validate(f64::from(speed.clamp(0, 6) + 2) / 4.0, enabled);
        } else if speed < 0 {
            speed = speed.saturating_neg();
            enabled = false;
        }
        validate(
            (f64::from(speed) / 100.0).clamp(SPEED_MIN, SPEED_MAX),
            enabled,
        )
    }
}

/// Recent emoji entry serialized as `(emoji_or_doc_id, rating)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentEmojiPreload {
    /// Emoji unicode sequence or `"<doc_id>-<0|1>"` custom emoji key.
    pub id: String,
    /// Usage rating counter.
    pub rating: u16,
}

/// Hidden group-call tooltip bitflags (`Calls::Group::StickedTooltip`).
pub mod group_call_tooltip {
    /// Camera tooltip hidden (`0x01`).
    pub const CAMERA: u32 = 0x01;
    /// Microphone tooltip hidden (`0x02`).
    pub const MICROPHONE: u32 = 0x02;
}

/// Power saving feature disable flags (`PowerSaving::Flag` in `ui/power_saving.h`,
/// serialized as `qint32` in `dbiPowerSaving`).
///
/// Each set bit disables the corresponding animation or visual effect to conserve power.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PowerSavingFlags(u32);

impl PowerSavingFlags {
    /// No power-saving restrictions enabled (`PowerSaving::kDefault = 0`).
    pub const NONE: Self = Self(0);
    /// Disable general UI animations (`kAnimations = 1 << 0`).
    pub const ANIMATIONS: Self = Self(1 << 0);
    /// Disable animated stickers in the sticker panel (`kStickersPanel = 1 << 1`).
    pub const STICKERS_PANEL: Self = Self(1 << 1);
    /// Disable animated stickers in chats (`kStickersChat = 1 << 2`).
    pub const STICKERS_CHAT: Self = Self(1 << 2);
    /// Disable animated custom emoji in the emoji panel (`kEmojiPanel = 1 << 3`).
    pub const EMOJI_PANEL: Self = Self(1 << 3);
    /// Disable emoji reaction animations (`kEmojiReactions = 1 << 4`).
    pub const EMOJI_REACTIONS: Self = Self(1 << 4);
    /// Disable animated custom emoji in chats (`kEmojiChat = 1 << 5`).
    pub const EMOJI_CHAT: Self = Self(1 << 5);
    /// Disable animated chat backgrounds (`kChatBackground = 1 << 6`).
    pub const CHAT_BACKGROUND: Self = Self(1 << 6);
    /// Disable message/media spoiler particle animations (`kChatSpoiler = 1 << 7`).
    pub const CHAT_SPOILER: Self = Self(1 << 7);
    /// Disable call animations (`kCalls = 1 << 8`).
    pub const CALLS: Self = Self(1 << 8);
    /// Disable animated emoji statuses (`kEmojiStatus = 1 << 9`).
    pub const EMOJI_STATUS: Self = Self(1 << 9);
    /// Disable fullscreen message/chat effects (`kChatEffects = 1 << 10`).
    pub const CHAT_EFFECTS: Self = Self(1 << 10);
    /// All known power-saving flags combined (`kAll = (1 << 11) - 1`).
    pub const ALL: Self = Self((1 << 11) - 1);

    /// Constructs flags from raw bits, truncating unknown high bits beyond [`Self::ALL`].
    #[must_use]
    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits & Self::ALL.0)
    }

    /// Constructs flags from a signed `qint32` (`dbiPowerSaving` wire representation).
    #[must_use]
    pub const fn from_i32(raw: i32) -> Self {
        Self::from_bits_truncate(raw as u32)
    }

    /// Returns the raw `u32` bitmask matching `PowerSaving::Flags`.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Returns the `qint32` bitmask for `dbiPowerSaving` serialization.
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self.0 as i32
    }

    /// Returns `true` when no power-saving flags are enabled.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns `true` when every known power-saving flag is enabled.
    #[must_use]
    pub const fn is_all(self) -> bool {
        (self.0 & Self::ALL.0) == Self::ALL.0
    }

    /// Returns `true` if all bits in `other` are set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns `true` if any bit in `other` is set in `self`.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    /// Enables all bits in `other`.
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0 & Self::ALL.0;
    }

    /// Disables all bits in `other`.
    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    /// Sets or clears `flag` according to `enabled`.
    pub fn set(&mut self, flag: Self, enabled: bool) {
        if enabled {
            self.insert(flag);
        } else {
            self.remove(flag);
        }
    }
}

impl std::ops::BitOr for PowerSavingFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self((self.0 | rhs.0) & Self::ALL.0)
    }
}

impl std::ops::BitOrAssign for PowerSavingFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.insert(rhs);
    }
}

impl std::ops::BitAnd for PowerSavingFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl std::ops::BitAndAssign for PowerSavingFlags {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl std::ops::Not for PowerSavingFlags {
    type Output = Self;

    fn not(self) -> Self::Output {
        Self((!self.0) & Self::ALL.0)
    }
}

/// Inline media autoplay toggles mirroring `Data::AutoDownload::Type::AutoPlay*`
/// (`data/data_auto_download.h`: `AutoPlayGIF`, `AutoPlayVideo`, `AutoPlayVideoMessage`)
/// and video sticker autoplay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AutoplaySettings {
    /// Autoplay GIF animations inline (`AutoPlayGIF`, default `true`).
    pub gifs: bool,
    /// Autoplay videos muted inline (`AutoPlayVideo`, default `true`).
    pub videos: bool,
    /// Autoplay round video messages inline (`AutoPlayVideoMessage`, default `true`).
    pub round_videos: bool,
    /// Autoplay animated/video stickers inline (default `true`).
    pub video_stickers: bool,
}

impl Default for AutoplaySettings {
    fn default() -> Self {
        Self {
            gifs: true,
            videos: true,
            round_videos: true,
            video_stickers: true,
        }
    }
}

impl AutoplaySettings {
    const BIT_GIFS: u8 = 1 << 0;
    const BIT_VIDEOS: u8 = 1 << 1;
    const BIT_ROUND_VIDEOS: u8 = 1 << 2;
    const BIT_VIDEO_STICKERS: u8 = 1 << 3;

    /// Packs the autoplay booleans into a compact bitmask byte.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        let mut bits = 0u8;
        if self.gifs {
            bits |= Self::BIT_GIFS;
        }
        if self.videos {
            bits |= Self::BIT_VIDEOS;
        }
        if self.round_videos {
            bits |= Self::BIT_ROUND_VIDEOS;
        }
        if self.video_stickers {
            bits |= Self::BIT_VIDEO_STICKERS;
        }
        bits
    }

    /// Unpacks the autoplay booleans from a bitmask byte.
    #[must_use]
    pub const fn from_u8(bits: u8) -> Self {
        Self {
            gifs: (bits & Self::BIT_GIFS) != 0,
            videos: (bits & Self::BIT_VIDEOS) != 0,
            round_videos: (bits & Self::BIT_ROUND_VIDEOS) != 0,
            video_stickers: (bits & Self::BIT_VIDEO_STICKERS) != 0,
        }
    }
}

/// Reserved `_prefs` key storing typed [`PowerSavingFlags`] + `force_all` without altering
/// tdesktop's `Core::Settings` wire field ordering or language-codes tail.
const PREF_KEY_POWER_SAVING: &[u8] = b"__tgfork_power_saving";
/// Reserved `_prefs` key storing typed [`AutoplaySettings`] without altering tdesktop's
/// `Core::Settings` wire field ordering or language-codes tail.
const PREF_KEY_AUTOPLAY: &[u8] = b"__tgfork_autoplay";
/// Reserved `_prefs` key storing experimental option overrides (`base::options` equivalent)
/// without altering tdesktop's `Core::Settings` wire field ordering.
const PREF_KEY_EXPERIMENTAL_OPTIONS: &[u8] = b"__tgfork_experimental_options";
/// Reserved `_prefs` key storing recent Settings search entry IDs (`Settings::RecentSearches`)
/// without altering tdesktop's `Core::Settings` wire field ordering.
const PREF_KEY_RECENT_SETTINGS_SEARCHES: &[u8] = b"__tgfork_recent_settings_searches";

/// Full application settings (`Core::Settings` in `core/core_settings.h` / `.cpp`).
#[derive(Debug, Clone, PartialEq)]
pub struct CoreSettings {
    /// Proxy configuration (`_proxy`).
    pub proxy: SettingsProxy,
    /// Per-theme custom accent colors (`_themesAccentColors`).
    pub themes_accent_colors: AccentColors,

    /// Adaptive layout for wide windows (`_adaptiveForWide`).
    pub adaptive_for_wide: bool,
    /// Moderate mode flag (`_moderateModeEnabled`).
    pub moderate_mode_enabled: bool,
    /// Audio/song volume `0.0..=1.0` (`_songVolume`).
    pub song_volume: f64,
    /// Video volume `0.0..=1.0` (`_videoVolume`).
    pub video_volume: f64,
    /// Ask save path on each download (`_askDownloadPath`).
    pub ask_download_path: bool,
    /// Default download directory path (`_downloadPath`).
    pub download_path: String,
    /// macOS/sandbox bookmark for download path (`_downloadPathBookmark`).
    pub download_path_bookmark: Vec<u8>,

    /// Play notification sound (`_soundNotify`).
    pub sound_notify: bool,
    /// Show desktop notifications (`_desktopNotify`).
    pub desktop_notify: bool,
    /// Flash taskbar / bounce dock on message (`_flashBounceNotify`).
    pub flash_bounce_notify: bool,
    /// Notification preview detail (`_notifyView`).
    pub notify_view: NotifyView,
    /// Explicit override for native OS notifications (`None` = platform default).
    pub native_notifications: Option<bool>,
    /// Suppress notification toasts while window has focus (`_skipToastsInFocus`).
    pub skip_toasts_in_focus: bool,
    /// Maximum simultaneous notification toasts (`_notificationsCount`).
    pub notifications_count: i32,
    /// Screen corner for custom toasts (`_notificationsCorner`).
    pub notifications_corner: ScreenCorner,
    /// Checksum of monitor chosen for notifications (`_notificationsDisplayChecksum`).
    pub notifications_display_checksum: i32,
    /// Notification sound volume `0..=100` (`_notificationsVolume`).
    pub notifications_volume: u16,

    /// Include muted chats in unread badge (`_includeMutedCounter`).
    pub include_muted_counter: bool,
    /// Include muted chats in folder unread badges (`_includeMutedCounterFolders`).
    pub include_muted_counter_folders: bool,
    /// Count unread messages rather than unread chats (`_countUnreadMessages`).
    pub count_unread_messages: bool,
    /// Notify about pinned messages (`_notifyAboutPinned`).
    pub notify_about_pinned: bool,
    /// Auto-lock timeout in seconds (`_autoLock`, default 3600).
    pub auto_lock: i32,

    /// Default playback audio device ID (`_playbackDeviceId`).
    pub playback_device_id: String,
    /// Default microphone capture device ID (`_captureDeviceId`).
    pub capture_device_id: String,
    /// Default camera device ID (`_cameraDeviceId`).
    pub camera_device_id: String,
    /// Call audio output device ID (`_callPlaybackDeviceId`).
    pub call_playback_device_id: String,
    /// Call audio input device ID (`_callCaptureDeviceId`).
    pub call_capture_device_id: String,
    /// Call speaker volume percentage (`_callOutputVolume`).
    pub call_output_volume: i32,
    /// Call microphone volume percentage (`_callInputVolume`).
    pub call_input_volume: i32,
    /// Duck other audio during calls (`_callAudioDuckingEnabled`).
    pub call_audio_ducking_enabled: bool,
    /// Legacy disable-calls flag read from older settings (`_disableCallsLegacy`).
    pub disable_calls_legacy: bool,

    /// Push-to-talk enabled in group calls (`_groupCallPushToTalk`).
    pub group_call_push_to_talk: bool,
    /// Push-to-talk shortcut key bytes (`_groupCallPushToTalkShortcut`).
    pub group_call_push_to_talk_shortcut: Vec<u8>,
    /// Push-to-talk release delay in ms (`_groupCallPushToTalkDelay`).
    pub group_call_push_to_talk_delay: i64,
    /// Noise suppression in group calls (`_groupCallNoiseSuppression`).
    pub group_call_noise_suppression: bool,
    /// Hidden group-call tooltips bitmask (`_hiddenGroupCallTooltips`).
    pub hidden_group_call_tooltips: u32,

    /// Last-seen warning dialog seen (`_lastSeenWarningSeen`).
    pub last_seen_warning_seen: bool,
    /// Custom sound file path overrides (`_soundOverrides`).
    pub sound_overrides: BTreeMap<String, String>,
    /// File sending mode flags (`_sendFilesWay`).
    pub send_files_way: SendFilesWay,
    /// Message submit key (`_sendSubmitWay`).
    pub send_submit_way: InputSubmitSettings,
    /// File extensions exempted from executable-launch warning (`_noWarningExtensions`).
    pub no_warning_extensions: BTreeSet<String>,
    /// Warn about IP address reveal in peer-to-peer calls (`_ipRevealWarning`).
    pub ip_reveal_warning: bool,

    /// Loop animated stickers (`_loopAnimatedStickers`).
    pub loop_animated_stickers: bool,
    /// Enlarge emoji-only messages (`_largeEmoji`).
    pub large_emoji: bool,
    /// Replace emoticons like `:)` with emoji (`_replaceEmoji`).
    pub replace_emoji: bool,
    /// Apply OS text replacements (`_systemTextReplace`).
    pub system_text_replace: bool,
    /// Show emoji suggestions when typing `:code` (`_suggestEmoji`).
    pub suggest_emoji: bool,
    /// Suggest stickers matching typed emoji (`_suggestStickersByEmoji`).
    pub suggest_stickers_by_emoji: bool,
    /// Suggest animated custom emoji (`_suggestAnimatedEmoji`).
    pub suggest_animated_emoji: bool,
    /// Show corner quick-reaction button (`_cornerReaction`).
    pub corner_reaction: bool,
    /// Show corner quick-reply button (`_cornerReply`).
    pub corner_reply: bool,
    /// Pull at bottom of channel to switch to next unread channel (`_pullToNextChannel`).
    pub pull_to_next_channel: bool,

    /// Spellchecker enabled (`_spellcheckerEnabled`).
    pub spellchecker_enabled: bool,
    /// Enabled spellchecker dictionary locale IDs (`_dictionariesEnabled`).
    pub dictionaries_enabled: Vec<i32>,
    /// Automatically download spellchecker dictionaries (`_autoDownloadDictionaries`).
    pub auto_download_dictionaries: bool,

    /// Video playback speed (`_videoPlaybackSpeed`).
    pub video_playback_speed: PlaybackSpeed,
    /// Voice message playback speed (`_voicePlaybackSpeed`).
    pub voice_playback_speed: PlaybackSpeed,
    /// Audio file playback speed (`_audioPlaybackSpeed`).
    pub audio_playback_speed: PlaybackSpeed,
    /// Picture-in-picture video geometry bytes (`_videoPipGeometry`).
    pub video_pip_geometry: Vec<u8>,

    /// Show accounts section expanded in main menu (`_mainMenuAccountsShown`).
    pub main_menu_accounts_shown: bool,
    /// Third-column tabbed emoji/sticker selector open (`_tabbedSelectorSectionEnabled`).
    pub tabbed_selector_section_enabled: bool,
    /// Floating media player column (`_floatPlayerColumn`).
    pub float_player_column: WindowColumn,
    /// Floating media player corner (`_floatPlayerCorner`).
    pub float_player_corner: RectPartCorner,
    /// Third-column chat info panel open (`_thirdSectionInfoEnabled`).
    pub third_section_info_enabled: bool,
    /// Saved third-section extended width (`_thirdSectionExtendedBy`).
    pub third_section_extended_by: i32,
    /// Dialogs list width ratio when chat is open (`_dialogsWithChatWidthRatio`).
    pub dialogs_with_chat_width_ratio: f64,
    /// Dialogs list width ratio when no chat is open (`_dialogsNoChatWidthRatio`).
    pub dialogs_no_chat_width_ratio: f64,
    /// Third column width (`_thirdColumnWidth`).
    pub third_column_width: i32,

    /// Show notifications from all accounts (`_notifyFromAll`).
    pub notify_from_all: bool,
    /// Use native OS window frame (`_nativeWindowFrame`).
    pub native_window_frame: bool,
    /// Follow OS dark mode (`_systemDarkModeEnabled`).
    pub system_dark_mode_enabled: bool,
    /// Follow OS accent color (`_systemAccentColorEnabled`).
    pub system_accent_color_enabled: bool,
    /// Window title content visibility (`_windowTitleContent`).
    pub window_title_content: WindowTitleContent,

    /// Main window saved position (`_windowPosition`).
    pub window_position: WindowPosition,
    /// Media viewer saved position (`_mediaViewPosition`).
    pub media_view_position: WindowPosition,
    /// Instant View window saved position (`_ivPosition`).
    pub iv_position: WindowPosition,
    /// Call panel saved position (`_callPanelPosition`).
    pub call_panel_position: WindowPosition,

    /// Recent emoji preload table (`_recentEmojiPreload`).
    pub recent_emoji_preload: Vec<RecentEmojiPreload>,
    /// Hidden default recent emojis (`_recentEmojiSkip`).
    pub recent_emoji_skip: BTreeSet<String>,
    /// Saved skin-tone variant index per base emoji (`_emojiVariants`).
    pub emoji_variants: BTreeMap<String, u8>,

    /// Force-disable OpenGL rendering (`_disableOpenGL`).
    pub disable_opengl: bool,
    /// Tray / taskbar work mode (`_workMode`).
    pub work_mode: WorkMode,
    /// Saved photo editor brush state (`_photoEditorBrush`).
    pub photo_editor_brush: Vec<u8>,
    /// Window close behavior (`_closeBehavior`).
    pub close_behavior: CloseBehavior,
    /// Use monochrome tray icon (`_trayIconMonochrome`).
    pub tray_icon_monochrome: bool,
    /// Custom device model string reported to Telegram (`_customDeviceModel`).
    pub custom_device_model: String,
    /// Media player repeat mode (`_playerRepeatMode`).
    pub player_repeat_mode: RepeatMode,
    /// Media player track order mode (`_playerOrderMode`).
    pub player_order_mode: OrderMode,
    /// Warn before quitting via Cmd+Q on macOS (`_macWarnBeforeQuit`).
    pub mac_warn_before_quit: bool,
    /// Custom account display order by session unique ID (`_accountsOrder`).
    pub accounts_order: Vec<u64>,
    /// Hardware-accelerated video decoding enabled (`_hardwareAcceleratedVideo`).
    pub hardware_accelerated_video: bool,
    /// Double-click message quick action (`_chatQuickAction`).
    pub chat_quick_action: DoubleClickQuickAction,

    /// Show "Translate" button in message context menu (`_translateButtonEnabled`).
    pub translate_button_enabled: bool,
    /// Use OS/platform translation provider (`_usePlatformTranslation`).
    pub use_platform_translation: bool,
    /// Translate entire chat toggle (`_translateChatEnabled`).
    pub translate_chat_enabled: bool,
    /// Legacy numeric target language ID (`_translateToRaw`).
    pub translate_to_raw: u64,
    /// Legacy numeric skip-translation language IDs.
    pub skip_translation_languages_raw: Vec<u64>,
    /// ISO language code for translation target (format v1 tail).
    pub translate_to_code: String,
    /// ISO language codes skipped from translation (format v1 tail).
    pub skip_translation_language_codes: Vec<String>,

    /// Remember "delete only for me" checkbox state (`_rememberedDeleteMessageOnlyForYou`).
    pub remembered_delete_message_only_for_you: bool,
    /// Ignore OS battery saver restrictions (`_ignoreBatterySaving`).
    pub ignore_battery_saving: bool,
    /// macOS round icon digest (`_macRoundIconDigest`).
    pub mac_round_icon_digest: Option<u64>,
    /// Stories click tooltip dismissed (`_storiesClickTooltipHidden`).
    pub stories_click_tooltip_hidden: bool,
    /// TTL voice message click tooltip dismissed (`_ttlVoiceClickTooltipHidden`).
    pub ttl_voice_click_tooltip_hidden: bool,
    /// Custom UI font family name (`_customFontFamily`).
    pub custom_font_family: String,
    /// OS biometric/system unlock enabled (`_systemUnlockEnabled`).
    pub system_unlock_enabled: bool,
    /// Weather unit override (`None` = locale default, `Some(true)` = Celsius, `Some(false)` = Fahrenheit).
    pub weather_in_celsius: Option<bool>,
    /// TON site storage token (`_tonsiteStorageToken`).
    pub tonsite_storage_token: Vec<u8>,
    /// Horizontal folder tabs layout (`_chatFiltersHorizontal`).
    pub chat_filters_horizontal: bool,
    /// Folder tabs label/icon mode (`_chatFiltersTabsMode`).
    pub chat_filters_tabs_mode: ChatsFiltersTabsMode,
    /// Record round video messages by default instead of voice (`_recordVideoMessages`).
    pub record_video_messages: bool,
    /// Preferred video playback quality (`_videoQuality`).
    pub video_quality: VideoQuality,
    /// Instant View zoom percentage (`0` = auto).
    pub iv_zoom: i32,
    /// Quick dialog swipe/hover action (`_quickDialogAction`).
    pub quick_dialog_action: QuickDialogAction,
    /// Shared media grid zoom step (`_mediaGridZoomStep`).
    pub media_grid_zoom_step: i32,
    /// Power-saving feature flags (`PowerSaving::Flags` / `dbiPowerSaving`).
    pub power_saving: PowerSavingFlags,
    /// Forced battery-saving override (`PowerSaving::ForceAll()`, bypassed when
    /// `ignore_battery_saving` is `true`).
    pub power_saving_force_all: bool,
    /// Typed inline media autoplay preferences (`AutoPlayGIF`, `AutoPlayVideo`,
    /// `AutoPlayVideoMessage`, and video stickers).
    pub autoplay: AutoplaySettings,
    /// Persisted experimental feature flag overrides (`base::options` in `settings_experimental.cpp`).
    pub experimental_options: BTreeMap<String, bool>,
    /// Persisted recent Settings search entry IDs (`Settings::RecentSearches` in `settings_recent_searches.cpp`).
    pub recent_settings_searches: Vec<String>,
    /// Generic key-value preference store (`_prefs`).
    pub prefs: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl Default for CoreSettings {
    fn default() -> Self {
        Self::new()
    }
}

impl CoreSettings {
    /// Creates settings initialized to Telegram Desktop's exact defaults (`Settings::Settings()`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            proxy: SettingsProxy::new(),
            themes_accent_colors: AccentColors::default(),
            adaptive_for_wide: true,
            moderate_mode_enabled: false,
            song_volume: DEFAULT_VOLUME,
            video_volume: DEFAULT_VOLUME,
            ask_download_path: false,
            download_path: String::new(),
            download_path_bookmark: Vec::new(),
            sound_notify: true,
            desktop_notify: true,
            flash_bounce_notify: true,
            notify_view: NotifyView::ShowPreview,
            native_notifications: None,
            skip_toasts_in_focus: false,
            notifications_count: 3,
            notifications_corner: ScreenCorner::BottomRight,
            notifications_display_checksum: 0,
            notifications_volume: 100,
            include_muted_counter: true,
            include_muted_counter_folders: true,
            count_unread_messages: true,
            notify_about_pinned: true,
            auto_lock: 3600,
            playback_device_id: String::new(),
            capture_device_id: String::new(),
            camera_device_id: String::new(),
            call_playback_device_id: String::new(),
            call_capture_device_id: String::new(),
            call_output_volume: 100,
            call_input_volume: 100,
            call_audio_ducking_enabled: true,
            disable_calls_legacy: false,
            group_call_push_to_talk: false,
            group_call_push_to_talk_shortcut: Vec::new(),
            group_call_push_to_talk_delay: 20,
            group_call_noise_suppression: false,
            hidden_group_call_tooltips: 0,
            last_seen_warning_seen: false,
            sound_overrides: BTreeMap::new(),
            send_files_way: SendFilesWay::default(),
            send_submit_way: InputSubmitSettings::Enter,
            no_warning_extensions: BTreeSet::new(),
            ip_reveal_warning: true,
            loop_animated_stickers: true,
            large_emoji: true,
            replace_emoji: true,
            system_text_replace: true,
            suggest_emoji: true,
            suggest_stickers_by_emoji: true,
            suggest_animated_emoji: true,
            corner_reaction: true,
            corner_reply: true,
            pull_to_next_channel: true,
            spellchecker_enabled: true,
            dictionaries_enabled: Vec::new(),
            auto_download_dictionaries: true,
            video_playback_speed: PlaybackSpeed::default(),
            voice_playback_speed: PlaybackSpeed::default(),
            audio_playback_speed: PlaybackSpeed::default(),
            video_pip_geometry: Vec::new(),
            main_menu_accounts_shown: true,
            tabbed_selector_section_enabled: false,
            float_player_column: WindowColumn::Second,
            float_player_corner: RectPartCorner::TopRight,
            third_section_info_enabled: true,
            third_section_extended_by: -1,
            dialogs_with_chat_width_ratio: DEFAULT_DIALOGS_WIDTH_RATIO,
            dialogs_no_chat_width_ratio: DEFAULT_DIALOGS_WIDTH_RATIO,
            third_column_width: 0,
            notify_from_all: true,
            native_window_frame: false,
            system_dark_mode_enabled: true,
            system_accent_color_enabled: false,
            window_title_content: WindowTitleContent::default(),
            window_position: WindowPosition::default(),
            media_view_position: WindowPosition {
                maximized: 2,
                ..WindowPosition::default()
            },
            iv_position: WindowPosition::default(),
            call_panel_position: WindowPosition::default(),
            recent_emoji_preload: Vec::new(),
            recent_emoji_skip: BTreeSet::new(),
            emoji_variants: BTreeMap::new(),
            disable_opengl: false,
            work_mode: WorkMode::WindowAndTray,
            photo_editor_brush: Vec::new(),
            close_behavior: CloseBehavior::Quit,
            tray_icon_monochrome: true,
            custom_device_model: String::new(),
            player_repeat_mode: RepeatMode::None,
            player_order_mode: OrderMode::Default,
            mac_warn_before_quit: true,
            accounts_order: Vec::new(),
            hardware_accelerated_video: cfg!(target_os = "macos"),
            chat_quick_action: DoubleClickQuickAction::Reply,
            translate_button_enabled: false,
            use_platform_translation: false,
            translate_chat_enabled: true,
            translate_to_raw: 0,
            skip_translation_languages_raw: Vec::new(),
            translate_to_code: String::new(),
            skip_translation_language_codes: Vec::new(),
            remembered_delete_message_only_for_you: false,
            ignore_battery_saving: false,
            mac_round_icon_digest: None,
            stories_click_tooltip_hidden: false,
            ttl_voice_click_tooltip_hidden: false,
            custom_font_family: String::new(),
            system_unlock_enabled: false,
            weather_in_celsius: None,
            tonsite_storage_token: Vec::new(),
            chat_filters_horizontal: false,
            chat_filters_tabs_mode: ChatsFiltersTabsMode::Default,
            record_video_messages: false,
            video_quality: VideoQuality::default(),
            iv_zoom: 0,
            quick_dialog_action: QuickDialogAction::Disabled,
            media_grid_zoom_step: 0,
            power_saving: PowerSavingFlags::NONE,
            power_saving_force_all: false,
            autoplay: AutoplaySettings::default(),
            experimental_options: BTreeMap::new(),
            recent_settings_searches: Vec::new(),
            prefs: BTreeMap::new(),
        }
    }

    /// Returns `true` when `flag` is disabled by power saving (`PowerSaving::On` in
    /// `ui/power_saving.cpp`), taking `power_saving_force_all` and `ignore_battery_saving`
    /// into account.
    #[must_use]
    pub const fn is_power_saving_on(&self, flag: PowerSavingFlags) -> bool {
        if flag.is_empty() {
            return false;
        }
        (self.power_saving_force_all && !self.ignore_battery_saving)
            || self.power_saving.contains(flag)
    }

    /// Returns whether inline GIF autoplay is enabled (`AutoPlayGIF`).
    #[must_use]
    pub const fn autoplay_gifs(&self) -> bool {
        self.autoplay.gifs
    }

    /// Returns whether inline muted video autoplay is enabled (`AutoPlayVideo`).
    #[must_use]
    pub const fn autoplay_videos(&self) -> bool {
        self.autoplay.videos
    }

    /// Returns whether inline round video message autoplay is enabled (`AutoPlayVideoMessage`).
    #[must_use]
    pub const fn autoplay_round_videos(&self) -> bool {
        self.autoplay.round_videos
    }

    /// Returns whether inline video/animated sticker autoplay is enabled.
    #[must_use]
    pub const fn autoplay_video_stickers(&self) -> bool {
        self.autoplay.video_stickers
    }

    /// Reads a raw preference entry by key (`readPrefGeneric`).
    #[must_use]
    pub fn read_pref(&self, key: &str) -> Option<&[u8]> {
        self.prefs.get(key.as_bytes()).map(Vec::as_slice)
    }

    /// Writes a raw preference entry (`writePrefGeneric`).
    pub fn write_pref(&mut self, key: &str, value: Vec<u8>) {
        self.prefs.insert(key.as_bytes().to_vec(), value);
    }

    /// Removes a preference entry (`clearPref`).
    pub fn clear_pref(&mut self, key: &str) {
        self.prefs.remove(key.as_bytes());
    }

    /// Reads a boolean preference (non-empty byte slice = `true`).
    #[must_use]
    pub fn read_pref_bool(&self, key: &str, fallback: bool) -> bool {
        self.read_pref(key)
            .map_or(fallback, |bytes| !bytes.is_empty())
    }

    /// Writes a boolean preference (`b"\x01"` for `true`, empty for `false`).
    pub fn write_pref_bool(&mut self, key: &str, value: bool) {
        self.write_pref(key, if value { vec![1] } else { Vec::new() });
    }

    /// Resets per-account-session preferences when the last account logs out (`resetOnLastLogout`).
    pub fn reset_on_last_logout(&mut self) {
        let defaults = Self::new();
        self.adaptive_for_wide = defaults.adaptive_for_wide;
        self.moderate_mode_enabled = defaults.moderate_mode_enabled;
        self.song_volume = defaults.song_volume;
        self.video_volume = defaults.video_volume;
        self.ask_download_path = defaults.ask_download_path;
        self.download_path.clear();
        self.download_path_bookmark.clear();
        self.sound_notify = true;
        self.desktop_notify = true;
        self.flash_bounce_notify = true;
        self.notify_view = NotifyView::ShowPreview;
        self.include_muted_counter = true;
        self.include_muted_counter_folders = true;
        self.count_unread_messages = true;
        self.notify_about_pinned = true;
        self.disable_calls_legacy = false;
        self.group_call_push_to_talk = false;
        self.group_call_push_to_talk_shortcut.clear();
        self.group_call_push_to_talk_delay = 20;
        self.group_call_noise_suppression = false;
        self.last_seen_warning_seen = false;
        self.send_files_way = SendFilesWay::default();
        self.sound_overrides.clear();
        self.no_warning_extensions.clear();
        self.ip_reveal_warning = true;
        self.loop_animated_stickers = true;
        self.large_emoji = true;
        self.replace_emoji = true;
        self.system_text_replace = true;
        self.suggest_emoji = true;
        self.suggest_stickers_by_emoji = true;
        self.suggest_animated_emoji = true;
        self.spellchecker_enabled = true;
        self.video_playback_speed = PlaybackSpeed::default();
        self.voice_playback_speed = PlaybackSpeed::default();
        self.audio_playback_speed = PlaybackSpeed::default();
        self.dictionaries_enabled.clear();
        self.auto_download_dictionaries = true;
        self.main_menu_accounts_shown = true;
        self.tabbed_selector_section_enabled = false;
        self.float_player_column = WindowColumn::Second;
        self.float_player_corner = RectPartCorner::TopRight;
        self.third_section_info_enabled = true;
        self.third_section_extended_by = -1;
        self.dialogs_with_chat_width_ratio = DEFAULT_DIALOGS_WIDTH_RATIO;
        self.dialogs_no_chat_width_ratio = DEFAULT_DIALOGS_WIDTH_RATIO;
        self.third_column_width = 0;
        self.notify_from_all = true;
        self.hidden_group_call_tooltips = 0;
        self.stories_click_tooltip_hidden = false;
        self.ttl_voice_click_tooltip_hidden = false;
        let sr_disabled = self.read_pref_bool(SCREEN_READER_MODE_DISABLED_KEY, false);
        self.prefs.clear();
        if sr_disabled {
            self.write_pref_bool(SCREEN_READER_MODE_DISABLED_KEY, true);
        }
        self.iv_zoom = 0;
        self.record_video_messages = false;
        self.video_quality = VideoQuality {
            manual: false,
            height: 0,
            original: false,
        };
        self.chat_filters_horizontal = false;
        self.chat_filters_tabs_mode = ChatsFiltersTabsMode::Default;
        self.pull_to_next_channel = true;
        self.quick_dialog_action = QuickDialogAction::Disabled;
        self.notifications_volume = 100;
        self.autoplay = AutoplaySettings::default();
        self.recent_settings_searches.clear();
        self.recent_emoji_preload.clear();
        self.emoji_variants.clear();
        self.accounts_order.clear();
    }

    /// Serializes all fields in the exact `QDataStream` big-endian layout of
    /// `Core::Settings::serialize()` (`core/core_settings.cpp:421-606`).
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let themes_accent_colors = self.themes_accent_colors.serialize();
        let window_position = self.window_position.serialize();
        let media_view_position = self.media_view_position.serialize();
        let iv_position = self.iv_position.serialize();
        let call_panel_position = self.call_panel_position.serialize();
        let proxy = self.proxy.serialize();
        let no_warning_extensions: String = self
            .no_warning_extensions
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");

        let mut w = QDataStreamWriter::new();

        // Block 1 (initial core settings fields, lines 424-472)
        w.write_byte_array(&themes_accent_colors);
        w.write_bool_i32(self.adaptive_for_wide);
        w.write_bool_i32(self.moderate_mode_enabled);
        w.write_i32((self.song_volume * 1e6).round() as i32);
        w.write_i32((self.video_volume * 1e6).round() as i32);
        w.write_bool_i32(self.ask_download_path);
        w.write_qstring(&self.download_path);
        w.write_byte_array(&self.download_path_bookmark);
        w.write_i32(1); // legacy non-default voice speed
        w.write_bool_i32(self.sound_notify);
        w.write_bool_i32(self.desktop_notify);
        w.write_bool_i32(self.flash_bounce_notify);
        w.write_i32(self.notify_view as i32);
        w.write_i32(match self.native_notifications {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        });
        w.write_i32(self.notifications_count);
        w.write_i32(self.notifications_corner as i32);
        w.write_i32(self.auto_lock);
        w.write_qstring(""); // legacy call output device id
        w.write_qstring(""); // legacy call input device id
        w.write_i32(self.call_output_volume);
        w.write_i32(self.call_input_volume);
        w.write_bool_i32(self.call_audio_ducking_enabled);
        w.write_bool_i32(self.last_seen_warning_seen);
        w.write_i32(self.sound_overrides.len() as i32);
        for (key, value) in &self.sound_overrides {
            w.write_qstring(key);
            w.write_qstring(value);
        }
        w.write_i32(self.send_files_way.serialize());
        w.write_i32(self.send_submit_way as i32);
        w.write_bool_i32(self.include_muted_counter);
        w.write_bool_i32(self.count_unread_messages);
        w.write_i32(1); // legacy exe launch warning
        w.write_bool_i32(self.notify_about_pinned);
        w.write_bool_i32(self.loop_animated_stickers);
        w.write_bool_i32(self.large_emoji);
        w.write_bool_i32(self.replace_emoji);
        w.write_bool_i32(self.suggest_emoji);
        w.write_bool_i32(self.suggest_stickers_by_emoji);
        w.write_bool_i32(self.spellchecker_enabled);
        w.write_i32(self.video_playback_speed.serialize());
        w.write_byte_array(&self.video_pip_geometry);
        w.write_i32(self.dictionaries_enabled.len() as i32);
        for &lang_id in &self.dictionaries_enabled {
            w.write_u64(lang_id as i64 as u64);
        }
        w.write_bool_i32(self.auto_download_dictionaries);
        w.write_bool_i32(self.main_menu_accounts_shown);

        // Block 2 (column layout & window state, lines 473-495)
        w.write_bool_i32(self.tabbed_selector_section_enabled);
        w.write_i32(self.float_player_column as i32);
        w.write_i32(self.float_player_corner as i32);
        w.write_bool_i32(self.third_section_info_enabled);
        w.write_i32(
            ((self.dialogs_with_chat_width_ratio * 1_000_000.0).round() as i32).clamp(0, 1_000_000),
        );
        w.write_i32(self.third_column_width);
        w.write_i32(self.third_section_extended_by);
        w.write_bool_i32(self.notify_from_all);

        // Appended versioned fields in exact tdesktop chronological order (lines 485-603)
        w.write_bool_i32(self.native_window_frame);
        w.write_i32(0); // legacy system dark mode
        w.write_qstring(&self.camera_device_id);
        w.write_bool_i32(self.ip_reveal_warning);
        w.write_bool_i32(self.group_call_push_to_talk);
        w.write_byte_array(&self.group_call_push_to_talk_shortcut);
        w.write_i64(self.group_call_push_to_talk_delay);
        w.write_i32(0); // legacy call audio backend
        w.write_i32(0); // legacy disable calls
        w.write_byte_array(&window_position);
        w.write_i32(self.recent_emoji_preload.len() as i32);
        for entry in &self.recent_emoji_preload {
            w.write_qstring(&entry.id);
            w.write_u16(entry.rating);
        }
        w.write_i32(self.emoji_variants.len() as i32);
        for (id, &variant) in &self.emoji_variants {
            w.write_qstring(id);
            w.write_u8(variant);
        }
        w.write_i32(0); // old disable OpenGL
        w.write_i32(0); // old noise suppression
        w.write_i32(self.work_mode as i32);
        w.write_byte_array(&proxy);
        w.write_i32(self.hidden_group_call_tooltips as i32);
        w.write_bool_i32(self.disable_opengl);
        w.write_byte_array(&self.photo_editor_brush);
        w.write_bool_i32(self.group_call_noise_suppression);
        w.write_i32(self.voice_playback_speed.serialize());
        w.write_i32(self.close_behavior as i32);
        w.write_qstring(&self.custom_device_model);
        w.write_i32(self.player_repeat_mode as i32);
        w.write_i32(self.player_order_mode as i32);
        w.write_bool_i32(self.mac_warn_before_quit);

        w.write_i32(self.accounts_order.len() as i32);
        for &id in &self.accounts_order {
            w.write_u64(id);
        }

        w.write_i32(0); // old hardwareAcceleratedVideo
        w.write_i32(self.chat_quick_action as i32);
        w.write_bool_i32(self.hardware_accelerated_video);
        w.write_bool_i32(self.suggest_animated_emoji);
        w.write_bool_i32(self.corner_reaction);
        w.write_bool_i32(self.translate_button_enabled);

        w.write_i32(self.skip_translation_languages_raw.len() as i32);
        for &lang in &self.skip_translation_languages_raw {
            w.write_u64(lang);
        }

        w.write_bool_i32(self.remembered_delete_message_only_for_you);
        w.write_bool_i32(self.translate_chat_enabled);
        w.write_u64(self.translate_to_raw);
        w.write_bool_i32(self.window_title_content.hide_chat_name);
        w.write_bool_i32(self.window_title_content.hide_account_name);
        w.write_bool_i32(self.window_title_content.hide_total_unread);
        w.write_byte_array(&media_view_position);
        w.write_bool_i32(self.ignore_battery_saving);
        w.write_u64(self.mac_round_icon_digest.unwrap_or(0));
        w.write_bool_i32(self.stories_click_tooltip_hidden);
        w.write_i32(self.recent_emoji_skip.len() as i32);
        for id in &self.recent_emoji_skip {
            w.write_qstring(id);
        }
        w.write_bool_i32(self.tray_icon_monochrome);
        w.write_bool_i32(self.ttl_voice_click_tooltip_hidden);
        w.write_qstring(&self.playback_device_id);
        w.write_qstring(&self.capture_device_id);
        w.write_qstring(&self.call_playback_device_id);
        w.write_qstring(&self.call_capture_device_id);
        w.write_byte_array(&iv_position);
        w.write_qstring(&no_warning_extensions);
        w.write_qstring(&self.custom_font_family);
        w.write_i32(
            ((self.dialogs_no_chat_width_ratio * 1_000_000.0).round() as i32).clamp(0, 1_000_000),
        );
        w.write_bool_i32(self.system_unlock_enabled);
        w.write_i32(match self.weather_in_celsius {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        });
        w.write_byte_array(&self.tonsite_storage_token);
        w.write_bool_i32(self.include_muted_counter_folders);
        w.write_bool_i32(self.chat_filters_horizontal);
        w.write_bool_i32(self.skip_toasts_in_focus);
        w.write_bool_i32(self.record_video_messages);
        w.write_u32(self.video_quality.to_u32());
        w.write_i32(self.iv_zoom);
        w.write_bool_i32(self.system_dark_mode_enabled);
        w.write_i32(self.quick_dialog_action as i32);
        w.write_u16(self.notifications_volume);
        w.write_i32(self.notifications_display_checksum);
        w.write_byte_array(&call_panel_position);
        w.write_bool_i32(self.corner_reply);
        w.write_bool_i32(self.system_accent_color_enabled);
        w.write_bool_i32(self.use_platform_translation);
        w.write_bool_i32(self.system_text_replace);

        let mut serialized_prefs = self.prefs.clone();
        if self.power_saving != PowerSavingFlags::NONE || self.power_saving_force_all {
            let mut val = [0u8; 5];
            val[..4].copy_from_slice(&self.power_saving.bits().to_be_bytes());
            val[4] = u8::from(self.power_saving_force_all);
            serialized_prefs.insert(PREF_KEY_POWER_SAVING.to_vec(), val.to_vec());
        } else {
            serialized_prefs.remove(PREF_KEY_POWER_SAVING);
        }
        if self.autoplay != AutoplaySettings::default() {
            serialized_prefs.insert(PREF_KEY_AUTOPLAY.to_vec(), vec![self.autoplay.to_u8()]);
        } else {
            serialized_prefs.remove(PREF_KEY_AUTOPLAY);
        }
        if !self.experimental_options.is_empty() {
            let mut ew = QDataStreamWriter::new();
            ew.write_u32(self.experimental_options.len() as u32);
            for (k, &v) in &self.experimental_options {
                ew.write_byte_array(k.as_bytes());
                ew.write_u8(u8::from(v));
            }
            serialized_prefs.insert(PREF_KEY_EXPERIMENTAL_OPTIONS.to_vec(), ew.into_bytes());
        } else {
            serialized_prefs.remove(PREF_KEY_EXPERIMENTAL_OPTIONS);
        }
        if !self.recent_settings_searches.is_empty() {
            let mut rw = QDataStreamWriter::new();
            let count = self.recent_settings_searches.len().min(32);
            rw.write_u32(count as u32);
            for entry in self.recent_settings_searches.iter().take(count) {
                rw.write_byte_array(entry.as_bytes());
            }
            serialized_prefs.insert(PREF_KEY_RECENT_SETTINGS_SEARCHES.to_vec(), rw.into_bytes());
        } else {
            serialized_prefs.remove(PREF_KEY_RECENT_SETTINGS_SEARCHES);
        }

        w.write_u32(serialized_prefs.len() as u32);
        for (key, value) in &serialized_prefs {
            w.write_byte_array(key);
            w.write_byte_array(value);
        }

        w.write_i32(self.audio_playback_speed.serialize());
        w.write_i32(self.media_grid_zoom_step);
        w.write_bool_i32(self.pull_to_next_channel);
        w.write_i32(self.chat_filters_tabs_mode as i32);

        // Language codes v1 tail (lines 597-602)
        w.write_i32(LANGUAGE_CODES_FORMAT);
        w.write_i32(self.skip_translation_language_codes.len() as i32);
        for code in &self.skip_translation_language_codes {
            w.write_qstring(code);
        }
        w.write_qstring(&self.translate_to_code);

        w.into_bytes()
    }

    /// Merges fields from a serialized `Core::Settings` blob (`Settings::addFromSerialized`).
    ///
    /// Supports older/shorter tdesktop versions cleanly: any trailing fields absent at EOF keep
    /// their defaults (or tdesktop's legacy migration defaults, e.g., `tray_icon_monochrome = false`,
    /// `call_*_device_id` falling back to legacy device IDs, and `audio_playback_speed` inheriting
    /// `voice_playback_speed`).
    pub fn add_from_serialized(
        &mut self,
        serialized: &[u8],
        _writer_version: i32,
    ) -> Result<(), SettingsError> {
        if serialized.is_empty() {
            return Ok(());
        }

        let mut r = QDataStreamReader::new(serialized);

        let themes_accent_colors = r.read_byte_array()?;

        let mut adaptive_for_wide = i32::from(self.adaptive_for_wide);
        let mut moderate_mode_enabled = i32::from(self.moderate_mode_enabled);
        let mut song_volume = (self.song_volume * 1e6).round() as i32;
        let mut video_volume = (self.video_volume * 1e6).round() as i32;
        let mut ask_download_path = i32::from(self.ask_download_path);
        let mut download_path = self.download_path.clone();
        let mut download_path_bookmark = self.download_path_bookmark.clone();
        let mut non_default_voice_playback_speed = 1i32;
        let mut sound_notify = i32::from(self.sound_notify);
        let mut desktop_notify = i32::from(self.desktop_notify);
        let mut flash_bounce_notify = i32::from(self.flash_bounce_notify);
        let mut notify_view = self.notify_view as i32;
        let mut native_notifications = match self.native_notifications {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        };
        let mut notifications_count = self.notifications_count;
        let mut notifications_corner = self.notifications_corner as i32;
        let mut notifications_display_checksum = self.notifications_display_checksum;
        let mut auto_lock = self.auto_lock;
        let mut playback_device_id = self.playback_device_id.clone();
        let mut capture_device_id = self.capture_device_id.clone();
        let mut camera_device_id = self.camera_device_id.clone();
        let mut legacy_call_playback_device_id = self.call_playback_device_id.clone();
        let mut legacy_call_capture_device_id = self.call_capture_device_id.clone();
        let mut call_output_volume = self.call_output_volume;
        let mut call_input_volume = self.call_input_volume;
        let mut call_audio_ducking_enabled = i32::from(self.call_audio_ducking_enabled);
        let mut last_seen_warning_seen = i32::from(self.last_seen_warning_seen);
        let mut sound_overrides = BTreeMap::new();
        let mut send_files_way = self.send_files_way.serialize();
        let mut send_submit_way = self.send_submit_way as i32;
        let mut include_muted_counter = i32::from(self.include_muted_counter);
        let mut include_muted_counter_folders = i32::from(self.include_muted_counter_folders);
        let mut count_unread_messages = i32::from(self.count_unread_messages);
        let mut no_warning_extensions: Option<String> = None;
        let mut notify_about_pinned = i32::from(self.notify_about_pinned);
        let mut loop_animated_stickers = i32::from(self.loop_animated_stickers);
        let mut large_emoji = i32::from(self.large_emoji);
        let mut replace_emoji = i32::from(self.replace_emoji);
        let mut suggest_emoji = i32::from(self.suggest_emoji);
        let mut suggest_stickers_by_emoji = i32::from(self.suggest_stickers_by_emoji);
        let mut spellchecker_enabled = i32::from(self.spellchecker_enabled);
        let mut video_playback_speed = self.video_playback_speed.serialize();
        let mut voice_playback_speed = self.voice_playback_speed.serialize();
        let mut audio_playback_speed: Option<i32> = None;
        let mut video_pip_geometry = self.video_pip_geometry.clone();
        let mut dictionaries_enabled = Vec::new();
        let mut auto_download_dictionaries = i32::from(self.auto_download_dictionaries);
        let mut main_menu_accounts_shown = i32::from(self.main_menu_accounts_shown);
        let mut tabbed_selector_section_enabled = 1i32;
        let mut float_player_column = WindowColumn::Second as i32;
        let mut float_player_corner = RectPartCorner::TopRight as i32;
        let mut third_section_info_enabled = 0i32;
        let mut dialogs_with_chat_width_ratio = self.dialogs_with_chat_width_ratio;
        let mut dialogs_no_chat_width_ratio = self.dialogs_no_chat_width_ratio;
        let mut third_column_width = self.third_column_width;
        let mut third_section_extended_by = self.third_section_extended_by;
        let mut notify_from_all = i32::from(self.notify_from_all);
        let mut native_window_frame = i32::from(self.native_window_frame);
        let mut system_dark_mode_enabled = i32::from(self.system_dark_mode_enabled);
        let mut ip_reveal_warning = i32::from(self.ip_reveal_warning);
        let mut group_call_push_to_talk = i32::from(self.group_call_push_to_talk);
        let mut group_call_push_to_talk_shortcut = self.group_call_push_to_talk_shortcut.clone();
        let mut group_call_push_to_talk_delay = self.group_call_push_to_talk_delay;
        let mut disable_calls_legacy = 0i32;
        let mut window_position = Vec::new();
        let mut recent_emoji_preload = Vec::new();
        let mut emoji_variants = BTreeMap::new();
        let mut disable_opengl = i32::from(self.disable_opengl);
        let mut group_call_noise_suppression = i32::from(self.group_call_noise_suppression);
        let mut work_mode = self.work_mode as i32;
        let mut proxy_bytes = Vec::new();
        let mut hidden_group_call_tooltips = self.hidden_group_call_tooltips as i32;
        let mut photo_editor_brush = self.photo_editor_brush.clone();
        let mut close_behavior = self.close_behavior as i32;
        let mut custom_device_model = self.custom_device_model.clone();
        let mut player_repeat_mode = self.player_repeat_mode as i32;
        let mut player_order_mode = self.player_order_mode as i32;
        let mut mac_warn_before_quit = i32::from(self.mac_warn_before_quit);
        let mut accounts_order = Vec::new();
        let mut hardware_accelerated_video = i32::from(self.hardware_accelerated_video);
        let mut chat_quick_action = self.chat_quick_action as i32;
        let mut suggest_animated_emoji = i32::from(self.suggest_animated_emoji);
        let mut corner_reply = i32::from(self.corner_reply);
        let mut corner_reaction = i32::from(self.corner_reaction);
        let mut pull_to_next_channel = i32::from(self.pull_to_next_channel);
        let mut chat_filters_tabs_mode = self.chat_filters_tabs_mode as i32;
        let mut legacy_skip_translation_for_language = i32::from(self.translate_button_enabled);
        let mut skip_translation_languages_raw = Vec::new();
        let mut remembered_delete_message_only_for_you =
            i32::from(self.remembered_delete_message_only_for_you);
        let mut translate_chat_enabled = i32::from(self.translate_chat_enabled);
        let mut translate_to_raw = self.translate_to_raw;
        let mut hide_chat_name = i32::from(self.window_title_content.hide_chat_name);
        let mut hide_account_name = i32::from(self.window_title_content.hide_account_name);
        let mut hide_total_unread = i32::from(self.window_title_content.hide_total_unread);
        let mut media_view_position = Vec::new();
        let mut ignore_battery_saving = i32::from(self.ignore_battery_saving);
        let mut mac_round_icon_digest = self.mac_round_icon_digest.unwrap_or(0);
        let mut stories_click_tooltip_hidden = i32::from(self.stories_click_tooltip_hidden);
        let mut recent_emoji_skip = BTreeSet::new();
        let mut ttl_voice_click_tooltip_hidden = i32::from(self.ttl_voice_click_tooltip_hidden);
        let mut iv_position = Vec::new();
        let mut call_panel_position = Vec::new();
        let mut custom_font_family = self.custom_font_family.clone();
        let mut system_unlock_enabled = i32::from(self.system_unlock_enabled);
        let mut weather_in_celsius = match self.weather_in_celsius {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        };
        let mut tonsite_storage_token = self.tonsite_storage_token.clone();
        let mut iv_zoom = self.iv_zoom;
        let mut skip_toasts_in_focus = i32::from(self.skip_toasts_in_focus);
        let mut record_video_messages = i32::from(self.record_video_messages);
        let mut video_quality = self.video_quality.to_u32();
        let mut chat_filters_horizontal = u32::from(self.chat_filters_horizontal);
        let mut quick_dialog_action = self.quick_dialog_action as u32;
        let mut notifications_volume = self.notifications_volume;
        let mut system_accent_color_enabled = i32::from(self.system_accent_color_enabled);
        let mut use_platform_translation = i32::from(self.use_platform_translation);
        let mut system_text_replace = i32::from(self.system_text_replace);
        let mut prefs_parsed: Option<BTreeMap<Vec<u8>, Vec<u8>>> = None;
        let mut media_grid_zoom_step = self.media_grid_zoom_step;
        let mut language_codes_format = 0i32;
        let mut skip_translation_language_codes = Vec::new();
        let mut translate_to_code = String::new();

        if !r.at_end() {
            adaptive_for_wide = r.read_i32()?;
            moderate_mode_enabled = r.read_i32()?;
            song_volume = r.read_i32()?;
            video_volume = r.read_i32()?;
            ask_download_path = r.read_i32()?;
            download_path = r.read_qstring_lossy()?;
            download_path_bookmark = r.read_byte_array()?;
            non_default_voice_playback_speed = r.read_i32()?;
            sound_notify = r.read_i32()?;
            desktop_notify = r.read_i32()?;
            flash_bounce_notify = r.read_i32()?;
            notify_view = r.read_i32()?;
            native_notifications = r.read_i32()?;
            notifications_count = r.read_i32()?;
            notifications_corner = r.read_i32()?;
            auto_lock = r.read_i32()?;
            legacy_call_playback_device_id = r.read_qstring_lossy()?;
            legacy_call_capture_device_id = r.read_qstring_lossy()?;
            call_output_volume = r.read_i32()?;
            call_input_volume = r.read_i32()?;
            call_audio_ducking_enabled = r.read_i32()?;
            last_seen_warning_seen = r.read_i32()?;
            let sound_overrides_count = r.read_i32()?;
            if sound_overrides_count < 0 {
                return Err(SettingsError::InvalidCount(sound_overrides_count));
            }
            for _ in 0..sound_overrides_count {
                let key = r.read_qstring_lossy()?;
                let value = r.read_qstring_lossy()?;
                sound_overrides.insert(key, value);
            }
            send_files_way = r.read_i32()?;
            send_submit_way = r.read_i32()?;
            include_muted_counter = r.read_i32()?;
            count_unread_messages = r.read_i32()?;
            let _legacy_exe_launch_warning = r.read_i32()?;
            notify_about_pinned = r.read_i32()?;
            loop_animated_stickers = r.read_i32()?;
            large_emoji = r.read_i32()?;
            replace_emoji = r.read_i32()?;
            suggest_emoji = r.read_i32()?;
            suggest_stickers_by_emoji = r.read_i32()?;
            spellchecker_enabled = r.read_i32()?;
            video_playback_speed = r.read_i32()?;
            video_pip_geometry = r.read_byte_array()?;
            let dictionaries_count = r.read_i32()?;
            if dictionaries_count < 0 {
                return Err(SettingsError::InvalidCount(dictionaries_count));
            }
            dictionaries_enabled.reserve(dictionaries_count as usize);
            for _ in 0..dictionaries_count {
                let lang_id = r.read_i64()?;
                dictionaries_enabled.push(lang_id as i32);
            }
            auto_download_dictionaries = r.read_i32()?;
            main_menu_accounts_shown = r.read_i32()?;
        }

        if !r.at_end() {
            tabbed_selector_section_enabled = r.read_i32()?;
            float_player_column = r.read_i32()?;
            float_player_corner = r.read_i32()?;
            third_section_info_enabled = r.read_i32()?;
            let ratio_int = r.read_i32()?;
            third_column_width = r.read_i32()?;
            third_section_extended_by = r.read_i32()?;
            notify_from_all = r.read_i32()?;
            dialogs_with_chat_width_ratio = (f64::from(ratio_int) / 1_000_000.0).clamp(0.0, 1.0);
        }
        if !r.at_end() {
            native_window_frame = r.read_i32()?;
        }
        if !r.at_end() {
            // Legacy system dark mode; overwritten below if newer field exists.
            system_dark_mode_enabled = r.read_i32()?;
        }
        if !r.at_end() {
            camera_device_id = r.read_qstring_lossy()?;
        }
        if !r.at_end() {
            ip_reveal_warning = r.read_i32()?;
        }
        if !r.at_end() {
            group_call_push_to_talk = r.read_i32()?;
            group_call_push_to_talk_shortcut = r.read_byte_array()?;
            group_call_push_to_talk_delay = r.read_i64()?;
        }
        if !r.at_end() {
            let _legacy_call_audio_backend = r.read_i32()?;
        }
        if !r.at_end() {
            disable_calls_legacy = r.read_i32()?;
        }
        if !r.at_end() {
            window_position = r.read_byte_array()?;
        }
        if !r.at_end() {
            let recent_count = r.read_i32()?;
            if recent_count > 0 && recent_count < 10_000 {
                recent_emoji_preload.reserve(recent_count as usize);
                for _ in 0..recent_count {
                    let id = r.read_qstring_lossy()?;
                    let rating = r.read_u16()?;
                    recent_emoji_preload.push(RecentEmojiPreload { id, rating });
                }
            }
            let variants_count = r.read_i32()?;
            if variants_count > 0 && variants_count < 10_000 {
                for _ in 0..variants_count {
                    let id = r.read_qstring_lossy()?;
                    let variant = r.read_u8()?;
                    emoji_variants.insert(id, variant);
                }
            }
        }
        if !r.at_end() {
            let _disable_opengl_old = r.read_i32()?;
        }
        if !r.at_end() {
            let _group_call_noise_suppression_old = r.read_i32()?;
        }
        if !r.at_end() {
            work_mode = r.read_i32()?;
        }
        if !r.at_end() {
            proxy_bytes = r.read_byte_array()?;
        }
        if !r.at_end() {
            hidden_group_call_tooltips = r.read_i32()?;
        }
        if !r.at_end() {
            disable_opengl = r.read_i32()?;
        }
        if !r.at_end() {
            photo_editor_brush = r.read_byte_array()?;
        }
        if !r.at_end() {
            group_call_noise_suppression = r.read_i32()?;
        }
        if !r.at_end() {
            voice_playback_speed = r.read_i32()?;
        }
        if !r.at_end() {
            close_behavior = r.read_i32()?;
        }
        if !r.at_end() {
            custom_device_model = r.read_qstring_lossy()?;
        }
        if !r.at_end() {
            player_repeat_mode = r.read_i32()?;
            player_order_mode = r.read_i32()?;
        }
        if !r.at_end() {
            mac_warn_before_quit = r.read_i32()?;
        }
        if !r.at_end() {
            let count = r.read_i32()?;
            if count < 0 {
                return Err(SettingsError::InvalidCount(count));
            }
            accounts_order.reserve(count as usize);
            for _ in 0..count {
                accounts_order.push(r.read_u64()?);
            }
        }
        if !r.at_end() {
            let _legacy_hw_accel = r.read_i32()?;
        }
        if !r.at_end() {
            chat_quick_action = r.read_i32()?;
        }
        if !r.at_end() {
            hardware_accelerated_video = r.read_i32()?;
        }
        if !r.at_end() {
            suggest_animated_emoji = r.read_i32()?;
        }
        if !r.at_end() {
            corner_reaction = r.read_i32()?;
        }
        if !r.at_end() {
            legacy_skip_translation_for_language = r.read_i32()?;
        }
        if !r.at_end() {
            let count = r.read_i32()?;
            if count < 0 {
                return Err(SettingsError::InvalidCount(count));
            }
            skip_translation_languages_raw.reserve(count as usize);
            for _ in 0..count {
                skip_translation_languages_raw.push(r.read_u64()?);
            }
        }
        if !r.at_end() {
            remembered_delete_message_only_for_you = r.read_i32()?;
        }
        if !r.at_end() {
            translate_chat_enabled = r.read_i32()?;
            translate_to_raw = r.read_u64()?;
        }
        if !r.at_end() {
            hide_chat_name = r.read_i32()?;
            hide_account_name = r.read_i32()?;
            hide_total_unread = r.read_i32()?;
        }
        if !r.at_end() {
            media_view_position = r.read_byte_array()?;
        }
        if !r.at_end() {
            ignore_battery_saving = r.read_i32()?;
        }
        if !r.at_end() {
            mac_round_icon_digest = r.read_u64()?;
        }
        if !r.at_end() {
            stories_click_tooltip_hidden = r.read_i32()?;
        }
        if !r.at_end() {
            let count = r.read_i32()?;
            if count < 0 {
                return Err(SettingsError::InvalidCount(count));
            }
            for _ in 0..count {
                recent_emoji_skip.insert(r.read_qstring_lossy()?);
            }
        }
        let tray_icon_monochrome = if !r.at_end() {
            r.read_i32()?
        } else {
            // Match core_settings.cpp:1019: existing older clients default to false.
            0
        };
        if !r.at_end() {
            ttl_voice_click_tooltip_hidden = r.read_i32()?;
        }
        if !r.at_end() {
            playback_device_id = r.read_qstring_lossy()?;
            capture_device_id = r.read_qstring_lossy()?;
        }
        let (call_playback_device_id, call_capture_device_id) = if !r.at_end() {
            (r.read_qstring_lossy()?, r.read_qstring_lossy()?)
        } else {
            (
                if legacy_call_playback_device_id == "default" {
                    String::new()
                } else {
                    legacy_call_playback_device_id
                },
                if legacy_call_capture_device_id == "default" {
                    String::new()
                } else {
                    legacy_call_capture_device_id
                },
            )
        };
        if !r.at_end() {
            iv_position = r.read_byte_array()?;
        }
        if !r.at_end() {
            no_warning_extensions = Some(r.read_qstring_lossy()?);
        }
        if !r.at_end() {
            custom_font_family = r.read_qstring_lossy()?;
        }
        if !r.at_end() {
            let ratio_int = r.read_i32()?;
            dialogs_no_chat_width_ratio = (f64::from(ratio_int) / 1_000_000.0).clamp(0.0, 1.0);
        }
        if !r.at_end() {
            system_unlock_enabled = r.read_i32()?;
        }
        if !r.at_end() {
            weather_in_celsius = r.read_i32()?;
        }
        if !r.at_end() {
            tonsite_storage_token = r.read_byte_array()?;
        }
        if !r.at_end() {
            include_muted_counter_folders = r.read_i32()?;
        }
        if !r.at_end() {
            chat_filters_horizontal = r.read_u32()?;
        }
        if !r.at_end() {
            skip_toasts_in_focus = r.read_i32()?;
        }
        if !r.at_end() {
            record_video_messages = r.read_i32()?;
        }
        if !r.at_end() {
            video_quality = r.read_u32()?;
        }
        if !r.at_end() {
            iv_zoom = r.read_i32()?;
        }
        if !r.at_end() {
            system_dark_mode_enabled = r.read_i32()?;
        }
        if !r.at_end() {
            quick_dialog_action = r.read_u32()?;
        }
        if !r.at_end() {
            notifications_volume = r.read_u16()?;
        }
        if !r.at_end() {
            notifications_display_checksum = r.read_i32()?;
        }
        if !r.at_end() {
            call_panel_position = r.read_byte_array()?;
        }
        if !r.at_end() {
            corner_reply = r.read_i32()?;
        }
        if !r.at_end() {
            system_accent_color_enabled = r.read_i32()?;
        }
        if !r.at_end() {
            use_platform_translation = r.read_i32()?;
        }
        if !r.at_end() {
            system_text_replace = r.read_i32()?;
        }
        if !r.at_end() {
            let prefs_count = r.read_u32()?;
            let mut parsed = BTreeMap::new();
            for _ in 0..prefs_count {
                let key = r.read_byte_array()?;
                let value = r.read_byte_array()?;
                parsed.insert(key, value);
            }
            prefs_parsed = Some(parsed);
        }
        if !r.at_end() {
            audio_playback_speed = Some(r.read_i32()?);
        }
        if !r.at_end() {
            media_grid_zoom_step = r.read_i32()?;
        }
        if !r.at_end() {
            pull_to_next_channel = r.read_i32()?;
        }
        if !r.at_end() {
            chat_filters_tabs_mode = r.read_i32()?;
        }
        if !r.at_end() {
            // Tail block (core_settings.cpp:1152-1182): parsed defensively, ignored if unknown format.
            let tail = r.read_rest();
            let mut tr = QDataStreamReader::new(tail);
            if let Ok(format) = tr.read_i32()
                && format == LANGUAGE_CODES_FORMAT
                && let Ok(count) = tr.read_i32()
                && (0..10_000).contains(&count)
            {
                let mut codes = Vec::with_capacity(count as usize);
                let mut ok = true;
                for _ in 0..count {
                    match tr.read_qstring_lossy() {
                        Ok(code) => codes.push(code),
                        Err(_) => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok && let Ok(target) = tr.read_qstring_lossy() {
                    skip_translation_language_codes = codes;
                    translate_to_code = target;
                    language_codes_format = format;
                }
            }
        }

        // Validate sub-blobs before mutating `self` (transactional like tdesktop).
        let mut new_accent_colors = self.themes_accent_colors.clone();
        new_accent_colors.set_from_serialized(&themes_accent_colors)?;

        let mut new_proxy = self.proxy.clone();
        new_proxy.set_from_serialized(&proxy_bytes)?;

        let parsed_window_pos = if window_position.is_empty() {
            None
        } else {
            Some(WindowPosition::deserialize(&window_position)?)
        };
        let parsed_media_view_pos = if media_view_position.is_empty() {
            None
        } else {
            let mut pos = WindowPosition::deserialize(&media_view_position)?;
            if pos.w == 0 && pos.maximized == 0 {
                pos = WindowPosition {
                    maximized: 2,
                    ..WindowPosition::default()
                };
            }
            Some(pos)
        };
        let parsed_iv_pos = if iv_position.is_empty() {
            None
        } else {
            Some(WindowPosition::deserialize(&iv_position)?)
        };
        let parsed_call_panel_pos = if call_panel_position.is_empty() {
            None
        } else {
            Some(WindowPosition::deserialize(&call_panel_position)?)
        };

        // Commit all parsed values to `self`.
        self.themes_accent_colors = new_accent_colors;
        self.proxy = new_proxy;
        self.adaptive_for_wide = adaptive_for_wide == 1;
        self.moderate_mode_enabled = moderate_mode_enabled == 1;
        self.song_volume = (f64::from(song_volume) / 1e6).clamp(0.0, 1.0);
        self.video_volume = (f64::from(video_volume) / 1e6).clamp(0.0, 1.0);
        self.ask_download_path = ask_download_path == 1;
        self.download_path = download_path;
        self.download_path_bookmark = download_path_bookmark;
        self.sound_notify = sound_notify == 1;
        self.desktop_notify = desktop_notify == 1;
        self.flash_bounce_notify = flash_bounce_notify == 1;
        if let Some(v) = NotifyView::from_i32(notify_view) {
            self.notify_view = v;
        }
        match native_notifications {
            0 => self.native_notifications = None,
            1 => self.native_notifications = Some(true),
            2 => self.native_notifications = Some(false),
            _ => {}
        }
        self.notifications_count = if notifications_count > 0 {
            notifications_count
        } else {
            3
        };
        if let Some(c) = ScreenCorner::from_i32(notifications_corner) {
            self.notifications_corner = c;
        }
        self.notifications_display_checksum = notifications_display_checksum;
        self.system_accent_color_enabled = system_accent_color_enabled == 1;
        self.use_platform_translation = use_platform_translation == 1;
        self.include_muted_counter = include_muted_counter == 1;
        self.include_muted_counter_folders = include_muted_counter_folders == 1;
        self.count_unread_messages = count_unread_messages == 1;
        self.notify_about_pinned = notify_about_pinned == 1;
        self.auto_lock = auto_lock;
        self.playback_device_id = playback_device_id;
        self.capture_device_id = capture_device_id;
        self.camera_device_id = camera_device_id;
        self.call_playback_device_id = call_playback_device_id;
        self.call_capture_device_id = call_capture_device_id;
        self.call_output_volume = call_output_volume;
        self.call_input_volume = call_input_volume;
        self.call_audio_ducking_enabled = call_audio_ducking_enabled == 1;
        self.last_seen_warning_seen = last_seen_warning_seen == 1;
        self.sound_overrides = sound_overrides;
        if let Some(way) = SendFilesWay::from_serialized(send_files_way) {
            self.send_files_way = way;
        }
        match send_submit_way {
            0 => self.send_submit_way = InputSubmitSettings::Enter,
            1 => self.send_submit_way = InputSubmitSettings::CtrlEnter,
            _ => {}
        }
        if let Some(ext_str) = no_warning_extensions {
            let truncated: String = ext_str.chars().take(10_240).collect();
            self.no_warning_extensions = truncated
                .split(' ')
                .filter(|s| !s.is_empty())
                .take(1024)
                .map(ToOwned::to_owned)
                .collect();
        }
        self.ip_reveal_warning = ip_reveal_warning == 1;
        self.loop_animated_stickers = loop_animated_stickers == 1;
        self.large_emoji = large_emoji == 1;
        self.replace_emoji = replace_emoji == 1;
        self.system_text_replace = system_text_replace == 1;
        self.suggest_emoji = suggest_emoji == 1;
        self.suggest_stickers_by_emoji = suggest_stickers_by_emoji == 1;
        self.spellchecker_enabled = spellchecker_enabled == 1;
        self.video_playback_speed = PlaybackSpeed::deserialize(video_playback_speed);
        let mut voice_speed = PlaybackSpeed::deserialize(voice_playback_speed);
        if non_default_voice_playback_speed != 1 {
            voice_speed.enabled = false;
        }
        self.voice_playback_speed = voice_speed;
        self.audio_playback_speed = audio_playback_speed
            .map(PlaybackSpeed::deserialize)
            .unwrap_or(voice_speed);
        self.video_pip_geometry = video_pip_geometry;
        self.dictionaries_enabled = dictionaries_enabled;
        self.auto_download_dictionaries = auto_download_dictionaries == 1;
        self.main_menu_accounts_shown = main_menu_accounts_shown == 1;
        self.tabbed_selector_section_enabled = tabbed_selector_section_enabled == 1;
        if let Some(col) = WindowColumn::from_i32(float_player_column) {
            self.float_player_column = col;
        }
        if let Some(corner) = RectPartCorner::from_i32(float_player_corner) {
            self.float_player_corner = corner;
        }
        self.third_section_info_enabled = third_section_info_enabled != 0;
        self.dialogs_with_chat_width_ratio = dialogs_with_chat_width_ratio;
        self.dialogs_no_chat_width_ratio = if dialogs_with_chat_width_ratio > 0.0 {
            dialogs_with_chat_width_ratio
        } else {
            dialogs_no_chat_width_ratio
        };
        self.third_column_width = third_column_width;
        self.third_section_extended_by = third_section_extended_by;
        if self.third_section_info_enabled {
            self.tabbed_selector_section_enabled = false;
        }
        self.notify_from_all = notify_from_all == 1;
        self.native_window_frame = native_window_frame == 1;
        self.system_dark_mode_enabled = system_dark_mode_enabled == 1;
        self.group_call_push_to_talk = group_call_push_to_talk == 1;
        self.group_call_push_to_talk_shortcut = group_call_push_to_talk_shortcut;
        self.group_call_push_to_talk_delay = group_call_push_to_talk_delay;
        self.disable_calls_legacy = disable_calls_legacy == 1;
        if let Some(pos) = parsed_window_pos {
            self.window_position = pos;
        }
        self.recent_emoji_preload = recent_emoji_preload;
        self.emoji_variants = emoji_variants;
        self.disable_opengl = disable_opengl == 1;
        self.group_call_noise_suppression = group_call_noise_suppression == 1;
        if let Some(mode) = WorkMode::from_i32(work_mode) {
            self.work_mode = mode;
        }
        let mut tooltips = 0u32;
        if (hidden_group_call_tooltips as u32 & group_call_tooltip::CAMERA) != 0 {
            tooltips |= group_call_tooltip::CAMERA;
        }
        if (hidden_group_call_tooltips as u32 & group_call_tooltip::MICROPHONE) != 0 {
            tooltips |= group_call_tooltip::MICROPHONE;
        }
        self.hidden_group_call_tooltips = tooltips;
        self.photo_editor_brush = photo_editor_brush;
        if let Some(cb) = CloseBehavior::from_i32(close_behavior) {
            self.close_behavior = cb;
        }
        self.custom_device_model = custom_device_model;
        self.accounts_order = accounts_order;
        if let Some(rm) = RepeatMode::from_i32(player_repeat_mode) {
            self.player_repeat_mode = rm;
        }
        if let Some(om) = OrderMode::from_i32(player_order_mode) {
            self.player_order_mode = om;
        }
        self.mac_warn_before_quit = mac_warn_before_quit == 1;
        self.hardware_accelerated_video = hardware_accelerated_video == 1;
        if let Some(qa) = DoubleClickQuickAction::from_i32(chat_quick_action) {
            self.chat_quick_action = qa;
        }
        self.suggest_animated_emoji = suggest_animated_emoji == 1;
        self.corner_reply = corner_reply == 1;
        self.corner_reaction = corner_reaction == 1;
        self.pull_to_next_channel = pull_to_next_channel == 1;
        if let Some(mode) = ChatsFiltersTabsMode::from_i32(chat_filters_tabs_mode) {
            self.chat_filters_tabs_mode = mode;
        }
        if legacy_skip_translation_for_language == 0 {
            self.translate_button_enabled = false;
        } else if legacy_skip_translation_for_language == 1 {
            self.translate_button_enabled = true;
        } else {
            self.translate_button_enabled = legacy_skip_translation_for_language > 0;
            skip_translation_languages_raw
                .push(legacy_skip_translation_for_language.unsigned_abs() as u64);
        }
        self.skip_translation_languages_raw = skip_translation_languages_raw;
        self.remembered_delete_message_only_for_you = remembered_delete_message_only_for_you == 1;
        self.translate_chat_enabled = translate_chat_enabled == 1;
        self.translate_to_raw = translate_to_raw;
        if language_codes_format == LANGUAGE_CODES_FORMAT {
            self.skip_translation_language_codes = skip_translation_language_codes;
            self.translate_to_code = translate_to_code;
        }
        self.window_title_content = WindowTitleContent {
            hide_chat_name: hide_chat_name == 1,
            hide_account_name: hide_account_name == 1,
            hide_total_unread: hide_total_unread == 1,
        };
        if let Some(pos) = parsed_media_view_pos {
            self.media_view_position = pos;
        }
        self.ignore_battery_saving = ignore_battery_saving == 1;
        self.mac_round_icon_digest = if mac_round_icon_digest != 0 {
            Some(mac_round_icon_digest)
        } else {
            None
        };
        self.stories_click_tooltip_hidden = stories_click_tooltip_hidden == 1;
        self.recent_emoji_skip = recent_emoji_skip;
        self.tray_icon_monochrome = tray_icon_monochrome == 1;
        self.ttl_voice_click_tooltip_hidden = ttl_voice_click_tooltip_hidden == 1;
        if let Some(pos) = parsed_iv_pos {
            self.iv_position = pos;
        }
        if let Some(pos) = parsed_call_panel_pos {
            self.call_panel_position = pos;
        }
        self.custom_font_family = custom_font_family;
        self.system_unlock_enabled = system_unlock_enabled == 1;
        self.weather_in_celsius = match weather_in_celsius {
            0 => None,
            1 => Some(true),
            _ => Some(false),
        };
        self.tonsite_storage_token = tonsite_storage_token;
        self.iv_zoom = iv_zoom;
        self.skip_toasts_in_focus = skip_toasts_in_focus == 1;
        self.record_video_messages = record_video_messages == 1;
        self.video_quality = VideoQuality::from_u32(video_quality);
        self.chat_filters_horizontal = chat_filters_horizontal == 1;
        if let Some(qda) = QuickDialogAction::from_i32(quick_dialog_action as i32) {
            self.quick_dialog_action = qda;
        }
        self.notifications_volume = notifications_volume;
        if let Some(mut prefs) = prefs_parsed {
            if let Some(ps_bytes) = prefs.remove(PREF_KEY_POWER_SAVING)
                && ps_bytes.len() >= 4
            {
                let bits = u32::from_be_bytes([ps_bytes[0], ps_bytes[1], ps_bytes[2], ps_bytes[3]]);
                self.power_saving = PowerSavingFlags::from_bits_truncate(bits);
                self.power_saving_force_all = ps_bytes.get(4).copied().unwrap_or(0) != 0;
            }
            if let Some(ap_bytes) = prefs.remove(PREF_KEY_AUTOPLAY)
                && let Some(&bits) = ap_bytes.first()
            {
                self.autoplay = AutoplaySettings::from_u8(bits);
            }
            if let Some(exp_bytes) = prefs.remove(PREF_KEY_EXPERIMENTAL_OPTIONS) {
                let mut er = QDataStreamReader::new(&exp_bytes);
                if let Ok(count) = er.read_u32() {
                    let count = (count as usize).min(256);
                    let mut parsed = BTreeMap::new();
                    for _ in 0..count {
                        let Ok(key_bytes) = er.read_byte_array() else {
                            break;
                        };
                        let Ok(val_byte) = er.read_u8() else {
                            break;
                        };
                        if let Ok(key) = String::from_utf8(key_bytes) {
                            parsed.insert(key, val_byte != 0);
                        }
                    }
                    self.experimental_options = parsed;
                }
            }
            if let Some(rs_bytes) = prefs.remove(PREF_KEY_RECENT_SETTINGS_SEARCHES) {
                let mut rr = QDataStreamReader::new(&rs_bytes);
                if let Ok(count) = rr.read_u32() {
                    let count = (count as usize).min(32);
                    let mut parsed = Vec::with_capacity(count);
                    for _ in 0..count {
                        let Ok(entry_bytes) = rr.read_byte_array() else {
                            break;
                        };
                        if let Ok(entry) = String::from_utf8(entry_bytes)
                            && !entry.is_empty()
                        {
                            parsed.push(entry);
                        }
                    }
                    self.recent_settings_searches = parsed;
                }
            }
            self.prefs = prefs;
        }
        self.media_grid_zoom_step = media_grid_zoom_step;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_proxy::{ProxyData, ProxySettingsMode, ProxyType};
    use crate::version::TDESKTOP_COMPAT_VERSION;

    #[test]
    fn full_settings_round_trip() {
        let mut s = CoreSettings::new();
        s.themes_accent_colors.set(
            EmbeddedThemeType::Night,
            QtColor::new(0x10, 0x20, 0x30, 0xFF),
        );
        s.adaptive_for_wide = false;
        s.moderate_mode_enabled = true;
        s.song_volume = 0.75;
        s.video_volume = 0.42;
        s.ask_download_path = true;
        s.download_path = "D:/Downloads/Rustgram".to_owned();
        s.download_path_bookmark = vec![1, 2, 3, 4];
        s.sound_notify = false;
        s.desktop_notify = true;
        s.flash_bounce_notify = false;
        s.notify_view = NotifyView::ShowName;
        s.native_notifications = Some(false);
        s.notifications_count = 5;
        s.notifications_corner = ScreenCorner::TopLeft;
        s.auto_lock = 600;
        s.call_output_volume = 85;
        s.call_input_volume = 90;
        s.call_audio_ducking_enabled = false;
        s.last_seen_warning_seen = true;
        s.sound_overrides
            .insert("msg_incoming".to_owned(), "C:/sounds/ding.mp3".to_owned());
        s.send_files_way = SendFilesWay {
            group_files: false,
            send_images_as_photos: true,
            send_large_photos: true,
        };
        s.send_submit_way = InputSubmitSettings::CtrlEnter;
        s.include_muted_counter = false;
        s.count_unread_messages = false;
        s.notify_about_pinned = false;
        s.loop_animated_stickers = false;
        s.large_emoji = false;
        s.replace_emoji = false;
        s.suggest_emoji = false;
        s.suggest_stickers_by_emoji = false;
        s.spellchecker_enabled = false;
        s.video_playback_speed = PlaybackSpeed {
            value: 1.5,
            enabled: true,
        };
        s.voice_playback_speed = PlaybackSpeed {
            value: 2.0,
            enabled: true,
        };
        s.audio_playback_speed = PlaybackSpeed {
            value: 1.25,
            enabled: true,
        };
        s.video_pip_geometry = vec![0xDE, 0xAD];
        s.dictionaries_enabled = vec![31, 96];
        s.auto_download_dictionaries = false;
        s.main_menu_accounts_shown = false;
        s.tabbed_selector_section_enabled = true;
        s.third_section_info_enabled = false;
        s.float_player_column = WindowColumn::Third;
        s.float_player_corner = RectPartCorner::BottomLeft;
        s.dialogs_with_chat_width_ratio = 0.35;
        s.dialogs_no_chat_width_ratio = 0.35;
        s.third_column_width = 320;
        s.third_section_extended_by = 48;
        s.notify_from_all = false;
        s.native_window_frame = true;
        s.camera_device_id = "cam-0".to_owned();
        s.ip_reveal_warning = false;
        s.group_call_push_to_talk = true;
        s.group_call_push_to_talk_shortcut = vec![0x20];
        s.group_call_push_to_talk_delay = 150;
        s.window_position = WindowPosition {
            moncrc: 12345,
            maximized: 1,
            scale: 125,
            x: 100,
            y: 200,
            w: 1280,
            h: 800,
        };
        s.recent_emoji_preload.push(RecentEmojiPreload {
            id: "5368324170671202286-0".to_owned(),
            rating: 42,
        });
        s.emoji_variants.insert("👍".to_owned(), 2);
        s.work_mode = WorkMode::TrayOnly;
        s.proxy.set_settings(ProxySettingsMode::Enabled);
        s.proxy.set_try_ipv6(true);
        s.proxy.set_use_proxy_for_calls(true);
        let p = ProxyData {
            proxy_type: ProxyType::Socks5,
            host: "127.0.0.1".to_owned(),
            port: 9050,
            user: String::new(),
            password: String::new(),
        };
        s.proxy.set_selected(p.clone());
        s.proxy.add_to_list(p);
        s.hidden_group_call_tooltips = group_call_tooltip::CAMERA | group_call_tooltip::MICROPHONE;
        s.disable_opengl = true;
        s.photo_editor_brush = vec![7, 8, 9];
        s.group_call_noise_suppression = true;
        s.close_behavior = CloseBehavior::RunInBackground;
        s.custom_device_model = "Rustgram Workstation".to_owned();
        s.player_repeat_mode = RepeatMode::All;
        s.player_order_mode = OrderMode::Shuffle;
        s.mac_warn_before_quit = false;
        s.accounts_order = vec![111, 222, 333];
        s.chat_quick_action = DoubleClickQuickAction::React;
        s.hardware_accelerated_video = true;
        s.suggest_animated_emoji = false;
        s.corner_reaction = false;
        s.translate_button_enabled = true;
        s.skip_translation_languages_raw = vec![31, 96];
        s.remembered_delete_message_only_for_you = true;
        s.translate_chat_enabled = false;
        s.translate_to_raw = 31;
        s.window_title_content = WindowTitleContent {
            hide_chat_name: true,
            hide_account_name: false,
            hide_total_unread: true,
        };
        s.media_view_position = WindowPosition {
            moncrc: 99,
            maximized: 0,
            scale: 100,
            x: 10,
            y: 20,
            w: 900,
            h: 700,
        };
        s.ignore_battery_saving = true;
        s.mac_round_icon_digest = Some(0xAABB_CCDD_EEFF_0011);
        s.stories_click_tooltip_hidden = true;
        s.recent_emoji_skip.insert("😂".to_owned());
        s.tray_icon_monochrome = false;
        s.ttl_voice_click_tooltip_hidden = true;
        s.playback_device_id = "out-1".to_owned();
        s.capture_device_id = "in-1".to_owned();
        s.call_playback_device_id = "call-out-1".to_owned();
        s.call_capture_device_id = "call-in-1".to_owned();
        s.iv_position = WindowPosition {
            x: 50,
            y: 60,
            w: 640,
            h: 480,
            moncrc: 1,
            maximized: 0,
            scale: 100,
        };
        s.no_warning_extensions.insert("pdf".to_owned());
        s.no_warning_extensions.insert("txt".to_owned());
        s.custom_font_family = "Inter".to_owned();
        s.system_unlock_enabled = true;
        s.weather_in_celsius = Some(true);
        s.tonsite_storage_token = vec![0xAA, 0xBB];
        s.include_muted_counter_folders = false;
        s.chat_filters_horizontal = true;
        s.skip_toasts_in_focus = true;
        s.record_video_messages = true;
        s.video_quality = VideoQuality {
            manual: true,
            height: 1080,
            original: false,
        };
        s.iv_zoom = 125;
        s.system_dark_mode_enabled = false;
        s.quick_dialog_action = QuickDialogAction::Archive;
        s.notifications_volume = 65;
        s.notifications_display_checksum = 777;
        s.call_panel_position = WindowPosition {
            x: 15,
            y: 25,
            w: 400,
            h: 300,
            moncrc: 2,
            maximized: 0,
            scale: 100,
        };
        s.corner_reply = false;
        s.system_accent_color_enabled = true;
        s.use_platform_translation = true;
        s.system_text_replace = false;
        s.write_pref("custom_flag", b"yes".to_vec());
        s.media_grid_zoom_step = 2;
        s.pull_to_next_channel = false;
        s.chat_filters_tabs_mode = ChatsFiltersTabsMode::IconsOnly;
        s.skip_translation_language_codes = vec!["en".to_owned(), "uk".to_owned()];
        s.translate_to_code = "en".to_owned();

        let bytes = s.serialize();
        let mut decoded = CoreSettings::new();
        decoded
            .add_from_serialized(&bytes, TDESKTOP_COMPAT_VERSION)
            .unwrap();

        assert_eq!(decoded, s);
        // Re-serializing produces byte-for-byte identical output.
        assert_eq!(decoded.serialize(), bytes);
    }

    #[test]
    fn byte_level_prefix_and_truncated_older_version_migration() {
        // Construct a minimal older-version blob containing only themesAccentColors + Block 1
        // (up to mainMenuAccountsShown), verifying exact big-endian field ordering and migration
        // defaults for newer fields.
        let mut w = QDataStreamWriter::new();
        w.write_byte_array(&[]); // empty themesAccentColors -> 0xFFFFFFFF
        w.write_i32(0); // adaptive_for_wide = false
        w.write_i32(1); // moderate_mode_enabled = true
        w.write_i32(500_000); // song_volume = 0.5
        w.write_i32(250_000); // video_volume = 0.25
        w.write_i32(1); // ask_download_path = true
        w.write_qstring("C:/dl"); // download_path
        w.write_byte_array(&[]); // download_path_bookmark
        w.write_i32(0); // legacy nonDefaultVoicePlaybackSpeed != 1 -> disables voice speed
        w.write_i32(1); // sound_notify
        w.write_i32(1); // desktop_notify
        w.write_i32(0); // flash_bounce_notify
        w.write_i32(2); // NotifyView::ShowNothing
        w.write_i32(1); // native_notifications = Some(true)
        w.write_i32(4); // notifications_count
        w.write_i32(3); // ScreenCorner::BottomLeft
        w.write_i32(900); // auto_lock
        w.write_qstring("legacy-out"); // legacy call playback device id
        w.write_qstring("default"); // legacy call capture device id -> maps to ""
        w.write_i32(80); // call_output_volume
        w.write_i32(70); // call_input_volume
        w.write_i32(1); // call_audio_ducking_enabled
        w.write_i32(0); // last_seen_warning_seen
        w.write_i32(0); // sound_overrides count
        w.write_i32(2); // send_files_way: group=false, photos=false, large=false
        w.write_i32(1); // send_submit_way: CtrlEnter
        w.write_i32(0); // include_muted_counter
        w.write_i32(1); // count_unread_messages
        w.write_i32(1); // legacy_exe_launch_warning
        w.write_i32(1); // notify_about_pinned
        w.write_i32(1); // loop_animated_stickers
        w.write_i32(1); // large_emoji
        w.write_i32(1); // replace_emoji
        w.write_i32(1); // suggest_emoji
        w.write_i32(1); // suggest_stickers_by_emoji
        w.write_i32(0); // spellchecker_enabled
        w.write_i32(200); // video_playback_speed = 2.0x enabled
        w.write_byte_array(&[]); // video_pip_geometry
        w.write_i32(0); // dictionaries_enabled count
        w.write_i32(1); // auto_download_dictionaries
        w.write_i32(0); // main_menu_accounts_shown
        let bytes = w.into_bytes();

        // Check exact opening bytes: null QByteArray (FF FF FF FF), adaptive_for_wide=0, moderate=1
        assert_eq!(&bytes[0..4], &[0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(&bytes[4..8], &0i32.to_be_bytes());
        assert_eq!(&bytes[8..12], &1i32.to_be_bytes());

        let mut s = CoreSettings::new();
        s.add_from_serialized(&bytes, 4_000_000).unwrap();

        assert!(!s.adaptive_for_wide);
        assert!(s.moderate_mode_enabled);
        assert!((s.song_volume - 0.5).abs() < 1e-9);
        assert!((s.video_volume - 0.25).abs() < 1e-9);
        assert_eq!(s.download_path, "C:/dl");
        assert_eq!(s.notify_view, NotifyView::ShowNothing);
        assert_eq!(s.native_notifications, Some(true));
        assert_eq!(s.notifications_corner, ScreenCorner::BottomLeft);
        // Legacy call device fallback: "legacy-out" preserved, "default" cleared to ""
        assert_eq!(s.call_playback_device_id, "legacy-out");
        assert_eq!(s.call_capture_device_id, "");
        // Older blobs default tray_icon_monochrome to false
        assert!(!s.tray_icon_monochrome);
        // Audio speed inherits voice speed when absent
        assert_eq!(s.audio_playback_speed, s.voice_playback_speed);
        assert!(!s.voice_playback_speed.enabled);
        assert_eq!(
            s.video_playback_speed,
            PlaybackSpeed {
                value: 2.0,
                enabled: true
            }
        );
    }

    #[test]
    fn playback_speed_and_video_quality_bit_packing() {
        // Legacy speed encoding (0..10): 0->0.5x, 2->1.0x (disabled, default 1.7), 6->2.0x
        assert_eq!(
            PlaybackSpeed::deserialize(0),
            PlaybackSpeed {
                value: 0.5,
                enabled: true
            }
        );
        assert_eq!(
            PlaybackSpeed::deserialize(2),
            PlaybackSpeed {
                value: SPED_UP_DEFAULT,
                enabled: false
            }
        );
        assert_eq!(
            PlaybackSpeed::deserialize(-170),
            PlaybackSpeed {
                value: 1.7,
                enabled: false
            }
        );
        assert_eq!(
            PlaybackSpeed {
                value: 1.7,
                enabled: false
            }
            .serialize(),
            -170
        );

        let q = VideoQuality {
            manual: true,
            height: 720,
            original: false,
        };
        assert_eq!(q.to_u32(), 1 | (720 << 1));
        assert_eq!(VideoQuality::from_u32(q.to_u32()), q);
        // Heights above 4320 reset to zeroed VideoQuality
        let bad = (5000u32 << 1) | 1;
        assert_eq!(VideoQuality::from_u32(bad).height, 0);
    }

    #[test]
    fn language_codes_tail_exact_wire_order_and_reset_on_last_logout() {
        let mut s = CoreSettings::new();
        s.skip_translation_language_codes = vec!["de".to_owned(), "fr".to_owned()];
        s.translate_to_code = "es".to_owned();
        s.chat_filters_tabs_mode = ChatsFiltersTabsMode::TextOnly;
        s.pull_to_next_channel = false;
        s.media_grid_zoom_step = 3;
        s.write_pref_bool(SCREEN_READER_MODE_DISABLED_KEY, true);
        s.write_pref("ephemeral_pref", b"dropme".to_vec());

        let bytes = s.serialize();
        // Verify exact trailing bytes match:
        // [media_grid_zoom_step=3][pull_to_next_channel=0][chat_filters_tabs_mode=3]
        // [format=1][count=2]["de"]["fr"]["es"]
        let mut tail = QDataStreamWriter::new();
        tail.write_i32(3);
        tail.write_i32(0);
        tail.write_i32(3);
        tail.write_i32(LANGUAGE_CODES_FORMAT);
        tail.write_i32(2);
        tail.write_qstring("de");
        tail.write_qstring("fr");
        tail.write_qstring("es");
        let expected_tail = tail.into_bytes();
        assert!(bytes.ends_with(&expected_tail));

        s.reset_on_last_logout();
        assert!(s.read_pref_bool(SCREEN_READER_MODE_DISABLED_KEY, false));
        assert!(s.read_pref("ephemeral_pref").is_none());
        assert_eq!(s.chat_filters_tabs_mode, ChatsFiltersTabsMode::Default);
    }

    #[test]
    fn power_saving_flags_exact_tdesktop_bits_and_force_all_logic() {
        // Exact bit values from Telegram/SourceFiles/ui/power_saving.h
        assert_eq!(PowerSavingFlags::NONE.bits(), 0);
        assert_eq!(PowerSavingFlags::ANIMATIONS.bits(), 1 << 0);
        assert_eq!(PowerSavingFlags::STICKERS_PANEL.bits(), 1 << 1);
        assert_eq!(PowerSavingFlags::STICKERS_CHAT.bits(), 1 << 2);
        assert_eq!(PowerSavingFlags::EMOJI_PANEL.bits(), 1 << 3);
        assert_eq!(PowerSavingFlags::EMOJI_REACTIONS.bits(), 1 << 4);
        assert_eq!(PowerSavingFlags::EMOJI_CHAT.bits(), 1 << 5);
        assert_eq!(PowerSavingFlags::CHAT_BACKGROUND.bits(), 1 << 6);
        assert_eq!(PowerSavingFlags::CHAT_SPOILER.bits(), 1 << 7);
        assert_eq!(PowerSavingFlags::CALLS.bits(), 1 << 8);
        assert_eq!(PowerSavingFlags::EMOJI_STATUS.bits(), 1 << 9);
        assert_eq!(PowerSavingFlags::CHAT_EFFECTS.bits(), 1 << 10);
        assert_eq!(PowerSavingFlags::ALL.bits(), (1 << 11) - 1);

        let combined = PowerSavingFlags::ANIMATIONS
            | PowerSavingFlags::STICKERS_CHAT
            | PowerSavingFlags::CHAT_SPOILER
            | PowerSavingFlags::CALLS;
        assert!(combined.contains(PowerSavingFlags::ANIMATIONS));
        assert!(combined.contains(PowerSavingFlags::STICKERS_CHAT));
        assert!(combined.contains(PowerSavingFlags::CHAT_SPOILER));
        assert!(combined.contains(PowerSavingFlags::CALLS));
        assert!(!combined.contains(PowerSavingFlags::EMOJI_STATUS));

        let mut s = CoreSettings::new();
        assert!(!s.is_power_saving_on(PowerSavingFlags::ANIMATIONS));
        s.power_saving_force_all = true;
        assert!(s.is_power_saving_on(PowerSavingFlags::ANIMATIONS));
        assert!(s.is_power_saving_on(PowerSavingFlags::EMOJI_STATUS));
        // ignore_battery_saving suppresses force_all while preserving explicit flags
        s.ignore_battery_saving = true;
        assert!(!s.is_power_saving_on(PowerSavingFlags::ANIMATIONS));
        s.power_saving.insert(PowerSavingFlags::CHAT_SPOILER);
        assert!(s.is_power_saving_on(PowerSavingFlags::CHAT_SPOILER));
        assert!(!s.is_power_saving_on(PowerSavingFlags::ANIMATIONS));
    }

    #[test]
    fn power_saving_and_autoplay_round_trip_preserves_tdesktop_wire_order() {
        let mut s = CoreSettings::new();
        // Use exact 1e-6 fixed-point ratios so PartialEq holds after wire quantization.
        s.dialogs_with_chat_width_ratio = 0.35;
        s.dialogs_no_chat_width_ratio = 0.35;
        s.power_saving = PowerSavingFlags::STICKERS_PANEL
            | PowerSavingFlags::STICKERS_CHAT
            | PowerSavingFlags::EMOJI_PANEL
            | PowerSavingFlags::EMOJI_REACTIONS
            | PowerSavingFlags::EMOJI_CHAT
            | PowerSavingFlags::EMOJI_STATUS
            | PowerSavingFlags::CHAT_BACKGROUND
            | PowerSavingFlags::CHAT_SPOILER
            | PowerSavingFlags::CALLS;
        s.power_saving_force_all = true;
        s.autoplay = AutoplaySettings {
            gifs: false,
            videos: true,
            round_videos: false,
            video_stickers: false,
        };
        s.skip_translation_language_codes = vec!["en".to_owned()];
        s.translate_to_code = "fr".to_owned();

        let bytes = s.serialize();

        // Trailing language-codes block remains exact at the end of the blob.
        let mut tail = QDataStreamWriter::new();
        tail.write_i32(LANGUAGE_CODES_FORMAT);
        tail.write_i32(1);
        tail.write_qstring("en");
        tail.write_qstring("fr");
        assert!(bytes.ends_with(&tail.into_bytes()));

        let mut decoded = CoreSettings::new();
        decoded
            .add_from_serialized(&bytes, TDESKTOP_COMPAT_VERSION)
            .unwrap();
        assert_eq!(decoded, s);
        assert_eq!(decoded.power_saving, s.power_saving);
        assert!(decoded.power_saving_force_all);
        assert_eq!(decoded.autoplay, s.autoplay);
        // Internal serialization keys do not leak into public prefs map.
        assert!(decoded.prefs.is_empty());
        assert_eq!(decoded.serialize(), bytes);

        // Reset on last logout restores autoplay defaults while retaining device power_saving.
        decoded.reset_on_last_logout();
        assert_eq!(decoded.autoplay, AutoplaySettings::default());
        assert_eq!(decoded.power_saving, s.power_saving);

        // Default CoreSettings serializes with zero internal pref entries.
        let default_bytes = CoreSettings::new().serialize();
        assert!(
            !default_bytes
                .windows(PREF_KEY_POWER_SAVING.len())
                .any(|w| w == PREF_KEY_POWER_SAVING)
        );
        assert!(
            !default_bytes
                .windows(PREF_KEY_AUTOPLAY.len())
                .any(|w| w == PREF_KEY_AUTOPLAY)
        );
    }
}
