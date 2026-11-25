//! Advanced settings: Connection/Proxy editor (`tg-transport`), Data & Storage,
//! Auto-Download limits, Export Data, Power Saving, System Integration (`tg-platform`),
//! Window Title options, Spellchecker, Updates, and Debug/Experimental options.
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_advanced.cpp`,
//! `Telegram/SourceFiles/settings/settings_power_saving.cpp`,
//! `Telegram/SourceFiles/boxes/connection_box.cpp`, and
//! `Telegram/SourceFiles/boxes/auto_download_box.cpp`.

use std::path::Path;
use std::sync::Arc;

use egui::{Align2, Color32, CornerRadius, CursorIcon, Label, Pos2, Rect, Sense, Ui, Vec2};
use tg_core::settings::{CloseBehavior, PowerSavingFlags as CorePowerSavingFlags, WorkMode};
use tg_core::settings_proxy::{
    PROXY_ROTATION_TIMEOUTS, ProxyData as CoreProxyData, ProxySettingsMode,
    ProxyType as CoreProxyType,
};
use tg_core::{CoreSettings, GlobalConfig};
use tg_lang::tr;
use tg_platform::autostart::{
    AutostartEntry, AutostartProvider, DEFAULT_STARTUP_TASK_ID, StartupTaskState,
    WinRtStartupTaskProvider, WindowsRunKeyAutostartProvider,
};
use tg_platform::registration::{
    AppLinkConfig, is_send_to_enabled_in, send_to_folder_dir, set_send_to_in,
};
use tg_platform::spellcheck::{is_system_spellchecker_supported, supported_languages};
use tg_platform::system_info;
use tg_transport::{ProxyData as TransportProxyData, ProxyStatus};
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, InputFieldStyle, MediaSliderStyle, RoundButtonStyle, ToggleStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::checkbox::{Toggle, paint_toggle_view};
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::sliders::MediaSlider;

use crate::settings::dictionaries::{
    DictionarySourceSlot, ManageDictionariesState, MockDictionaryCloudSource, dictionaries_root,
    render_manage_dictionaries_panel,
};
use tg_app_core::core_types::misc::format_proxy_connection_status;
use tg_app_core::core_types::restore_windows::{RestoreWindowsPolicy, SavedWindowsSnapshot};
use tg_app_core::core_types::startup_types::StartupPrefs;
use tg_app_core::core_types::startup_types::{UpdatePreferencesSink, UpdateSnapshot, UpdateState};
use tg_app_core::core_types::window_types::ColumnLayout;
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::types::ConnectionStatus;

use super::backend::{
    AutoDownloadSource, AutoDownloadType, ExportFormat, ExportRequest, MAX_AUTO_DOWNLOAD_BYTES,
    ProxyCheckStatus, SettingsBackend, format_bytes,
};
use super::codes::{DebugOptions, SecretCodeAction};
use super::strings;
use super::widgets::{
    SETTINGS_LIST_SKIP, SettingsIconKind, paint_badge_pill, paint_tinted_icon, settings_card,
    settings_checkbox_row, settings_divider, settings_radio_row, settings_row, settings_row_pad_x,
    settings_section_divider, settings_split_toggle_row, settings_toggle_row, subsection_title,
};

/// Power saving flag group mask for Animated Stickers (`PowerSavingLabels` in `settings_power_saving.cpp`).
pub const POWER_SAVING_GROUP_STICKERS: CorePowerSavingFlags =
    CorePowerSavingFlags::from_bits_truncate(
        CorePowerSavingFlags::STICKERS_PANEL.bits() | CorePowerSavingFlags::STICKERS_CHAT.bits(),
    );
/// Power saving flag group mask for Animated Emoji (`PowerSavingLabels` in `settings_power_saving.cpp`).
pub const POWER_SAVING_GROUP_EMOJI: CorePowerSavingFlags = CorePowerSavingFlags::from_bits_truncate(
    CorePowerSavingFlags::EMOJI_PANEL.bits()
        | CorePowerSavingFlags::EMOJI_REACTIONS.bits()
        | CorePowerSavingFlags::EMOJI_CHAT.bits()
        | CorePowerSavingFlags::EMOJI_STATUS.bits(),
);
/// Power saving flag group mask for Animations in Chats (`PowerSavingLabels` in `settings_power_saving.cpp`).
pub const POWER_SAVING_GROUP_CHAT: CorePowerSavingFlags = CorePowerSavingFlags::from_bits_truncate(
    CorePowerSavingFlags::CHAT_BACKGROUND.bits()
        | CorePowerSavingFlags::CHAT_SPOILER.bits()
        | CorePowerSavingFlags::CHAT_EFFECTS.bits(),
);
/// Power saving flag group mask for Animations in Calls (`PowerSavingLabels` in `settings_power_saving.cpp`).
pub const POWER_SAVING_GROUP_CALLS: CorePowerSavingFlags = CorePowerSavingFlags::CALLS;
/// Power saving flag group mask for Interface animations (`PowerSavingLabels` in `settings_power_saving.cpp`).
pub const POWER_SAVING_GROUP_UI: CorePowerSavingFlags = CorePowerSavingFlags::ANIMATIONS;

/// Power saving bitflags mirror matching `PowerSaving::Flags` in `settings_power_saving.cpp`.
///
/// [`CoreSettings::power_saving`] (`tg_core::settings::PowerSavingFlags`) is the authoritative
/// source of truth; this struct is kept mirrored on [`AdvancedState`] so the live settings bridge
/// in `window/main_widget.rs` stays a pure pass-through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PowerSavingFlags {
    /// Disable animated stickers in sticker panel (`kStickersPanel`).
    pub stickers_panel: bool,
    /// Disable animated stickers in chats (`kStickersChat`).
    pub stickers_chat: bool,
    /// Disable animated emoji in panel (`kEmojiPanel`).
    pub emoji_panel: bool,
    /// Disable reaction animations (`kEmojiReactions`).
    pub emoji_reactions: bool,
    /// Disable animated emoji in chats (`kEmojiChat`).
    pub emoji_chat: bool,
    /// Disable animated emoji statuses (`kEmojiStatus`).
    pub emoji_status: bool,
    /// Disable animated chat backgrounds (`kChatBackground`).
    pub chat_background: bool,
    /// Disable spoiler shimmer animations (`kChatSpoiler`).
    pub chat_spoiler: bool,
    /// Disable message screen effects (`kChatEffects`).
    pub chat_effects: bool,
    /// Disable animations during calls (`kCalls`).
    pub calls: bool,
    /// Disable general UI animations (`kAnimations`).
    pub animations: bool,
}

impl PowerSavingFlags {
    /// Constructs a UI mirror from `tg-core`'s typed [`CorePowerSavingFlags`].
    #[must_use]
    pub const fn from_core(flags: CorePowerSavingFlags) -> Self {
        Self {
            stickers_panel: flags.contains(CorePowerSavingFlags::STICKERS_PANEL),
            stickers_chat: flags.contains(CorePowerSavingFlags::STICKERS_CHAT),
            emoji_panel: flags.contains(CorePowerSavingFlags::EMOJI_PANEL),
            emoji_reactions: flags.contains(CorePowerSavingFlags::EMOJI_REACTIONS),
            emoji_chat: flags.contains(CorePowerSavingFlags::EMOJI_CHAT),
            emoji_status: flags.contains(CorePowerSavingFlags::EMOJI_STATUS),
            chat_background: flags.contains(CorePowerSavingFlags::CHAT_BACKGROUND),
            chat_spoiler: flags.contains(CorePowerSavingFlags::CHAT_SPOILER),
            chat_effects: flags.contains(CorePowerSavingFlags::CHAT_EFFECTS),
            calls: flags.contains(CorePowerSavingFlags::CALLS),
            animations: flags.contains(CorePowerSavingFlags::ANIMATIONS),
        }
    }

    /// Converts this UI mirror into `tg-core`'s typed [`CorePowerSavingFlags`].
    #[must_use]
    pub fn to_core(self) -> CorePowerSavingFlags {
        let mut flags = CorePowerSavingFlags::NONE;
        flags.set(CorePowerSavingFlags::STICKERS_PANEL, self.stickers_panel);
        flags.set(CorePowerSavingFlags::STICKERS_CHAT, self.stickers_chat);
        flags.set(CorePowerSavingFlags::EMOJI_PANEL, self.emoji_panel);
        flags.set(CorePowerSavingFlags::EMOJI_REACTIONS, self.emoji_reactions);
        flags.set(CorePowerSavingFlags::EMOJI_CHAT, self.emoji_chat);
        flags.set(CorePowerSavingFlags::EMOJI_STATUS, self.emoji_status);
        flags.set(CorePowerSavingFlags::CHAT_BACKGROUND, self.chat_background);
        flags.set(CorePowerSavingFlags::CHAT_SPOILER, self.chat_spoiler);
        flags.set(CorePowerSavingFlags::CHAT_EFFECTS, self.chat_effects);
        flags.set(CorePowerSavingFlags::CALLS, self.calls);
        flags.set(CorePowerSavingFlags::ANIMATIONS, self.animations);
        flags
    }

    /// Returns the number of currently disabled animation categories (`0..=11`).
    #[must_use]
    pub const fn disabled_count(self) -> usize {
        self.stickers_panel as usize
            + self.stickers_chat as usize
            + self.emoji_panel as usize
            + self.emoji_reactions as usize
            + self.emoji_chat as usize
            + self.emoji_status as usize
            + self.chat_background as usize
            + self.chat_spoiler as usize
            + self.chat_effects as usize
            + self.calls as usize
            + self.animations as usize
    }

    /// Sets every power-saving flag to `enabled`.
    pub fn set_all(&mut self, enabled: bool) {
        self.stickers_panel = enabled;
        self.stickers_chat = enabled;
        self.emoji_panel = enabled;
        self.emoji_reactions = enabled;
        self.emoji_chat = enabled;
        self.emoji_status = enabled;
        self.chat_background = enabled;
        self.chat_spoiler = enabled;
        self.chat_effects = enabled;
        self.calls = enabled;
        self.animations = enabled;
    }

    /// Returns a compact summary string for the Advanced settings row.
    #[must_use]
    pub fn summary(self) -> String {
        let n = self.disabled_count();
        if n == 0 {
            tr::lng_settings_cloud_password_off()
        } else if n == 11 {
            strings::POWER_ALL_OFF.to_owned()
        } else {
            format!("{n}/11 reduced")
        }
    }
}

/// Returns `true` when OS battery saving is forcing all animations off (`PowerSavingBox` lock).
#[must_use]
pub const fn is_power_saving_locked_by_battery(core: &CoreSettings) -> bool {
    core.power_saving_force_all && !core.ignore_battery_saving
}

/// Returns the effective [`CorePowerSavingFlags`] taking `power_saving_force_all` and
/// `ignore_battery_saving` into account.
#[must_use]
pub const fn power_saving_effective_flags(core: &CoreSettings) -> CorePowerSavingFlags {
    if is_power_saving_locked_by_battery(core) {
        CorePowerSavingFlags::ALL
    } else {
        core.power_saving
    }
}

/// Updates [`CoreSettings::power_saving_force_all`] from OS battery saver status when available.
pub fn sync_battery_saver_to_core(core: &mut CoreSettings, battery_saver_active: Option<bool>) {
    if let Some(active) = battery_saver_active {
        core.power_saving_force_all = active;
    }
}

/// Enables or disables a single animation flag directly on [`CoreSettings::power_saving`]
/// (matching `settings_power_saving.cpp`: checked = animation ON = flag bit cleared).
pub fn set_power_saving_flag_enabled(
    core: &mut CoreSettings,
    flag: CorePowerSavingFlags,
    animation_enabled: bool,
) {
    if is_power_saving_locked_by_battery(core) {
        return;
    }
    core.power_saving.set(flag, !animation_enabled);
}

/// Enables or disables every animation flag in `group_mask` directly on [`CoreSettings::power_saving`].
pub fn set_power_saving_group_enabled(
    core: &mut CoreSettings,
    group_mask: CorePowerSavingFlags,
    animations_enabled: bool,
) {
    if is_power_saving_locked_by_battery(core) {
        return;
    }
    core.power_saving.set(group_mask, !animations_enabled);
}

/// Toggles "Save Power on Low Battery" (`lng_settings_power_auto`) directly on
/// [`CoreSettings::ignore_battery_saving`].
pub fn set_power_saving_auto_on_low_battery(core: &mut CoreSettings, enabled: bool) {
    core.ignore_battery_saving = !enabled;
}

/// Formats a compact summary string for the Advanced settings Power Saving row directly from
/// [`CoreSettings`].
#[must_use]
pub fn power_saving_summary(core: &CoreSettings) -> String {
    PowerSavingFlags::from_core(power_saving_effective_flags(core)).summary()
}

/// Returns the localized `"Update Rustgram"` label derived from `tg_lang::tr::lng_update_telegram()`.
#[must_use]
pub fn localized_update_tgfork() -> String {
    tr::lng_update_telegram().replace("Telegram", "Rustgram")
}

/// Formatted update version/status display strings derived from [`UpdateSnapshot`] using
/// `tg_lang::tr::*` phrases (ports `SetDefaultStatus` / `ShowDownloadProgress` in
/// `settings_advanced.cpp`).
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateStatusDisplay {
    /// Version line formatted with `lng_settings_current_version`.
    pub version_line: String,
    /// Primary localized status line (`Checking`, `UpToDate`, `Downloading`, `Ready`, `Failed`, ...).
    pub status_line: String,
    /// Optional secondary detail line (download bytes, ready version, or error message).
    pub detail_line: Option<String>,
    /// Download progress fraction in `0.0..=1.0` when downloading with known total size.
    pub download_fraction: Option<f32>,
    /// Whether the status represents an error.
    pub is_error: bool,
    /// Whether a verified update is staged and ready to install on restart.
    pub is_ready: bool,
}

/// Formats [`UpdateSnapshot`] into localized `tg_lang` strings matching `SetupUpdate` in
/// `settings/sections/settings_advanced.cpp`.
#[must_use]
pub fn format_update_status(snapshot: &UpdateSnapshot) -> UpdateStatusDisplay {
    let ver_str = if snapshot.allow_beta && !snapshot.current_version.is_prerelease() {
        format!("{} beta", snapshot.current_version)
    } else {
        snapshot.current_version.to_string()
    };
    let version_line = tr::lng_settings_current_version(&ver_str);

    match &snapshot.state {
        UpdateState::Disabled => {
            if snapshot.disabled_by_cli {
                UpdateStatusDisplay {
                    version_line,
                    status_line: snapshot.status_label(),
                    detail_line: None,
                    download_fraction: None,
                    is_error: false,
                    is_ready: false,
                }
            } else {
                UpdateStatusDisplay {
                    status_line: version_line.clone(),
                    version_line,
                    detail_line: Some(snapshot.status_label()),
                    download_fraction: None,
                    is_error: false,
                    is_ready: false,
                }
            }
        }
        UpdateState::Idle => UpdateStatusDisplay {
            status_line: version_line.clone(),
            version_line,
            detail_line: None,
            download_fraction: None,
            is_error: false,
            is_ready: false,
        },
        UpdateState::Checking => UpdateStatusDisplay {
            version_line,
            status_line: tr::lng_settings_update_checking(),
            detail_line: None,
            download_fraction: None,
            is_error: false,
            is_ready: false,
        },
        UpdateState::UpToDate { .. } => UpdateStatusDisplay {
            version_line,
            status_line: tr::lng_settings_latest_installed(),
            detail_line: None,
            download_fraction: None,
            is_error: false,
            is_ready: false,
        },
        UpdateState::Available(avail) => UpdateStatusDisplay {
            version_line,
            status_line: tr::lng_settings_downloading_update("0%"),
            detail_line: Some(format!("v{}", avail.version)),
            download_fraction: Some(0.0),
            is_error: false,
            is_ready: false,
        },
        UpdateState::Downloading {
            version,
            downloaded_bytes,
            total_bytes,
        } => {
            let (progress_str, fraction, detail) = if *total_bytes > 0 {
                let pct =
                    ((*downloaded_bytes as f64 * 100.0) / (*total_bytes as f64)).clamp(0.0, 100.0);
                let frac = ((*downloaded_bytes as f64) / (*total_bytes as f64)).clamp(0.0, 1.0);
                let pct_label = format!("{pct:.0}%");
                let detail = format!(
                    "v{version}  ·  {} / {}",
                    format_bytes((*downloaded_bytes).min(i64::MAX as u64) as i64),
                    format_bytes((*total_bytes).min(i64::MAX as u64) as i64),
                );
                (pct_label, Some(frac as f32), Some(detail))
            } else {
                let dl = format_bytes((*downloaded_bytes).min(i64::MAX as u64) as i64);
                (dl, None, Some(format!("v{version}")))
            };
            UpdateStatusDisplay {
                version_line,
                status_line: tr::lng_settings_downloading_update(&progress_str),
                detail_line: detail,
                download_fraction: fraction,
                is_error: false,
                is_ready: false,
            }
        }
        UpdateState::Ready { version, .. } => UpdateStatusDisplay {
            version_line,
            status_line: tr::lng_settings_update_ready(),
            detail_line: Some(format!("v{version} — {}", strings::RESTART_TO_UPDATE)),
            download_fraction: None,
            is_error: false,
            is_ready: true,
        },
        UpdateState::Failed { message, .. } => UpdateStatusDisplay {
            version_line,
            status_line: tr::lng_settings_update_fail(),
            detail_line: (!message.is_empty()).then(|| message.clone()),
            download_fraction: None,
            is_error: true,
            is_ready: false,
        },
    }
}

/// Persists updated `auto_update` / `install_beta_version` toggles into [`StartupPrefs`]
/// (`<working_dir>/tdata/startup_prefs.json`) and forwards them to [`UpdateController::update_preferences`].
pub fn propagate_update_preferences(
    working_dir: Option<&Path>,
    auto_update: bool,
    install_beta: bool,
    updater: Option<&dyn UpdatePreferencesSink>,
) -> std::io::Result<()> {
    if let Some(ctrl) = updater {
        ctrl.update_preferences(auto_update, install_beta);
    }
    if let Some(dir) = working_dir
        && !dir.as_os_str().is_empty()
    {
        let mut prefs = StartupPrefs::load_from(dir);
        prefs.auto_update = auto_update;
        prefs.install_beta = install_beta;
        prefs.save_to(dir)?;
    }
    Ok(())
}

pub use tg_app_core::core_types::misc::{
    core_to_transport_proxy, format_proxy_label, transport_to_core_proxy,
};

/// Active sub-panel open inside Advanced settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AdvancedSubPanel {
    /// No sub-panel expanded.
    #[default]
    None,
    /// Connection Type & Proxy list editor (`boxes/connection_box.cpp`).
    ProxyEditor,
    /// Local storage cache breakdown & clear.
    LocalStorage,
    /// Auto-download rules for a specific source (`boxes/auto_download_box.cpp`).
    AutoDownload(AutoDownloadSource),
    /// Export Telegram Data configuration (`export/export_manager.cpp`).
    ExportData,
    /// Power saving granular flags (`settings_power_saving.cpp`).
    PowerSaving,
    /// Manage Hunspell & system dictionaries (`boxes/dictionaries_manager.cpp`).
    Dictionaries,
    /// Debug / Secret codes & layout diagnostics.
    DebugTools,
}

/// Draft form for adding or editing a proxy server entry.
#[derive(Debug, Clone)]
pub struct ProxyFormDraft {
    /// Proxy protocol type.
    pub proxy_type: CoreProxyType,
    /// Server hostname or IP.
    pub host: String,
    /// Port string.
    pub port: String,
    /// Username (SOCKS5/HTTP) or Web base path.
    pub user: String,
    /// Password (SOCKS5/HTTP) or MTProto secret.
    pub password: String,
    /// Paste box for `tg://proxy?...` or `https://t.me/socks?...` links.
    pub link_input: String,
    /// Validation error or status hint.
    pub error: Option<String>,
}

