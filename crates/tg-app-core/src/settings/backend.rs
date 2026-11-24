//! Server-side settings backend trait and domain types.
//!
//! Port origin: `Telegram/SourceFiles/api/api_user_privacy.h`,
//! `Telegram/SourceFiles/api/api_authorizations.h`,
//! `Telegram/SourceFiles/api/api_cloud_password.h`,
//! `Telegram/SourceFiles/data/data_auto_download.h`,
//! `Telegram/SourceFiles/data/data_chat_filters.h`,
//! `Telegram/SourceFiles/data/notify/data_notify_settings.h`,
//! `Telegram/SourceFiles/boxes/self_destruction_box.cpp`,
//! and `Telegram/SourceFiles/boxes/username_box.cpp`.

use std::collections::BTreeMap;

use tg_lang::{LangPackLanguageInfo, tr};

/// Maximum character length of a non-premium Telegram bio (`kMaxBioLength`).
pub const MAX_BIO_LEN: usize = 70;
/// Maximum character length of a chat folder title (`kMaxFilterTitleLength`).
pub const MAX_FOLDER_TITLE_LEN: usize = 12;
/// Maximum auto-download file size limit (`Data::AutoDownload::kMaxBytesLimit` = 4,000 MiB).
pub const MAX_AUTO_DOWNLOAD_BYTES: i64 = 8000 * 512 * 1024;
/// Maximum number of simultaneous desktop notification toasts (`kMaxNotificationsCount`).
pub const MAX_NOTIFICATIONS_COUNT: i32 = 5;
/// Account self-destruction TTL choices in days (`SelfDestructionBox::Values(Account)`).
pub const ACCOUNT_TTL_OPTIONS: [i32; 6] = [30, 90, 180, 365, 548, 720];
/// Inactive session auto-termination TTL choices in days (`SelfDestructionBox::Values(Sessions)`).
pub const SESSION_TTL_OPTIONS: [i32; 5] = [7, 30, 90, 180, 365];
/// Auto-lock preset timeouts in seconds (`AutoLockBox::kOptions`: 1m, 5m, 1h, 5h).
pub const AUTO_LOCK_PRESETS_SECS: [i32; 4] = [60, 300, 3600, 18000];
/// Global auto-delete message TTL options in seconds (Off, 1d, 1w, 1m).
pub const GLOBAL_TTL_OPTIONS_SECS: [i32; 4] = [0, 86_400, 604_800, 2_592_000];
/// Default maximum custom ringtone file size in bytes (`ringtone_size_max` = 100 KiB).
pub const DEFAULT_RINGTONE_MAX_SIZE: usize = 100 * 1024;
/// Default maximum custom ringtone duration in seconds (`ringtone_duration_max` = 5s).
pub const DEFAULT_RINGTONE_MAX_DURATION_SECS: f64 = 5.0;
/// Default maximum saved custom ringtones count (`ringtone_saved_count_max` = 100).
pub const DEFAULT_RINGTONE_MAX_SAVED_COUNT: usize = 100;

/// Lightweight reference to a peer used in privacy exceptions, blocked lists, and folders.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerRef {
    /// Stable numeric peer identifier.
    pub id: i64,
    /// Display name of the user, group, or channel.
    pub name: String,
    /// Optional `@username` without leading `@`.
    pub username: String,
    /// Subtitle (e.g., `"last seen recently"` or `"14 members"`).
    pub subtitle: String,
    /// Whether this peer is a group or channel rather than a user.
    pub is_group_or_channel: bool,
}

/// Optional user birthday (`Data::Birthday`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Birthday {
    /// Day of month (`1..=31`).
    pub day: u8,
    /// Month (`1..=12`).
    pub month: u8,
    /// Optional birth year (e.g. `1994`).
    pub year: Option<u16>,
}

impl Birthday {
    /// Validates that day/month/year represent a plausible calendar date.
    #[must_use]
    pub fn is_valid(self) -> bool {
        if !(1..=12).contains(&self.month) || self.day == 0 {
            return false;
        }
        let max_day = match self.month {
            2 => 29,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        if self.day > max_day {
            return false;
        }
        self.year.is_none_or(|y| (1900..=2100).contains(&y))
    }

    /// Formats the birthday in human-readable form (e.g. `"May 14, 1995"` or `"May 14"`).
    #[must_use]
    pub fn format_display(self) -> String {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let month_name = MONTHS
            .get(usize::from(self.month.saturating_sub(1)))
            .copied()
            .unwrap_or("Jan");
        match self.year {
            Some(year) => format!("{month_name} {}, {year}", self.day),
            None => format!("{month_name} {}", self.day),
        }
    }
}

/// Editable server-side user profile information (`settings_information.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileInfo {
    /// Stable self user ID.
    pub user_id: u64,
    /// First name (required, 1..=64 chars).
    pub first_name: String,
    /// Optional last name (0..=64 chars).
    pub last_name: String,
    /// Public `@username` without leading `@` (empty if unset).
    pub username: String,
    /// Formatted phone number string (e.g. `"+1 555 010 0199"`).
    pub phone: String,
    /// Short bio text (`<= MAX_BIO_LEN` chars).
    pub bio: String,
    /// Optional birthday.
    pub birthday: Option<Birthday>,
    /// Selected avatar palette color index (`0..7`).
    pub photo_color_index: u8,
    /// Optional custom photo label/tag (when a profile photo is set).
    pub photo_label: Option<String>,
    /// Whether the phone number is hidden behind a spoiler in the settings cover.
    pub phone_hidden: bool,
}

impl ProfileInfo {
    /// Returns the combined `"First Last"` display name.
    #[must_use]
    pub fn full_name(&self) -> String {
        if self.last_name.trim().is_empty() {
            self.first_name.trim().to_owned()
        } else {
            format!("{} {}", self.first_name.trim(), self.last_name.trim())
        }
    }
}

/// Validation result for a Telegram `@username` (`boxes/username_box.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsernameStatus {
    /// Empty username (clears current username).
    Empty,
    /// Valid syntax and available.
    Available,
    /// Valid syntax and matches the user's current username.
    Current,
    /// Already taken by another account.
    Taken,
    /// Shorter than 5 characters.
    TooShort,
    /// Longer than 32 characters.
    TooLong,
    /// Starts with a digit or underscore, ends with underscore, or contains invalid characters.
    InvalidChars,
}

/// Validates a Telegram username according to tdesktop's `UsernameBox` rules.
///
/// Rules: empty (clear) OR 5..=32 ASCII chars `[a-zA-Z0-9_]`, starting with a letter,
/// not ending with `_`, and containing no consecutive `__`.
#[must_use]
pub fn validate_username_syntax(raw: &str) -> UsernameStatus {
    let trimmed = raw.trim().strip_prefix('@').unwrap_or(raw.trim());
    if trimmed.is_empty() {
        return UsernameStatus::Empty;
    }
    if trimmed.len() < 5 {
        return UsernameStatus::TooShort;
    }
    if trimmed.len() > 32 {
        return UsernameStatus::TooLong;
    }
    let bytes = trimmed.as_bytes();
    if !bytes[0].is_ascii_alphabetic() || bytes[bytes.len() - 1] == b'_' {
        return UsernameStatus::InvalidChars;
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
    {
        return UsernameStatus::InvalidChars;
    }
    if trimmed.contains("__") {
        return UsernameStatus::InvalidChars;
    }
    UsernameStatus::Available
}

/// All Telegram user privacy keys (`Api::UserPrivacy::Key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrivacyKey {
    /// Who can see my phone number.
    PhoneNumber,
    /// Who can find me by my phone number.
    AddedByPhone,
    /// Who can see my last seen & online timestamp.
    LastSeen,
    /// Who can see my profile photos and videos.
    ProfilePhoto,
    /// Who can add a link to my account when forwarding my messages.
    Forwards,
    /// Who can call me.
    Calls,
    /// Who can use peer-to-peer connections in calls.
    CallsPeer2Peer,
    /// Who can add me to groups and channels.
    GroupsInvite,
    /// Who can send me voice or video messages.
    VoiceMessages,
    /// Who can see my bio.
    About,
    /// Who can see my date of birth.
    Birthday,
    /// Who can send me gifts that appear on my profile.
    GiftsAutoSave,
    /// Who can see my saved profile music.
    SavedMusic,
}

impl PrivacyKey {
    /// Canonical ordering of privacy keys shown in Privacy & Security settings.
    pub const ALL: [Self; 13] = [
        Self::PhoneNumber,
        Self::AddedByPhone,
        Self::LastSeen,
        Self::ProfilePhoto,
        Self::Forwards,
        Self::Calls,
        Self::CallsPeer2Peer,
        Self::GroupsInvite,
        Self::VoiceMessages,
        Self::About,
        Self::Birthday,
        Self::GiftsAutoSave,
        Self::SavedMusic,
    ];

