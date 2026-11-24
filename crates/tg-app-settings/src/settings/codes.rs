//! Secret keyboard codes and debug/experimental options in Settings.
//!
//! Port origin: `Telegram/SourceFiles/settings/settings_codes.cpp` and
//! `Telegram/SourceFiles/settings/settings_experimental.cpp`.

/// Actions triggered by typing secret codes on the Settings screen (`CodesFeedString`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretCodeAction {
    /// Toggle `debugmode` (detailed network/app debug logging).
    ToggleDebugMode,
    /// Open/show log file location (`viewlogs`).
    ViewLogs,
    /// Trigger an update checker dry run (`testupdate`).
    TestUpdate,
    /// Switch to custom language pack (`loadlang`).
    LoadCustomLang,
    /// Toggle `moderate` mode (`CoreSettings::moderate_mode_enabled`).
    ToggleModerateMode,
    /// Trigger `updates.getDifference` sync (`getdifference`).
    GetDifference,
    /// Open custom `.tdesktop-palette` file (`loadcolors`).
    LoadColors,
    /// Open custom `.tdesktop-endpoints` file (`endpoints`).
    LoadEndpoints,
    /// Toggle test/production DC environment (`testmode`).
    ToggleTestMode,
    /// Jump straight to Chat Folders settings (`folders`).
    OpenFolders,
    /// Force-register `tg://` custom URL scheme (`registertg`).
    RegisterUrlScheme,
    /// Toggle fast buttons mode (`numberbuttons`).
    ToggleFastButtons,
    /// Toggle WebView proxy transport block (`externalweb`).
    ToggleExternalWeb,
    /// Reset all custom sound overrides (`sounds_reset`).
    ResetSoundOverrides,
    /// Toggle testing chat theme colors (`testchatcolors`).
    ToggleTestChatColors,
    /// Override a specific notification/call sound key (`msg_incoming`, `call_*`).
    OverrideSound(&'static str),
}

const KNOWN_CODES: &[(&str, SecretCodeAction)] = &[
    ("debugmode", SecretCodeAction::ToggleDebugMode),
    ("viewlogs", SecretCodeAction::ViewLogs),
    ("testupdate", SecretCodeAction::TestUpdate),
    ("loadlang", SecretCodeAction::LoadCustomLang),
    ("moderate", SecretCodeAction::ToggleModerateMode),
    ("getdifference", SecretCodeAction::GetDifference),
    ("loadcolors", SecretCodeAction::LoadColors),
    ("endpoints", SecretCodeAction::LoadEndpoints),
    ("testmode", SecretCodeAction::ToggleTestMode),
    ("folders", SecretCodeAction::OpenFolders),
    ("registertg", SecretCodeAction::RegisterUrlScheme),
    ("numberbuttons", SecretCodeAction::ToggleFastButtons),
    ("externalweb", SecretCodeAction::ToggleExternalWeb),
    ("sounds_reset", SecretCodeAction::ResetSoundOverrides),
    ("testchatcolors", SecretCodeAction::ToggleTestChatColors),
    (
        "msg_incoming",
        SecretCodeAction::OverrideSound("msg_incoming"),
    ),
    (
        "call_incoming",
        SecretCodeAction::OverrideSound("call_incoming"),
    ),
    (
        "call_outgoing",
        SecretCodeAction::OverrideSound("call_outgoing"),
    ),
    ("call_busy", SecretCodeAction::OverrideSound("call_busy")),
    (
        "call_connect",
        SecretCodeAction::OverrideSound("call_connect"),
    ),
    ("call_end", SecretCodeAction::OverrideSound("call_end")),
];

/// Tracks typed characters and detects secret Settings codes.
///
/// Exact port of the suffix-matching algorithm in `CodesFeedString` (`settings_codes.cpp:298-325`).
#[derive(Debug, Clone, Default)]
pub struct SecretCodeTracker {
    buffer: String,
}

impl SecretCodeTracker {
    /// Creates an empty tracker.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    /// Feeds newly typed text and returns the triggered [`SecretCodeAction`], if any.
    pub fn feed(&mut self, text: &str) -> Option<SecretCodeAction> {
        self.buffer.push_str(&text.to_ascii_lowercase());
        let mut triggered = None;
        let len = self.buffer.len();
        let mut from = 0;
        while len > from {
            if !self.buffer.is_char_boundary(from) {
                from += 1;
                continue;
            }
            let piece = &self.buffer[from..];
            if let Some((_, action)) = KNOWN_CODES.iter().find(|(k, _)| *k == piece) {
                triggered = Some(action.clone());
                from = len;
                break;
            }
            if KNOWN_CODES.iter().any(|(k, _)| k.starts_with(piece)) {
                break;
            }
            from += 1;
        }
        if len > from {
            self.buffer = self.buffer[from..].to_owned();
        } else {
            self.buffer.clear();
        }
        triggered
    }

    /// Returns the currently buffered potential code prefix.
    #[must_use]
    pub fn pending(&self) -> &str {
        &self.buffer
    }

    /// Clears any buffered characters.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    /// Lists all supported secret code strings.
    #[must_use]
    pub fn all_code_names() -> Vec<&'static str> {
        KNOWN_CODES.iter().map(|(k, _)| *k).collect()
    }
}

/// Experimental & debug runtime flags toggled via secret codes or Advanced -> Experimental.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DebugOptions {
    /// `numberbuttons`: fast keyboard button selection mode.
    pub fast_buttons_mode: bool,
    /// `externalweb`: disable WebView proxy transport.
    pub webview_transport_blocked: bool,
    /// `testchatcolors`: preview chat theme colors.
    pub testing_chat_colors: bool,
    /// `testmode`: test DC environment toggle.
    pub test_dc_environment: bool,
    /// Smooth scrolling experimental option.
    pub smooth_scrolling: bool,
    /// Experimental hardware video decoder diagnostics overlay.
    pub media_debug_overlay: bool,
}