impl Default for ProxyFormDraft {
    fn default() -> Self {
        Self {
            proxy_type: CoreProxyType::Socks5,
            host: String::new(),
            port: "1080".to_owned(),
            user: String::new(),
            password: String::new(),
            link_input: String::new(),
            error: None,
        }
    }
}

impl ProxyFormDraft {
    /// Populates a draft form from an existing [`CoreProxyData`] for editing.
    #[must_use]
    pub fn from_proxy(proxy: &CoreProxyData) -> Self {
        Self {
            proxy_type: proxy.proxy_type,
            host: proxy.host.clone(),
            port: proxy.port.to_string(),
            user: proxy.user.clone(),
            password: proxy.password.clone(),
            link_input: core_to_transport_proxy(proxy)
                .to_tg_link()
                .unwrap_or_default(),
            error: None,
        }
    }

    /// Parses `self.link_input` via [`tg_transport::ProxyData::from_link`] and populates form fields.
    pub fn apply_link(&mut self) -> Result<TransportProxyData, String> {
        let parsed = TransportProxyData::from_link(&self.link_input)
            .map_err(|e| format!("Invalid proxy link: {e}"))?;
        let status = parsed.status();
        if status != ProxyStatus::Valid {
            return Err(format!("Proxy link validation failed: {status:?}"));
        }
        let core = transport_to_core_proxy(&parsed);
        self.proxy_type = core.proxy_type;
        self.host = core.host;
        self.port = core.port.to_string();
        self.user = core.user;
        self.password = core.password;
        self.error = None;
        Ok(parsed)
    }

    /// Validates the form fields using `tg-transport` and returns a [`CoreProxyData`].
    pub fn build_validated(&self) -> Result<CoreProxyData, String> {
        let port = self
            .port
            .trim()
            .parse::<u16>()
            .map_err(|_| "Port must be a number in 1..=65535.".to_owned())?;
        let mut core = CoreProxyData {
            proxy_type: self.proxy_type,
            host: self.host.trim().to_owned(),
            port: if self.proxy_type == CoreProxyType::Web {
                443
            } else {
                u32::from(port)
            },
            user: self.user.trim().to_owned(),
            password: self.password.trim().to_owned(),
        };
        let mut tp = core_to_transport_proxy(&core);
        if self.proxy_type == CoreProxyType::Web {
            let raw_addr = if core.user.is_empty() || core.host.contains('/') {
                core.host.clone()
            } else {
                format!("{}/{}", core.host, core.user)
            };
            tp.set_web_address(&raw_addr);
            core = transport_to_core_proxy(&tp);
        }
        match tp.status() {
            ProxyStatus::Valid => Ok(core),
            ProxyStatus::IncorrectSecret => {
                Err("MTProto secret looks incorrect (check hex/base64url prefix).".to_owned())
            }
            ProxyStatus::Unsupported => Err("Unsupported proxy secret format.".to_owned()),
            ProxyStatus::Invalid => {
                Err("Enter a valid server host, port, and credentials/secret.".to_owned())
            }
        }
    }
}

/// Cloneable + Debuggable slot holding an optional [`AutostartProvider`] (defaults to Windows
/// StartupTask / `HKCU\...\Run`, injectable with `FakeAutostartProvider` for tests).
#[derive(Clone, Default)]
pub struct AutostartProviderSlot(pub Option<Arc<dyn AutostartProvider>>);

impl std::fmt::Debug for AutostartProviderSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AutostartProviderSlot")
            .field(&self.0.is_some())
            .finish()
    }
}