    /// Primary display title in the Privacy & Security list.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::PhoneNumber => "Phone Number",
            Self::AddedByPhone => "Find Me by Phone Number",
            Self::LastSeen => "Last Seen & Online",
            Self::ProfilePhoto => "Profile Photos",
            Self::Forwards => "Forwarded Messages",
            Self::Calls => "Calls",
            Self::CallsPeer2Peer => "Peer-to-Peer in Calls",
            Self::GroupsInvite => "Invites",
            Self::VoiceMessages => "Voice Messages",
            Self::About => "Bio",
            Self::Birthday => "Date of Birth",
            Self::GiftsAutoSave => "Gifts",
            Self::SavedMusic => "Saved Music",
        }
    }

    /// Question shown above the radio options in `EditPrivacyBox`.
    #[must_use]
    pub const fn question(self) -> &'static str {
        match self {
            Self::PhoneNumber => "Who can see my phone number?",
            Self::AddedByPhone => "Who can find me by my number?",
            Self::LastSeen => "Who can see my Last Seen time?",
            Self::ProfilePhoto => "Who can see my profile photos?",
            Self::Forwards => "Who can add a link to my account when forwarding my messages?",
            Self::Calls => "Who can call me?",
            Self::CallsPeer2Peer => "Use peer-to-peer with:",
            Self::GroupsInvite => "Who can add me to group chats and channels?",
            Self::VoiceMessages => "Who can send me voice or video messages?",
            Self::About => "Who can see my bio?",
            Self::Birthday => "Who can see my date of birth?",
            Self::GiftsAutoSave => "Who can display gifts on my profile?",
            Self::SavedMusic => "Who can see my saved music?",
        }
    }

    /// Whether `Nobody` is permitted for this key (`AddedByPhone` only allows Everybody/Contacts).
    #[must_use]
    pub const fn allows_nobody(self) -> bool {
        !matches!(self, Self::AddedByPhone)
    }

    /// Whether per-user Always/Never exceptions apply to this key.
    #[must_use]
    pub const fn supports_exceptions(self) -> bool {
        !matches!(self, Self::AddedByPhone)
    }

    /// Optional explanatory footer text shown inside `EditPrivacyBox`.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::PhoneNumber => {
                "Users who have your number saved in their contacts will also see it on Telegram."
            }
            Self::AddedByPhone => {
                "Choose whether people who have your phone number can find your Telegram account."
            }
            Self::LastSeen => {
                "You won't see Last Seen timestamps for people with whom you don't share your own. Approximate values are shown instead."
            }
            Self::ProfilePhoto => {
                "You can restrict who sees your profile photos and set a public fallback photo."
            }
            Self::Forwards => {
                "People who forward your messages will or won't include a clickable link back to your profile."
            }
            Self::Calls => {
                "You can restrict who can reach you with Telegram voice and video calls."
            }
            Self::CallsPeer2Peer => {
                "Disabling peer-to-peer routes calls through Telegram servers to hide your IP address, slightly reducing audio quality."
            }
            Self::GroupsInvite => {
                "You can restrict who can add you to groups and channels with granular precision."
            }
            Self::VoiceMessages => {
                "Control who is allowed to send you voice or round video messages."
            }
            Self::About => "Choose who can view the bio text on your profile.",
            Self::Birthday => "Choose who can see your birthday on your profile.",
            Self::GiftsAutoSave => {
                "Control which received gifts appear automatically on your profile."
            }
            Self::SavedMusic => "Choose who can see saved audio tracks pinned to your profile.",
        }
    }
    /// Localized display title from `tg_lang::tr::*`.
    #[must_use]
    pub fn localized_title(self) -> String {
        match self {
            Self::PhoneNumber => tr::lng_settings_phone_number_privacy(),
            Self::AddedByPhone => tr::lng_edit_privacy_phone_number_find(),
            Self::LastSeen => tr::lng_edit_privacy_lastseen_title(),
            Self::ProfilePhoto => self.title().to_owned(),
            Self::Forwards => tr::lng_edit_privacy_forwards_title(),
            Self::Calls => tr::lng_edit_privacy_calls_title(),
            Self::CallsPeer2Peer => tr::lng_settings_calls_peer_to_peer_title(),
            Self::GroupsInvite => tr::lng_edit_privacy_groups_title(),
            Self::VoiceMessages => self.title().to_owned(),
            Self::About => tr::lng_edit_privacy_about_title(),
            Self::Birthday => tr::lng_settings_birthday_privacy(),
            Self::GiftsAutoSave => tr::lng_edit_privacy_gifts_title(),
            Self::SavedMusic => tr::lng_edit_privacy_saved_music_title(),
        }
    }

    /// Localized question shown above the radio options in `EditPrivacyBox`.
    #[must_use]
    pub fn localized_question(self) -> String {
        match self {
            Self::PhoneNumber => tr::lng_edit_privacy_phone_number_header(),
            Self::AddedByPhone => tr::lng_edit_privacy_phone_number_find(),
            Self::LastSeen => tr::lng_edit_privacy_lastseen_header(),
            Self::Forwards => tr::lng_edit_privacy_forwards_header(),
            Self::Calls => tr::lng_edit_privacy_calls_header(),
            Self::CallsPeer2Peer => tr::lng_settings_calls_peer_to_peer_button(),
            Self::GroupsInvite => tr::lng_edit_privacy_groups_header(),
            Self::About => tr::lng_edit_privacy_about_header(),
            Self::Birthday => tr::lng_edit_privacy_birthday_header(),
            Self::GiftsAutoSave => tr::lng_edit_privacy_gifts_header(),
            Self::SavedMusic => tr::lng_edit_privacy_saved_music_header(),
            Self::ProfilePhoto | Self::VoiceMessages => self.question().to_owned(),
        }
    }

    /// Localized explanatory footer text shown inside `EditPrivacyBox`.
    #[must_use]
    pub fn localized_description(self) -> String {
        match self {
            Self::PhoneNumber => tr::lng_edit_privacy_phone_number_warning(),
            Self::AddedByPhone => tr::lng_edit_privacy_phone_number_contacts(),
            Self::LastSeen => tr::lng_edit_privacy_lastseen_warning(),
            Self::GroupsInvite => tr::lng_edit_privacy_groups_exceptions(),
            Self::About => tr::lng_edit_privacy_about_exceptions(),
            Self::Birthday => tr::lng_edit_privacy_birthday_exceptions(),
            _ => self.description().to_owned(),
        }
    }
}

/// Primary audience option for a privacy rule (`Api::UserPrivacy::Option`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrivacyOption {
    /// Visible/allowed for everyone.
    #[default]
    Everybody,
    /// Visible/allowed only for contacts.
    Contacts,
    /// Visible/allowed for no one (unless explicitly listed in `always_allow`).
    Nobody,
}

impl PrivacyOption {
    /// Formats the base option label, accounting for `CallsPeer2Peer` wording from `settings_privacy_security.cpp`.
    #[must_use]
    pub const fn label_for(self, key: PrivacyKey) -> &'static str {
        match (key, self) {
            (PrivacyKey::CallsPeer2Peer, Self::Everybody) => "Always",
            (PrivacyKey::CallsPeer2Peer, Self::Contacts) => "My Contacts",
            (PrivacyKey::CallsPeer2Peer, Self::Nobody) => "Never",
            (_, Self::Everybody) => "Everybody",
            (_, Self::Contacts) => "My Contacts",
            (_, Self::Nobody) => "Nobody",
        }
    }

    /// Localized base option label from `tg_lang::tr::*`.
    #[must_use]
    pub fn localized_label_for(self, key: PrivacyKey) -> String {
        match (key, self) {
            (PrivacyKey::CallsPeer2Peer, Self::Everybody) => "Always".to_owned(),
            (PrivacyKey::CallsPeer2Peer, Self::Contacts) => tr::lng_edit_privacy_contacts(),
            (PrivacyKey::CallsPeer2Peer, Self::Nobody) => tr::lng_local_storage_limit_never(),
            (_, Self::Everybody) => tr::lng_edit_privacy_everyone(),
            (_, Self::Contacts) => tr::lng_edit_privacy_contacts(),
            (_, Self::Nobody) => tr::lng_edit_privacy_nobody(),
        }
    }
}

/// Complete privacy rule including base option and Always/Never exceptions (`Api::UserPrivacy::Rule`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrivacyRule {
    /// Primary audience setting.
    pub option: PrivacyOption,
    /// Peer IDs explicitly allowed regardless of `option`.
    pub always_allow: Vec<i64>,
    /// Peer IDs explicitly disallowed regardless of `option`.
    pub never_allow: Vec<i64>,
    /// Allow Telegram Premium users in `always_allow` (for `GroupsInvite`).
    pub always_premiums: bool,
    /// Allow Mini Apps in `always_allow` (for `GiftsAutoSave`).
    pub always_miniapps: bool,
    /// Disallow Mini Apps in `never_allow` (for `GiftsAutoSave`).
    pub never_miniapps: bool,
}

impl PrivacyRule {
    /// Formats the compact summary string shown on the Privacy & Security row
    /// (exact port of `PrivacyString` in `settings_privacy_security.cpp:124-145`).
    #[must_use]
    pub fn summary(&self, key: PrivacyKey) -> String {
        let base = self.option.label_for(key);
        let mut parts = Vec::new();
        if !self.never_allow.is_empty() {
            parts.push(format!("-{}", self.never_allow.len()));
        }
        if !self.always_allow.is_empty() {
            parts.push(format!("+{}", self.always_allow.len()));
        }
        if parts.is_empty() {
            base.to_owned()
        } else {
            format!("{base} ({})", parts.join(", "))
        }
    }

    /// Ensures a peer ID is not in both `always_allow` and `never_allow` and removes duplicates.
    pub fn normalize(&mut self) {
        self.always_allow.sort_unstable();
        self.always_allow.dedup();
        self.never_allow.sort_unstable();
        self.never_allow.dedup();
        if self.option == PrivacyOption::Everybody {
            self.always_allow.clear();
        } else if self.option == PrivacyOption::Nobody {
            self.never_allow.clear();
        }
        let never = &self.never_allow;
        self.always_allow.retain(|id| !never.contains(id));
    }
}

/// Platform/device icon classification matching `TypeFromEntry` in `settings_active_sessions.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Windows,
    Mac,
    Ubuntu,
    Linux,
    IPhone,
    IPad,
    Android,
    Web,
    Chrome,
    Edge,
    Firefox,
    Safari,
    Other,
}

impl DeviceKind {
    /// Short badge text drawn inside the session device circle.
    #[must_use]
    pub const fn badge_text(self) -> &'static str {
        match self {
            Self::Windows => "WIN",
            Self::Mac => "MAC",
            Self::Ubuntu => "UBU",
            Self::Linux => "LNX",
            Self::IPhone => "iOS",
            Self::IPad => "PAD",
            Self::Android => "AND",
            Self::Web => "WEB",
            Self::Chrome => "CHR",
            Self::Edge => "EDG",
            Self::Firefox => "FFX",
            Self::Safari => "SAF",
            Self::Other => "DEV",
        }
    }

    /// Classifies a session entry by platform, device model, system version, and API ID.
    #[must_use]
    pub fn classify(platform: &str, device: &str, system: &str, api_id: i32) -> Self {
        let p = platform.to_ascii_lowercase();
        let d = device.to_ascii_lowercase();
        let s = system.to_ascii_lowercase();

        let detect_browser = || {
            if d.contains("edg/") || d.contains("edgios/") || d.contains("edga/") {
                Some(Self::Edge)
            } else if d.contains("chrome") {
                Some(Self::Chrome)
            } else if d.contains("safari") {
                Some(Self::Safari)
            } else if d.contains("firefox") {
                Some(Self::Firefox)
            } else {
                None
            }
        };
        let detect_desktop = || {
            if p.contains("windows") || s.contains("windows") {
                Some(Self::Windows)
            } else if p.contains("macos") || s.contains("macos") {
                Some(Self::Mac)
            } else if p.contains("ubuntu")
                || s.contains("ubuntu")
                || p.contains("unity")
                || s.contains("unity")
            {
                Some(Self::Ubuntu)
            } else if p.contains("linux") || s.contains("linux") {
                Some(Self::Linux)
            } else {
                None
            }
        };

        if matches!(api_id, 5 | 6 | 24 | 1026 | 1083 | 2458 | 2521 | 21724) {
            Self::Android
        } else if matches!(api_id, 2040 | 17349 | 611335) {
            detect_desktop().unwrap_or(Self::Linux)
        } else if api_id == 2834 {
            Self::Mac
        } else if matches!(api_id, 2496 | 739222 | 1025907) {
            detect_browser().unwrap_or(Self::Web)
        } else if d.contains("chromebook") {
            Self::Other
        } else if let Some(b) = detect_browser() {
            b
        } else if d.contains("iphone") {
            Self::IPhone
        } else if d.contains("ipad") {
            Self::IPad
        } else if matches!(api_id, 1 | 7 | 10840 | 16352) {
            Self::IPhone
        } else if let Some(desk) = detect_desktop() {
            desk
        } else if p.contains("android") || s.contains("android") {
            Self::Android
        } else if p.contains("ios") || s.contains("ios") {
            Self::IPhone
        } else {
            Self::Other
        }
    }
}

