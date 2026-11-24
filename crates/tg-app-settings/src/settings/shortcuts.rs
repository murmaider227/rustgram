//! Custom keyboard shortcuts registry, tdesktop `shortcuts-custom.json` persistence,
//! and interactive Shortcuts settings editor.
//!
//! Port of:
//! - `Telegram/SourceFiles/core/shortcuts.{h,cpp}`
//! - `Telegram/SourceFiles/settings/sections/settings_shortcuts.{h,cpp}`

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use egui::{CornerRadius, CursorIcon, Event, Key, Modifiers, Sense, Stroke, StrokeKind, Ui, Vec2};
use serde_json::{Map, Value};
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::st;

use crate::settings::widgets::{settings_card, settings_divider, subsection_title};

/// Maximum number of entries accepted from `shortcuts-custom.json` (`kCountLimit = 256`).
pub const SHORTCUTS_JSON_COUNT_LIMIT: usize = 256;

/// Relative filename under `tdata/` matching tdesktop (`CustomFilePath()`).
pub const CUSTOM_SHORTCUTS_FILENAME: &str = "shortcuts-custom.json";

/// Relative filename under `tdata/` for default reference output (`DefaultFilePath()`).
pub const DEFAULT_SHORTCUTS_FILENAME: &str = "shortcuts-default.json";

/// Shortcut commands matching `Shortcuts::Command` (`core/shortcuts.h:17-98`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ShortcutCommand {
    Close,
    Lock,
    Minimize,
    Quit,
    ReopenClosedWindow,
    CloseOtherWindows,
    MediaPlay,
    MediaPause,
    MediaPlayPause,
    MediaStop,
    MediaPrevious,
    MediaNext,
    Search,
    ChatPrevious,
    ChatNext,
    ChatFirst,
    ChatLast,
    ChatSelf,
    ChatPinned1,
    ChatPinned2,
    ChatPinned3,
    ChatPinned4,
    ChatPinned5,
    ChatPinned6,
    ChatPinned7,
    ChatPinned8,
    ShowAccount1,
    ShowAccount2,
    ShowAccount3,
    ShowAccount4,
    ShowAccount5,
    ShowAccount6,
    ShowAllChats,
    ShowFolder1,
    ShowFolder2,
    ShowFolder3,
    ShowFolder4,
    ShowFolder5,
    ShowFolder6,
    ShowFolderLast,
    FolderNext,
    FolderPrevious,
    ShowScheduled,
    ShowArchive,
    ShowContacts,
    ShowDownloads,
    JustSendMessage,
    SendSilentMessage,
    ScheduleMessage,
    ComposeAiApplyInPlace,
    ShowRichEditor,
    ToggleWebPagePreview,
    StashMessage,
    RecordVoice,
    RecordRound,
    ReadChat,
    ArchiveChat,
    MediaViewerFullscreen,
    ShowChatMenu,
    ShowChatPreview,
    ShowAdminLog,
}