/// Persistent UI state for the Advanced section.
#[derive(Debug, Clone)]
pub struct AdvancedState {
    /// Currently expanded sub-panel.
    pub panel: AdvancedSubPanel,
    /// Draft form for adding or editing a proxy server.
    pub proxy_form: ProxyFormDraft,
    /// Whether the "Add/Edit Proxy" sub-form is open inside the proxy editor.
    pub adding_proxy: bool,
    /// Original proxy entry being edited (`None` when adding a new proxy).
    pub editing_proxy: Option<CoreProxyData>,
    /// Whether `core.proxy` has been synced from `backend.proxy_settings()`.
    pub proxy_synced: bool,
    /// Last proxy settings snapshot observed from `backend.proxy_settings()` so background
    /// proxy rotation or link-confirmed proxy changes refresh `core.proxy` automatically.
    pub last_backend_proxy: Option<tg_core::SettingsProxy>,
    /// Draft export configuration.
    pub export_request: ExportRequest,
    /// Granular power saving mirror (`CoreSettings::power_saving` is authoritative).
    pub power_saving: PowerSavingFlags,
    /// Expand/collapse state for the 3 grouped sections in "Battery and Animations"
    /// (`[Animated Stickers, Animated Emoji, Animations in Chats]`).
    pub power_saving_expanded: [bool; 3],
    /// Cached Windows spellcheck supported languages.
    pub spellcheck_langs: Vec<String>,
    /// State for `Settings -> Advanced -> Manage dictionaries` (`ManageDictionariesBox`).
    pub dictionaries: ManageDictionariesState,
    /// Offline/default dictionary cloud source used when live cloud download is not attached.
    pub dictionary_source: MockDictionaryCloudSource,
    /// Live cloud dictionary source from `SettingsBackend::dictionary_cloud_source`, when the
    /// session provides one (takes precedence over `dictionary_source`).
    pub live_dictionary_source: DictionarySourceSlot,
    /// Injectable OS autostart provider (`tg_platform::autostart::AutostartProvider`).
    pub autostart_provider: AutostartProviderSlot,
    /// Last observed Windows `StartupTaskState` from [`Self::autostart_provider`].
    pub autostart_state: StartupTaskState,
    /// Last autostart toggle error, if any.
    pub autostart_error: Option<String>,
    /// Session window-restore policy (`Ask` / `Always` / `Never`).
    pub restore_windows_policy: RestoreWindowsPolicy,
    /// Whether `restore_windows_policy` has been loaded from `tdata/restore_windows.json`.
    pub restore_windows_loaded: bool,
    /// Whether platform status has been probed.
    pub platform_probed: bool,
    /// Quick secret code input box in Debug Tools.
    pub manual_code_input: String,
    /// Whether custom download path editor is open.
    pub editing_download_path: bool,
}

impl AdvancedState {
    /// Installs an explicit [`AutostartProvider`] (e.g. `FakeAutostartProvider` in tests)
    /// and synchronizes [`Self::autostart_state`] immediately.
    pub fn set_autostart_provider(&mut self, provider: Arc<dyn AutostartProvider>) {
        if let Ok(state) = provider.query_state() {
            self.autostart_state = state;
            self.autostart_error = None;
        }
        self.autostart_provider = AutostartProviderSlot(Some(provider));
    }
}

impl Default for AdvancedState {
    fn default() -> Self {
        Self {
            panel: AdvancedSubPanel::None,
            proxy_form: ProxyFormDraft::default(),
            adding_proxy: false,
            editing_proxy: None,
            proxy_synced: false,
            last_backend_proxy: None,
            export_request: ExportRequest::default(),
            power_saving: PowerSavingFlags::default(),
            power_saving_expanded: [true, true, true],
            spellcheck_langs: Vec::new(),
            dictionaries: ManageDictionariesState::default(),
            dictionary_source: MockDictionaryCloudSource::default(),
            live_dictionary_source: DictionarySourceSlot::default(),
            autostart_provider: AutostartProviderSlot::default(),
            autostart_state: StartupTaskState::Disabled,
            autostart_error: None,
            restore_windows_policy: RestoreWindowsPolicy::Ask,
            restore_windows_loaded: false,
            platform_probed: false,
            manual_code_input: String::new(),
            editing_download_path: false,
        }
    }
}

/// Side-effect outputs emitted by [`render_advanced_section`].
#[derive(Debug, Clone, Default)]
pub struct AdvancedOutput {
    /// Sync simulated connection status with shell.
    pub connection_status: Option<ConnectionStatus>,
    /// Sync proxy enabled indicator with shell.
    pub proxy_enabled: Option<bool>,
    /// User clicked "Check for updates" (`lng_settings_check_now`).
    pub check_for_updates: bool,
    /// User clicked "Update Rustgram" / "Restart to update" when an update is ready.
    pub restart_to_update: bool,
    /// Updated `(auto_update, install_beta_version)` preferences to propagate to
    /// [`StartupPrefs`] and [`UpdateController::update_preferences`].
    pub update_prefs_changed: Option<(bool, bool)>,
    /// Secret code triggered manually from Debug Tools.
    pub secret_code: Option<SecretCodeAction>,
    /// User clicked Export Telegram Data (`AppExportHost::open_full_export`).
    pub open_full_export: bool,
    /// User clicked Experimental settings (`SettingsSection::Experimental`).
    pub open_experimental: bool,
    /// Toast message to display.
    pub toast: Option<String>,
}

fn theme_mode_from_colors(colors: &dyn Colors) -> tg_app_core::stand_in::theme::ThemeMode {
    let bg = colors.color("windowBg");
    let luma = u16::from(bg.r()) + u16::from(bg.g()) + u16::from(bg.b());
    if luma < 384 {
        tg_app_core::stand_in::theme::ThemeMode::Night
    } else {
        tg_app_core::stand_in::theme::ThemeMode::Day
    }
}

/// Vertical skip around dividers and titles (`Builder::addSkip`).
const SECTION_SKIP: f32 = 8.0;
/// `st::settings_checkboxes_skip`: the skip that ends the data and auto-download sections.
const CHECKBOXES_SKIP: f32 = 12.0;