/// An active Telegram authorization/session (`Api::Authorizations::Entry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSession {
    /// Session hash (`0` denotes the current session).
    pub hash: u64,
    /// Client API ID used for device classification.
    pub api_id: i32,
    /// Device model name (e.g. `"Desktop Workstation"`).
    pub device_model: String,
    /// Platform string (e.g. `"Windows"`).
    pub platform: String,
    /// OS version string (e.g. `"Windows 11 x64"`).
    pub system_version: String,
    /// Client application name and version (e.g. `"Rustgram 0.1.0"`).
    pub app_name_version: String,
    /// Approximate location (e.g. `"Zurich, Switzerland"`).
    pub location: String,
    /// IP address string.
    pub ip: String,
    /// Human-readable last active string (e.g. `"online"` or `"2 hours ago"`).
    pub active_time: String,
    /// Whether this session accepts incoming calls.
    pub accept_calls: bool,
    /// Whether this session accepts new secret chats.
    pub accept_secret_chats: bool,
}

impl ActiveSession {
    /// Returns `true` if this entry represents the current session (`hash == 0`).
    #[must_use]
    pub const fn is_current(&self) -> bool {
        self.hash == 0
    }

    /// Classifies the device kind for badge rendering.
    #[must_use]
    pub fn device_kind(&self) -> DeviceKind {
        DeviceKind::classify(
            &self.platform,
            &self.device_model,
            &self.system_version,
            self.api_id,
        )
    }

    /// Formats location + last active string (`LocationAndDate` in `settings_active_sessions.cpp`).
    #[must_use]
    pub fn location_and_date(&self) -> String {
        let loc = if self.location.is_empty() {
            &self.ip
        } else {
            &self.location
        };
        if self.is_current() {
            loc.to_owned()
        } else {
            format!("{loc}  ·  {}", self.active_time)
        }
    }
}

/// A website authorization created via Telegram Login Widget (`Api::Websites::Entry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAuthorization {
    /// Unique authorization hash passed to `account.resetWebAuthorization`.
    pub hash: u64,
    /// Telegram user ID of the bot owning this website login.
    pub bot_id: i64,
    /// Display name of the bot.
    pub bot_name: String,
    /// Website domain (e.g. `"docs.example.test"`).
    pub domain: String,
    /// Browser string reported by the server (e.g. `"Firefox 132"`).
    pub browser: String,
    /// Platform string (e.g. `"Windows"`).
    pub platform: String,
    /// IP address string.
    pub ip: String,
    /// Approximate location / region string.
    pub region: String,
    /// Unix creation timestamp.
    pub date_created: i32,
    /// Unix last-active timestamp.
    pub date_active: i32,
    /// Human-readable active date string.
    pub active_time: String,
}

impl WebAuthorization {
    /// Formats `domain, browser, platform` joining non-empty fields (`JoinNonEmpty` in `settings_websites.cpp`).
    #[must_use]
    pub fn summary_line(&self) -> String {
        [
            self.domain.as_str(),
            self.browser.as_str(),
            self.platform.as_str(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
    }

    /// Formats `browser, platform` joining non-empty fields.
    #[must_use]
    pub fn browser_and_platform(&self) -> String {
        [self.browser.as_str(), self.platform.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Formats location (falling back to IP) and active date (`LocationAndDate` in `settings_websites.cpp`).
    #[must_use]
    pub fn location_and_date(&self) -> String {
        let loc = if self.region.is_empty() {
            &self.ip
        } else {
            &self.region
        };
        if self.hash != 0 && !self.active_time.is_empty() {
            format!("{loc}  ·  {}", self.active_time)
        } else {
            loc.to_owned()
        }
    }
}

/// A saved custom notification ringtone document (`account.getSavedRingtones` / `boxes/ringtones_box.cpp`).
#[derive(Debug, Clone, PartialEq)]
pub struct SavedRingtone {
    /// Telegram document ID.
    pub id: i64,
    /// Document access hash (needed for `account.saveRingtone`).
    pub access_hash: i64,
    /// Document file reference bytes (needed for `account.saveRingtone`).
    pub file_reference: Vec<u8>,
    /// Display name derived from filename (without extension) or audio metadata (`ExtractRingtoneName`).
    pub title: String,
    /// Original filename (e.g. `"bell.mp3"`).
    pub file_name: String,
    /// MIME type (e.g. `"audio/mpeg"`).
    pub mime_type: String,
    /// File size in bytes.
    pub size: i64,
    /// Audio duration in seconds.
    pub duration_secs: f64,
    /// Upload Unix timestamp.
    pub date: i32,
}

impl SavedRingtone {
    /// Extracts a human-readable ringtone label matching `ExtractRingtoneName` in `boxes/ringtones_box.cpp:92-113`.
    #[must_use]
    pub fn display_name(&self) -> String {
        if !self.title.trim().is_empty() {
            return self.title.clone();
        }
        if !self.file_name.trim().is_empty() {
            if let Some((stem, ext)) = self.file_name.rsplit_once('.')
                && !stem.is_empty()
                && !ext.is_empty()
            {
                return stem.to_owned();
            }
            return self.file_name.clone();
        }
        format!("Ringtone {}", self.id)
    }
}

/// Server-side 2FA cloud password state (`Core::CloudPasswordState`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CloudPasswordState {
    /// Whether a 2FA cloud password is currently configured.
    pub has_password: bool,
    /// Whether a verified recovery email is configured.
    pub has_recovery: bool,
    /// Optional password hint text.
    pub hint: String,
    /// Masked recovery email pattern when confirmed (e.g. `"a***@example.test"`).
    pub email_pattern: String,
    /// Unconfirmed recovery email pattern awaiting verification code.
    pub unconfirmed_email_pattern: String,
    /// Expected confirmation code length parsed from `EMAIL_UNCONFIRMED_<N>` (`0` if none).
    pub unconfirmed_code_length: i32,
    /// Login email pattern (if configured separately).
    pub login_email_pattern: String,
    /// Pending wait-period password reset completion Unix timestamp (`0` if none).
    pub pending_reset_date: i32,
}

/// Outcome of `account.resetPassword` (`Api::CloudPassword::resetPassword`, `api_cloud_password.cpp:141-170`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudPasswordResetOutcome {
    /// Password was reset immediately (`account.resetPasswordOk`).
    ResetOk(CloudPasswordState),
    /// Reset request scheduled with a wait period (`account.resetPasswordRequestedWait`).
    RequestedWait {
        /// Unix timestamp when the reset will complete.
        until_date: i32,
    },
    /// Previous reset was cancelled recently; must wait before retrying
    /// (`account.resetPasswordFailedWait`).
    FailedWait {
        /// Unix timestamp when another reset may be requested.
        retry_date: i32,
    },
}

/// Errors emitted by 2FA cloud password operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudPasswordError {
    /// Supplied current password did not match.
    WrongPassword,
    /// New password cannot be empty.
    EmptyPassword,
    /// Hint matched the password itself.
    HintMatchesPassword,
    /// Email format is invalid.
    InvalidEmail,
    /// Verification code did not match.
    InvalidCode,
    /// Passport `secure_settings` failed decryption or secret ID verification (`SUGGEST_SECRET_RESET`).
    SuggestSecretReset,
    /// Other server-reported RPC error string.
    Server(String),
}

impl std::fmt::Display for CloudPasswordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongPassword => f.write_str("Incorrect current password."),
            Self::EmptyPassword => f.write_str("Password cannot be empty."),
            Self::HintMatchesPassword => {
                f.write_str("Password hint cannot be the same as the password.")
            }
            Self::InvalidEmail => f.write_str("Please enter a valid email address."),
            Self::InvalidCode => f.write_str("Invalid verification code (mock code: 123456)."),
            Self::SuggestSecretReset => f.write_str(
                "Telegram Passport secret could not be decrypted and must be reset before changing the password.",
            ),
            Self::Server(err) => write!(f, "Server error: {err}"),
        }
    }
}

/// Notification chat scope (`Data::DefaultNotify`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NotifyScope {
    /// Private 1:1 chats (`DefaultNotify::User`).
    PrivateChats,
    /// Group chats (`DefaultNotify::Group`).
    Groups,
    /// Broadcast channels (`DefaultNotify::Broadcast`).
    Channels,
}

impl NotifyScope {
    /// All notification scopes in tdesktop order.
    pub const ALL: [Self; 3] = [Self::PrivateChats, Self::Groups, Self::Channels];

    /// Display label for this scope.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::PrivateChats => "Private Chats",
            Self::Groups => "Groups",
            Self::Channels => "Channels",
        }
    }

    /// Localized display label from `tg_lang::tr::*`.
    #[must_use]
    pub fn localized_title(self) -> String {
        match self {
            Self::PrivateChats => tr::lng_notification_private_chats(),
            Self::Groups => tr::lng_notification_groups(),
            Self::Channels => tr::lng_notification_channels(),
        }
    }
}

/// Per-chat notification override entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyException {
    /// Target peer.
    pub peer: PeerRef,
    /// Whether notifications are muted for this peer.
    pub muted: bool,
    /// Custom sound name (empty = default).
    pub sound: String,
}

/// Per-scope server notification settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeNotifySettings {
    /// Whether notifications are enabled by default for this scope.
    pub enabled: bool,
    /// Show message text preview for this scope.
    pub show_preview: bool,
    /// Sound name (`"Default"`, `"None"`, or custom ringtone name).
    pub sound: String,
    /// Per-chat exceptions within this scope.
    pub exceptions: Vec<NotifyException>,
}

impl Default for ScopeNotifySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            show_preview: true,
            sound: "Default".to_owned(),
            exceptions: Vec::new(),
        }
    }
}