impl ShortcutCommand {
    /// Exact JSON command string used in tdesktop `shortcuts-custom.json` (`CommandByName`).
    #[must_use]
    pub const fn json_name(self) -> &'static str {
        match self {
            Self::Close => "close_telegram",
            Self::Lock => "lock_telegram",
            Self::Minimize => "minimize_telegram",
            Self::Quit => "quit_telegram",
            Self::ReopenClosedWindow => "reopen_closed_window",
            Self::CloseOtherWindows => "close_other_windows",
            Self::MediaPlay => "media_play",
            Self::MediaPause => "media_pause",
            Self::MediaPlayPause => "media_playpause",
            Self::MediaStop => "media_stop",
            Self::MediaPrevious => "media_previous",
            Self::MediaNext => "media_next",
            Self::Search => "search",
            Self::ChatPrevious => "previous_chat",
            Self::ChatNext => "next_chat",
            Self::ChatFirst => "first_chat",
            Self::ChatLast => "last_chat",
            Self::ChatSelf => "self_chat",
            Self::ChatPinned1 => "pinned_chat1",
            Self::ChatPinned2 => "pinned_chat2",
            Self::ChatPinned3 => "pinned_chat3",
            Self::ChatPinned4 => "pinned_chat4",
            Self::ChatPinned5 => "pinned_chat5",
            Self::ChatPinned6 => "pinned_chat6",
            Self::ChatPinned7 => "pinned_chat7",
            Self::ChatPinned8 => "pinned_chat8",
            Self::ShowAccount1 => "account1",
            Self::ShowAccount2 => "account2",
            Self::ShowAccount3 => "account3",
            Self::ShowAccount4 => "account4",
            Self::ShowAccount5 => "account5",
            Self::ShowAccount6 => "account6",
            Self::ShowAllChats => "all_chats",
            Self::ShowFolder1 => "folder1",
            Self::ShowFolder2 => "folder2",
            Self::ShowFolder3 => "folder3",
            Self::ShowFolder4 => "folder4",
            Self::ShowFolder5 => "folder5",
            Self::ShowFolder6 => "folder6",
            Self::ShowFolderLast => "last_folder",
            Self::FolderNext => "next_folder",
            Self::FolderPrevious => "previous_folder",
            Self::ShowScheduled => "show_scheduled",
            Self::ShowArchive => "show_archive",
            Self::ShowContacts => "show_contacts",
            Self::ShowDownloads => "show_downloads",
            Self::JustSendMessage => "message",
            Self::SendSilentMessage => "message_silently",
            Self::ScheduleMessage => "message_scheduled",
            Self::ComposeAiApplyInPlace => "ai_compose_apply",
            Self::ShowRichEditor => "show_rich_editor",
            Self::ToggleWebPagePreview => "toggle_webpage_preview",
            Self::StashMessage => "stash_message",
            Self::RecordVoice => "record_voice",
            Self::RecordRound => "record_round",
            Self::ReadChat => "read_chat",
            Self::ArchiveChat => "archive_chat",
            Self::MediaViewerFullscreen => "media_viewer_video_fullscreen",
            Self::ShowChatMenu => "show_chat_menu",
            Self::ShowChatPreview => "show_chat_preview",
            Self::ShowAdminLog => "show_admin_log",
        }
    }

    /// Parses a tdesktop JSON command name back into [`ShortcutCommand`].
    #[must_use]
    pub fn from_json_name(name: &str) -> Option<Self> {
        ALL_COMMANDS
            .iter()
            .copied()
            .find(|cmd| cmd.json_name() == name)
    }

    /// Human-readable label shown in the Shortcuts settings editor (`Entries()` in
    /// `settings_shortcuts.cpp:44-135`).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Close => "Close window",
            Self::Lock => "Lock app",
            Self::Minimize => "Minimize",
            Self::Quit => "Quit",
            Self::ReopenClosedWindow => "Reopen closed window",
            Self::CloseOtherWindows => "Close other windows",
            Self::MediaPlay => "Media play",
            Self::MediaPause => "Media pause",
            Self::MediaPlayPause => "Media play / pause",
            Self::MediaStop => "Media stop",
            Self::MediaPrevious => "Previous track",
            Self::MediaNext => "Next track",
            Self::Search => "Search",
            Self::ChatPrevious => "Previous chat",
            Self::ChatNext => "Next chat",
            Self::ChatFirst => "First chat",
            Self::ChatLast => "Last chat",
            Self::ChatSelf => "Saved Messages",
            Self::ChatPinned1 => "Pinned chat 1",
            Self::ChatPinned2 => "Pinned chat 2",
            Self::ChatPinned3 => "Pinned chat 3",
            Self::ChatPinned4 => "Pinned chat 4",
            Self::ChatPinned5 => "Pinned chat 5",
            Self::ChatPinned6 => "Pinned chat 6",
            Self::ChatPinned7 => "Pinned chat 7",
            Self::ChatPinned8 => "Pinned chat 8",
            Self::ShowAccount1 => "Account 1",
            Self::ShowAccount2 => "Account 2",
            Self::ShowAccount3 => "Account 3",
            Self::ShowAccount4 => "Account 4",
            Self::ShowAccount5 => "Account 5",
            Self::ShowAccount6 => "Account 6",
            Self::ShowAllChats => "All Chats folder",
            Self::ShowFolder1 => "Folder 1",
            Self::ShowFolder2 => "Folder 2",
            Self::ShowFolder3 => "Folder 3",
            Self::ShowFolder4 => "Folder 4",
            Self::ShowFolder5 => "Folder 5",
            Self::ShowFolder6 => "Folder 6",
            Self::ShowFolderLast => "Last folder",
            Self::FolderNext => "Next folder",
            Self::FolderPrevious => "Previous folder",
            Self::ShowScheduled => "Show scheduled messages",
            Self::ShowArchive => "Show archived chats",
            Self::ShowContacts => "Show contacts",
            Self::ShowDownloads => "Show downloads",
            Self::JustSendMessage => "Send message",
            Self::SendSilentMessage => "Send silently",
            Self::ScheduleMessage => "Schedule message",
            Self::ComposeAiApplyInPlace => "Apply AI compose in place",
            Self::ShowRichEditor => "Open rich text editor",
            Self::ToggleWebPagePreview => "Toggle link preview",
            Self::StashMessage => "Stash draft message",
            Self::RecordVoice => "Record voice message",
            Self::RecordRound => "Record video message",
            Self::ReadChat => "Mark chat as read",
            Self::ArchiveChat => "Archive chat",
            Self::MediaViewerFullscreen => "Toggle media viewer fullscreen",
            Self::ShowChatMenu => "Open chat menu",
            Self::ShowChatPreview => "Show chat preview",
            Self::ShowAdminLog => "Show recent actions (Admin log)",
        }
    }
}

/// Ordered list of all configurable [`ShortcutCommand`] entries.
pub const ALL_COMMANDS: &[ShortcutCommand] = &[
    ShortcutCommand::Close,
    ShortcutCommand::Lock,
    ShortcutCommand::Minimize,
    ShortcutCommand::Quit,
    ShortcutCommand::ReopenClosedWindow,
    ShortcutCommand::CloseOtherWindows,
    ShortcutCommand::Search,
    ShortcutCommand::ChatPrevious,
    ShortcutCommand::ChatNext,
    ShortcutCommand::ChatFirst,
    ShortcutCommand::ChatLast,
    ShortcutCommand::ChatSelf,
    ShortcutCommand::ChatPinned1,
    ShortcutCommand::ChatPinned2,
    ShortcutCommand::ChatPinned3,
    ShortcutCommand::ChatPinned4,
    ShortcutCommand::ChatPinned5,
    ShortcutCommand::ChatPinned6,
    ShortcutCommand::ChatPinned7,
    ShortcutCommand::ChatPinned8,
    ShortcutCommand::ShowAccount1,
    ShortcutCommand::ShowAccount2,
    ShortcutCommand::ShowAccount3,
    ShortcutCommand::ShowAccount4,
    ShortcutCommand::ShowAccount5,
    ShortcutCommand::ShowAccount6,
    ShortcutCommand::ShowAllChats,
    ShortcutCommand::ShowFolder1,
    ShortcutCommand::ShowFolder2,
    ShortcutCommand::ShowFolder3,
    ShortcutCommand::ShowFolder4,
    ShortcutCommand::ShowFolder5,
    ShortcutCommand::ShowFolder6,
    ShortcutCommand::ShowFolderLast,
    ShortcutCommand::FolderNext,
    ShortcutCommand::FolderPrevious,
    ShortcutCommand::ShowArchive,
    ShortcutCommand::ShowContacts,
    ShortcutCommand::ShowDownloads,
    ShortcutCommand::ReadChat,
    ShortcutCommand::ArchiveChat,
    ShortcutCommand::ShowScheduled,
    ShortcutCommand::ShowChatMenu,
    ShortcutCommand::ShowChatPreview,
    ShortcutCommand::JustSendMessage,
    ShortcutCommand::SendSilentMessage,
    ShortcutCommand::ScheduleMessage,
    ShortcutCommand::ComposeAiApplyInPlace,
    ShortcutCommand::ShowRichEditor,
    ShortcutCommand::ToggleWebPagePreview,
    ShortcutCommand::StashMessage,
    ShortcutCommand::RecordVoice,
    ShortcutCommand::RecordRound,
    ShortcutCommand::ShowAdminLog,
    ShortcutCommand::MediaViewerFullscreen,
    ShortcutCommand::MediaPlay,
    ShortcutCommand::MediaPause,
    ShortcutCommand::MediaPlayPause,
    ShortcutCommand::MediaStop,
    ShortcutCommand::MediaPrevious,
    ShortcutCommand::MediaNext,
];