/// Renders the Advanced settings section (`settings_advanced.cpp`).
#[allow(clippy::too_many_arguments)]
pub fn render_advanced_section(
    ui: &mut Ui,
    state: &mut AdvancedState,
    core: &mut CoreSettings,
    global: &mut GlobalConfig,
    debug_opts: &mut DebugOptions,
    update_snapshot: &UpdateSnapshot,
    connection_status: ConnectionStatus,
    layout: ColumnLayout,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
) -> AdvancedOutput {
    let mut out = AdvancedOutput::default();
    ui.spacing_mut().item_spacing.y = 0.0;

    if !state.platform_probed {
        state.platform_probed = true;
        if let Ok(langs) = supported_languages() {
            state.spellcheck_langs = langs;
        }
        let provider = resolve_autostart_provider(&state.autostart_provider, &global.working_dir);
        if let Ok(st) = provider.query_state() {
            state.autostart_state = st;
            global.auto_start = st.is_enabled();
        }
        if let Ok(send_to_dir) = send_to_folder_dir() {
            global.send_to_menu = is_send_to_enabled_in(&send_to_dir, "Rustgram.lnk");
        }
    }

    if let Some(backend_proxy) = backend.proxy_settings() {
        let externally_updated = if !state.proxy_synced {
            state.proxy_synced = true;
            backend.is_live_proxy_backend() || backend_proxy != tg_core::SettingsProxy::new()
        } else {
            state.last_backend_proxy.as_ref() != Some(&backend_proxy)
        };
        if externally_updated {
            core.proxy = backend_proxy.clone();
        }
        state.last_backend_proxy = Some(backend_proxy);
    }

    // Match `settings_advanced.cpp` (`kMeta`): promote Version & Updates to the top when
    // automatic updates are disabled or a verified update is ready to install.
    let update_at_top = update_snapshot.is_ready() || !global.auto_update;
    if update_at_top {
        render_update_section(ui, global, update_snapshot, colors, &mut out);
        settings_divider(ui, None, colors);
    }

    // 1. Data and storage (`BuildDataStorageSection`): Connection type, Download path,
    // Manage local storage, Downloads, Ask download path.
    ui.add_space(SETTINGS_LIST_SKIP);
    subsection_title(ui, &tr::lng_settings_data_storage(), colors);

    // `connectionType` in `settings_advanced.cpp`: "Default (TCP used)" unless a proxy is
    // enabled ("TCP with proxy"); the system-proxy mode counts as not enabled.
    let transport = "TCP";
    let conn_label = if core.proxy.settings() == ProxySettingsMode::Enabled
        && core.proxy.selected().is_valid()
    {
        tr::lng_connection_proxy(transport)
    } else {
        tr::lng_connection_auto(transport)
    };
    if settings_row(
        ui,
        &tr::lng_settings_connection_type(),
        None,
        Some(&conn_label),
        Some(SettingsIconKind::Network),
        false,
        false,
        colors,
    )
    .clicked()
    {
        toggle_panel(&mut state.panel, AdvancedSubPanel::ProxyEditor);
        if state.panel == AdvancedSubPanel::ProxyEditor {
            // Auto-check untested proxies on opening the connection box, matching tdesktop.
            for item in core.proxy.list() {
                if matches!(
                    backend.proxy_check_status(item),
                    ProxyCheckStatus::NotTested
                ) {
                    backend.check_proxy(item);
                }
            }
        }
    }

    if state.panel == AdvancedSubPanel::ProxyEditor {
        ui.add_space(4.0);
        render_proxy_editor(
            ui,
            state,
            core,
            connection_status,
            backend,
            colors,
            &mut out,
        );
        ui.add_space(4.0);
    }

    let dl_path_label = if core.download_path.is_empty() {
        tr::lng_download_path_default()
    } else {
        core.download_path.clone()
    };
    if !core.ask_download_path
        && settings_row(
            ui,
            &tr::lng_download_path(),
            None,
            Some(&dl_path_label),
            Some(SettingsIconKind::Folder),
            false,
            false,
            colors,
        )
        .clicked()
    {
        state.editing_download_path = !state.editing_download_path;
    }
    if state.editing_download_path && !core.ask_download_path {
        ui.add_space(4.0);
        settings_card(ui, colors, |ui| {
            InputField::single_line(
                "settings_download_path",
                &mut core.download_path,
                strings::DOWNLOAD_PATH_PH,
                InputFieldStyle::default_input(colors),
            )
            .show(ui);
        });
    }

    let manage_storage_lbl = tr::lng_settings_manage_local_storage();
    if settings_row(
        ui,
        &manage_storage_lbl,
        None,
        None,
        Some(SettingsIconKind::LocalStorage),
        false,
        false,
        colors,
    )
    .clicked()
    {
        toggle_panel(&mut state.panel, AdvancedSubPanel::LocalStorage);
    }

    if state.panel == AdvancedSubPanel::LocalStorage {
        ui.add_space(4.0);
        render_local_storage_panel(ui, backend, colors, &mut out);
        ui.add_space(4.0);
    }

    if settings_row(
        ui,
        &tr::lng_downloads_section(),
        None,
        None,
        Some(SettingsIconKind::Download),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.toast = Some(tr::lng_downloads_section());
    }

    settings_toggle_row(
        ui,
        strings::ASK_DOWNLOAD_PATH,
        None,
        None,
        &mut core.ask_download_path,
        true,
        colors,
    );

    if state.panel == AdvancedSubPanel::ExportData {
        ui.add_space(4.0);
        render_export_panel(ui, state, backend, colors, &mut out);
        ui.add_space(4.0);
    }

    ui.add_space(CHECKBOXES_SKIP);
    settings_section_divider(ui, colors);
    ui.add_space(SETTINGS_LIST_SKIP);

    // 2. Automatic media download (`BuildAutoDownloadSection`): per-source split toggles.
    subsection_title(ui, &tr::lng_media_auto_settings(), colors);
    let mut ad = backend.auto_download();
    for source in AutoDownloadSource::ALL {
        let mut enabled = ad.has_enabled_types(source);
        let (title, icon) = match source {
            AutoDownloadSource::Private => {
                (tr::lng_media_auto_in_private(), SettingsIconKind::Account)
            }
            AutoDownloadSource::Groups => {
                (tr::lng_media_auto_in_groups(), SettingsIconKind::Groups)
            }
            AutoDownloadSource::Channels => {
                (tr::lng_media_auto_in_channels(), SettingsIconKind::Channel)
            }
        };
        let (clicked, toggled) =
            settings_split_toggle_row(ui, &title, "", Some(icon), &mut enabled, colors);
        if toggled {
            if enabled {
                ad.set_defaults_for_source(source);
            } else {
                ad.set_disabled_for_source(source);
            }
            backend.set_auto_download(ad.clone());
        }
        if clicked {
            toggle_panel(&mut state.panel, AdvancedSubPanel::AutoDownload(source));
        }
        if state.panel == AdvancedSubPanel::AutoDownload(source) {
            ui.add_space(4.0);
            render_auto_download_editor(ui, source, &mut ad, backend, colors, &mut out);
            ui.add_space(4.0);
        }
    }

    ui.add_space(CHECKBOXES_SKIP);
    settings_section_divider(ui, colors);
    ui.add_space(SETTINGS_LIST_SKIP);

    // 3. Window title bar (`BuildWindowTitleSection`): checkboxes.
    subsection_title(ui, &tr::lng_settings_window_system(), colors);
    let mut show_chat_name = !core.window_title_content.hide_chat_name;
    if settings_checkbox_row(
        ui,
        &mut show_chat_name,
        &tr::lng_settings_title_chat_name(),
        true,
        colors,
    )
    .changed()
    {
        core.window_title_content.hide_chat_name = !show_chat_name;
    }
    let mut show_account_name = !core.window_title_content.hide_account_name;
    // tdesktop adds this checkbox only with more than one account.
    if core.accounts_order.len() > 1
        && settings_checkbox_row(
            ui,
            &mut show_account_name,
            strings::WINDOW_TITLE_ACCOUNT_NAME,
            true,
            colors,
        )
        .changed()
    {
        core.window_title_content.hide_account_name = !show_account_name;
    }
    let mut show_total_unread = !core.window_title_content.hide_total_unread;
    if settings_checkbox_row(
        ui,
        &mut show_total_unread,
        &tr::lng_settings_title_total_count(),
        true,
        colors,
    )
    .changed()
    {
        core.window_title_content.hide_total_unread = !show_total_unread;
    }
    let native_frame_lbl = tr::lng_settings_native_frame();
    settings_checkbox_row(
        ui,
        &mut core.native_window_frame,
        &native_frame_lbl,
        true,
        colors,
    );

    ui.add_space(SETTINGS_LIST_SKIP);
    settings_section_divider(ui, colors);
    ui.add_space(SETTINGS_LIST_SKIP);

    // 4. System integration (`BuildSystemIntegrationSection`): checkboxes.
    subsection_title(ui, &tr::lng_settings_native_title(), colors);

    let mut show_tray = matches!(core.work_mode, WorkMode::WindowAndTray | WorkMode::TrayOnly);
    let mut show_taskbar = matches!(
        core.work_mode,
        WorkMode::WindowAndTray | WorkMode::WindowOnly
    );
    let tray_lbl = tr::lng_settings_workmode_tray();
    let tray_changed = settings_checkbox_row(ui, &mut show_tray, &tray_lbl, true, colors).changed();
    let taskbar_lbl = tr::lng_settings_workmode_window();
    let taskbar_changed =
        settings_checkbox_row(ui, &mut show_taskbar, &taskbar_lbl, true, colors).changed();
    if tray_changed || taskbar_changed {
        core.work_mode = match (show_taskbar, show_tray) {
            (true, true) => WorkMode::WindowAndTray,
            (false, true) => WorkMode::TrayOnly,
            (true, false) => WorkMode::WindowOnly,
            (false, false) => WorkMode::WindowAndTray,
        };
    }

    settings_checkbox_row(
        ui,
        &mut core.tray_icon_monochrome,
        &tr::lng_settings_monochrome_icon(),
        show_tray,
        colors,
    );

    let pad_x = settings_row_pad_x();
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        ui.label(
            egui::RichText::new(format!("{}:", tr::lng_settings_window_close()))
                .font(font_id_from_style(st::normal_font))
                .color(colors.color("windowSubTextFg")),
        );
    });
    settings_radio_row(
        ui,
        &mut core.close_behavior,
        CloseBehavior::Quit,
        strings::CLOSE_BEHAVIOR_QUIT,
        true,
        colors,
    );
    settings_radio_row(
        ui,
        &mut core.close_behavior,
        CloseBehavior::CloseToTaskbar,
        &tr::lng_settings_close_to_taskbar(),
        true,
        colors,
    );
    settings_radio_row(
        ui,
        &mut core.close_behavior,
        CloseBehavior::RunInBackground,
        strings::CLOSE_BEHAVIOR_BACKGROUND,
        true,
        colors,
    );

    let autostart_lbl = tr::lng_settings_auto_start();
    let _autostart_hint = match state.autostart_state {
        StartupTaskState::DisabledByUser => "Disabled in Windows Startup Apps settings",
        StartupTaskState::DisabledByPolicy => "Disabled by your organization's policy",
        StartupTaskState::EnabledByPolicy => "Enabled by your organization's policy",
        StartupTaskState::Enabled | StartupTaskState::Disabled => {
            "Start automatically when you sign in to Windows"
        }
    };
    let can_toggle_autostart = state.autostart_state.can_toggle();
    let autostart_resp = settings_checkbox_row(
        ui,
        &mut global.auto_start,
        &autostart_lbl,
        can_toggle_autostart,
        colors,
    );
    if autostart_resp.changed() {
        let provider = resolve_autostart_provider(&state.autostart_provider, &global.working_dir);
        let desired = global.auto_start;
        match provider.set_enabled(desired) {
            Ok(new_state) => {
                state.autostart_state = new_state;
                global.auto_start = new_state.is_enabled();
                if !global.auto_start {
                    global.start_minimized = false;
                }
                state.autostart_error = None;
                out.toast = Some(match new_state {
                    StartupTaskState::Enabled | StartupTaskState::EnabledByPolicy => {
                        strings::AUTOSTART_CREATED_TOAST.to_owned()
                    }
                    StartupTaskState::Disabled | StartupTaskState::DisabledByPolicy => {
                        strings::AUTOSTART_REMOVED_TOAST.to_owned()
                    }
                    StartupTaskState::DisabledByUser => {
                        "Autostart is disabled in Windows Startup Apps settings.".to_owned()
                    }
                });
            }
            Err(err) => {
                global.auto_start = state.autostart_state.is_enabled();
                let msg = err.to_string();
                state.autostart_error = Some(msg.clone());
                out.toast = Some(format!("Couldn't update startup setting: {msg}"));
            }
        }
    }

    if state.autostart_state == StartupTaskState::DisabledByUser {
        ui.add_space(4.0);
        settings_card(ui, colors, |ui| {
            ui.add(
                Label::new(
                    egui::RichText::new(tr::lng_settings_auto_start_disabled_uwp())
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                )
                .wrap(),
            );
            ui.add_space(8.0);
            if RoundButton::new(
                "Open Startup Apps Settings",
                RoundButtonStyle::default_light(colors),
            )
            .show(ui)
            .clicked()
            {
                let provider =
                    resolve_autostart_provider(&state.autostart_provider, &global.working_dir);
                match provider.open_startup_settings() {
                    Ok(()) => {
                        out.toast = Some("Opened Windows Startup Apps settings.".to_owned());
                    }
                    Err(err) => {
                        out.toast = Some(format!("Couldn't open Startup Apps settings: {err}"));
                    }
                }
            }
        });
        ui.add_space(4.0);
    } else if let Some(err) = state.autostart_error.clone() {
        ui.add_space(2.0);
        egui::Frame::NONE
            .inner_margin(egui::Margin {
                left: pad_x as i8,
                right: pad_x as i8,
                top: 0,
                bottom: 4,
            })
            .show(ui, |ui| {
                ui.add(
                    Label::new(
                        egui::RichText::new(err)
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("attentionButtonFg")),
                    )
                    .wrap(),
                );
            });
    }

    let start_min_lbl = tr::lng_settings_start_min();
    settings_checkbox_row(
        ui,
        &mut global.start_minimized,
        &start_min_lbl,
        global.auto_start,
        colors,
    );

    let send_to_lbl = tr::lng_settings_add_sendto();
    if settings_checkbox_row(ui, &mut global.send_to_menu, &send_to_lbl, true, colors).changed() {
        let res = apply_send_to_shortcut(global.send_to_menu);
        out.toast = Some(match res {
            Ok(()) => strings::SEND_TO_UPDATED_TOAST.to_owned(),
            Err(err) => format!("Preference saved (SendTo shortcut: {err})"),
        });
    }

    if !state.restore_windows_loaded && !global.working_dir.as_os_str().is_empty() {
        state.restore_windows_policy = SavedWindowsSnapshot::load_from(&global.working_dir).policy;
        state.restore_windows_loaded = true;
    }
    let restore_val = match state.restore_windows_policy {
        RestoreWindowsPolicy::Ask => "Ask",
        RestoreWindowsPolicy::Always => "Always",
        RestoreWindowsPolicy::Never => "Never",
    };
    if settings_row(
        ui,
        "Restore windows on launch",
        Some("Reopen saved secondary chat windows after restart"),
        Some(restore_val),
        None,
        false,
        false,
        colors,
    )
    .clicked()
    {
        state.restore_windows_policy = match state.restore_windows_policy {
            RestoreWindowsPolicy::Ask => RestoreWindowsPolicy::Always,
            RestoreWindowsPolicy::Always => RestoreWindowsPolicy::Never,
            RestoreWindowsPolicy::Never => RestoreWindowsPolicy::Ask,
        };
        if !global.working_dir.as_os_str().is_empty() {
            let mut snap = SavedWindowsSnapshot::load_from(&global.working_dir);
            snap.policy = state.restore_windows_policy;
            let _ = snap.save_to(&global.working_dir);
        }
        out.toast = Some(match state.restore_windows_policy {
            RestoreWindowsPolicy::Always => tr::lng_restore_windows_enabled_toast(),
            RestoreWindowsPolicy::Never => tr::lng_restore_windows_disabled_toast(),
            RestoreWindowsPolicy::Ask => "Will ask to restore windows on launch.".to_owned(),
        });
    }

    ui.add_space(SECTION_SKIP);
    settings_divider(ui, None, colors);
    ui.add_space(SECTION_SKIP);

    // 5. Performance (`BuildPerformanceSection`).
    subsection_title(ui, &tr::lng_settings_performance(), colors);
    let power_summary = power_saving_summary(core);
    let power_menu_lbl = tr::lng_settings_power_menu();
    if settings_row(
        ui,
        &power_menu_lbl,
        None,
        Some(&power_summary),
        Some(SettingsIconKind::Power),
        false,
        false,
        colors,
    )
    .clicked()
    {
        toggle_panel(&mut state.panel, AdvancedSubPanel::PowerSaving);
    }

    if state.panel == AdvancedSubPanel::PowerSaving {
        ui.add_space(4.0);
        render_power_saving_panel(
            ui,
            &mut state.power_saving,
            &mut state.power_saving_expanded,
            core,
            colors,
        );
        ui.add_space(4.0);
    }

    settings_toggle_row(
        ui,
        strings::HARDWARE_VIDEO_DECODING,
        None,
        None,
        &mut core.hardware_accelerated_video,
        true,
        colors,
    );
    settings_toggle_row(
        ui,
        strings::DISABLE_OPENGL,
        None,
        None,
        &mut core.disable_opengl,
        true,
        colors,
    );

    ui.add_space(SECTION_SKIP);
    settings_divider(ui, None, colors);
    ui.add_space(SECTION_SKIP);

    // 5. Spell Checker (`tg-platform::spellcheck`).
    subsection_title(ui, &tr::lng_settings_spellchecker(), colors);
    let spell_supported = is_system_spellchecker_supported();
    let spell_sub = if spell_supported {
        if state.spellcheck_langs.is_empty() {
            strings::SPELLCHECKER_AVAILABLE.to_owned()
        } else {
            let preview: Vec<&str> = state
                .spellcheck_langs
                .iter()
                .take(5)
                .map(String::as_str)
                .collect();
            format!(
                "Windows ISpellChecker ({} languages: {})",
                state.spellcheck_langs.len(),
                preview.join(", ")
            )
        }
    } else {
        strings::SPELLCHECKER_UNAVAILABLE.to_owned()
    };
    settings_toggle_row(
        ui,
        strings::USE_SYSTEM_SPELLCHECKER,
        Some(&spell_sub),
        None,
        &mut core.spellchecker_enabled,
        spell_supported,
        colors,
    );
    let auto_dict_lbl = tr::lng_settings_auto_download_dictionaries();
    settings_toggle_row(
        ui,
        &auto_dict_lbl,
        None,
        None,
        &mut core.auto_download_dictionaries,
        core.spellchecker_enabled,
        colors,
    );

    let manage_dicts_lbl = tr::lng_settings_manage_dictionaries();
    let dicts_badge = if core.dictionaries_enabled.is_empty() {
        None
    } else {
        Some(core.dictionaries_enabled.len().to_string())
    };
    if settings_row(
        ui,
        &manage_dicts_lbl,
        Some("Hunspell & system spellcheck dictionaries"),
        dicts_badge.as_deref(),
        None,
        false,
        true,
        colors,
    )
    .clicked()
    {
        if !global.working_dir.as_os_str().is_empty() {
            state.dictionaries.dictionaries_dir = dictionaries_root(&global.working_dir);
        }
        toggle_panel(&mut state.panel, AdvancedSubPanel::Dictionaries);
    }

    if state.panel == AdvancedSubPanel::Dictionaries {
        ui.add_space(4.0);
        if !state.live_dictionary_source.is_attached()
            && let Some(source) = backend.dictionary_cloud_source()
        {
            state.live_dictionary_source = DictionarySourceSlot::new(source);
        }
        let live = state.live_dictionary_source.clone();
        let toast = live
            .with(|source| {
                render_manage_dictionaries_panel(
                    ui,
                    &mut state.dictionaries,
                    &state.spellcheck_langs,
                    core,
                    source,
                    colors,
                )
            })
            .unwrap_or_else(|| {
                render_manage_dictionaries_panel(
                    ui,
                    &mut state.dictionaries,
                    &state.spellcheck_langs,
                    core,
                    &mut state.dictionary_source,
                    colors,
                )
            });
        if let Some(msg) = toast {
            out.toast = Some(msg);
        }
    }

    settings_divider(ui, None, colors);

    // Export Telegram data (`BuildExportSection`).
    if settings_row(
        ui,
        &tr::lng_settings_export_data(),
        None,
        None,
        Some(SettingsIconKind::Export),
        false,
        false,
        colors,
    )
    .clicked()
    {
        crate::export::AppExportHost::open_full_export(ui.ctx(), theme_mode_from_colors(colors));
        out.open_full_export = true;
    }
    ui.add_space(SECTION_SKIP);
    settings_divider(ui, None, colors);
    ui.add_space(SECTION_SKIP);

    // 6. Version & Updates (`SetupUpdate` in `settings_advanced.cpp`) + Debug / Secret Codes.
    if !update_at_top {
        render_update_section(ui, global, update_snapshot, colors, &mut out);
        settings_divider(ui, None, colors);
    }

    let exp_lbl = tr::lng_settings_experimental();
    if settings_row(
        ui,
        &exp_lbl,
        Some(strings::EXPERIMENTAL_DEBUG_SUBTITLE),
        None,
        Some(SettingsIconKind::Debug),
        false,
        true,
        colors,
    )
    .clicked()
    {
        out.open_experimental = true;
    }

    if settings_row(
        ui,
        "Debug tools & secret codes",
        None,
        None,
        None,
        false,
        true,
        colors,
    )
    .clicked()
    {
        toggle_panel(&mut state.panel, AdvancedSubPanel::DebugTools);
    }

    if state.panel == AdvancedSubPanel::DebugTools {
        ui.add_space(4.0);
        render_debug_tools(
            ui, state, core, global, debug_opts, layout, colors, &mut out,
        );
    }

    ui.add_space(16.0);
    out
}