impl ScopeNotifySettings {
    /// Formats the subtitle under a scope row (matching `AddTypeButton` in `settings_notifications.cpp`).
    #[must_use]
    pub fn subtitle(&self) -> String {
        let count = self.exceptions.len();
        if count == 0 {
            let state = if self.enabled {
                tr::lng_settings_cloud_password_on()
            } else {
                tr::lng_settings_cloud_password_off()
            };
            format!("{state}, {}", tr::lng_notification_click_to_change())
        } else {
            let exc = tr::lng_notification_exceptions(count as i64);
            if self.enabled {
                tr::lng_notification_on(&exc)
            } else {
                tr::lng_notification_off(&exc)
            }
        }
    }
}

/// Reactions notification source filter (`Api::ReactionsNotifyFrom`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReactionsNotifyFrom {
    /// Disabled.
    None,
    /// From contacts only.
    Contacts,
    /// From everyone.
    #[default]
    All,
}

impl ReactionsNotifyFrom {
    /// Human-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "Nobody",
            Self::Contacts => "Contacts",
            Self::All => "Everyone",
        }
    }

    /// Localized label from `tg_lang::tr::*`.
    #[must_use]
    pub fn localized_label(self) -> String {
        match self {
            Self::None => tr::lng_notification_reactions_from_nobody(),
            Self::Contacts => tr::lng_notification_reactions_from_contacts(),
            Self::All => tr::lng_notification_reactions_from_all(),
        }
    }
}

/// Server-side reactions and contact-joined notification settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationEventsSettings {
    /// Notify when a contact joins Telegram.
    pub contact_joined: bool,
    /// Notify about reactions to your messages (`messages_notify_from`).
    pub message_reactions: ReactionsNotifyFrom,
    /// Notify about reactions to your stories (`stories_notify_from`).
    pub story_reactions: ReactionsNotifyFrom,
    /// Notify about votes in your polls (`poll_votes_notify_from`).
    pub poll_votes: ReactionsNotifyFrom,
    /// Whether to display the sender's name in reaction notifications (`show_previews`).
    pub show_reaction_previews: bool,
}

impl Default for NotificationEventsSettings {
    fn default() -> Self {
        Self {
            contact_joined: true,
            message_reactions: ReactionsNotifyFrom::All,
            story_reactions: ReactionsNotifyFrom::All,
            poll_votes: ReactionsNotifyFrom::All,
            show_reaction_previews: true,
        }
    }
}

impl NotificationEventsSettings {
    /// Returns `true` if either message reactions or poll vote notifications are enabled
    /// (`ReactionsNotifySettings::enabledCurrent()` in `api_reactions_notify_settings.cpp`).
    #[must_use]
    pub const fn reactions_enabled(&self) -> bool {
        !matches!(self.message_reactions, ReactionsNotifyFrom::None)
            || !matches!(self.poll_votes, ReactionsNotifyFrom::None)
    }

    /// Sets both message reaction and poll vote filters to `from`
    /// (`ReactionsNotifySettings::setAllFrom()` in `api_reactions_notify_settings.cpp`).
    pub const fn set_all_reactions_from(&mut self, from: ReactionsNotifyFrom) {
        self.message_reactions = from;
        self.poll_votes = from;
    }

    /// Returns a localized summary for the Notifications -> Reactions split-toggle row.
    #[must_use]
    pub fn reactions_summary(&self) -> String {
        let msg_on = self.message_reactions != ReactionsNotifyFrom::None;
        let poll_on = self.poll_votes != ReactionsNotifyFrom::None;
        match (msg_on, poll_on) {
            (true, true) => format!(
                "{}, {}",
                tr::lng_notification_reactions_messages(),
                tr::lng_notification_reactions_poll_votes()
            ),
            (true, false) => tr::lng_notification_reactions_messages(),
            (false, true) => tr::lng_notification_reactions_poll_votes(),
            (false, false) => tr::lng_notification_reactions_from_nobody(),
        }
    }
}

/// Auto-download source category (`Data::AutoDownload::Source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AutoDownloadSource {
    /// Private chats (`0`).
    Private = 0,
    /// Group chats (`1`).
    Groups = 1,
    /// Channels (`2`).
    Channels = 2,
}

impl AutoDownloadSource {
    /// All three sources in tdesktop order.
    pub const ALL: [Self; 3] = [Self::Private, Self::Groups, Self::Channels];

    /// Display title.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Private => "In private chats",
            Self::Groups => "In groups",
            Self::Channels => "In channels",
        }
    }
}

/// Auto-download media type (`Data::AutoDownload::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AutoDownloadType {
    /// Photos (`0`).
    Photo = 0,
    /// Videos (`1`).
    AutoPlayVideo = 1,
    /// Voice messages (`2`).
    VoiceMessage = 2,
    /// Round video messages (`3`).
    AutoPlayVideoMessage = 3,
    /// Music / audio files (`4`).
    Music = 4,
    /// GIF animations (`5`).
    AutoPlayGif = 5,
    /// Generic files/documents (`6`).
    File = 6,
}

impl AutoDownloadType {
    /// All 7 media types in tdesktop order.
    pub const ALL: [Self; 7] = [
        Self::Photo,
        Self::AutoPlayVideo,
        Self::VoiceMessage,
        Self::AutoPlayVideoMessage,
        Self::Music,
        Self::AutoPlayGif,
        Self::File,
    ];

    /// Display label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Photo => "Photos",
            Self::AutoPlayVideo => "Videos",
            Self::VoiceMessage => "Voice messages",
            Self::AutoPlayVideoMessage => "Video messages",
            Self::Music => "Music",
            Self::AutoPlayGif => "GIFs",
            Self::File => "Files",
        }
    }

    /// Whether this type supports configurable max byte size limits (Photos are on/off).
    #[must_use]
    pub const fn has_size_limit(self) -> bool {
        !matches!(self, Self::Photo)
    }
}

/// Auto-download byte limits per source and media type (`Data::AutoDownload::Full`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoDownloadSettings {
    /// Byte limits indexed by `[source][media_type]`; `0` means disabled.
    pub limits: [[i64; 7]; 3],
    /// Global media autoplay toggles (Chat Settings).
    pub autoplay_videos: bool,
    /// Autoplay GIF animations.
    pub autoplay_gifs: bool,
    /// Autoplay round video messages.
    pub autoplay_round_videos: bool,
}

impl Default for AutoDownloadSettings {
    fn default() -> Self {
        let mut s = Self {
            limits: [[0; 7]; 3],
            autoplay_videos: true,
            autoplay_gifs: true,
            autoplay_round_videos: true,
        };
        for src in AutoDownloadSource::ALL {
            s.set_defaults_for_source(src);
        }
        s
    }
}

impl AutoDownloadSettings {
    /// Returns the configured byte limit (`0` = disabled) for `(source, kind)`.
    #[must_use]
    pub const fn bytes_limit(&self, source: AutoDownloadSource, kind: AutoDownloadType) -> i64 {
        self.limits[source as usize][kind as usize]
    }

    /// Sets the byte limit clamped to `0..=MAX_AUTO_DOWNLOAD_BYTES`.
    pub fn set_bytes_limit(
        &mut self,
        source: AutoDownloadSource,
        kind: AutoDownloadType,
        bytes: i64,
    ) {
        self.limits[source as usize][kind as usize] = bytes.clamp(0, MAX_AUTO_DOWNLOAD_BYTES);
    }

    /// Returns `true` if a file of `file_size` bytes should auto-download.
    #[must_use]
    pub const fn should_download(
        &self,
        source: AutoDownloadSource,
        kind: AutoDownloadType,
        file_size: i64,
    ) -> bool {
        let limit = self.bytes_limit(source, kind);
        limit > 0 && file_size <= limit
    }

    /// Returns `true` if any media type is enabled for `source` (`HasEnabledTypes`).
    #[must_use]
    pub fn has_enabled_types(&self, source: AutoDownloadSource) -> bool {
        self.limits[source as usize].iter().any(|&v| v > 0)
    }

    /// Disables all types for `source` (`SetDisabledForSource`).
    pub fn set_disabled_for_source(&mut self, source: AutoDownloadSource) {
        self.limits[source as usize] = [0; 7];
    }

    /// Restores tdesktop defaults for `source` (`SetDefaultsForSource`).
    pub fn set_defaults_for_source(&mut self, source: AutoDownloadSource) {
        const MB: i64 = 1024 * 1024;
        self.limits[source as usize] = [
            10 * MB, // Photo
            15 * MB, // Video
            10 * MB, // Voice
            10 * MB, // Round Video
            10 * MB, // Music
            10 * MB, // GIF
            5 * MB,  // File
        ];
    }

    /// Formats a compact summary of enabled media types for `source`.
    #[must_use]
    pub fn source_summary(&self, source: AutoDownloadSource) -> String {
        let enabled: Vec<&str> = AutoDownloadType::ALL
            .iter()
            .filter(|&&t| self.bytes_limit(source, t) > 0)
            .map(|t| t.label())
            .collect();
        if enabled.is_empty() {
            "Disabled".to_owned()
        } else if enabled.len() == AutoDownloadType::ALL.len() {
            "All media types".to_owned()
        } else {
            enabled.join(", ")
        }
    }
}

/// Formats a byte count into a clean human-readable string (KB / MB / GB).
#[must_use]
pub fn format_bytes(bytes: i64) -> String {
    const KB: i64 = 1024;
    const MB: i64 = 1024 * KB;
    const GB: i64 = 1024 * MB;
    if bytes <= 0 {
        "Off".to_owned()
    } else if bytes >= GB {
        let gb = bytes as f64 / GB as f64;
        if (gb - gb.round()).abs() < 0.05 {
            format!("{} GB", gb.round() as i64)
        } else {
            format!("{gb:.1} GB")
        }
    } else if bytes >= MB {
        let mb = bytes as f64 / MB as f64;
        if (mb - mb.round()).abs() < 0.05 {
            format!("{} MB", mb.round() as i64)
        } else {
            format!("{mb:.1} MB")
        }
    } else if bytes >= KB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Installed or archived sticker/emoji set metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerSetInfo {
    /// Unique set ID.
    pub id: u64,
    /// Display title.
    pub title: String,
    /// Short shareable slug.
    pub short_name: String,
    /// Number of stickers/emoji in the set.
    pub count: u32,
    /// Whether this is a custom emoji set rather than a sticker set.
    pub is_emoji: bool,
    /// Whether this set is currently archived.
    pub archived: bool,
    /// Animated / video sticker indicator.
    pub animated: bool,
}

/// Maximum character length of a shareable folder invite link title (`kMaxLinkTitleLength = 32`).
pub const MAX_FOLDER_LINK_TITLE_LEN: usize = 32;

/// Exact 7 Telegram chat folder color tag palette swatches (`color_index` 0..7).
pub const FOLDER_TAG_COLORS: [(u8, u32, &str); 7] = [
    (0, 0xCC5049, "Red"),
    (1, 0xD67722, "Orange"),
    (2, 0x955CDB, "Violet"),
    (3, 0x40A920, "Green"),
    (4, 0x309EBA, "Cyan"),
    (5, 0x368AD1, "Blue"),
    (6, 0xC7508B, "Pink"),
];

/// Shareable chat folder invite link (`Data::ChatFilterLink` / `ExportedChatlistInvite`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderInviteLink {
    /// Local dialog filter ID owning this link.
    pub filter_id: i32,
    /// Optional human-readable label for the link (`<= 32` chars).
    pub title: String,
    /// Full URL (e.g. `https://t.me/addlist/<slug>`).
    pub url: String,
    /// Invite slug extracted from `url`.
    pub slug: String,
    /// Included peer IDs in this invite link.
    pub peers: Vec<i64>,
}