/// Normalized keyboard shortcut chord compatible with Qt/tdesktop `QKeySequence` strings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyChord {
    /// Control modifier (`ctrl`).
    pub ctrl: bool,
    /// Alt modifier (`alt`).
    pub alt: bool,
    /// Shift modifier (`shift`).
    pub shift: bool,
    /// Meta / Win / Command modifier (`meta`).
    pub meta: bool,
    /// Canonical lowercase key token (e.g. `"w"`, `"f4"`, `"pgdown"`, `"down"`, `"]"`, `"media play"`).
    pub key: String,
}

impl KeyChord {
    /// Parses a tdesktop/Qt shortcut string such as `"ctrl+shift+t"`, `"alt+down"`, `"f5"`, or
    /// `"media play"`. Returns `None` if empty or missing a primary key.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let lower = trimmed.to_ascii_lowercase();
        // Multi-word media key names have no '+' separator.
        if matches!(
            lower.as_str(),
            "media play"
                | "media pause"
                | "toggle media play/pause"
                | "media stop"
                | "media previous"
                | "media next"
                | "search"
                | "find"
        ) {
            return Some(Self {
                ctrl: false,
                alt: false,
                shift: false,
                meta: false,
                key: lower,
            });
        }

        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut meta = false;
        let mut primary: Option<String> = None;

        // Handle literal '+' key like "ctrl++" by tracking trailing empty split segments.
        let parts: Vec<&str> = lower.split('+').collect();
        let mut idx = 0;
        while idx < parts.len() {
            let token = parts[idx].trim();
            if token.is_empty() {
                if idx + 1 == parts.len() && idx > 0 {
                    primary = Some("+".to_owned());
                }
                idx += 1;
                continue;
            }
            match token {
                "ctrl" | "control" => ctrl = true,
                "alt" | "opt" | "option" => alt = true,
                "shift" => shift = true,
                "meta" | "cmd" | "command" | "win" | "super" => meta = true,
                other => {
                    primary = Some(normalize_key_token(other));
                }
            }
            idx += 1;
        }

        let key = primary?;
        if key.is_empty() {
            return None;
        }
        Some(Self {
            ctrl,
            alt,
            shift,
            meta,
            key,
        })
    }

    /// Formats the chord in tdesktop's lowercase `shortcuts-custom.json` notation (e.g. `"ctrl+shift+t"`).
    #[must_use]
    pub fn to_json_string(&self) -> String {
        let mut parts = Vec::with_capacity(5);
        if self.ctrl {
            parts.push("ctrl");
        }
        if self.alt {
            parts.push("alt");
        }
        if self.shift {
            parts.push("shift");
        }
        if self.meta {
            parts.push("meta");
        }
        parts.push(&self.key);
        parts.join("+")
    }

    /// Formats the chord for display in the Settings UI (e.g. `"Ctrl+Shift+T"`).
    #[must_use]
    pub fn to_display_string(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(5);
        if self.ctrl {
            parts.push("Ctrl".to_owned());
        }
        if self.alt {
            parts.push("Alt".to_owned());
        }
        if self.shift {
            parts.push("Shift".to_owned());
        }
        if self.meta {
            parts.push("Meta".to_owned());
        }
        parts.push(display_key_token(&self.key));
        parts.join("+")
    }

    /// Builds a [`KeyChord`] from an `egui::Key` + `egui::Modifiers` event.
    #[must_use]
    pub fn from_egui_key(key: Key, modifiers: Modifiers) -> Option<Self> {
        let key_token = egui_key_to_token(key)?;
        Some(Self {
            ctrl: modifiers.ctrl || modifiers.command,
            alt: modifiers.alt,
            shift: modifiers.shift,
            meta: false,
            key: key_token.to_owned(),
        })
    }

    /// Returns `true` when the chord is valid for binding (either has a modifier or is an F-key /
    /// media key allowed without modifiers, mirroring `AllowWithoutModifiers`).
    #[must_use]
    pub fn is_valid_binding(&self) -> bool {
        if self.ctrl || self.alt || self.meta {
            return true;
        }
        if self.key.starts_with('f') && self.key[1..].parse::<u8>().is_ok() {
            return true;
        }
        matches!(
            self.key.as_str(),
            "media play"
                | "media pause"
                | "toggle media play/pause"
                | "media stop"
                | "media previous"
                | "media next"
                | "search"
                | "find"
        )
    }
}

fn normalize_key_token(token: &str) -> String {
    match token {
        "pageup" | "pgup" => "pgup".to_owned(),
        "pagedown" | "pgdown" => "pgdown".to_owned(),
        "arrowup" | "up" => "up".to_owned(),
        "arrowdown" | "down" => "down".to_owned(),
        "arrowleft" | "left" => "left".to_owned(),
        "arrowright" | "right" => "right".to_owned(),
        "del" | "delete" => "delete".to_owned(),
        "ins" | "insert" => "insert".to_owned(),
        "esc" | "escape" => "escape".to_owned(),
        "return" | "enter" => "enter".to_owned(),
        "backslash" => "\\".to_owned(),
        "openbracket" => "[".to_owned(),
        "closebracket" => "]".to_owned(),
        other => other.to_owned(),
    }
}