/// Renders the "Version and updates" block (`BuildUpdateSection` / `SetupUpdate` in
/// `settings/sections/settings_advanced.cpp`).
fn render_update_section(
    ui: &mut Ui,
    global: &mut GlobalConfig,
    update_snapshot: &UpdateSnapshot,
    colors: &dyn Colors,
    out: &mut AdvancedOutput,
) {
    let title = tr::lng_settings_version_info();
    subsection_title(ui, &title, colors);

    let display = format_update_status(update_snapshot);
    let pad_x = settings_row_pad_x();

    // Version & live status card.
    settings_card(ui, colors, |ui| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&display.version_line)
                        .font(font_id_from_style(st::semibold_font))
                        .color(colors.color("windowBoldFg")),
                );
                if update_snapshot.allow_beta || global.install_beta_version {
                    ui.add_space(4.0);
                    paint_badge_pill(ui, "BETA", colors.color("windowBgActive"), Color32::WHITE);
                }
                if display.is_ready {
                    ui.add_space(4.0);
                    paint_badge_pill(ui, "READY", colors.color("settingsIconBg2"), Color32::WHITE);
                }
            });

            ui.add_space(4.0);
            let status_color = if display.is_error {
                colors.color("boxTextFgError")
            } else if display.is_ready {
                colors.color("windowActiveTextFg")
            } else {
                colors.color("windowSubTextFg")
            };
            ui.label(
                egui::RichText::new(&display.status_line)
                    .font(font_id_from_style(st::normal_font))
                    .color(status_color),
            );

            if let Some(detail) = &display.detail_line {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(detail)
                        .font(egui::FontId::new(12.0, tg_ui::fonts::family_regular()))
                        .color(if display.is_error {
                            colors.color("boxTextFgError")
                        } else {
                            colors.color("windowSubTextFg")
                        }),
                );
            }

            if let Some(frac) = display.download_fraction {
                ui.add_space(6.0);
                let bar_w = ui.available_width().max(120.0);
                let (bar_rect, _) = ui.allocate_exact_size(Vec2::new(bar_w, 6.0), Sense::hover());
                ui.painter()
                    .rect_filled(bar_rect, CornerRadius::same(3), colors.color("shadowFg"));
                let fill_rect = Rect::from_min_size(
                    bar_rect.min,
                    Vec2::new((bar_w * frac.clamp(0.0, 1.0)).max(4.0), 6.0),
                );
                ui.painter().rect_filled(
                    fill_rect,
                    CornerRadius::same(3),
                    colors.color("windowBgActive"),
                );
            }
        });
    });

    ui.add_space(4.0);

    let auto_upd_lbl = tr::lng_settings_update_automatically();
    let auto_toggled = settings_toggle_row(
        ui,
        &auto_upd_lbl,
        Some(&display.status_line),
        None,
        &mut global.auto_update,
        !update_snapshot.disabled_by_cli,
        colors,
    );

    let beta_lbl = tr::lng_settings_install_beta();
    let beta_enabled =
        global.auto_update && !update_snapshot.disabled_by_cli && !update_snapshot.is_busy();
    let beta_toggled = settings_toggle_row(
        ui,
        &beta_lbl,
        None,
        None,
        &mut global.install_beta_version,
        beta_enabled,
        colors,
    );

    if auto_toggled || beta_toggled {
        out.update_prefs_changed = Some((global.auto_update, global.install_beta_version));
    }

    ui.add_space(4.0);
    if let Some(ready_ver) = update_snapshot.ready_version() {
        // Highlighted "Update Rustgram" / "Restart to update" banner row (`st::settingsUpdate`).
        let update_title = format!("{} (v{ready_ver})", localized_update_tgfork());
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            let w = (ui.available_width() - pad_x).max(200.0);
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 48.0), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let active_bg = colors.color("windowBgActive");
            let fill = if resp.hovered() {
                Color32::from_rgb(
                    active_bg.r().saturating_add(12),
                    active_bg.g().saturating_add(12),
                    active_bg.b().saturating_add(12),
                )
            } else {
                active_bg
            };
            ui.painter().rect_filled(rect, CornerRadius::same(8), fill);

            let badge_center = Pos2::new(rect.left() + 24.0, rect.center().y);
            ui.painter().circle_filled(
                badge_center,
                12.0,
                Color32::from_rgba_unmultiplied(255, 255, 255, 48),
            );
            tg_app_core::ui_icons::paint_arrow(
                ui.painter(),
                badge_center,
                14.0,
                tg_app_core::ui_icons::ArrowDir::Up,
                Color32::WHITE,
            );
            ui.painter().text(
                Pos2::new(rect.left() + 46.0, rect.top() + 9.0),
                Align2::LEFT_TOP,
                &update_title,
                font_id_from_style(st::semibold_font),
                Color32::WHITE,
            );
            ui.painter().text(
                Pos2::new(rect.left() + 46.0, rect.top() + 27.0),
                Align2::LEFT_TOP,
                strings::RESTART_TO_UPDATE,
                egui::FontId::new(12.0, tg_ui::fonts::family_regular()),
                Color32::from_rgba_unmultiplied(255, 255, 255, 220),
            );
            if resp.clicked() {
                out.restart_to_update = true;
            }
        });
    } else if update_snapshot.can_check_now() {
        let check_lbl = tr::lng_settings_check_now();
        if settings_row(ui, &check_lbl, None, None, None, false, false, colors).clicked() {
            out.check_for_updates = true;
        }
    } else {
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            let check_lbl = tr::lng_settings_check_now();
            ui.label(
                egui::RichText::new(check_lbl)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
        });
    }
}

fn toggle_panel(slot: &mut AdvancedSubPanel, target: AdvancedSubPanel) {
    if *slot == target {
        *slot = AdvancedSubPanel::None;
    } else {
        *slot = target;
    }
}