/// Icon choice for a chat folder (`Ui::FilterIcon` in `ui/filter_icons.h` & `ui/filter_icon_panel.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FolderIcon {
    #[default]
    Custom,
    All,
    Unread,
    Unmuted,
    Bots,
    Channels,
    Groups,
    Private,
    Work,
    Star,
    Book,
    Game,
    Cat,
    Money,
    Light,
    Like,
    Note,
    Palette,
    Travel,
    Sport,
    Study,
    Airplane,
    Crown,
    Flower,
    Home,
    Love,
    Mask,
    Party,
    Trade,
    Setup,
}

impl FolderIcon {
    /// All selectable icons in `ui/filter_icon_panel.cpp` (`kIcons`, 6 columns x 5 rows = 30 icons).
    pub const ALL: [Self; 30] = [
        Self::Cat,
        Self::Book,
        Self::Money,
        Self::Game,
        Self::Light,
        Self::Like,
        Self::Note,
        Self::Palette,
        Self::Travel,
        Self::Sport,
        Self::Star,
        Self::Study,
        Self::Airplane,
        Self::Private,
        Self::Groups,
        Self::All,
        Self::Unread,
        Self::Bots,
        Self::Crown,
        Self::Flower,
        Self::Home,
        Self::Love,
        Self::Mask,
        Self::Party,
        Self::Trade,
        Self::Work,
        Self::Unmuted,
        Self::Channels,
        Self::Custom,
        Self::Setup,
    ];

    /// Compact ASCII/symbolic label for rendering in icon pills.
    #[must_use]
    pub const fn badge(self) -> &'static str {
        match self {
            Self::Custom => "FLD",
            Self::All => "ALL",
            Self::Unread => "NEW",
            Self::Unmuted => "SND",
            Self::Bots => "BOT",
            Self::Channels => "CHN",
            Self::Groups => "GRP",
            Self::Private => "USR",
            Self::Work => "WRK",
            Self::Star => "STR",
            Self::Book => "LIB",
            Self::Game => "FUN",
            Self::Cat => "CAT",
            Self::Money => "PAY",
            Self::Light => "IDE",
            Self::Like => "LIK",
            Self::Note => "MUS",
            Self::Palette => "ART",
            Self::Travel => "TRV",
            Self::Sport => "SPT",
            Self::Study => "EDU",
            Self::Airplane => "AIR",
            Self::Crown => "VIP",
            Self::Flower => "FLW",
            Self::Home => "HOM",
            Self::Love => "LOV",
            Self::Mask => "MSK",
            Self::Party => "PTY",
            Self::Trade => "TRD",
            Self::Setup => "SET",
        }
    }

    /// Human-readable icon name.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Custom => "Folder",
            Self::All => "All Chats",
            Self::Unread => "Unread",
            Self::Unmuted => "Unmuted",
            Self::Bots => "Bots",
            Self::Channels => "Channels",
            Self::Groups => "Groups",
            Self::Private => "Personal",
            Self::Work => "Work",
            Self::Star => "Favorites",
            Self::Book => "Reading",
            Self::Game => "Games",
            Self::Cat => "Cat",
            Self::Money => "Money",
            Self::Light => "Idea",
            Self::Like => "Like",
            Self::Note => "Music",
            Self::Palette => "Art",
            Self::Travel => "Travel",
            Self::Sport => "Sport",
            Self::Study => "Study",
            Self::Airplane => "Flight",
            Self::Crown => "VIP",
            Self::Flower => "Nature",
            Self::Home => "Home",
            Self::Love => "Love",
            Self::Mask => "Entertainment",
            Self::Party => "Party",
            Self::Trade => "Finance",
            Self::Setup => "Setup",
        }
    }

    /// Wire emoticon string sent in `dialogFilter.emoticon` (exact table from `ui/filter_icons.cpp`).
    #[must_use]
    pub const fn emoticon(self) -> &'static str {
        match self {
            Self::Cat => "\u{1F431}",
            Self::Book => "\u{1F4D5}",
            Self::Money => "\u{1F4B0}",
            Self::Game => "\u{1F3AE}",
            Self::Light => "\u{1F4A1}",
            Self::Like => "\u{1F44C}",
            Self::Note => "\u{1F3B5}",
            Self::Palette => "\u{1F3A8}",
            Self::Travel => "\u{2708}\u{FE0F}",
            Self::Sport => "\u{26BD}\u{FE0F}",
            Self::Star => "\u{2B50}",
            Self::Study => "\u{1F393}",
            Self::Airplane => "\u{1F6EB}",
            Self::Private => "\u{1F464}",
            Self::Groups => "\u{1F465}",
            Self::All => "\u{1F4AC}",
            Self::Unread => "\u{2705}",
            Self::Bots => "\u{1F916}",
            Self::Crown => "\u{1F451}",
            Self::Flower => "\u{1F339}",
            Self::Home => "\u{1F3E0}",
            Self::Love => "\u{2764}",
            Self::Mask => "\u{1F3AD}",
            Self::Party => "\u{1F378}",
            Self::Trade => "\u{1F4C8}",
            Self::Work => "\u{1F4BC}",
            Self::Unmuted => "\u{1F514}",
            Self::Channels => "\u{1F4E2}",
            Self::Custom => "\u{1F4C1}",
            Self::Setup => "\u{1F4CB}",
        }
    }

    /// Looks up a [`FolderIcon`] by its TL `dialogFilter.emoticon` string (`LookupFilterIconByEmoji`).
    #[must_use]
    pub fn from_emoticon(emoji: &str) -> Option<Self> {
        let trimmed = emoji.trim();
        if trimmed.is_empty() {
            return None;
        }
        let stripped: String = trimmed.chars().filter(|&c| c != '\u{FE0F}').collect();
        for icon in Self::ALL {
            let candidate = icon.emoticon();
            if candidate == trimmed {
                return Some(icon);
            }
            let cand_stripped: String = candidate.chars().filter(|&c| c != '\u{FE0F}').collect();
            if cand_stripped == stripped {
                return Some(icon);
            }
        }
        None
    }
}

/// Chat folder / dialog filter (`Data::ChatFilter`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatFolder {
    /// Server-assigned filter ID (`2..=255`; `0` for unsaved drafts).
    pub id: i32,
    /// Folder title (max [`MAX_FOLDER_TITLE_LEN`] chars).
    pub title: String,
    /// Visual icon tag.
    pub icon: FolderIcon,
    /// Optional color tag index (`0..7`).
    pub color_index: Option<u8>,
    /// Include contacts.
    pub include_contacts: bool,
    /// Include non-contacts.
    pub include_non_contacts: bool,
    /// Include groups.
    pub include_groups: bool,
    /// Include channels.
    pub include_channels: bool,
    /// Include bots.
    pub include_bots: bool,
    /// Exclude muted chats.
    pub exclude_muted: bool,
    /// Exclude read chats.
    pub exclude_read: bool,
    /// Exclude archived chats.
    pub exclude_archived: bool,
    /// Explicitly included peer IDs.
    pub included_peers: Vec<i64>,
    /// Explicitly excluded peer IDs.
    pub excluded_peers: Vec<i64>,
    /// Whether this folder has an active shareable invite link.
    pub shareable: bool,
}

impl Default for ChatFolder {
    fn default() -> Self {
        Self {
            id: 0,
            title: String::new(),
            icon: FolderIcon::Custom,
            color_index: None,
            include_contacts: false,
            include_non_contacts: false,
            include_groups: false,
            include_channels: false,
            include_bots: false,
            exclude_muted: false,
            exclude_read: false,
            exclude_archived: false,
            included_peers: Vec::new(),
            excluded_peers: Vec::new(),
            shareable: false,
        }
    }
}

impl ChatFolder {
    /// Validates that a folder has a non-empty title (`<= 12` chars) and at least one inclusion rule.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let title = self.title.trim();
        let has_title = !title.is_empty() && title.chars().count() <= MAX_FOLDER_TITLE_LEN;
        let has_include = self.include_contacts
            || self.include_non_contacts
            || self.include_groups
            || self.include_channels
            || self.include_bots
            || !self.included_peers.is_empty();
        has_title && has_include
    }

    /// Formats a short human-readable summary of included rules/chats.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.include_contacts {
            parts.push("Contacts".to_owned());
        }
        if self.include_non_contacts {
            parts.push("Non-Contacts".to_owned());
        }
        if self.include_groups {
            parts.push("Groups".to_owned());
        }
        if self.include_channels {
            parts.push("Channels".to_owned());
        }
        if self.include_bots {
            parts.push("Bots".to_owned());
        }
        if !self.included_peers.is_empty() {
            let n = self.included_peers.len();
            parts.push(if n == 1 {
                "1 chat".to_owned()
            } else {
                format!("{n} chats")
            });
        }
        let mut out = if parts.is_empty() {
            "No chats".to_owned()
        } else {
            parts.join(", ")
        };
        if self.shareable {
            out.push_str("  ·  Shareable");
        }
        out
    }
}

/// Server-suggested chat folder (`Data::SuggestedFilter`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestedFolder {
    /// Template folder definition.
    pub folder: ChatFolder,
    /// Explanation shown below the suggested folder title.
    pub description: String,
}