fn display_key_token(token: &str) -> String {
    match token {
        "pgup" => "PgUp".to_owned(),
        "pgdown" => "PgDown".to_owned(),
        "up" => "Up".to_owned(),
        "down" => "Down".to_owned(),
        "left" => "Left".to_owned(),
        "right" => "Right".to_owned(),
        "home" => "Home".to_owned(),
        "end" => "End".to_owned(),
        "delete" => "Delete".to_owned(),
        "insert" => "Insert".to_owned(),
        "escape" => "Esc".to_owned(),
        "enter" => "Enter".to_owned(),
        "tab" => "Tab".to_owned(),
        "space" => "Space".to_owned(),
        other if other.len() == 1 => other.to_ascii_uppercase(),
        other if other.starts_with('f') && other[1..].parse::<u8>().is_ok() => {
            other.to_ascii_uppercase()
        }
        other => other.to_owned(),
    }
}

fn egui_key_to_token(key: Key) -> Option<&'static str> {
    Some(match key {
        Key::A => "a",
        Key::B => "b",
        Key::C => "c",
        Key::D => "d",
        Key::E => "e",
        Key::F => "f",
        Key::G => "g",
        Key::H => "h",
        Key::I => "i",
        Key::J => "j",
        Key::K => "k",
        Key::L => "l",
        Key::M => "m",
        Key::N => "n",
        Key::O => "o",
        Key::P => "p",
        Key::Q => "q",
        Key::R => "r",
        Key::S => "s",
        Key::T => "t",
        Key::U => "u",
        Key::V => "v",
        Key::W => "w",
        Key::X => "x",
        Key::Y => "y",
        Key::Z => "z",
        Key::Num0 => "0",
        Key::Num1 => "1",
        Key::Num2 => "2",
        Key::Num3 => "3",
        Key::Num4 => "4",
        Key::Num5 => "5",
        Key::Num6 => "6",
        Key::Num7 => "7",
        Key::Num8 => "8",
        Key::Num9 => "9",
        Key::F1 => "f1",
        Key::F2 => "f2",
        Key::F3 => "f3",
        Key::F4 => "f4",
        Key::F5 => "f5",
        Key::F6 => "f6",
        Key::F7 => "f7",
        Key::F8 => "f8",
        Key::F9 => "f9",
        Key::F10 => "f10",
        Key::F11 => "f11",
        Key::F12 => "f12",
        Key::ArrowUp => "up",
        Key::ArrowDown => "down",
        Key::ArrowLeft => "left",
        Key::ArrowRight => "right",
        Key::PageUp => "pgup",
        Key::PageDown => "pgdown",
        Key::Home => "home",
        Key::End => "end",
        Key::Insert => "insert",
        Key::Delete => "delete",
        Key::Tab => "tab",
        Key::Space => "space",
        Key::Enter => "enter",
        Key::Backslash => "\\",
        Key::OpenBracket => "[",
        Key::CloseBracket => "]",
        Key::Minus => "-",
        Key::Plus => "+",
        Key::Comma => ",",
        Key::Period => ".",
        Key::Slash => "/",
        Key::Semicolon => ";",
        _ => return None,
    })
}

/// Strips C/JS-style `//` line comments and `/* ... */` block comments outside JSON string
/// literals (`base::parse::stripComments`), matching tdesktop's `shortcuts-custom.json` loader.
#[must_use]
pub fn strip_json_comments(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;

    while i < bytes.len() {
        let ch = bytes[i] as char;
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            i += 1;
        } else if ch == '/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if ch == '/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            }
        } else {
            // Copy full UTF-8 character safely.
            let next_ch = input[i..].chars().next().unwrap_or(ch);
            out.push(next_ch);
            i += next_ch.len_utf8();
        }
    }

    out
}

/// Runtime shortcut map holding default bindings, active customized bindings, and parse diagnostics.
#[derive(Debug, Clone)]
pub struct ShortcutManager {
    defaults: BTreeMap<KeyChord, BTreeSet<ShortcutCommand>>,
    current: BTreeMap<KeyChord, BTreeSet<ShortcutCommand>>,
    errors: Vec<String>,
    tdata_dir: Option<PathBuf>,
}

impl Default for ShortcutManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ShortcutManager {
    /// Creates a [`ShortcutManager`] initialized with tdesktop's default bindings.
    #[must_use]
    pub fn new() -> Self {
        let defaults = build_default_bindings();
        Self {
            current: defaults.clone(),
            defaults,
            errors: Vec::new(),
            tdata_dir: None,
        }
    }