fn render_proxy_editor(
    ui: &mut Ui,
    state: &mut AdvancedState,
    core: &mut CoreSettings,
    connection_status: ConnectionStatus,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
    out: &mut AdvancedOutput,
) {
    let mut proxy_dirty = false;
    settings_card(ui, colors, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(strings::PROXY_EDITOR_TITLE)
                    .font(font_id_from_style(st::box_title_font))
                    .color(colors.color("windowBoldFg")),
            );
            let proxy_active = core.proxy.is_enabled();
            let indicator =
                format_proxy_connection_status(connection_status, proxy_active, &EnglishStrings);
            if !indicator.is_empty() {
                ui.add_space(8.0);
                let badge_bg = if matches!(connection_status, ConnectionStatus::Connected) {
                    colors.color("settingsIconBg2")
                } else {
                    colors.color("windowBgActive")
                };
                paint_badge_pill(ui, &indicator, badge_bg, Color32::WHITE);
            }
        });
        ui.add_space(6.0);

        let mut mode = core.proxy.settings();
        let mode_changed = settings_radio_row(
            ui,
            &mut mode,
            ProxySettingsMode::Disabled,
            strings::PROXY_DISABLE_DIRECT,
            true,
            colors,
        )
        .changed()
            || settings_radio_row(
                ui,
                &mut mode,
                ProxySettingsMode::System,
                strings::PROXY_USE_SYSTEM,
                true,
                colors,
            )
            .changed()
            || settings_radio_row(
                ui,
                &mut mode,
                ProxySettingsMode::Enabled,
                strings::PROXY_USE_CUSTOM,
                true,
                colors,
            )
            .changed();
        if mode_changed {
            if mode == ProxySettingsMode::Enabled && core.proxy.list().is_empty() {
                state.adding_proxy = true;
                state.editing_proxy = None;
                state.proxy_form = ProxyFormDraft::default();
            } else {
                if mode == ProxySettingsMode::Enabled
                    && !core.proxy.selected().is_valid()
                    && let Some(last_valid) = core
                        .proxy
                        .list()
                        .iter()
                        .rev()
                        .find(|p| p.is_valid())
                        .cloned()
                {
                    core.proxy.set_selected(last_valid);
                }
                core.proxy.set_settings(mode);
                out.proxy_enabled = Some(mode == ProxySettingsMode::Enabled);
                proxy_dirty = true;
            }
        }

        ui.add_space(6.0);
        let mut try_ipv6 = core.proxy.try_ipv6();
        if settings_checkbox_row(
            ui,
            &mut try_ipv6,
            &tr::lng_connection_try_ipv6(),
            true,
            colors,
        )
        .changed()
        {
            core.proxy.set_try_ipv6(try_ipv6);
            proxy_dirty = true;
        }
        let mut use_for_calls = core.proxy.use_proxy_for_calls();
        if settings_checkbox_row(
            ui,
            &mut use_for_calls,
            strings::PROXY_USE_FOR_CALLS,
            true,
            colors,
        )
        .changed()
        {
            core.proxy.set_use_proxy_for_calls(use_for_calls);
            proxy_dirty = true;
        }
        let mut rotation = core.proxy.proxy_rotation_enabled();
        if settings_checkbox_row(
            ui,
            &mut rotation,
            strings::PROXY_AUTO_ROTATION,
            true,
            colors,
        )
        .changed()
        {
            core.proxy.set_proxy_rotation_enabled(rotation);
            proxy_dirty = true;
        }
        if rotation {
            ui.horizontal(|ui| {
                ui.add_space(settings_row_pad_x());
                ui.label(
                    egui::RichText::new(strings::PROXY_ROTATION_TIMEOUT)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                );
                for &secs in &PROXY_ROTATION_TIMEOUTS {
                    if ui
                        .selectable_label(
                            core.proxy.proxy_rotation_timeout() == secs,
                            format!("{secs}s"),
                        )
                        .clicked()
                    {
                        core.proxy.set_proxy_rotation_timeout(secs);
                        proxy_dirty = true;
                    }
                }
            });
        }

        // Saved proxy list.
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("Saved proxies ({})", core.proxy.list().len()))
                    .size(13.0)
                    .strong()
                    .color(colors.color("windowActiveTextFg")),
            );
            if !core.proxy.list().is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(strings::PROXY_CHECK_ALL_BTN).clicked() {
                        for item in core.proxy.list() {
                            backend.check_proxy(item);
                        }
                    }
                });
            }
        });
        let list_snapshot = core.proxy.list().to_vec();
        let selected_snapshot = core.proxy.selected().clone();
        let mut select_proxy: Option<CoreProxyData> = None;
        let mut edit_proxy: Option<CoreProxyData> = None;
        let mut remove_proxy: Option<CoreProxyData> = None;
        let mut check_one: Option<CoreProxyData> = None;

        if list_snapshot.is_empty() {
            ui.label(
                egui::RichText::new(strings::PROXY_NO_CUSTOM)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
        } else {
            let del_lbl = tr::lng_filters_remove_yes();
            let is_connected = matches!(connection_status, ConnectionStatus::Connected);
            for item in &list_snapshot {
                let is_sel = *item == selected_snapshot && core.proxy.is_enabled();
                let check_status = backend.proxy_check_status(item);
                let status_line = check_status.format_label(is_sel && is_connected);
                let status_color = match check_status {
                    ProxyCheckStatus::Available { .. } => colors.color("settingsIconBg2"),
                    ProxyCheckStatus::Unavailable { .. } => colors.color("boxTextFgError"),
                    ProxyCheckStatus::Checking => colors.color("windowActiveTextFg"),
                    ProxyCheckStatus::NotTested => colors.color("windowSubTextFg"),
                };
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        if ui
                            .selectable_label(is_sel, format_proxy_label(item))
                            .clicked()
                        {
                            select_proxy = Some(item.clone());
                        }
                        ui.label(
                            egui::RichText::new(status_line)
                                .font(egui::FontId::new(11.5, tg_ui::fonts::family_regular()))
                                .color(status_color),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(&del_lbl).clicked() {
                            remove_proxy = Some(item.clone());
                        }
                        if ui.small_button(strings::PROXY_EDIT_BTN).clicked() {
                            edit_proxy = Some(item.clone());
                        }
                        if let Some(tg_link) = core_to_transport_proxy(item).to_tg_link() {
                            let https_link = tg_link.replacen("tg://", "https://t.me/", 1);
                            if ui.small_button(strings::PROXY_SHARE_BTN).clicked() {
                                ui.ctx().copy_text(https_link.clone());
                                out.toast =
                                    Some(format!("{}: {https_link}", strings::PROXY_SHARED_TOAST));
                            }
                            if ui.small_button(strings::PROXY_COPY_LINK).clicked() {
                                ui.ctx().copy_text(tg_link.clone());
                                out.toast =
                                    Some(format!("{}: {tg_link}", strings::PROXY_COPY_LINK));
                            }
                        }
                        if ui.small_button(strings::PROXY_CHECK_BTN).clicked() {
                            check_one = Some(item.clone());
                        }
                    });
                });
                ui.add_space(2.0);
            }
        }
        if let Some(p) = check_one {
            backend.check_proxy(&p);
        }
        if let Some(p) = select_proxy {
            core.proxy.set_selected(p.clone());
            core.proxy.set_settings(ProxySettingsMode::Enabled);
            out.proxy_enabled = Some(true);
            backend.check_proxy(&p);
            proxy_dirty = true;
        }
        if let Some(p) = edit_proxy {
            state.proxy_form = ProxyFormDraft::from_proxy(&p);
            state.editing_proxy = Some(p);
            state.adding_proxy = true;
        }
        if let Some(p) = remove_proxy {
            core.proxy.remove_from_list(&p);
            if state.editing_proxy.as_ref() == Some(&p) {
                state.editing_proxy = None;
                state.adding_proxy = false;
                state.proxy_form = ProxyFormDraft::default();
            }
            if *core.proxy.selected() == p {
                core.proxy.set_selected(CoreProxyData::default());
                core.proxy.set_settings(ProxySettingsMode::Disabled);
                out.proxy_enabled = Some(false);
            }
            out.toast = Some(strings::PROXY_REMOVED_TOAST.to_owned());
            proxy_dirty = true;
        }

        ui.add_space(8.0);
        let toggle_form_label = if state.adding_proxy {
            if state.editing_proxy.is_some() {
                strings::PROXY_CANCEL_EDIT_BTN
            } else {
                strings::PROXY_HIDE_ADD_BTN
            }
        } else {
            strings::PROXY_ADD_BTN
        };
        if RoundButton::new(toggle_form_label, RoundButtonStyle::default_active(colors))
            .show(ui)
            .clicked()
        {
            if state.adding_proxy {
                state.adding_proxy = false;
                state.editing_proxy = None;
                state.proxy_form = ProxyFormDraft::default();
            } else {
                state.adding_proxy = true;
                state.editing_proxy = None;
                state.proxy_form = ProxyFormDraft::default();
            }
        }

        if state.adding_proxy {
            ui.add_space(8.0);
            if state.editing_proxy.is_some() {
                ui.label(
                    egui::RichText::new(strings::PROXY_EDIT_TITLE)
                        .font(font_id_from_style(st::semibold_font))
                        .color(colors.color("windowBoldFg")),
                );
                ui.add_space(4.0);
            }
            ui.label(
                egui::RichText::new(strings::PROXY_IMPORT_HINT)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
            ui.horizontal(|ui| {
                InputField::single_line(
                    "proxy_link_paste",
                    &mut state.proxy_form.link_input,
                    "tg://proxy?server=...&port=443&secret=...",
                    InputFieldStyle::default_input(colors),
                )
                .width(240.0)
                .show(ui);
                ui.add_space(6.0);
                if RoundButton::new(
                    strings::PROXY_PARSE_LINK_BTN,
                    RoundButtonStyle::default_light(colors),
                )
                .show(ui)
                .clicked()
                {
                    match state.proxy_form.apply_link() {
                        Ok(_) => {
                            out.toast = Some(strings::PROXY_PARSED_TOAST.to_owned());
                        }
                        Err(e) => state.proxy_form.error = Some(e),
                    }
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                for (kind, name) in [
                    (CoreProxyType::Socks5, "SOCKS5"),
                    (CoreProxyType::Http, "HTTP"),
                    (CoreProxyType::Mtproto, "MTProto"),
                    (CoreProxyType::Web, "Web"),
                ] {
                    if ui
                        .selectable_label(state.proxy_form.proxy_type == kind, name)
                        .clicked()
                    {
                        state.proxy_form.proxy_type = kind;
                        if kind == CoreProxyType::Web {
                            state.proxy_form.port = "443".to_owned();
                        }
                    }
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                InputField::single_line(
                    "proxy_host",
                    &mut state.proxy_form.host,
                    &tr::lng_connection_host_ph(),
                    InputFieldStyle::default_input(colors),
                )
                .width(210.0)
                .show(ui);
                ui.add_space(8.0);
                InputField::single_line(
                    "proxy_port",
                    &mut state.proxy_form.port,
                    &tr::lng_connection_port_ph(),
                    InputFieldStyle::default_input(colors),
                )
                .width(80.0)
                .show(ui);
            });
            ui.add_space(6.0);
            match state.proxy_form.proxy_type {
                CoreProxyType::Socks5 | CoreProxyType::Http => {
                    ui.horizontal(|ui| {
                        InputField::single_line(
                            "proxy_user",
                            &mut state.proxy_form.user,
                            &tr::lng_connection_user_ph(),
                            InputFieldStyle::default_input(colors),
                        )
                        .width(145.0)
                        .show(ui);
                        ui.add_space(8.0);
                        InputField::single_line(
                            "proxy_pass",
                            &mut state.proxy_form.password,
                            &tr::lng_connection_password_ph(),
                            InputFieldStyle::default_input(colors),
                        )
                        .password(true)
                        .width(145.0)
                        .show(ui);
                    });
                }
                CoreProxyType::Mtproto | CoreProxyType::Web => {
                    InputField::single_line(
                        "proxy_secret",
                        &mut state.proxy_form.password,
                        &tr::lng_connection_proxy_secret_ph(),
                        InputFieldStyle::default_input(colors),
                    )
                    .show(ui);
                }
                CoreProxyType::None => {}
            }
            if let Some(err) = &state.proxy_form.error {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(err)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("boxTextFgError")),
                );
            }
            ui.add_space(8.0);
            if RoundButton::new(
                &tr::lng_connection_save(),
                RoundButtonStyle::default_active(colors),
            )
            .show(ui)
            .clicked()
            {
                match state.proxy_form.build_validated() {
                    Ok(valid) => {
                        if let Some(old) = state.editing_proxy.take() {
                            core.proxy.replace_in_list(&old, valid.clone());
                            if *core.proxy.selected() == old {
                                core.proxy.set_selected(valid.clone());
                            }
                            out.toast = Some(strings::PROXY_UPDATED_TOAST.to_owned());
                        } else {
                            if core.proxy.index_in_list(&valid).is_none() {
                                core.proxy.add_to_list(valid.clone());
                            }
                            core.proxy.set_selected(valid.clone());
                            core.proxy.set_settings(ProxySettingsMode::Enabled);
                            out.proxy_enabled = Some(true);
                            out.toast = Some(strings::PROXY_ADDED_TOAST.to_owned());
                        }
                        backend.check_proxy(&valid);
                        state.proxy_form = ProxyFormDraft::default();
                        state.adding_proxy = false;
                        proxy_dirty = true;
                    }
                    Err(e) => state.proxy_form.error = Some(e),
                }
            }
        }

        // Connection indicator test controls (offline mock simulator only; hidden in live mode).
        if !backend.is_live_proxy_backend() {
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(strings::PROXY_SIMULATOR_LABEL)
                    .font(egui::FontId::new(12.0, tg_ui::fonts::family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(
                        matches!(connection_status, ConnectionStatus::Connected),
                        strings::CONN_CONNECTED,
                    )
                    .clicked()
                {
                    out.connection_status = Some(ConnectionStatus::Connected);
                }
                if ui
                    .selectable_label(
                        matches!(connection_status, ConnectionStatus::Connecting),
                        strings::CONN_CONNECTING,
                    )
                    .clicked()
                {
                    out.connection_status = Some(ConnectionStatus::Connecting);
                }
                if ui
                    .selectable_label(
                        matches!(connection_status, ConnectionStatus::Waiting { .. }),
                        strings::CONN_WAITING_8S,
                    )
                    .clicked()
                {
                    out.connection_status = Some(ConnectionStatus::Waiting { remaining_ms: 8000 });
                }
            });
        }
    });

    if proxy_dirty {
        state.last_backend_proxy = Some(core.proxy.clone());
        if let Err(err) = backend.apply_proxy_settings(&core.proxy) {
            out.toast = Some(format!("Failed to apply proxy settings: {err}"));
        }
    }
}