/// Global server-side privacy and content toggles (`Api::GlobalPrivacy`,
/// `Api::SelfDestruct`, and `Api::SensitiveContent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalPrivacySettings {
    /// Automatically archive and mute new chats from non-contacts.
    pub archive_and_mute_new: bool,
    /// Keep archived chats in the archive even when new messages arrive.
    pub keep_archived_unmuted: bool,
    /// Keep archived folders collapsed.
    pub keep_archived_folders: bool,
    /// Hide read time in private chats.
    pub hide_read_time: bool,
    /// Require Telegram Premium for new chats from non-contacts.
    pub new_noncontact_peers_require_premium: bool,
    /// Required Telegram Stars charge for incoming messages from non-contacts (`None` or `Some(0)` = free).
    pub noncontact_peers_paid_stars: Option<i64>,
    /// Disable sensitive content filtering (18+ channels).
    pub disable_sensitive_content_filtering: bool,
    /// Whether the server allows changing the sensitive content setting for this account.
    pub sensitive_can_change: bool,
    /// Global auto-delete timer for new chats in seconds (`0` = disabled).
    pub global_ttl_seconds: i32,
}

impl Default for GlobalPrivacySettings {
    fn default() -> Self {
        Self {
            archive_and_mute_new: false,
            keep_archived_unmuted: false,
            keep_archived_folders: false,
            hide_read_time: false,
            new_noncontact_peers_require_premium: false,
            noncontact_peers_paid_stars: None,
            disable_sensitive_content_filtering: false,
            sensitive_can_change: true,
            global_ttl_seconds: 0,
        }
    }
}

/// Request payload for uploading a new profile photo or video avatar (`photos.uploadProfilePhoto`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProfilePhotoUpload {
    /// Static image file `(file_name, raw_bytes)`, e.g. JPEG/PNG.
    pub image: Option<(String, Vec<u8>)>,
    /// Animated video avatar file `(file_name, raw_bytes)`, e.g. MP4.
    pub video: Option<(String, Vec<u8>)>,
    /// Optional preview frame timestamp in seconds for a video avatar (`video_start_ts`).
    pub video_start_ts: Option<f64>,
    /// When `true`, sets the public fallback profile photo (`fallback = true`).
    pub fallback: bool,
}

impl ProfilePhotoUpload {
    /// Creates a static profile photo upload request.
    #[must_use]
    pub fn static_photo(name: impl Into<String>, bytes: Vec<u8>, fallback: bool) -> Self {
        Self {
            image: Some((name.into(), bytes)),
            video: None,
            video_start_ts: None,
            fallback,
        }
    }

    /// Creates a video avatar upload request with an optional frame timestamp.
    #[must_use]
    pub fn video_avatar(
        name: impl Into<String>,
        bytes: Vec<u8>,
        video_start_ts: Option<f64>,
        fallback: bool,
    ) -> Self {
        Self {
            image: None,
            video: Some((name.into(), bytes)),
            video_start_ts,
            fallback,
        }
    }
}

/// Output format selected in the Export Telegram Data entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportFormat {
    /// Human-readable HTML archive.
    #[default]
    Html,
    /// Machine-readable JSON archive.
    Json,
    /// Both HTML and JSON.
    HtmlAndJson,
}

/// Export configuration passed to [`SettingsBackend::request_export`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRequest {
    /// Export account profile & contacts.
    pub account_info: bool,
    /// Export personal chats.
    pub personal_chats: bool,
    /// Export group chats & channels.
    pub groups_and_channels: bool,
    /// Export media attachments (photos, videos, files).
    pub include_media: bool,
    /// Output format.
    pub format: ExportFormat,
    /// Target export directory path.
    pub path: String,
}

impl Default for ExportRequest {
    fn default() -> Self {
        Self {
            account_info: true,
            personal_chats: true,
            groups_and_channels: false,
            include_media: true,
            format: ExportFormat::Html,
            path: "Downloads/Rustgram Export".to_owned(),
        }
    }
}

/// Local cache storage statistics shown in Manage Local Storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalStorageStats {
    /// Cached photos & thumbnails in bytes.
    pub photos_bytes: i64,
    /// Cached videos & animations in bytes.
    pub videos_bytes: i64,
    /// Cached documents & music in bytes.
    pub files_bytes: i64,
    /// Cached stickers & emoji in bytes.
    pub stickers_bytes: i64,
    /// Maximum cache size limit in bytes (`0` = unlimited).
    pub size_limit_bytes: i64,
    /// Maximum retention age in days (`0` = forever).
    pub time_limit_days: i32,
}

impl LocalStorageStats {
    /// Total cached bytes across all categories.
    #[must_use]
    pub const fn total_bytes(&self) -> i64 {
        self.photos_bytes + self.videos_bytes + self.files_bytes + self.stickers_bytes
    }
}

/// Confirmation or notification prompt for cloud language pack actions
/// (`ConfirmSwitchBox`, `NotReadyBox`, `showOfferSwitchBox` in `lang_cloud_manager.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanguagePrompt {
    /// Confirmation dialog before switching via a `tg://setlanguage?lang=...` or
    /// `t.me/setlanguage/...` link (`ConfirmSwitchBox`).
    ConfirmLinkSwitch {
        /// Full cloud language metadata (official/beta/unofficial, completion %, URL).
        info: LangPackLanguageInfo,
        /// Localized title (`lng_language_switch_title`).
        title: String,
        /// Localized description (`lng_language_switch_about_official` or `_unofficial`).
        body: String,
        /// Localized confirmation button label (`lng_language_switch_apply`).
        apply_label: String,
    },
    /// Requested custom language pack has zero translated strings (`NotReadyBox`).
    NotReady {
        /// Cloud language metadata.
        info: LangPackLanguageInfo,
        /// Localized title (`lng_language_not_ready_title`).
        title: String,
        /// Localized explanation (`lng_language_not_ready_about`).
        body: String,
    },
    /// User is already using the requested language (`lng_language_already`).
    AlreadyUsing {
        /// Requested language code.
        lang_code: String,
        /// Localized notice text.
        message: String,
    },
    /// Server reported `LANG_CODE_NOT_SUPPORTED` (`lng_language_not_found`).
    NotFound {
        /// Requested language code.
        lang_code: String,
        /// Localized error text.
        message: String,
    },
    /// First-run suggested-language prompt from `help.getConfig` (`suggested_lang_code`).
    SuggestedLanguage {
        /// Suggested language descriptor.
        language: tg_lang::Language,
        /// Localized CTA (e.g. `lng_switch_to_this` in the suggested language).
        switch_cta: String,
        /// Prompt message (`showOfferSwitchBox` in `lang_cloud_manager.cpp:372`).
        prompt_text: String,
    },
}

/// Registered account passkey entry (`Data::PasskeyEntry` / `passkey#98613ebf`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasskeyEntry {
    /// Server-assigned passkey credential identifier.
    pub id: String,
    /// User-friendly authenticator / provider name (e.g. `"Windows Hello"`).
    pub name: String,
    /// Creation timestamp (Unix seconds).
    pub date: i32,
    /// Optional custom emoji document ID representing the password manager.
    pub software_emoji_id: Option<i64>,
    /// Optional timestamp of the most recent sign-in using this passkey (Unix seconds).
    pub last_usage_date: Option<i32>,
}

/// Live reachability/ping check status for a configured proxy (`boxes/connection_box.cpp`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProxyCheckStatus {
    /// Not yet tested in this session.
    #[default]
    NotTested,
    /// Currently probing the proxy endpoint.
    Checking,
    /// Proxy responded with a valid `resPQ` probe and measured RTT in milliseconds.
    Available {
        /// Round-trip latency in milliseconds.
        ping_ms: u32,
    },
    /// Proxy check failed or timed out.
    Unavailable {
        /// Diagnostic failure reason.
        reason: String,
    },
}

impl ProxyCheckStatus {
    /// Formats a human-readable status label matching tdesktop's `ProxiesBoxController`.
    #[must_use]
    pub fn format_label(&self, is_selected_connected: bool) -> String {
        match self {
            Self::NotTested => {
                if is_selected_connected {
                    "connected".to_owned()
                } else {
                    "not tested".to_owned()
                }
            }
            Self::Checking => "checking...".to_owned(),
            Self::Available { ping_ms } => {
                let state = if is_selected_connected {
                    "connected"
                } else {
                    "available"
                };
                format!("{state} (ping: {ping_ms} ms)")
            }
            Self::Unavailable { reason } => {
                if reason.is_empty() {
                    "unavailable".to_owned()
                } else {
                    format!("unavailable ({reason})")
                }
            }
        }
    }
}

/// Trait abstracting all server-side Telegram settings operations.
///
/// Local desktop settings live directly in [`tg_core::CoreSettings`]; all account/cloud
/// settings go through this trait, implemented by [`super::mock::MockSettingsBackend`]
/// until live `tg-api` integration is wired.
pub trait SettingsBackend: Send {
    /// Returns the cloud Hunspell dictionary source for `Settings -> Advanced -> Manage
    /// dictionaries` (`DictManifestLoader` / `DictLoader` in `spellchecker_common.cpp`).
    ///
    /// `None` (the default) makes the manager use its offline built-in source.
    fn dictionary_cloud_source(
        &self,
    ) -> Option<Box<dyn crate::core_types::dto::dictionaries::DictionaryCloudSource + Send>> {
        None
    }

    /// Returns the current user's profile info.
    fn profile(&self) -> ProfileInfo;
    /// Updates first and last name.
    fn update_name(&mut self, first_name: &str, last_name: &str) -> Result<(), String>;
    /// Updates user bio (`<= MAX_BIO_LEN`).
    fn update_bio(&mut self, bio: &str) -> Result<(), String>;
    /// Checks username syntax and availability without saving.
    fn check_username(&self, username: &str) -> UsernameStatus;
    /// Updates the user's `@username` (empty clears it).
    fn update_username(&mut self, username: &str) -> Result<(), String>;
    /// Updates or clears the user's birthday.
    fn update_birthday(&mut self, birthday: Option<Birthday>) -> Result<(), String>;
    /// Updates avatar color index and optional custom photo label (or local file path in live mode).
    fn update_profile_photo(&mut self, color_index: u8, photo_label: Option<String>);
    /// Uploads a profile photo, video avatar, or public fallback photo (`photos.uploadProfilePhoto`),
    /// returning the assigned server photo ID on success.
    fn upload_profile_photo(&mut self, request: ProfilePhotoUpload) -> Result<i64, String> {
        let label = request
            .image
            .as_ref()
            .or(request.video.as_ref())
            .map(|(name, _)| name.clone());
        let color = self.profile().photo_color_index;
        self.update_profile_photo(color, label);
        Ok(0)
    }
    /// Toggles phone number spoiler visibility in the settings header.
    fn set_phone_hidden(&mut self, hidden: bool);

    /// Returns known contacts/peers available for privacy exceptions, blocking, and folders.
    fn contacts(&self) -> Vec<PeerRef>;