    /// Attaches a `tdata` directory and loads `shortcuts-custom.json` if present.
    pub fn load_from_tdata_dir(&mut self, tdata_dir: &Path) -> Result<(), String> {
        self.tdata_dir = Some(tdata_dir.to_path_buf());
        let custom_path = tdata_dir.join(CUSTOM_SHORTCUTS_FILENAME);
        if !custom_path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&custom_path)
            .map_err(|e| format!("failed to read {}: {e}", custom_path.display()))?;
        self.apply_custom_json(&raw);
        Ok(())
    }

    /// Persists custom overrides to `<tdata>/shortcuts-custom.json` when a `tdata` directory is set.
    pub fn save_to_tdata_dir(&self, tdata_dir: &Path) -> Result<PathBuf, String> {
        std::fs::create_dir_all(tdata_dir)
            .map_err(|e| format!("failed to create {}: {e}", tdata_dir.display()))?;
        let custom_path = tdata_dir.join(CUSTOM_SHORTCUTS_FILENAME);
        let json = self.serialize_custom_json();
        std::fs::write(&custom_path, json)
            .map_err(|e| format!("failed to write {}: {e}", custom_path.display()))?;
        Ok(custom_path)
    }

    /// Saves into the attached `tdata` directory if configured.
    pub fn persist_if_attached(&self) -> Result<(), String> {
        if let Some(dir) = &self.tdata_dir {
            let _ = self.save_to_tdata_dir(dir)?;
        }
        Ok(())
    }

    /// Returns the attached `tdata` directory, if configured.
    #[must_use]
    pub fn tdata_dir(&self) -> Option<&Path> {
        self.tdata_dir.as_deref()
    }

    /// Returns any non-fatal parse errors collected from `shortcuts-custom.json`.
    #[must_use]
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// Returns the default binding map.
    #[must_use]
    pub fn defaults(&self) -> &BTreeMap<KeyChord, BTreeSet<ShortcutCommand>> {
        &self.defaults
    }

    /// Returns the currently active binding map.
    #[must_use]
    pub fn current(&self) -> &BTreeMap<KeyChord, BTreeSet<ShortcutCommand>> {
        &self.current
    }

    /// Returns all currently assigned chords for `command`.
    #[must_use]
    pub fn chords_for(&self, command: ShortcutCommand) -> Vec<KeyChord> {
        self.current
            .iter()
            .filter(|(_, cmds)| cmds.contains(&command))
            .map(|(chord, _)| chord.clone())
            .collect()
    }

    /// Returns all default chords for `command`.
    #[must_use]
    pub fn default_chords_for(&self, command: ShortcutCommand) -> Vec<KeyChord> {
        self.defaults
            .iter()
            .filter(|(_, cmds)| cmds.contains(&command))
            .map(|(chord, _)| chord.clone())
            .collect()
    }

    /// Looks up all commands bound to `chord`.
    #[must_use]
    pub fn commands_for(&self, chord: &KeyChord) -> Vec<ShortcutCommand> {
        self.current
            .get(chord)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Returns `true` when any command differs from its default bindings.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.current != self.defaults
    }

    /// Returns `true` when `command` differs from its default bindings.
    #[must_use]
    pub fn is_command_modified(&self, command: ShortcutCommand) -> bool {
        self.chords_for(command) != self.default_chords_for(command)
    }

    /// Replaces `old_chord` with `new_chord` for `command`, replacing any existing command on
    /// `new_chord` (`Manager::change`).
    pub fn change_binding(
        &mut self,
        command: ShortcutCommand,
        old_chord: Option<&KeyChord>,
        new_chord: Option<KeyChord>,
    ) {
        if let Some(old) = old_chord {
            self.remove_command_chord(old, command);
        }
        if let Some(chord) = new_chord {
            self.set_binding(chord, command, true);
        }
    }

    /// Restores `command` to its default chord(s), removing any custom bindings for it.
    pub fn restore_default_for(&mut self, command: ShortcutCommand) {
        let existing = self.chords_for(command);
        for chord in existing {
            self.remove_command_chord(&chord, command);
        }
        let defaults = self.default_chords_for(command);
        for chord in defaults {
            self.set_binding(chord, command, false);
        }
    }

    /// Resets all shortcuts to tdesktop defaults.
    pub fn reset_to_defaults(&mut self) {
        self.current = self.defaults.clone();
    }

    /// Parses and applies a tdesktop `shortcuts-custom.json` document on top of defaults.
    pub fn apply_custom_json(&mut self, raw_json: &str) {
        self.errors.clear();
        self.current = self.defaults.clone();

        let stripped = strip_json_comments(raw_json);
        let parsed: Value = match serde_json::from_str(&stripped) {
            Ok(v) => v,
            Err(err) => {
                self.errors.push(format!("Failed to parse! Error: {err}"));
                return;
            }
        };
        let Some(entries) = parsed.as_array() else {
            self.errors
                .push("Failed to parse! Error: array expected".to_owned());
            return;
        };

        for (idx, item) in entries.iter().enumerate() {
            if idx >= SHORTCUTS_JSON_COUNT_LIMIT {
                self.errors.push(format!(
                    "Too many entries! Limit is {SHORTCUTS_JSON_COUNT_LIMIT}"
                ));
                break;
            }
            let Some(obj) = item.as_object() else {
                self.errors
                    .push("Bad entry! Error: object expected".to_owned());
                continue;
            };
            let Some(keys_str) = obj.get("keys").and_then(Value::as_str) else {
                self.errors.push(
                    "Bad entry! {\"keys\": \"...\", \"command\": [ \"...\" | null ]} expected."
                        .to_owned(),
                );
                continue;
            };
            let Some(cmd_val) = obj.get("command") else {
                self.errors.push(
                    "Bad entry! {\"keys\": \"...\", \"command\": [ \"...\" | null ]} expected."
                        .to_owned(),
                );
                continue;
            };
            let Some(chord) = KeyChord::parse(keys_str) else {
                self.errors
                    .push(format!("Invalid key sequence: '{keys_str}'"));
                continue;
            };

            if cmd_val.is_null() {
                self.current.remove(&chord);
            } else if let Some(cmd_name) = cmd_val.as_str() {
                let Some(cmd) = ShortcutCommand::from_json_name(cmd_name) else {
                    continue;
                };
                let removed = obj.get("removed").and_then(Value::as_bool).unwrap_or(false);
                if removed {
                    self.remove_command_chord(&chord, cmd);
                } else {
                    self.set_binding(chord, cmd, true);
                }
            } else {
                self.errors.push(
                    "Bad entry! {\"keys\": \"...\", \"command\": [ \"...\" | null ]} expected."
                        .to_owned(),
                );
            }
        }
    }

    /// Serializes only the differences between `self.current` and `self.defaults` into tdesktop's
    /// `shortcuts-custom.json` format (`Manager::writeCustomFile`).
    #[must_use]
    pub fn serialize_custom_json(&self) -> String {
        let mut entries: Vec<Value> = Vec::new();

        // 1. Removed default bindings -> {"keys": "...", "command": "...", "removed": true}
        for (chord, default_cmds) in &self.defaults {
            let active_cmds = self.current.get(chord);
            for &cmd in default_cmds {
                if !active_cmds.is_some_and(|set| set.contains(&cmd)) {
                    let mut map = Map::new();
                    map.insert("keys".to_owned(), Value::String(chord.to_json_string()));
                    map.insert(
                        "command".to_owned(),
                        Value::String(cmd.json_name().to_owned()),
                    );
                    map.insert("removed".to_owned(), Value::Bool(true));
                    entries.push(Value::Object(map));
                }
            }
        }

        // 2. Added / customized bindings -> {"keys": "...", "command": "..."}
        for (chord, active_cmds) in &self.current {
            let default_cmds = self.defaults.get(chord);
            for &cmd in active_cmds {
                if !default_cmds.is_some_and(|set| set.contains(&cmd)) {
                    let mut map = Map::new();
                    map.insert("keys".to_owned(), Value::String(chord.to_json_string()));
                    map.insert(
                        "command".to_owned(),
                        Value::String(cmd.json_name().to_owned()),
                    );
                    entries.push(Value::Object(map));
                }
            }
        }

        let header = "// This is a list of your own shortcuts for Rustgram / Telegram Desktop\n\
// You can see full list of commands in the 'shortcuts-default.json' file\n\n";
        let body = serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_owned());
        format!("{header}{body}\n")
    }

    fn set_binding(&mut self, chord: KeyChord, command: ShortcutCommand, replace: bool) {
        if replace {
            let entry = self.current.entry(chord).or_default();
            entry.clear();
            entry.insert(command);
        } else {
            self.current.entry(chord).or_default().insert(command);
        }
    }

    fn remove_command_chord(&mut self, chord: &KeyChord, command: ShortcutCommand) {
        if let Some(cmds) = self.current.get_mut(chord) {
            cmds.remove(&command);
            if cmds.is_empty() {
                self.current.remove(chord);
            }
        }
    }
}