fn render_local_storage_panel(
    ui: &mut Ui,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
    out: &mut AdvancedOutput,
) {
    let stats = backend.local_storage_stats();
    settings_card(ui, colors, |ui| {
        ui.label(
            egui::RichText::new(format!(
                "{} — {}",
                tr::lng_local_storage_title(),
                format_bytes(stats.total_bytes())
            ))
            .font(font_id_from_style(st::semibold_font))
            .color(colors.color("windowBoldFg")),
        );
        ui.add_space(4.0);
        for (name, bytes) in [
            (strings::STORAGE_PHOTOS, stats.photos_bytes),
            (strings::STORAGE_VIDEOS, stats.videos_bytes),
            (strings::STORAGE_FILES, stats.files_bytes),
            (strings::STORAGE_STICKERS, stats.stickers_bytes),
        ] {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(name)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowFg")),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format_bytes(bytes))
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("windowSubTextFg")),
                    );
                });
            });
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{}:", tr::lng_local_storage_size_limit_title()))
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
            const GB: i64 = 1024 * 1024 * 1024;
            for (bytes, label) in [
                (GB, "1 GB"),
                (2 * GB, "2 GB"),
                (5 * GB, "5 GB"),
                (0, strings::STORAGE_NO_LIMIT),
            ] {
                if ui
                    .selectable_label(stats.size_limit_bytes == bytes, label)
                    .clicked()
                {
                    backend.set_local_storage_limits(bytes, stats.time_limit_days);
                }
            }
        });
        ui.add_space(8.0);
        if RoundButton::new(
            &tr::lng_local_storage_clear(),
            RoundButtonStyle::attention_box(colors),
        )
        .enabled(stats.total_bytes() > 0)
        .show(ui)
        .clicked()
        {
            let freed = backend.clear_local_storage();
            let freed_str = format_bytes(freed);
            out.toast = Some(tr::lng_local_storage_cleared(&freed_str));
        }
    });
}

fn render_auto_download_editor(
    ui: &mut Ui,
    source: AutoDownloadSource,
    ad: &mut super::backend::AutoDownloadSettings,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
    out: &mut AdvancedOutput,
) {
    const MB: i64 = 1024 * 1024;
    let mut changed = false;

    settings_card(ui, colors, |ui| {
        ui.label(
            egui::RichText::new(format!(
                "{} — {}",
                tr::lng_media_auto_title(),
                source.title()
            ))
            .font(font_id_from_style(st::semibold_font))
            .color(colors.color("windowBoldFg")),
        );
        ui.add_space(6.0);

        for kind in AutoDownloadType::ALL {
            let cur_bytes = ad.bytes_limit(source, kind);
            let mut enabled = cur_bytes > 0;
            ui.horizontal(|ui| {
                if ui.checkbox(&mut enabled, kind.label()).changed() {
                    if enabled {
                        ad.set_bytes_limit(source, kind, 10 * MB);
                    } else {
                        ad.set_bytes_limit(source, kind, 0);
                    }
                    changed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let badge = if !enabled {
                        tr::lng_rights_slowmode_off()
                    } else if !kind.has_size_limit() {
                        strings::AUTO_DOWNLOAD_ON.to_owned()
                    } else {
                        tr::lng_media_size_up_to(&format_bytes(cur_bytes))
                    };
                    ui.label(
                        egui::RichText::new(badge)
                            .font(egui::FontId::new(12.0, tg_ui::fonts::family_regular()))
                            .color(colors.color("windowActiveTextFg")),
                    );
                });
            });

            if enabled && kind.has_size_limit() {
                // Logarithmic slider from 1 MB to 4,000 MB (`MAX_AUTO_DOWNLOAD_BYTES`).
                let max_mb = (MAX_AUTO_DOWNLOAD_BYTES / MB) as f64;
                let cur_mb = (cur_bytes / MB).max(1) as f64;
                let mut norm = (cur_mb.ln() / max_mb.ln()).clamp(0.0, 1.0);
                if MediaSlider::new(&mut norm, MediaSliderStyle::default_media_slider(colors))
                    .always_display_marker(true)
                    .show(ui)
                    .changed()
                {
                    let new_mb = (norm * max_mb.ln()).exp().round().max(1.0) as i64;
                    ad.set_bytes_limit(source, kind, new_mb * MB);
                    changed = true;
                }
            }
        }

        ui.add_space(8.0);
        if RoundButton::new(
            strings::AUTO_DOWNLOAD_RESET_BTN,
            RoundButtonStyle::default_light(colors),
        )
        .show(ui)
        .clicked()
        {
            ad.set_defaults_for_source(source);
            changed = true;
            out.toast = Some(format!(
                "{} — {}",
                tr::lng_settings_reset_button(),
                source.title()
            ));
        }
    });

    if changed {
        backend.set_auto_download(ad.clone());
    }
}

fn render_export_panel(
    ui: &mut Ui,
    state: &mut AdvancedState,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
    out: &mut AdvancedOutput,
) {
    settings_card(ui, colors, |ui| {
        ui.label(
            egui::RichText::new(tr::lng_export_title())
                .font(font_id_from_style(st::semibold_font))
                .color(colors.color("windowBoldFg")),
        );
        ui.add_space(6.0);
        settings_checkbox_row(
            ui,
            &mut state.export_request.account_info,
            strings::EXPORT_ACCOUNT_INFO,
            true,
            colors,
        );
        settings_checkbox_row(
            ui,
            &mut state.export_request.personal_chats,
            strings::EXPORT_PERSONAL_CHATS,
            true,
            colors,
        );
        settings_checkbox_row(
            ui,
            &mut state.export_request.groups_and_channels,
            strings::EXPORT_GROUPS_CHANNELS,
            true,
            colors,
        );
        settings_checkbox_row(
            ui,
            &mut state.export_request.include_media,
            strings::EXPORT_INCLUDE_MEDIA,
            true,
            colors,
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(strings::EXPORT_FORMAT_LABEL)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowFg")),
            );
            for (fmt, label) in [
                (ExportFormat::Html, strings::EXPORT_FORMAT_HTML),
                (ExportFormat::Json, strings::EXPORT_FORMAT_JSON),
                (ExportFormat::HtmlAndJson, strings::EXPORT_FORMAT_BOTH),
            ] {
                if ui
                    .selectable_label(state.export_request.format == fmt, label)
                    .clicked()
                {
                    state.export_request.format = fmt;
                }
            }
        });
        ui.add_space(6.0);
        InputField::single_line(
            "export_target_path",
            &mut state.export_request.path,
            strings::EXPORT_PATH_PH,
            InputFieldStyle::default_input(colors),
        )
        .show(ui);
        ui.add_space(8.0);
        if RoundButton::new(
            &tr::lng_export_start(),
            RoundButtonStyle::default_active(colors),
        )
        .show(ui)
        .clicked()
        {
            let _ = backend;
            crate::export::AppExportHost::open_full_export(
                ui.ctx(),
                theme_mode_from_colors(colors),
            );
            out.open_full_export = true;
        }
    });
}

/// Renders the "Battery and Animations" Power Saving panel (`settings_power_saving.cpp`),
/// writing directly to [`CoreSettings::power_saving`], [`CoreSettings::power_saving_force_all`],
/// and [`CoreSettings::ignore_battery_saving`] while mirroring into `legacy_flags` so the live
/// bridge in `window/main_widget.rs` stays a pure pass-through.
pub fn render_power_saving_panel(
    ui: &mut Ui,
    legacy_flags: &mut PowerSavingFlags,
    expanded: &mut [bool; 3],
    core: &mut CoreSettings,
    colors: &dyn Colors,
) {
    let power_status = system_info::power_status();
    sync_battery_saver_to_core(core, power_status.and_then(|s| s.battery_saver_enabled));
    let locked_by_battery = is_power_saving_locked_by_battery(core);
    let editable = !locked_by_battery;

    settings_card(ui, colors, |ui| {
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(tr::lng_settings_power_title())
                    .font(font_id_from_style(st::box_title_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.label(
                egui::RichText::new(tr::lng_settings_power_subtitle())
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowActiveTextFg")),
            );
            if let Some(ps) = power_status {
                let battery_saver_active = ps.battery_saver_enabled.unwrap_or(false);
                let summary = if ps.has_battery {
                    format!(
                        "Power: {} ({}%), OS Battery Saver: {}",
                        if ps.on_battery { "Battery" } else { "AC" },
                        ps.battery_percent.unwrap_or(0),
                        if battery_saver_active { "ON" } else { "off" }
                    )
                } else {
                    strings::POWER_AC_DESKTOP.to_owned()
                };
                ui.label(
                    egui::RichText::new(summary)
                        .font(egui::FontId::new(12.0, tg_ui::fonts::family_regular()))
                        .color(colors.color("windowSubTextFg")),
                );
            }

            if locked_by_battery {
                ui.add_space(6.0);
                egui::Frame::NONE
                    .fill(colors.color("windowBg"))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(tr::lng_settings_power_turn_off())
                                .font(egui::FontId::new(12.0, tg_ui::fonts::family_regular()))
                                .color(colors.color("windowActiveTextFg")),
                        );
                    });
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if RoundButton::new(
                    strings::POWER_DISABLE_ALL_BTN,
                    RoundButtonStyle::default_light(colors),
                )
                .enabled(editable)
                .show(ui)
                .clicked()
                {
                    core.power_saving = CorePowerSavingFlags::ALL;
                }
                ui.add_space(8.0);
                if RoundButton::new(
                    strings::POWER_ENABLE_ALL_BTN,
                    RoundButtonStyle::default_light(colors),
                )
                .enabled(editable)
                .show(ui)
                .clicked()
                {
                    core.power_saving = CorePowerSavingFlags::NONE;
                }
            });
            ui.add_space(8.0);

            // Exact 5 flag groups from `PowerSavingLabels()` in `settings_power_saving.cpp:138-190`.
            // Checked / ON means the animation is enabled (the power-saving disable bit is cleared).
            let stickers_items = [
                (
                    CorePowerSavingFlags::STICKERS_PANEL,
                    tr::lng_settings_power_stickers_panel(),
                ),
                (
                    CorePowerSavingFlags::STICKERS_CHAT,
                    tr::lng_settings_power_stickers_chat(),
                ),
            ];
            render_power_saving_group(
                ui,
                &tr::lng_settings_power_stickers(),
                POWER_SAVING_GROUP_STICKERS,
                &stickers_items,
                &mut expanded[0],
                core,
                editable,
                colors,
            );

            let emoji_items = [
                (
                    CorePowerSavingFlags::EMOJI_PANEL,
                    tr::lng_settings_power_emoji_panel(),
                ),
                (
                    CorePowerSavingFlags::EMOJI_REACTIONS,
                    tr::lng_settings_power_emoji_reactions(),
                ),
                (
                    CorePowerSavingFlags::EMOJI_CHAT,
                    tr::lng_settings_power_emoji_chat(),
                ),
                (
                    CorePowerSavingFlags::EMOJI_STATUS,
                    tr::lng_settings_power_emoji_status(),
                ),
            ];
            render_power_saving_group(
                ui,
                &tr::lng_settings_power_emoji(),
                POWER_SAVING_GROUP_EMOJI,
                &emoji_items,
                &mut expanded[1],
                core,
                editable,
                colors,
            );

            let chat_items = [
                (
                    CorePowerSavingFlags::CHAT_BACKGROUND,
                    tr::lng_settings_power_chat_background(),
                ),
                (
                    CorePowerSavingFlags::CHAT_SPOILER,
                    tr::lng_settings_power_chat_spoiler(),
                ),
                (
                    CorePowerSavingFlags::CHAT_EFFECTS,
                    tr::lng_settings_power_chat_effects(),
                ),
            ];
            render_power_saving_group(
                ui,
                &tr::lng_settings_power_chat(),
                POWER_SAVING_GROUP_CHAT,
                &chat_items,
                &mut expanded[2],
                core,
                editable,
                colors,
            );

            // Single-flag groups: Animations in Calls & Interface animations.
            let effective = power_saving_effective_flags(core);
            let mut calls_anim_on = !effective.contains(POWER_SAVING_GROUP_CALLS);
            let calls_lbl = tr::lng_settings_power_calls();
            if settings_toggle_row(
                ui,
                &calls_lbl,
                None,
                None,
                &mut calls_anim_on,
                editable,
                colors,
            ) {
                set_power_saving_flag_enabled(core, POWER_SAVING_GROUP_CALLS, calls_anim_on);
            }

            let effective = power_saving_effective_flags(core);
            let mut ui_anim_on = !effective.contains(POWER_SAVING_GROUP_UI);
            let ui_lbl = tr::lng_settings_power_ui();
            if settings_toggle_row(ui, &ui_lbl, None, None, &mut ui_anim_on, editable, colors) {
                set_power_saving_flag_enabled(core, POWER_SAVING_GROUP_UI, ui_anim_on);
            }

            ui.add_space(6.0);
            settings_divider(ui, None, colors);
            let mut auto_on_battery = !core.ignore_battery_saving;
            let power_auto_lbl = tr::lng_settings_power_auto();
            let power_auto_sub = tr::lng_settings_power_auto_about();
            if settings_toggle_row(
                ui,
                &power_auto_lbl,
                Some(&power_auto_sub),
                None,
                &mut auto_on_battery,
                true,
                colors,
            ) {
                set_power_saving_auto_on_low_battery(core, auto_on_battery);
            }
        });
    });

    // Mirror typed `CoreSettings::power_saving` back into `AdvancedState::power_saving`.
    *legacy_flags = PowerSavingFlags::from_core(core.power_saving);
}