    /// Returns all privacy rules keyed by [`PrivacyKey`].
    fn privacy_rules(&self) -> BTreeMap<PrivacyKey, PrivacyRule>;
    /// Returns the privacy rule for `key`.
    fn privacy_rule(&self, key: PrivacyKey) -> PrivacyRule;
    /// Saves an updated privacy rule for `key`.
    fn set_privacy_rule(&mut self, key: PrivacyKey, rule: PrivacyRule);

    /// Returns global privacy settings.
    fn global_privacy(&self) -> GlobalPrivacySettings;
    /// Updates global privacy settings.
    fn set_global_privacy(&mut self, settings: GlobalPrivacySettings);

    /// Returns account self-destruction TTL in days.
    fn account_ttl_days(&self) -> i32;
    /// Updates account self-destruction TTL in days.
    fn set_account_ttl_days(&mut self, days: i32);

    /// Clears saved payment/shipping info (`payments.clearSavedInfo`).
    fn clear_payment_info(&mut self, clear_shipping: bool, clear_credentials: bool);

    /// Returns active sessions/authorizations (current session first).
    fn active_sessions(&self) -> Vec<ActiveSession>;
    /// Terminates a single non-current session by hash.
    fn terminate_session(&mut self, hash: u64) -> bool;
    /// Terminates all sessions other than the current one; returns count terminated.
    fn terminate_all_other_sessions(&mut self) -> usize;
    /// Updates per-session call / secret chat permissions.
    fn update_session_permissions(
        &mut self,
        hash: u64,
        accept_calls: bool,
        accept_secret_chats: bool,
    );
    /// Confirms (`confirm = true`) or rejects (`confirm = false`) an unreviewed authorization
    /// (`account.changeAuthorizationSettings(confirmed = true)` / `account.resetAuthorization`).
    fn review_authorization(&mut self, _hash: u64, _confirm: bool) -> Result<bool, String> {
        Err("Authorization review is not supported by this backend.".to_owned())
    }
    /// Sets or clears (`None`) the user's personal channel (`account.updatePersonalChannel`).
    fn update_personal_channel(&mut self, _channel_id: Option<i64>) -> Result<bool, String> {
        Err("Personal channel updates are not supported by this backend.".to_owned())
    }
    /// Returns inactive session auto-termination TTL in days.
    fn session_ttl_days(&self) -> i32;
    /// Sets inactive session auto-termination TTL in days.
    fn set_session_ttl_days(&mut self, days: i32);

    /// Returns currently blocked users.
    fn blocked_users(&self) -> Vec<PeerRef>;
    /// Blocks a peer by ID.
    fn block_peer(&mut self, peer_id: i64);
    /// Unblocks a peer by ID.
    fn unblock_peer(&mut self, peer_id: i64);

    /// Returns current 2FA cloud password state.
    fn cloud_password_state(&self) -> CloudPasswordState;
    /// Verifies the current 2FA cloud password.
    fn verify_cloud_password(&self, password: &str) -> Result<(), CloudPasswordError>;
    /// Enables or changes the 2FA cloud password.
    fn set_cloud_password(
        &mut self,
        current_password: Option<&str>,
        new_password: &str,
        hint: &str,
        recovery_email: Option<&str>,
    ) -> Result<CloudPasswordState, CloudPasswordError>;
    /// Confirms a pending recovery email using the verification code.
    fn confirm_recovery_email(
        &mut self,
        code: &str,
    ) -> Result<CloudPasswordState, CloudPasswordError>;
    /// Cancels a pending unconfirmed recovery email.
    fn cancel_recovery_email(&mut self) -> CloudPasswordState;
    /// Disables the 2FA cloud password after verifying the current password.
    fn disable_cloud_password(
        &mut self,
        current_password: &str,
    ) -> Result<CloudPasswordState, CloudPasswordError>;
    /// Updates only the 2FA recovery email (`account.updatePasswordSettings` with `flags = 0x02`).
    fn set_recovery_email(
        &mut self,
        _current_password: &str,
        _recovery_email: &str,
    ) -> Result<CloudPasswordState, CloudPasswordError> {
        Err(CloudPasswordError::Server("NOT_SUPPORTED".to_owned()))
    }
    /// Resends the 2FA recovery email confirmation code (`account.resendPasswordEmail`).
    fn resend_recovery_email(&mut self) -> Result<CloudPasswordState, CloudPasswordError> {
        Err(CloudPasswordError::Server("NOT_SUPPORTED".to_owned()))
    }
    /// Requests or completes a wait-period 2FA password reset (`account.resetPassword`).
    fn reset_cloud_password(&mut self) -> Result<CloudPasswordResetOutcome, CloudPasswordError> {
        Err(CloudPasswordError::Server("NOT_SUPPORTED".to_owned()))
    }
    /// Declines a pending 2FA password reset (`account.declinePasswordReset`).
    fn decline_cloud_password_reset(&mut self) -> Result<CloudPasswordState, CloudPasswordError> {
        Err(CloudPasswordError::Server("NOT_SUPPORTED".to_owned()))
    }

    /// Returns notification settings for `scope`.
    fn scope_notifications(&self, scope: NotifyScope) -> ScopeNotifySettings;
    /// Updates notification settings for `scope`.
    fn set_scope_notifications(&mut self, scope: NotifyScope, settings: ScopeNotifySettings);
    /// Returns contact-joined & reactions notification settings.
    fn notification_events(&self) -> NotificationEventsSettings;
    /// Updates contact-joined & reactions notification settings.
    fn set_notification_events(&mut self, events: NotificationEventsSettings);
    /// Resets all notification settings & exceptions to defaults.
    fn reset_all_notifications(&mut self);

    /// Returns media auto-download and autoplay settings.
    fn auto_download(&self) -> AutoDownloadSettings;
    /// Updates media auto-download and autoplay settings.
    fn set_auto_download(&mut self, settings: AutoDownloadSettings);

    /// Returns installed and archived sticker/emoji sets.
    fn sticker_sets(&self) -> Vec<StickerSetInfo>;
    /// Archives or unarchives a sticker/emoji set by ID.
    fn set_sticker_set_archived(&mut self, id: u64, archived: bool);
    /// Removes a sticker/emoji set by ID.
    fn remove_sticker_set(&mut self, id: u64);

    /// Downloads and activates desktop emoji set `set_id` via
    /// [`crate::core_types::emoji_sets::EmojiSetsManager::load_and_switch_to`]
    /// (`@tdesktop_emoji` cloud archive download + safe ZIP unpack).
    ///
    /// Returns the number of extracted archive files (`0` if already ready or built-in).
    fn download_and_switch_emoji_set(
        &mut self,
        _manager: &mut crate::core_types::emoji_sets::EmojiSetsManager,
        _set_id: i32,
    ) -> Result<usize, String> {
        Err("Downloading cloud emoji sets is not supported by this backend.".to_owned())
    }

    /// Returns configured chat folders in display order.
    fn folders(&self) -> Vec<ChatFolder>;
    /// Returns suggested chat folders not yet added.
    fn suggested_folders(&self) -> Vec<SuggestedFolder>;
    /// Creates or updates a chat folder (assigns an ID if `folder.id == 0`).
    fn save_folder(&mut self, folder: ChatFolder) -> Result<ChatFolder, String>;
    /// Deletes a chat folder by ID.
    fn delete_folder(&mut self, id: i32) -> bool;
    /// Reorders chat folders by ID sequence.
    fn reorder_folders(&mut self, ids: &[i32]);
    /// Accepts a suggested folder by index and moves it into active folders.
    fn accept_suggested_folder(&mut self, index: usize) -> Option<ChatFolder>;
    /// Exports a shareable invite link for `folder_id` (`chatlists.exportChatlistInvite`).
    fn export_folder_invite_link(
        &mut self,
        _folder_id: i32,
        _title: &str,
        _peers: &[i64],
    ) -> Result<String, String> {
        Err("Exporting folder invite links is not supported by this backend.".to_owned())
    }

    /// Returns whether colored folder tags on chat rows are enabled (`messages.dialogFilters.tags_enabled`).
    fn folder_tags_enabled(&self) -> bool {
        false
    }

    /// Toggles colored folder tags on chat rows (`messages.toggleDialogFilterTags`).
    fn set_folder_tags_enabled(&mut self, _enabled: bool) -> Result<bool, String> {
        Ok(true)
    }

    /// Returns all exported shareable invite links for folder `filter_id` (`chatlists.getExportedInvites`).
    fn folder_invite_links(&self, _filter_id: i32) -> Vec<FolderInviteLink> {
        Vec::new()
    }

    /// Creates a new shareable invite link for folder `filter_id` (`chatlists.exportChatlistInvite`).
    fn create_folder_invite_link(
        &mut self,
        _filter_id: i32,
        _title: &str,
        _peers: &[i64],
    ) -> Result<FolderInviteLink, String> {
        Err("Shareable folder links are not supported by this backend.".to_owned())
    }

    /// Edits an existing shareable folder invite link (`chatlists.editExportedInvite`).
    fn edit_folder_invite_link(
        &mut self,
        _filter_id: i32,
        _slug: &str,
        _title: Option<&str>,
        _peers: Option<&[i64]>,
    ) -> Result<FolderInviteLink, String> {
        Err("Editing folder invite links is not supported by this backend.".to_owned())
    }

    /// Deletes a shareable folder invite link (`chatlists.deleteExportedInvite`).
    fn delete_folder_invite_link(&mut self, _filter_id: i32, _slug: &str) -> Result<bool, String> {
        Err("Deleting folder invite links is not supported by this backend.".to_owned())
    }

    /// Checks for new chats added to a joined shareable folder (`chatlists.getChatlistUpdates`).
    fn check_folder_updates(&mut self, _filter_id: i32) -> Result<Vec<i64>, String> {
        Ok(Vec::new())
    }

    /// Joins new chats returned by [`Self::check_folder_updates`] (`chatlists.joinChatlistUpdates`).
    fn join_folder_updates(&mut self, _filter_id: i32, _peers: &[i64]) -> Result<(), String> {
        Ok(())
    }

    /// Returns available cloud languages.
    fn languages(&self) -> Vec<LangPackLanguageInfo>;

    /// Returns the currently active language code (e.g. `"en"`, `"de"`).
    fn active_language_code(&self) -> String {
        let id = tg_lang::with_current(|inst| inst.id().to_owned());
        if id.is_empty() {
            tg_lang::default_language_id().to_owned()
        } else {
            id
        }
    }

    /// Returns recently used languages (`Local::readRecentLanguages`).
    fn recent_languages(&self) -> Vec<tg_lang::Language> {
        Vec::new()
    }