fn build_default_bindings() -> BTreeMap<KeyChord, BTreeSet<ShortcutCommand>> {
    let mut map: BTreeMap<KeyChord, BTreeSet<ShortcutCommand>> = BTreeMap::new();
    let mut add = |keys: &str, cmd: ShortcutCommand| {
        if let Some(chord) = KeyChord::parse(keys) {
            map.entry(chord).or_default().insert(cmd);
        }
    };

    add("ctrl+w", ShortcutCommand::Close);
    add("ctrl+f4", ShortcutCommand::Close);
    add("ctrl+l", ShortcutCommand::Lock);
    add("ctrl+m", ShortcutCommand::Minimize);
    add("ctrl+q", ShortcutCommand::Quit);
    add("ctrl+shift+t", ShortcutCommand::ReopenClosedWindow);
    add("ctrl+shift+w", ShortcutCommand::CloseOtherWindows);

    add("media play", ShortcutCommand::MediaPlay);
    add("media pause", ShortcutCommand::MediaPause);
    add("toggle media play/pause", ShortcutCommand::MediaPlayPause);
    add("media stop", ShortcutCommand::MediaStop);
    add("media previous", ShortcutCommand::MediaPrevious);
    add("media next", ShortcutCommand::MediaNext);

    add("ctrl+f", ShortcutCommand::Search);
    add("search", ShortcutCommand::Search);
    add("find", ShortcutCommand::Search);

    add("ctrl+pgdown", ShortcutCommand::ChatNext);
    add("alt+down", ShortcutCommand::ChatNext);
    add("ctrl+pgup", ShortcutCommand::ChatPrevious);
    add("alt+up", ShortcutCommand::ChatPrevious);

    add("ctrl+alt+home", ShortcutCommand::ChatFirst);
    add("ctrl+alt+end", ShortcutCommand::ChatLast);

    add("ctrl+1", ShortcutCommand::ChatPinned1);
    add("ctrl+2", ShortcutCommand::ChatPinned2);
    add("ctrl+3", ShortcutCommand::ChatPinned3);
    add("ctrl+4", ShortcutCommand::ChatPinned4);
    add("ctrl+5", ShortcutCommand::ChatPinned5);
    add("ctrl+6", ShortcutCommand::ChatPinned6);
    add("ctrl+7", ShortcutCommand::ChatPinned7);
    add("ctrl+8", ShortcutCommand::ChatPinned8);

    add("ctrl+1", ShortcutCommand::ShowAllChats);
    add("ctrl+2", ShortcutCommand::ShowFolder1);
    add("ctrl+3", ShortcutCommand::ShowFolder2);
    add("ctrl+4", ShortcutCommand::ShowFolder3);
    add("ctrl+5", ShortcutCommand::ShowFolder4);
    add("ctrl+6", ShortcutCommand::ShowFolder5);
    add("ctrl+7", ShortcutCommand::ShowFolder6);
    add("ctrl+8", ShortcutCommand::ShowFolderLast);

    add("ctrl+shift+down", ShortcutCommand::FolderNext);
    add("ctrl+shift+up", ShortcutCommand::FolderPrevious);

    add("ctrl+0", ShortcutCommand::ChatSelf);
    add("ctrl+9", ShortcutCommand::ShowArchive);
    add("ctrl+j", ShortcutCommand::ShowDownloads);

    add("ctrl+r", ShortcutCommand::ReadChat);
    add("ctrl+\\", ShortcutCommand::ShowChatMenu);
    add("ctrl+]", ShortcutCommand::ShowChatPreview);
    add("ctrl+r", ShortcutCommand::RecordVoice);
    add("ctrl+s", ShortcutCommand::StashMessage);

    map
}

/// UI state for the Settings -> Shortcuts editor (`settings_shortcuts.cpp`).
#[derive(Debug, Default)]
pub struct ShortcutsSettingsState {
    /// Shortcut registry and persistence manager.
    pub manager: ShortcutManager,
    /// Live search query filtering commands and key sequences.
    pub search_query: String,
    /// Currently recording command + optional existing chord being replaced.
    pub recording: Option<(ShortcutCommand, Option<KeyChord>)>,
    /// Status/conflict notice banner shown after a recording or reset action.
    pub status_notice: Option<String>,
}