#[allow(clippy::too_many_arguments)]
fn render_power_saving_group(
    ui: &mut Ui,
    title: &str,
    group_mask: CorePowerSavingFlags,
    items: &[(CorePowerSavingFlags, String)],
    expanded: &mut bool,
    core: &mut CoreSettings,
    editable: bool,
    colors: &dyn Colors,
) {
    let effective = power_saving_effective_flags(core);
    let enabled_count = items
        .iter()
        .filter(|(flag, _)| !effective.contains(*flag))
        .count();
    let total_count = items.len();
    let mut group_enabled = enabled_count > 0;

    let w = ui.available_width().max(200.0);
    let toggle_zone_w = 56.0;
    let (full_rect, _) = ui.allocate_exact_size(Vec2::new(w, 38.0), Sense::hover());
    let left_rect = Rect::from_min_max(
        full_rect.min,
        Pos2::new(full_rect.right() - toggle_zone_w, full_rect.bottom()),
    );
    let right_rect = Rect::from_min_max(
        Pos2::new(full_rect.right() - toggle_zone_w, full_rect.top()),
        full_rect.max,
    );

    let left_resp = ui.interact(
        left_rect,
        ui.scope_id().with(("ps_group_hdr", title)),
        Sense::click(),
    );
    let right_sense = if editable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let right_resp = ui.interact(
        right_rect,
        ui.scope_id().with(("ps_group_tgl", title)),
        right_sense,
    );

    if left_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(left_rect, CornerRadius::same(4), colors.color("windowBg"));
    }
    if left_resp.clicked() {
        *expanded = !*expanded;
    }

    let chev_rect = Rect::from_center_size(
        Pos2::new(full_rect.left() + 12.0, full_rect.center().y),
        Vec2::splat(16.0),
    );
    paint_tinted_icon(
        ui.painter(),
        chev_rect,
        if *expanded {
            st::menu_icon_collapse
        } else {
            st::menu_icon_expand
        },
        colors,
        Some(colors.color("windowSubTextFg")),
    );

    let header_text = format!("{title}  {enabled_count}/{total_count}");
    let text_x = full_rect.left() + 28.0;
    let text_max_w = (right_rect.left() - 8.0 - text_x).max(40.0);
    let header_fg = if editable {
        colors.color("windowBoldFg")
    } else {
        colors.color("windowSubTextFg")
    };
    let mut header_job = egui::text::LayoutJob {
        wrap: egui::text::TextWrapping {
            max_width: text_max_w,
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        },
        ..Default::default()
    };
    header_job.append(
        &header_text,
        0.0,
        egui::text::TextFormat {
            font_id: font_id_from_style(st::semibold_font),
            color: header_fg,
            ..Default::default()
        },
    );
    let header_galley = ui.painter().layout_job(header_job);
    let header_elided = header_galley.elided;
    let text_clip = Rect::from_min_max(
        Pos2::new(text_x, full_rect.top()),
        Pos2::new(right_rect.left() - 4.0, full_rect.bottom()),
    );
    ui.painter().with_clip_rect(text_clip).galley(
        Pos2::new(text_x, full_rect.center().y - header_galley.size().y * 0.5),
        header_galley,
        header_fg,
    );
    left_resp
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, header_text.clone()));
    if header_elided {
        let _ = left_resp.on_hover_text(header_text);
    }

    if editable && right_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if editable && right_resp.clicked() {
        group_enabled = !group_enabled;
        set_power_saving_group_enabled(core, group_mask, group_enabled);
    }
    right_resp.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::Role::CheckBox,
            editable,
            group_enabled,
            format!("Toggle {title}"),
        )
    });

    let toggle_st = ToggleStyle::default_toggle(colors);
    let tsize = Toggle::toggle_size(&toggle_st);
    let toggle_rect = Rect::from_center_size(right_rect.center(), tsize);
    let anim_secs = if ui.style().animation_time <= 0.0 {
        0.0
    } else {
        (toggle_st.duration_ms as f32 / 1000.0).min(0.20)
    };
    let toggled_val = f64::from(ui.ctx().animate_bool_with_time(
        right_resp.id,
        group_enabled,
        anim_secs,
    ));
    paint_toggle_view(
        ui.painter(),
        toggle_rect,
        &toggle_st,
        toggled_val,
        if editable { 1.0 } else { 0.45 },
    );

    if *expanded {
        let latest_effective = power_saving_effective_flags(core);
        for (flag, label) in items {
            let mut anim_on = !latest_effective.contains(*flag);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                if settings_checkbox_row(ui, &mut anim_on, label, editable, colors).changed() {
                    set_power_saving_flag_enabled(core, *flag, anim_on);
                }
            });
        }
        ui.add_space(4.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_debug_tools(
    ui: &mut Ui,
    state: &mut AdvancedState,
    core: &mut CoreSettings,
    global: &mut GlobalConfig,
    debug_opts: &mut DebugOptions,
    layout: ColumnLayout,
    colors: &dyn Colors,
    out: &mut AdvancedOutput,
) {
    settings_card(ui, colors, |ui| {
        ui.label(
            egui::RichText::new(strings::DEBUG_TOOLS_HEADER)
                .font(font_id_from_style(st::semibold_font))
                .color(colors.color("windowBoldFg")),
        );
        ui.add_space(6.0);
        settings_toggle_row(
            ui,
            strings::DEBUG_LOGGING,
            None,
            None,
            &mut global.debug_mode,
            true,
            colors,
        );
        settings_toggle_row(
            ui,
            strings::DEBUG_MODERATE,
            None,
            None,
            &mut core.moderate_mode_enabled,
            true,
            colors,
        );
        settings_toggle_row(
            ui,
            strings::DEBUG_FAST_BUTTONS,
            None,
            None,
            &mut debug_opts.fast_buttons_mode,
            true,
            colors,
        );
        settings_toggle_row(
            ui,
            strings::DEBUG_TEST_CHAT_COLORS,
            None,
            None,
            &mut debug_opts.testing_chat_colors,
            true,
            colors,
        );
        settings_toggle_row(
            ui,
            strings::DEBUG_SMOOTH_SCROLLING,
            None,
            None,
            &mut debug_opts.smooth_scrolling,
            true,
            colors,
        );
        settings_toggle_row(
            ui,
            strings::DEBUG_MEDIA_OVERLAY,
            None,
            None,
            &mut debug_opts.media_debug_overlay,
            true,
            colors,
        );

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            InputField::single_line(
                "settings_secret_code_input",
                &mut state.manual_code_input,
                strings::DEBUG_SECRET_CODE_PH,
                InputFieldStyle::default_input(colors),
            )
            .width(260.0)
            .show(ui);
            ui.add_space(8.0);
            if RoundButton::new(
                strings::DEBUG_RUN_BTN,
                RoundButtonStyle::default_active(colors),
            )
            .show(ui)
            .clicked()
            {
                let mut tracker = super::codes::SecretCodeTracker::new();
                if let Some(act) = tracker.feed(state.manual_code_input.trim()) {
                    out.secret_code = Some(act);
                    state.manual_code_input.clear();
                } else {
                    out.toast = Some(format!(
                        "Unknown code '{}'. Try: debugmode, moderate, folders, sounds_reset, testchatcolors",
                        state.manual_code_input.trim()
                    ));
                }
            }
        });

        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(strings::DEBUG_LAYOUT_DIAGNOSTICS)
                .font(font_id_from_style(st::semibold_font))
                .color(colors.color("windowActiveTextFg")),
        );
        for line in [
            format!(
                "WindowLayout: {:?} | ChatLayout: {:?}",
                layout.window_layout, layout.chat_layout
            ),
            format!(
                "Filters: {}px | Dialogs: {}px (compact: {}) | Main: {}px | Third: {}px",
                layout.filters_width,
                layout.dialogs_width,
                layout.dialogs_small_column,
                layout.chat_width,
                layout.third_width
            ),
            format!(
                "OS: {} | Device: {}",
                system_info::system_version_pretty(),
                system_info::device_model_pretty()
            ),
        ] {
            ui.label(
                egui::RichText::new(line)
                    .size(11.5)
                    .color(colors.color("windowSubTextFg")),
            );
        }
    });
}

fn resolve_autostart_provider(
    slot: &AutostartProviderSlot,
    working_dir: &Path,
) -> Arc<dyn AutostartProvider> {
    if let Some(custom) = &slot.0 {
        return Arc::clone(custom);
    }
    let winrt = WinRtStartupTaskProvider::new(DEFAULT_STARTUP_TASK_ID);
    if winrt.query_state().is_ok() {
        return Arc::new(winrt);
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("rustgram.exe"));
    let mut entry = AutostartEntry::for_executable(exe);
    if !working_dir.as_os_str().is_empty() {
        entry = entry.with_workdir(working_dir);
    }
    Arc::new(WindowsRunKeyAutostartProvider::hkcu(entry))
}

fn apply_send_to_shortcut(enabled: bool) -> Result<(), String> {
    let dir = send_to_folder_dir().map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let workdir = exe
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    let cfg = AppLinkConfig::for_executable(exe, workdir);
    set_send_to_in(&dir, &cfg, enabled).map_err(|e| e.to_string())
}