    /// Applies the chosen cloud language pack at runtime (downloads full + base pack in live mode).
    ///
    /// Returns the applied language display name on success.
    fn apply_language(&mut self, lang_code: &str) -> Result<String, String> {
        Ok(lang_code.to_owned())
    }

    /// Resolves a `tg://setlanguage?lang=...` or `t.me/setlanguage/...` link.
    ///
    /// Returns `Ok(Some(prompt))` for switch confirmations/errors or `Ok(None)` when the link
    /// simply opens the language picker (`tg://setlanguage`).
    fn open_set_language_link(&mut self, _url: &str) -> Result<Option<LanguagePrompt>, String> {
        Ok(None)
    }

    /// Confirms and applies the currently pending [`LanguagePrompt`] (link switch or suggestion).
    fn confirm_language_prompt(&mut self) -> Result<Option<String>, String> {
        Ok(None)
    }

    /// Dismisses the currently pending [`LanguagePrompt`] (or declines a first-run suggestion).
    fn dismiss_language_prompt(&mut self) {}

    /// Returns the currently pending [`LanguagePrompt`], if any.
    fn pending_language_prompt(&self) -> Option<LanguagePrompt> {
        None
    }

    /// Removes a non-official language from recent languages (`Local::removeRecentLanguage`).
    fn remove_recent_language(&mut self, _lang_code: &str) {}

    /// Returns local storage cache statistics.
    fn local_storage_stats(&self) -> LocalStorageStats;
    /// Clears local cache (returns bytes freed).
    fn clear_local_storage(&mut self) -> i64;
    /// Updates local storage cache limits.
    fn set_local_storage_limits(&mut self, size_limit_bytes: i64, time_limit_days: i32);

    /// Submits a data export request and returns a status message.
    fn request_export(&mut self, request: &ExportRequest) -> String;

    /// Returns authoritative proxy settings when managed by a live/persistent backend.
    fn proxy_settings(&self) -> Option<tg_core::SettingsProxy> {
        None
    }

    /// Applies updated proxy settings (persisting to `tdata` and reconnecting active DCs).
    fn apply_proxy_settings(&mut self, _settings: &tg_core::SettingsProxy) -> Result<(), String> {
        Ok(())
    }

    /// Triggers an asynchronous reachability/ping check for `proxy`.
    fn check_proxy(&mut self, _proxy: &tg_core::ProxyData) {}

    /// Returns the latest reachability/ping check status for `proxy`.
    fn proxy_check_status(&self, _proxy: &tg_core::ProxyData) -> ProxyCheckStatus {
        ProxyCheckStatus::NotTested
    }

    /// Returns `true` when this backend drives live proxy connections (hiding offline simulator controls).
    fn is_live_proxy_backend(&self) -> bool {
        false
    }

    /// Returns websites where the user logged in with Telegram (`account.getWebAuthorizations`).
    fn web_authorizations(&self) -> Vec<WebAuthorization> {
        Vec::new()
    }
    /// Disconnects a single website authorization (`account.resetWebAuthorization`) and
    /// optionally blocks its bot (`contacts.block`).
    fn disconnect_website(&mut self, _hash: u64, _block_bot: bool) -> Result<bool, String> {
        Ok(false)
    }
    /// Disconnects all connected websites (`account.resetWebAuthorizations`); returns count removed.
    fn disconnect_all_websites(&mut self) -> Result<usize, String> {
        Ok(0)
    }

    /// Returns saved custom notification ringtones (`account.getSavedRingtones`).
    fn saved_ringtones(&self) -> Vec<SavedRingtone> {
        Vec::new()
    }
    /// Saves (`unsave == false`) or removes (`unsave == true`) a custom ringtone (`account.saveRingtone`).
    fn save_ringtone(&mut self, _id: i64, _unsave: bool) -> Result<(), String> {
        Ok(())
    }
    /// Uploads a new custom notification ringtone (`upload.saveFilePart` + `account.uploadRingtone`).
    fn upload_ringtone(
        &mut self,
        _file_name: &str,
        _mime_type: &str,
        _content: &[u8],
    ) -> Result<SavedRingtone, String> {
        Err("Uploading custom ringtones is not supported by this backend.".to_owned())
    }

    /// Returns the account's registered passkeys (`account.getPasskeys`).
    fn passkeys(&self) -> Vec<PasskeyEntry> {
        Vec::new()
    }

    /// Refreshes the passkeys list from the backend.
    fn refresh_passkeys(&mut self) {}

    /// Returns whether the platform supports passkeys on this device.
    fn is_passkey_supported(&self) -> bool {
        false
    }

    /// Returns whether the account can register another passkey (supported and under quota).
    fn can_register_passkey(&self) -> bool {
        false
    }

    /// Registers a new passkey (`account.initPasskeyRegistration` + WebAuthn + `account.registerPasskey`).
    fn register_passkey(&mut self) -> Result<PasskeyEntry, String> {
        Err("Passkeys are not supported.".to_owned())
    }

    /// Initiates a cross-device caBLE / hybrid QR passkey registration and returns the `"FIDO:/..."` URI.
    fn start_passkey_hybrid_qr(&mut self) -> Result<String, String> {
        Err("Hybrid QR passkey registration is not supported.".to_owned())
    }

    /// Returns the live caBLE BLE proximity scanning status for an active hybrid QR ceremony.
    fn passkey_hybrid_qr_status(&self) -> PasskeyHybridQrStatus {
        PasskeyHybridQrStatus::Idle
    }

    /// Cancels an active hybrid QR passkey ceremony and stops the BLE proximity scanner.
    fn cancel_passkey_hybrid_qr(&mut self) {}

    /// Deletes a registered passkey by ID (`account.deletePasskey`).
    fn delete_passkey(&mut self, _id: &str) -> Result<(), String> {
        Err("Passkeys are not supported.".to_owned())
    }

    /// Fetches Telegram FAQ table-of-contents entries via `messages.getWebPage`
    /// (`settings/settings_faq_suggestions.cpp`).
    fn fetch_faq_suggestions(&self, _faq_url: &str) -> Result<Vec<FaqEntry>, String> {
        Ok(Vec::new())
    }

    /// Fetches peers suggested to leave when removing a shared chat folder
    /// (`chatlists.getLeaveChatlistSuggestions`, `api_chat_filters_remove_manager.cpp`).
    fn leave_folder_suggestions(&self, _filter_id: i32) -> Result<Vec<i64>, String> {
        Ok(Vec::new())
    }

    /// Confirms an unreviewed connected Business bot authorization
    /// (`account.confirmBotConnection`, `api_authorizations.cpp`).
    fn confirm_bot_connection(&mut self, _bot_id: i64) -> Result<bool, String> {
        Err("Confirming bot connections is not supported by this backend.".to_owned())
    }

    /// Fetches the Telegram Support contact ("Ask a Question", `help.getSupport`,
    /// `settings_main.cpp`).
    fn get_support_contact(&self) -> Result<SupportContact, String> {
        Err("Telegram Support lookup is not supported by this backend.".to_owned())
    }

    /// Bulk-replaces the blocked peers or hidden-stories blocklist (`contacts.setBlocked`,
    /// `api/api_stories.cpp:595-621`).
    fn set_blocked_peers(
        &mut self,
        _peer_ids: &[i64],
        _my_stories_from: bool,
    ) -> Result<bool, String> {
        Err("Bulk-updating blocked peers is not supported by this backend.".to_owned())
    }
}

/// Telegram volunteer support account returned by `help.getSupport` (`settings_main.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportContact {
    /// Support phone number string returned by `help.support.phone_number`.
    pub phone_number: String,
    /// Bare user ID of the support account (`help.support.user`).
    pub user_id: i64,
    /// Display name of the support user.
    pub display_name: String,
}

/// Single parsed Telegram FAQ suggestion entry (`Settings::FaqEntry` in
/// `settings/settings_faq_suggestions.h`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaqEntry {
    /// FAQ section heading (e.g. `"General Questions"`).
    pub section: String,
    /// Question title (e.g. `"What is Telegram? What do I do here?"`).
    pub title: String,
    /// Full FAQ anchor URL (e.g. `"https://telegram.org/faq#q-what-is-telegram"`).
    pub url: String,
}

/// Live caBLEv2 BLE proximity scanner state during a cross-device hybrid QR passkey ceremony
/// (`cable_box.cpp` / `cable_ceremony.cpp`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PasskeyHybridQrStatus {
    /// No hybrid QR ceremony is currently active.
    #[default]
    Idle,
    /// BLE scanner is actively listening for FIDO2 (`0xFFF9`) / Google (`0xFDE2`) EID adverts.
    Scanning,
    /// No Bluetooth LE radio is available or Bluetooth is turned off (`lng_passkey_cable_no_bluetooth`).
    NoBluetooth,
    /// A matching encrypted caBLEv2 EID advertisement was authenticated; connecting via tunnel
    /// (`lng_passkey_cable_connecting`).
    Connecting {
        /// Decoded caBLEv2 tunnel server domain (e.g. `"cable.ua5v.com"`).
        tunnel_domain: String,
    },
    /// Cross-device caBLEv2 passkey ceremony completed successfully.
    Completed,
    /// BLE scanner or ceremony encountered an error (`lng_passkey_cable_error`).
    Error(String),
}

/// Builds a [`LanguagePrompt::ConfirmLinkSwitch`] or [`LanguagePrompt::NotReady`] from cloud
/// language metadata using exact `tg_lang::tr::*` strings (`ConfirmSwitchBox` / `NotReadyBox` in
/// `lang_cloud_manager.cpp:64-143`).
#[must_use]
pub fn build_switch_confirmation_prompt(info: &tg_lang::LangPackLanguageInfo) -> LanguagePrompt {
    if info.strings_count <= 0 {
        let link = tg_lang::tr::lng_language_not_ready_link();
        return LanguagePrompt::NotReady {
            info: info.clone(),
            title: tg_lang::tr::lng_language_not_ready_title(),
            body: tg_lang::tr::lng_language_not_ready_about(&info.native_name, &link),
        };
    }
    let percent = info.translated_percent().to_string();
    let link = tg_lang::tr::lng_language_switch_link();
    let body = if info.official {
        tg_lang::tr::lng_language_switch_about_official(&info.native_name, &percent, &link)
    } else {
        tg_lang::tr::lng_language_switch_about_unofficial(&info.native_name, &percent, &link)
    };
    LanguagePrompt::ConfirmLinkSwitch {
        info: info.clone(),
        title: tg_lang::tr::lng_language_switch_title(),
        body,
        apply_label: tg_lang::tr::lng_language_switch_apply(),
    }
}