/// Renders the custom keyboard shortcuts editor inside the Settings pane.
///
/// Returns an optional toast message when the user changes or resets shortcuts.
pub fn render_shortcuts_section(
    ui: &mut Ui,
    state: &mut ShortcutsSettingsState,
    colors: &dyn Colors,
) -> Option<String> {
    let mut toast: Option<String> = None;

    // Handle active key capture before drawing rows so pressing a shortcut immediately binds it.
    if let Some((rec_cmd, old_chord)) = state.recording.clone() {
        let mut captured: Option<Option<KeyChord>> = None;
        ui.ctx().input(|i| {
            for ev in &i.events {
                if let Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = ev
                {
                    if *key == Key::Escape {
                        captured = Some(None);
                        break;
                    }
                    if let Some(chord) = KeyChord::from_egui_key(*key, *modifiers)
                        && chord.is_valid_binding()
                    {
                        captured = Some(Some(chord));
                        break;
                    }
                }
            }
        });
        if let Some(result) = captured {
            state.recording = None;
            if let Some(new_chord) = result {
                let display = new_chord.to_display_string();
                let conflicts: Vec<_> = state
                    .manager
                    .commands_for(&new_chord)
                    .into_iter()
                    .filter(|&c| c != rec_cmd)
                    .collect();
                state
                    .manager
                    .change_binding(rec_cmd, old_chord.as_ref(), Some(new_chord));
                if let Err(err) = state.manager.persist_if_attached() {
                    state.status_notice = Some(err);
                } else if !conflicts.is_empty() {
                    let prev_name = conflicts[0].label();
                    state.status_notice = Some(format!(
                        "Reassigned {display} from \"{prev_name}\" to \"{}\".",
                        rec_cmd.label()
                    ));
                } else {
                    state.status_notice = Some(format!(
                        "Shortcut \"{}\" set to {display}.",
                        rec_cmd.label()
                    ));
                }
                toast = state.status_notice.clone();
            }
        }
    }

    subsection_title(ui, "Keyboard Shortcuts", colors);

    settings_card(ui, colors, |ui| {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.search_query)
                    .hint_text("Search shortcuts or keys...")
                    .desired_width((ui.available_width() - 140.0).max(120.0)),
            );
            let reset_enabled = state.manager.is_modified();
            if ui
                .add_enabled(reset_enabled, egui::Button::new("Reset all"))
                .clicked()
            {
                state.manager.reset_to_defaults();
                state.recording = None;
                let _ = state.manager.persist_if_attached();
                toast = Some("All keyboard shortcuts reset to defaults.".to_owned());
            }
        });

        if let Some(notice) = &state.status_notice {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(notice)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowActiveTextFg")),
            );
        }

        if !state.manager.errors().is_empty() {
            ui.add_space(4.0);
            for err in state.manager.errors() {
                ui.label(
                    egui::RichText::new(err)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("attentionButtonFg")),
                );
            }
        }
    });

    settings_divider(ui, None, colors);

    let query = state.search_query.trim().to_ascii_lowercase();
    let groups: &[(&str, &[ShortcutCommand])] = &[
        (
            "Window & Application",
            &[
                ShortcutCommand::Close,
                ShortcutCommand::Lock,
                ShortcutCommand::Minimize,
                ShortcutCommand::Quit,
                ShortcutCommand::ReopenClosedWindow,
                ShortcutCommand::CloseOtherWindows,
                ShortcutCommand::Search,
            ],
        ),
        (
            "Chats & Folders",
            &[
                ShortcutCommand::ChatPrevious,
                ShortcutCommand::ChatNext,
                ShortcutCommand::ChatFirst,
                ShortcutCommand::ChatLast,
                ShortcutCommand::ChatSelf,
                ShortcutCommand::ShowAllChats,
                ShortcutCommand::ShowFolder1,
                ShortcutCommand::ShowFolder2,
                ShortcutCommand::ShowFolder3,
                ShortcutCommand::ShowFolder4,
                ShortcutCommand::ShowFolder5,
                ShortcutCommand::ShowFolder6,
                ShortcutCommand::ShowFolderLast,
                ShortcutCommand::FolderNext,
                ShortcutCommand::FolderPrevious,
                ShortcutCommand::ShowArchive,
                ShortcutCommand::ShowContacts,
                ShortcutCommand::ShowDownloads,
                ShortcutCommand::ReadChat,
                ShortcutCommand::ArchiveChat,
                ShortcutCommand::ShowScheduled,
                ShortcutCommand::ShowChatMenu,
                ShortcutCommand::ShowChatPreview,
                ShortcutCommand::ShowAdminLog,
            ],
        ),
        (
            "Messaging & Media",
            &[
                ShortcutCommand::JustSendMessage,
                ShortcutCommand::SendSilentMessage,
                ShortcutCommand::ScheduleMessage,
                ShortcutCommand::ShowRichEditor,
                ShortcutCommand::ToggleWebPagePreview,
                ShortcutCommand::StashMessage,
                ShortcutCommand::RecordVoice,
                ShortcutCommand::RecordRound,
                ShortcutCommand::MediaViewerFullscreen,
                ShortcutCommand::MediaPlayPause,
                ShortcutCommand::MediaPrevious,
                ShortcutCommand::MediaNext,
            ],
        ),
    ];

    for (group_title, cmds) in groups {
        let matching: Vec<ShortcutCommand> = cmds
            .iter()
            .copied()
            .filter(|cmd| {
                if query.is_empty() {
                    return true;
                }
                if cmd.label().to_ascii_lowercase().contains(&query)
                    || cmd.json_name().contains(&query)
                {
                    return true;
                }
                state
                    .manager
                    .chords_for(*cmd)
                    .iter()
                    .any(|c| c.to_display_string().to_ascii_lowercase().contains(&query))
            })
            .collect();

        if matching.is_empty() {
            continue;
        }

        subsection_title(ui, group_title, colors);
        for cmd in matching {
            let chords = state.manager.chords_for(cmd);
            let primary = chords.first().cloned();
            let is_recording = state.recording.as_ref().is_some_and(|(rc, _)| *rc == cmd);
            let is_modified = state.manager.is_command_modified(cmd);

            let badge_text = if is_recording {
                "Press shortcut… (Esc to cancel)".to_owned()
            } else if chords.is_empty() {
                "None".to_owned()
            } else {
                chords
                    .iter()
                    .map(KeyChord::to_display_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            let avail = ui.available_width().max(240.0);
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail, 36.0), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(4), colors.color("windowBgOver"));
            }
            if is_recording {
                ui.painter().rect_stroke(
                    rect,
                    CornerRadius::same(4),
                    Stroke::new(1.5, colors.color("windowActiveTextFg")),
                    StrokeKind::Inside,
                );
            }

            let label_pos = egui::Pos2::new(rect.left() + 18.0, rect.center().y);
            ui.painter().text(
                label_pos,
                egui::Align2::LEFT_CENTER,
                cmd.label(),
                font_id_from_style(st::normal_font),
                colors.color("windowFg"),
            );

            let badge_color = if is_recording || is_modified {
                colors.color("windowActiveTextFg")
            } else {
                colors.color("windowSubTextFg")
            };
            let right_anchor = if is_modified {
                rect.right() - 78.0
            } else {
                rect.right() - 18.0
            };
            ui.painter().text(
                egui::Pos2::new(right_anchor, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                &badge_text,
                font_id_from_style(st::normal_font),
                badge_color,
            );

            if is_modified {
                let reset_rect = egui::Rect::from_min_size(
                    egui::Pos2::new(rect.right() - 68.0, rect.center().y - 11.0),
                    Vec2::new(54.0, 22.0),
                );
                let reset_resp = ui.interact(
                    reset_rect,
                    ui.scope_id().with(("shortcut_reset", cmd.json_name())),
                    Sense::click(),
                );
                if reset_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter().text(
                    reset_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Reset",
                    font_id_from_style(st::normal_font),
                    colors.color("windowActiveTextFg"),
                );
                if reset_resp.clicked() {
                    state.manager.restore_default_for(cmd);
                    let _ = state.manager.persist_if_attached();
                    toast = Some(format!("Restored default for \"{}\".", cmd.label()));
                }
            }

            if resp.clicked() {
                if is_recording {
                    state.recording = None;
                } else {
                    state.recording = Some((cmd, primary));
                }
            } else if resp.secondary_clicked() && primary.is_some() {
                state.manager.change_binding(cmd, primary.as_ref(), None);
                let _ = state.manager.persist_if_attached();
                toast = Some(format!("Cleared shortcut for \"{}\".", cmd.label()));
            }
        }
    }

    toast
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_chord_parse_and_format() {
        let chord = KeyChord::parse("Ctrl+Shift+T").expect("should parse");
        assert!(chord.ctrl && chord.shift && !chord.alt && !chord.meta);
        assert_eq!(chord.key, "t");
        assert_eq!(chord.to_json_string(), "ctrl+shift+t");
        assert_eq!(chord.to_display_string(), "Ctrl+Shift+T");

        let pg = KeyChord::parse("ctrl+pgdown").expect("pgdown");
        assert_eq!(pg.to_display_string(), "Ctrl+PgDown");

        let media = KeyChord::parse("toggle media play/pause").expect("media");
        assert!(media.is_valid_binding());
        assert_eq!(media.to_json_string(), "toggle media play/pause");

        let plain_a = KeyChord::parse("a").expect("plain a");
        assert!(!plain_a.is_valid_binding());
        assert!(KeyChord::parse("f5").unwrap().is_valid_binding());
    }

    #[test]
    fn shortcuts_custom_json_parse_strip_comments_and_persist_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let custom_path = dir.path().join(CUSTOM_SHORTCUTS_FILENAME);

        let initial_json = r#"
// User customized shortcuts with comments
[
    /* Rebind search to Ctrl+K and remove Ctrl+L lock */
    { "keys": "ctrl+k", "command": "search" },
    { "keys": "ctrl+l", "command": null },
    { "keys": "ctrl+f", "command": "search", "removed": true }
]
"#;
        std::fs::write(&custom_path, initial_json).expect("write initial custom json");

        let mut mgr = ShortcutManager::new();
        mgr.load_from_tdata_dir(dir.path())
            .expect("load custom json");
        assert!(mgr.errors().is_empty(), "errors: {:?}", mgr.errors());

        let ctrl_k = KeyChord::parse("ctrl+k").unwrap();
        let ctrl_f = KeyChord::parse("ctrl+f").unwrap();
        let ctrl_l = KeyChord::parse("ctrl+l").unwrap();

        assert!(mgr.commands_for(&ctrl_k).contains(&ShortcutCommand::Search));
        assert!(!mgr.commands_for(&ctrl_f).contains(&ShortcutCommand::Search));
        assert!(mgr.commands_for(&ctrl_l).is_empty());
        assert!(mgr.is_modified());

        // Mutate a binding via editor API and persist back to tdata.
        let ctrl_shift_s = KeyChord::parse("ctrl+shift+s").unwrap();
        mgr.change_binding(
            ShortcutCommand::ScheduleMessage,
            None,
            Some(ctrl_shift_s.clone()),
        );
        mgr.save_to_tdata_dir(dir.path()).expect("save custom json");

        // Reload in a fresh manager and verify full round-trip fidelity.
        let mut reloaded = ShortcutManager::new();
        reloaded
            .load_from_tdata_dir(dir.path())
            .expect("reload custom json");
        assert!(reloaded.errors().is_empty());
        assert!(
            reloaded
                .commands_for(&ctrl_k)
                .contains(&ShortcutCommand::Search)
        );
        assert!(
            !reloaded
                .commands_for(&ctrl_f)
                .contains(&ShortcutCommand::Search)
        );
        assert!(reloaded.commands_for(&ctrl_l).is_empty());
        assert_eq!(
            reloaded.commands_for(&ctrl_shift_s),
            vec![ShortcutCommand::ScheduleMessage]
        );

        // Reset restores defaults and writes an empty diff array.
        reloaded.reset_to_defaults();
        assert!(!reloaded.is_modified());
        assert!(reloaded.serialize_custom_json().contains("[]"));
    }
}
