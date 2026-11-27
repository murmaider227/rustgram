//! In-memory mock implementation of [`SettingsBackend`] for offline UI development and tests.
//!
//! All sample users, sessions, folders, and stickers use strictly fictional placeholder names.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use tg_lang::LangPackLanguageInfo;
use tg_platform::cable_scanner::{
    CableBleScanner, FakeCableBleScanner, start_cable_proximity_match,
};

use super::backend::{
    ACCOUNT_TTL_OPTIONS, ActiveSession, AutoDownloadSettings, Birthday, ChatFolder,
    CloudPasswordError, CloudPasswordState, DEFAULT_RINGTONE_MAX_SIZE, ExportRequest, FolderIcon,
    FolderInviteLink, GlobalPrivacySettings, LocalStorageStats, MAX_BIO_LEN,
    MAX_FOLDER_LINK_TITLE_LEN, NotificationEventsSettings, NotifyException, NotifyScope,
    PasskeyEntry, PasskeyHybridQrStatus, PeerRef, PrivacyKey, PrivacyOption, PrivacyRule,
    ProfileInfo, ProxyCheckStatus, SESSION_TTL_OPTIONS, SavedRingtone, ScopeNotifySettings,
    SettingsBackend, SettingsRead, StickerSetInfo, SuggestedFolder, UsernameStatus,
    WebAuthorization, validate_username_syntax,
};

#[derive(Clone)]
struct CableScannerSlot(Arc<dyn CableBleScanner>);

impl std::fmt::Debug for CableScannerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CableScannerSlot")
            .field("running", &self.0.is_running())
            .finish()
    }
}

/// Mock verification code accepted by [`MockSettingsBackend::confirm_recovery_email`].
pub const MOCK_EMAIL_CONFIRM_CODE: &str = "123456";

/// Stateful in-memory [`SettingsBackend`] implementation with realistic fictional data.
#[derive(Debug, Clone)]
pub struct MockSettingsBackend {
    profile: ProfileInfo,
    contacts: Vec<PeerRef>,
    taken_usernames: Vec<String>,
    privacy: BTreeMap<PrivacyKey, PrivacyRule>,
    global_privacy: GlobalPrivacySettings,
    account_ttl_days: i32,
    sessions: Vec<ActiveSession>,
    session_ttl_days: i32,
    blocked_ids: Vec<i64>,
    cloud_password: CloudPasswordState,
    cloud_password_secret: Option<String>,
    pending_email_raw: Option<String>,
    notifications: BTreeMap<NotifyScope, ScopeNotifySettings>,
    notification_events: NotificationEventsSettings,
    auto_download: AutoDownloadSettings,
    sticker_sets: Vec<StickerSetInfo>,
    folders: Vec<ChatFolder>,
    suggested_folders: Vec<SuggestedFolder>,
    next_folder_id: i32,
    folder_tags_enabled: bool,
    folder_links: BTreeMap<i32, Vec<FolderInviteLink>>,
    next_folder_link_seq: u32,
    languages: Vec<LangPackLanguageInfo>,
    active_language_code: String,
    recent_languages: Vec<tg_lang::Language>,
    pending_language_prompt: Option<super::backend::LanguagePrompt>,
    storage: LocalStorageStats,
    export_count: u32,
    proxy_settings: tg_core::SettingsProxy,
    proxy_checks: Vec<(tg_core::ProxyData, ProxyCheckStatus)>,
    web_authorizations: Vec<WebAuthorization>,
    saved_ringtones: Vec<SavedRingtone>,
    next_ringtone_id: i64,
    passkeys: Vec<PasskeyEntry>,
    next_passkey_seq: u32,
    cable_scanner: CableScannerSlot,
    hybrid_qr_status: Arc<Mutex<PasskeyHybridQrStatus>>,
    active_cable_eid_key: Option<[u8; tg_platform::webauthn::CABLE_EID_KEY_LEN]>,
    /// Reads reported as still loading (TGB-3105), like a live backend before its first answer.
    pending_reads: HashSet<SettingsRead>,
}

impl Default for MockSettingsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockSettingsBackend {
    /// Creates a pre-populated [`MockSettingsBackend`] using strictly fictional sample records.
    #[must_use]
    pub fn new() -> Self {
        let contacts = vec![
            PeerRef {
                id: 1001,
                name: "Alex Rivera".to_owned(),
                username: "alex_rivera".to_owned(),
                subtitle: "online".to_owned(),
                is_group_or_channel: false,
            },
            PeerRef {
                id: 1002,
                name: "Morgan Chen".to_owned(),
                username: "morgan_c".to_owned(),
                subtitle: "last seen 12 minutes ago".to_owned(),
                is_group_or_channel: false,
            },
            PeerRef {
                id: 1003,
                name: "Casey Jordan".to_owned(),
                username: "casey_j".to_owned(),
                subtitle: "last seen 2 hours ago".to_owned(),
                is_group_or_channel: false,
            },
            PeerRef {
                id: 1004,
                name: "Riley Brooks".to_owned(),
                username: "riley_b".to_owned(),
                subtitle: "last seen recently".to_owned(),
                is_group_or_channel: false,
            },
            PeerRef {
                id: 1005,
                name: "Samira Patel".to_owned(),
                username: "samira_p".to_owned(),
                subtitle: "last seen yesterday".to_owned(),
                is_group_or_channel: false,
            },
            PeerRef {
                id: 2001,
                name: "Project Atlas".to_owned(),
                username: "project_atlas".to_owned(),
                subtitle: "18 members".to_owned(),
                is_group_or_channel: true,
            },
            PeerRef {
                id: 2002,
                name: "Design Guild".to_owned(),
                username: "design_guild".to_owned(),
                subtitle: "42 members".to_owned(),
                is_group_or_channel: true,
            },
            PeerRef {
                id: 3001,
                name: "Release Bulletin".to_owned(),
                username: "release_bulletin".to_owned(),
                subtitle: "1,240 subscribers".to_owned(),
                is_group_or_channel: true,
            },
        ];

        let mut privacy = BTreeMap::new();
        for key in PrivacyKey::ALL {
            let rule = match key {
                PrivacyKey::PhoneNumber => PrivacyRule {
                    option: PrivacyOption::Contacts,
                    always_allow: vec![],
                    never_allow: vec![1004],
                    ..PrivacyRule::default()
                },
                PrivacyKey::AddedByPhone => PrivacyRule {
                    option: PrivacyOption::Everybody,
                    ..PrivacyRule::default()
                },
                PrivacyKey::LastSeen => PrivacyRule {
                    option: PrivacyOption::Everybody,
                    never_allow: vec![1004, 1005],
                    ..PrivacyRule::default()
                },
                PrivacyKey::Calls => PrivacyRule {
                    option: PrivacyOption::Contacts,
                    always_allow: vec![1001],
                    ..PrivacyRule::default()
                },
                PrivacyKey::CallsPeer2Peer => PrivacyRule {
                    option: PrivacyOption::Contacts,
                    ..PrivacyRule::default()
                },
                PrivacyKey::GroupsInvite => PrivacyRule {
                    option: PrivacyOption::Contacts,
                    ..PrivacyRule::default()
                },
                PrivacyKey::Birthday => PrivacyRule {
                    option: PrivacyOption::Contacts,
                    ..PrivacyRule::default()
                },
                _ => PrivacyRule {
                    option: PrivacyOption::Everybody,
                    ..PrivacyRule::default()
                },
            };
            privacy.insert(key, rule);
        }

        let sessions = vec![
            ActiveSession {
                hash: 0,
                api_id: 2040,
                device_model: "Desktop Workstation".to_owned(),
                platform: "Windows".to_owned(),
                system_version: "Windows 11 x64".to_owned(),
                app_name_version: "Rustgram Desktop 0.1.0".to_owned(),
                location: "Zurich, Switzerland".to_owned(),
                ip: "198.51.100.14".to_owned(),
                active_time: "online".to_owned(),
                accept_calls: true,
                accept_secret_chats: true,
            },
            ActiveSession {
                hash: 0xA1B2_C3D4_0001,
                api_id: 21724,
                device_model: "Pixel 8 Pro".to_owned(),
                platform: "Android".to_owned(),
                system_version: "Android 15".to_owned(),
                app_name_version: "Telegram Android 11.4".to_owned(),
                location: "Zurich, Switzerland".to_owned(),
                ip: "198.51.100.22".to_owned(),
                active_time: "18 minutes ago".to_owned(),
                accept_calls: true,
                accept_secret_chats: true,
            },
            ActiveSession {
                hash: 0xA1B2_C3D4_0002,
                api_id: 2834,
                device_model: "MacBook Pro 14".to_owned(),
                platform: "macOS".to_owned(),
                system_version: "macOS 15.1".to_owned(),
                app_name_version: "Telegram macOS 11.3".to_owned(),
                location: "Geneva, Switzerland".to_owned(),
                ip: "203.0.113.45".to_owned(),
                active_time: "Yesterday at 19:40".to_owned(),
                accept_calls: true,
                accept_secret_chats: false,
            },
            ActiveSession {
                hash: 0xA1B2_C3D4_0003,
                api_id: 2496,
                device_model: "Firefox 132".to_owned(),
                platform: "Web".to_owned(),
                system_version: "Linux x64".to_owned(),
                app_name_version: "Telegram Web K 2.1".to_owned(),
                location: "Basel, Switzerland".to_owned(),
                ip: "192.0.2.88".to_owned(),
                active_time: "3 days ago".to_owned(),
                accept_calls: false,
                accept_secret_chats: false,
            },
        ];

        let mut notifications = BTreeMap::new();
        notifications.insert(
            NotifyScope::PrivateChats,
            ScopeNotifySettings {
                enabled: true,
                show_preview: true,
                sound: "Default".to_owned(),
                exceptions: vec![NotifyException {
                    peer: contacts[3].clone(),
                    muted: true,
                    sound: "None".to_owned(),
                }],
            },
        );
        notifications.insert(
            NotifyScope::Groups,
            ScopeNotifySettings {
                enabled: true,
                show_preview: true,
                sound: "Default".to_owned(),
                exceptions: vec![NotifyException {
                    peer: contacts[6].clone(),
                    muted: true,
                    sound: "None".to_owned(),
                }],
            },
        );
        notifications.insert(
            NotifyScope::Channels,
            ScopeNotifySettings {
                enabled: false,
                show_preview: true,
                sound: "None".to_owned(),
                exceptions: vec![NotifyException {
                    peer: contacts[7].clone(),
                    muted: false,
                    sound: "Default".to_owned(),
                }],
            },
        );

        let sticker_sets = vec![
            StickerSetInfo {
                id: 5001,
                title: "Geometric Shapes".to_owned(),
                short_name: "geometric_shapes".to_owned(),
                count: 24,
                is_emoji: false,
                archived: false,
                animated: true,
            },
            StickerSetInfo {
                id: 5002,
                title: "Workspace Badges".to_owned(),
                short_name: "workspace_badges".to_owned(),
                count: 18,
                is_emoji: false,
                archived: false,
                animated: false,
            },
            StickerSetInfo {
                id: 5003,
                title: "Minimal Status Icons".to_owned(),
                short_name: "minimal_status".to_owned(),
                count: 32,
                is_emoji: true,
                archived: false,
                animated: true,
            },
            StickerSetInfo {
                id: 5004,
                title: "Retro Pixels".to_owned(),
                short_name: "retro_pixels".to_owned(),
                count: 16,
                is_emoji: false,
                archived: true,
                animated: false,
            },
        ];

        let folders = vec![
            ChatFolder {
                id: 2,
                title: "Personal".to_owned(),
                icon: FolderIcon::Private,
                color_index: Some(3),
                include_contacts: true,
                include_non_contacts: true,
                included_peers: vec![1001, 1002],
                ..ChatFolder::default()
            },
            ChatFolder {
                id: 3,
                title: "Work".to_owned(),
                icon: FolderIcon::Work,
                color_index: Some(0),
                include_groups: true,
                exclude_muted: true,
                included_peers: vec![2001, 2002],
                shareable: true,
                ..ChatFolder::default()
            },
            ChatFolder {
                id: 4,
                title: "Channels".to_owned(),
                icon: FolderIcon::Channels,
                color_index: Some(4),
                include_channels: true,
                included_peers: vec![3001],
                ..ChatFolder::default()
            },
        ];

        let suggested_folders = vec![
            SuggestedFolder {
                folder: ChatFolder {
                    id: 0,
                    title: "Unread".to_owned(),
                    icon: FolderIcon::Unread,
                    include_contacts: true,
                    include_non_contacts: true,
                    include_groups: true,
                    include_channels: true,
                    include_bots: true,
                    exclude_read: true,
                    exclude_archived: true,
                    ..ChatFolder::default()
                },
                description: "All chats with unread messages".to_owned(),
            },
            SuggestedFolder {
                folder: ChatFolder {
                    id: 0,
                    title: "Bots".to_owned(),
                    icon: FolderIcon::Bots,
                    include_bots: true,
                    ..ChatFolder::default()
                },
                description: "All bot conversations in one place".to_owned(),
            },
        ];

        let languages = vec![
            LangPackLanguageInfo {
                official: true,
                rtl: false,
                beta: false,
                name: "English".to_owned(),
                native_name: "English".to_owned(),
                lang_code: "en".to_owned(),
                base_lang_code: None,
                plural_code: "en".to_owned(),
                strings_count: 6850,
                translated_count: 6850,
                translations_url: String::new(),
            },
            LangPackLanguageInfo {
                official: true,
                rtl: false,
                beta: false,
                name: "German".to_owned(),
                native_name: "Deutsch".to_owned(),
                lang_code: "de".to_owned(),
                base_lang_code: None,
                plural_code: "de".to_owned(),
                strings_count: 6850,
                translated_count: 6780,
                translations_url: String::new(),
            },
            LangPackLanguageInfo {
                official: true,
                rtl: false,
                beta: false,
                name: "French".to_owned(),
                native_name: "Français".to_owned(),
                lang_code: "fr".to_owned(),
                base_lang_code: None,
                plural_code: "fr".to_owned(),
                strings_count: 6850,
                translated_count: 6720,
                translations_url: String::new(),
            },
            LangPackLanguageInfo {
                official: true,
                rtl: false,
                beta: false,
                name: "Spanish".to_owned(),
                native_name: "Español".to_owned(),
                lang_code: "es".to_owned(),
                base_lang_code: None,
                plural_code: "es".to_owned(),
                strings_count: 6850,
                translated_count: 6810,
                translations_url: String::new(),
            },
            LangPackLanguageInfo {
                official: true,
                rtl: false,
                beta: false,
                name: "Italian".to_owned(),
                native_name: "Italiano".to_owned(),
                lang_code: "it".to_owned(),
                base_lang_code: None,
                plural_code: "it".to_owned(),
                strings_count: 6850,
                translated_count: 6690,
                translations_url: String::new(),
            },
            LangPackLanguageInfo {
                official: true,
                rtl: false,
                beta: false,
                name: "Portuguese (Brazil)".to_owned(),
                native_name: "Português (Brasil)".to_owned(),
                lang_code: "pt-br".to_owned(),
                base_lang_code: Some("pt".to_owned()),
                plural_code: "pt".to_owned(),
                strings_count: 6850,
                translated_count: 6740,
                translations_url: String::new(),
            },
            LangPackLanguageInfo {
                official: true,
                rtl: false,
                beta: false,
                name: "Ukrainian".to_owned(),
                native_name: "Українська".to_owned(),
                lang_code: "uk".to_owned(),
                base_lang_code: None,
                plural_code: "uk".to_owned(),
                strings_count: 6850,
                translated_count: 6830,
                translations_url: String::new(),
            },
            LangPackLanguageInfo {
                official: true,
                rtl: false,
                beta: false,
                name: "Polish".to_owned(),
                native_name: "Polski".to_owned(),
                lang_code: "pl".to_owned(),
                base_lang_code: None,
                plural_code: "pl".to_owned(),
                strings_count: 6850,
                translated_count: 6610,
                translations_url: String::new(),
            },
            LangPackLanguageInfo {
                official: true,
                rtl: false,
                beta: false,
                name: "Japanese".to_owned(),
                native_name: "日本語".to_owned(),
                lang_code: "ja".to_owned(),
                base_lang_code: None,
                plural_code: "ja".to_owned(),
                strings_count: 6850,
                translated_count: 6500,
                translations_url: String::new(),
            },
            LangPackLanguageInfo {
                official: true,
                rtl: false,
                beta: false,
                name: "Korean".to_owned(),
                native_name: "한국어".to_owned(),
                lang_code: "ko".to_owned(),
                base_lang_code: None,
                plural_code: "ko".to_owned(),
                strings_count: 6850,
                translated_count: 6590,
                translations_url: String::new(),
            },
            LangPackLanguageInfo {
                official: true,
                rtl: true,
                beta: false,
                name: "Arabic".to_owned(),
                native_name: "العربية".to_owned(),
                lang_code: "ar".to_owned(),
                base_lang_code: None,
                plural_code: "ar".to_owned(),
                strings_count: 6850,
                translated_count: 6680,
                translations_url: String::new(),
            },
            LangPackLanguageInfo {
                official: true,
                rtl: false,
                beta: true,
                name: "Dutch".to_owned(),
                native_name: "Nederlands".to_owned(),
                lang_code: "nl".to_owned(),
                base_lang_code: None,
                plural_code: "nl".to_owned(),
                strings_count: 6850,
                translated_count: 6410,
                translations_url: String::new(),
            },
        ];

        Self {
            profile: ProfileInfo {
                user_id: 42_001,
                first_name: "Taylor".to_owned(),
                last_name: "Kim".to_owned(),
                username: "taylor_kim".to_owned(),
                phone: "+1 555 010 0199".to_owned(),
                bio: "Building Rustgram in Rust. Offline mock profile.".to_owned(),
                birthday: Some(Birthday {
                    day: 14,
                    month: 5,
                    year: Some(1995),
                }),
                photo_color_index: 5,
                photo_label: None,
                phone_hidden: false,
            },
            contacts,
            taken_usernames: vec![
                "admin".to_owned(),
                "support".to_owned(),
                "alex_rivera".to_owned(),
                "morgan_c".to_owned(),
            ],
            privacy,
            global_privacy: GlobalPrivacySettings::default(),
            account_ttl_days: 180,
            sessions,
            session_ttl_days: 180,
            blocked_ids: vec![1005],
            cloud_password: CloudPasswordState::default(),
            cloud_password_secret: None,
            pending_email_raw: None,
            notifications,
            notification_events: NotificationEventsSettings::default(),
            auto_download: AutoDownloadSettings::default(),
            sticker_sets,
            folders,
            suggested_folders,
            next_folder_id: 5,
            folder_tags_enabled: false,
            folder_links: BTreeMap::new(),
            next_folder_link_seq: 1,
            languages,
            active_language_code: "en".to_owned(),
            recent_languages: Vec::new(),
            pending_language_prompt: None,
            storage: LocalStorageStats {
                photos_bytes: 128 * 1024 * 1024,
                videos_bytes: 312 * 1024 * 1024,
                files_bytes: 64 * 1024 * 1024,
                stickers_bytes: 28 * 1024 * 1024,
                size_limit_bytes: 2 * 1024 * 1024 * 1024,
                time_limit_days: 30,
            },
            export_count: 0,
            proxy_settings: tg_core::SettingsProxy::new(),
            proxy_checks: Vec::new(),
            pending_reads: HashSet::new(),
            web_authorizations: vec![
                WebAuthorization {
                    hash: 0x5EED_0001,
                    bot_id: 9001,
                    bot_name: "Atlas Docs Bot".to_owned(),
                    domain: "docs.atlas.example".to_owned(),
                    browser: "Firefox 132".to_owned(),
                    platform: "Windows".to_owned(),
                    ip: "198.51.100.40".to_owned(),
                    region: "Zurich, Switzerland".to_owned(),
                    date_created: 1_740_000_000,
                    date_active: 1_750_010_000,
                    active_time: "Today at 14:20".to_owned(),
                },
                WebAuthorization {
                    hash: 0x5EED_0002,
                    bot_id: 9002,
                    bot_name: "Relay Status Bot".to_owned(),
                    domain: "status.relay.example".to_owned(),
                    browser: "Edge 131".to_owned(),
                    platform: "Windows".to_owned(),
                    ip: "203.0.113.71".to_owned(),
                    region: "Geneva, Switzerland".to_owned(),
                    date_created: 1_738_000_000,
                    date_active: 1_749_500_000,
                    active_time: "3 days ago".to_owned(),
                },
            ],
            saved_ringtones: vec![
                SavedRingtone {
                    id: 700_001,
                    access_hash: 1_000_001,
                    file_reference: vec![1, 2, 3],
                    title: "Soft Chime".to_owned(),
                    file_name: "soft_chime.mp3".to_owned(),
                    mime_type: "audio/mpeg".to_owned(),
                    size: 18_432,
                    duration_secs: 2.4,
                    date: 1_750_000_000,
                },
                SavedRingtone {
                    id: 700_002,
                    access_hash: 1_000_002,
                    file_reference: vec![4, 5, 6],
                    title: "Glass Bell".to_owned(),
                    file_name: "glass_bell.ogg".to_owned(),
                    mime_type: "audio/ogg".to_owned(),
                    size: 12_288,
                    duration_secs: 1.8,
                    date: 1_749_800_000,
                },
            ],
            next_ringtone_id: 700_003,
            passkeys: vec![
                PasskeyEntry {
                    id: "demo-passkey-1".to_owned(),
                    name: "Windows Hello (Demo Workstation)".to_owned(),
                    date: 1_736_000_000,
                    software_emoji_id: None,
                    last_usage_date: Some(1_739_500_000),
                },
                PasskeyEntry {
                    id: "demo-passkey-2".to_owned(),
                    name: "USB Security Key (Backup)".to_owned(),
                    date: 1_737_200_000,
                    software_emoji_id: None,
                    last_usage_date: None,
                },
            ],
            next_passkey_seq: 3,
            cable_scanner: CableScannerSlot(Arc::new(FakeCableBleScanner::new(true))),
            hybrid_qr_status: Arc::new(Mutex::new(PasskeyHybridQrStatus::Idle)),
            active_cable_eid_key: None,
        }
    }

    /// Replaces the [`CableBleScanner`] used by [`MockSettingsBackend::start_passkey_hybrid_qr`].
    pub fn set_cable_scanner(&mut self, scanner: Arc<dyn CableBleScanner>) {
        self.cable_scanner.0.stop();
        self.cable_scanner = CableScannerSlot(scanner);
    }

    /// Overrides the simulated [`PasskeyHybridQrStatus`] (for UI demos and screenshot tests).
    pub fn set_passkey_hybrid_qr_status(&mut self, status: PasskeyHybridQrStatus) {
        if let Ok(mut guard) = self.hybrid_qr_status.lock() {
            *guard = status;
        }
    }

    /// Returns the active caBLEv2 `eid_key` generated by [`Self::start_passkey_hybrid_qr`].
    #[must_use]
    pub const fn active_cable_eid_key(
        &self,
    ) -> Option<[u8; tg_platform::webauthn::CABLE_EID_KEY_LEN]> {
        self.active_cable_eid_key
    }

    /// Masks an email address as `a***@domain.tld` for display.
    fn mask_email(email: &str) -> String {
        let Some((local, domain)) = email.split_once('@') else {
            return email.to_owned();
        };
        let first = local.chars().next().unwrap_or('*');
        format!("{first}***@{domain}")
    }

    fn is_valid_email(email: &str) -> bool {
        let trimmed = email.trim();
        let Some((local, domain)) = trimmed.split_once('@') else {
            return false;
        };
        !local.is_empty()
            && domain.contains('.')
            && !domain.starts_with('.')
            && !domain.ends_with('.')
            && !trimmed.contains(char::is_whitespace)
    }
}

impl MockSettingsBackend {
    /// Marks `read` as still loading (`true`) or answered (`false`), so screens show tdesktop's
    /// loading states as with a live backend waiting for the server (TGB-3070, TGB-3105).
    pub fn set_read_pending(&mut self, read: SettingsRead, pending: bool) {
        if pending {
            self.pending_reads.insert(read);
        } else {
            self.pending_reads.remove(&read);
        }
    }
}

impl SettingsBackend for MockSettingsBackend {
    fn settings_read_pending(&self, read: SettingsRead) -> bool {
        self.pending_reads.contains(&read)
    }

    fn profile(&self) -> ProfileInfo {
        self.profile.clone()
    }

    fn update_name(&mut self, first_name: &str, last_name: &str) -> Result<(), String> {
        let first = first_name.trim();
        let last = last_name.trim();
        if first.is_empty() {
            return Err("First name cannot be empty.".to_owned());
        }
        if first.chars().count() > 64 || last.chars().count() > 64 {
            return Err("Name must be 64 characters or fewer.".to_owned());
        }
        self.profile.first_name = first.to_owned();
        self.profile.last_name = last.to_owned();
        Ok(())
    }

    fn update_bio(&mut self, bio: &str) -> Result<(), String> {
        let trimmed = bio.trim();
        if trimmed.chars().count() > MAX_BIO_LEN {
            return Err(format!("Bio must be at most {MAX_BIO_LEN} characters."));
        }
        self.profile.bio = trimmed.to_owned();
        Ok(())
    }

    fn check_username(&self, username: &str) -> UsernameStatus {
        let trimmed = username.trim().strip_prefix('@').unwrap_or(username.trim());
        let syntax = validate_username_syntax(trimmed);
        if syntax != UsernameStatus::Available {
            return syntax;
        }
        if trimmed.eq_ignore_ascii_case(&self.profile.username) {
            return UsernameStatus::Current;
        }
        if self
            .taken_usernames
            .iter()
            .any(|t| t.eq_ignore_ascii_case(trimmed))
        {
            return UsernameStatus::Taken;
        }
        UsernameStatus::Available
    }

    fn update_username(&mut self, username: &str) -> Result<(), String> {
        let trimmed = username.trim().strip_prefix('@').unwrap_or(username.trim());
        match self.check_username(trimmed) {
            UsernameStatus::Empty => {
                self.profile.username.clear();
                Ok(())
            }
            UsernameStatus::Available | UsernameStatus::Current => {
                self.profile.username = trimmed.to_owned();
                Ok(())
            }
            UsernameStatus::Taken => Err("Sorry, this username is already taken.".to_owned()),
            UsernameStatus::TooShort => {
                Err("A username must have at least 5 characters.".to_owned())
            }
            UsernameStatus::TooLong => {
                Err("A username must be at most 32 characters long.".to_owned())
            }
            UsernameStatus::InvalidChars => Err(
                "Use a-z, 0-9 and underscores. Must start with a letter and not end with '_'."
                    .to_owned(),
            ),
        }
    }

    fn update_birthday(&mut self, birthday: Option<Birthday>) -> Result<(), String> {
        if let Some(b) = birthday
            && !b.is_valid()
        {
            return Err("Invalid birthday date.".to_owned());
        }
        self.profile.birthday = birthday;
        Ok(())
    }

    fn update_profile_photo(&mut self, color_index: u8, photo_label: Option<String>) {
        self.profile.photo_color_index = color_index % 7;
        self.profile.photo_label = photo_label;
    }

    fn set_phone_hidden(&mut self, hidden: bool) {
        self.profile.phone_hidden = hidden;
    }

    fn contacts(&self) -> Vec<PeerRef> {
        self.contacts.clone()
    }

    fn privacy_rules(&self) -> BTreeMap<PrivacyKey, PrivacyRule> {
        self.privacy.clone()
    }

    fn privacy_rule(&self, key: PrivacyKey) -> PrivacyRule {
        self.privacy.get(&key).cloned().unwrap_or_default()
    }

    fn set_privacy_rule(&mut self, key: PrivacyKey, mut rule: PrivacyRule) {
        if !key.allows_nobody() && rule.option == PrivacyOption::Nobody {
            rule.option = PrivacyOption::Contacts;
        }
        rule.normalize();
        self.privacy.insert(key, rule);
    }

    fn global_privacy(&self) -> GlobalPrivacySettings {
        self.global_privacy.clone()
    }

    fn set_global_privacy(&mut self, settings: GlobalPrivacySettings) {
        self.global_privacy = settings;
    }

    fn account_ttl_days(&self) -> i32 {
        self.account_ttl_days
    }

    fn set_account_ttl_days(&mut self, days: i32) {
        if ACCOUNT_TTL_OPTIONS.contains(&days) {
            self.account_ttl_days = days;
        }
    }

    fn clear_payment_info(&mut self, _clear_shipping: bool, _clear_credentials: bool) {}

    fn active_sessions(&self) -> Vec<ActiveSession> {
        self.sessions.clone()
    }

    fn terminate_session(&mut self, hash: u64) -> bool {
        if hash == 0 {
            return false;
        }
        let before = self.sessions.len();
        self.sessions.retain(|s| s.hash != hash);
        self.sessions.len() < before
    }

    fn terminate_all_other_sessions(&mut self) -> usize {
        let before = self.sessions.len();
        self.sessions.retain(ActiveSession::is_current);
        before.saturating_sub(self.sessions.len())
    }

    fn update_session_permissions(
        &mut self,
        hash: u64,
        accept_calls: bool,
        accept_secret_chats: bool,
    ) {
        if let Some(session) = self.sessions.iter_mut().find(|s| s.hash == hash) {
            session.accept_calls = accept_calls;
            session.accept_secret_chats = accept_secret_chats;
        }
    }

    fn session_ttl_days(&self) -> i32 {
        self.session_ttl_days
    }

    fn set_session_ttl_days(&mut self, days: i32) {
        if SESSION_TTL_OPTIONS.contains(&days) {
            self.session_ttl_days = days;
        }
    }

    fn blocked_users(&self) -> Vec<PeerRef> {
        self.blocked_ids
            .iter()
            .filter_map(|id| self.contacts.iter().find(|c| c.id == *id).cloned())
            .collect()
    }

    fn block_peer(&mut self, peer_id: i64) {
        if !self.blocked_ids.contains(&peer_id) {
            self.blocked_ids.push(peer_id);
        }
    }

    fn unblock_peer(&mut self, peer_id: i64) {
        self.blocked_ids.retain(|&id| id != peer_id);
    }

    fn cloud_password_state(&self) -> CloudPasswordState {
        self.cloud_password.clone()
    }

    fn verify_cloud_password(&self, password: &str) -> Result<(), CloudPasswordError> {
        match &self.cloud_password_secret {
            Some(secret) if secret == password => Ok(()),
            _ => Err(CloudPasswordError::WrongPassword),
        }
    }

    fn set_cloud_password(
        &mut self,
        current_password: Option<&str>,
        new_password: &str,
        hint: &str,
        recovery_email: Option<&str>,
    ) -> Result<CloudPasswordState, CloudPasswordError> {
        if self.cloud_password.has_password {
            let cur = current_password.unwrap_or("");
            self.verify_cloud_password(cur)?;
        }
        if new_password.is_empty() {
            return Err(CloudPasswordError::EmptyPassword);
        }
        let hint_trimmed = hint.trim();
        if !hint_trimmed.is_empty() && hint_trimmed == new_password {
            return Err(CloudPasswordError::HintMatchesPassword);
        }
        if let Some(email) = recovery_email {
            let trimmed = email.trim();
            if !trimmed.is_empty() && !Self::is_valid_email(trimmed) {
                return Err(CloudPasswordError::InvalidEmail);
            }
            if !trimmed.is_empty() {
                self.pending_email_raw = Some(trimmed.to_owned());
                self.cloud_password.unconfirmed_email_pattern = Self::mask_email(trimmed);
            }
        }
        self.cloud_password_secret = Some(new_password.to_owned());
        self.cloud_password.has_password = true;
        self.cloud_password.hint = hint_trimmed.to_owned();
        Ok(self.cloud_password.clone())
    }

    fn confirm_recovery_email(
        &mut self,
        code: &str,
    ) -> Result<CloudPasswordState, CloudPasswordError> {
        if code.trim() != MOCK_EMAIL_CONFIRM_CODE {
            return Err(CloudPasswordError::InvalidCode);
        }
        if let Some(raw) = self.pending_email_raw.take() {
            self.cloud_password.email_pattern = Self::mask_email(&raw);
            self.cloud_password.has_recovery = true;
        }
        self.cloud_password.unconfirmed_email_pattern.clear();
        Ok(self.cloud_password.clone())
    }

    fn cancel_recovery_email(&mut self) -> CloudPasswordState {
        self.pending_email_raw = None;
        self.cloud_password.unconfirmed_email_pattern.clear();
        self.cloud_password.clone()
    }

    fn disable_cloud_password(
        &mut self,
        current_password: &str,
    ) -> Result<CloudPasswordState, CloudPasswordError> {
        self.verify_cloud_password(current_password)?;
        self.cloud_password_secret = None;
        self.pending_email_raw = None;
        self.cloud_password = CloudPasswordState::default();
        Ok(self.cloud_password.clone())
    }

    fn scope_notifications(&self, scope: NotifyScope) -> ScopeNotifySettings {
        self.notifications.get(&scope).cloned().unwrap_or_default()
    }

    fn set_scope_notifications(&mut self, scope: NotifyScope, settings: ScopeNotifySettings) {
        self.notifications.insert(scope, settings);
    }

    fn notification_events(&self) -> NotificationEventsSettings {
        self.notification_events.clone()
    }

    fn set_notification_events(&mut self, events: NotificationEventsSettings) {
        self.notification_events = events;
    }

    fn reset_all_notifications(&mut self) {
        for scope in NotifyScope::ALL {
            self.notifications
                .insert(scope, ScopeNotifySettings::default());
        }
        self.notification_events = NotificationEventsSettings::default();
    }

    fn auto_download(&self) -> AutoDownloadSettings {
        self.auto_download.clone()
    }

    fn set_auto_download(&mut self, settings: AutoDownloadSettings) {
        self.auto_download = settings;
    }

    fn sticker_sets(&self) -> Vec<StickerSetInfo> {
        self.sticker_sets.clone()
    }

    fn set_sticker_set_archived(&mut self, id: u64, archived: bool) {
        if let Some(set) = self.sticker_sets.iter_mut().find(|s| s.id == id) {
            set.archived = archived;
        }
    }

    fn remove_sticker_set(&mut self, id: u64) {
        self.sticker_sets.retain(|s| s.id != id);
    }

    fn folders(&self) -> Vec<ChatFolder> {
        self.folders.clone()
    }

    fn suggested_folders(&self) -> Vec<SuggestedFolder> {
        self.suggested_folders.clone()
    }

    fn save_folder(&mut self, mut folder: ChatFolder) -> Result<ChatFolder, String> {
        folder.title = folder.title.trim().to_owned();
        if !folder.is_valid() {
            return Err(
                "Folder needs a title (1-12 chars) and at least one included chat or type."
                    .to_owned(),
            );
        }
        if folder.id == 0 {
            folder.id = self.next_folder_id;
            self.next_folder_id += 1;
            self.folders.push(folder.clone());
        } else if let Some(slot) = self.folders.iter_mut().find(|f| f.id == folder.id) {
            *slot = folder.clone();
        } else {
            self.folders.push(folder.clone());
        }
        Ok(folder)
    }

    fn delete_folder(&mut self, id: i32) -> bool {
        let before = self.folders.len();
        self.folders.retain(|f| f.id != id);
        self.folders.len() < before
    }

    fn reorder_folders(&mut self, ids: &[i32]) {
        let mut reordered = Vec::with_capacity(self.folders.len());
        for &id in ids {
            if let Some(found) = self.folders.iter().find(|f| f.id == id).cloned() {
                reordered.push(found);
            }
        }
        for remaining in &self.folders {
            if !reordered.iter().any(|f| f.id == remaining.id) {
                reordered.push(remaining.clone());
            }
        }
        self.folders = reordered;
    }

    fn accept_suggested_folder(&mut self, index: usize) -> Option<ChatFolder> {
        if index >= self.suggested_folders.len() {
            return None;
        }
        let suggested = self.suggested_folders.remove(index);
        self.save_folder(suggested.folder).ok()
    }

    fn export_folder_invite_link(
        &mut self,
        folder_id: i32,
        title: &str,
        _peers: &[i64],
    ) -> Result<String, String> {
        let slug: String = title
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(12)
            .collect();
        let slug = if slug.is_empty() {
            format!("folder{folder_id}")
        } else {
            format!("{slug}_{folder_id}")
        };
        Ok(format!("https://t.me/addlist/{slug}"))
    }

    fn folder_tags_enabled(&self) -> bool {
        self.folder_tags_enabled
    }

    fn set_folder_tags_enabled(&mut self, enabled: bool) -> Result<bool, String> {
        self.folder_tags_enabled = enabled;
        Ok(true)
    }

    fn folder_invite_links(&self, filter_id: i32) -> Vec<FolderInviteLink> {
        self.folder_links
            .get(&filter_id)
            .cloned()
            .unwrap_or_default()
    }

    fn create_folder_invite_link(
        &mut self,
        filter_id: i32,
        title: &str,
        peers: &[i64],
    ) -> Result<FolderInviteLink, String> {
        let seq = self.next_folder_link_seq;
        self.next_folder_link_seq = self.next_folder_link_seq.saturating_add(1);
        let slug = format!("f{filter_id}link{seq}");
        let link = FolderInviteLink {
            filter_id,
            title: title
                .trim()
                .chars()
                .take(MAX_FOLDER_LINK_TITLE_LEN)
                .collect(),
            url: format!("https://t.me/addlist/{slug}"),
            slug,
            peers: peers.to_vec(),
        };
        if let Some(folder) = self.folders.iter_mut().find(|f| f.id == filter_id) {
            folder.shareable = true;
        }
        self.folder_links
            .entry(filter_id)
            .or_default()
            .push(link.clone());
        Ok(link)
    }

    fn edit_folder_invite_link(
        &mut self,
        filter_id: i32,
        slug: &str,
        title: Option<&str>,
        peers: Option<&[i64]>,
    ) -> Result<FolderInviteLink, String> {
        let list = self
            .folder_links
            .get_mut(&filter_id)
            .ok_or_else(|| "Invite link not found.".to_owned())?;
        let link = list
            .iter_mut()
            .find(|l| l.slug == slug)
            .ok_or_else(|| "Invite link not found.".to_owned())?;
        if let Some(t) = title {
            link.title = t.trim().chars().take(MAX_FOLDER_LINK_TITLE_LEN).collect();
        }
        if let Some(ps) = peers {
            link.peers = ps.to_vec();
        }
        Ok(link.clone())
    }

    fn delete_folder_invite_link(&mut self, filter_id: i32, slug: &str) -> Result<bool, String> {
        let Some(list) = self.folder_links.get_mut(&filter_id) else {
            return Ok(false);
        };
        let before = list.len();
        list.retain(|l| l.slug != slug);
        Ok(list.len() < before)
    }

    fn check_folder_updates(&mut self, _filter_id: i32) -> Result<Vec<i64>, String> {
        Ok(Vec::new())
    }

    fn join_folder_updates(&mut self, filter_id: i32, peers: &[i64]) -> Result<(), String> {
        if let Some(folder) = self.folders.iter_mut().find(|f| f.id == filter_id) {
            for &pid in peers {
                if !folder.included_peers.contains(&pid) {
                    folder.included_peers.push(pid);
                }
            }
        }
        Ok(())
    }

    fn languages(&self) -> Vec<LangPackLanguageInfo> {
        self.languages.clone()
    }

    fn active_language_code(&self) -> String {
        self.active_language_code.clone()
    }

    fn recent_languages(&self) -> Vec<tg_lang::Language> {
        self.recent_languages.clone()
    }

    fn apply_language(&mut self, lang_code: &str) -> Result<String, String> {
        if lang_code == "#TEST_X" || lang_code == "#TEST_0" {
            let descriptor = tg_lang::Language {
                id: lang_code.to_owned(),
                plural_id: "en".to_owned(),
                base_id: String::new(),
                name: lang_code.to_owned(),
                native_name: lang_code.to_owned(),
            };
            tg_lang::with_current_mut(|global| global.switch_to_id(&descriptor));
            self.active_language_code = lang_code.to_owned();
            return Ok(lang_code.to_owned());
        }
        let info = self
            .languages
            .iter()
            .find(|l| l.lang_code == lang_code)
            .cloned()
            .ok_or_else(|| format!("Unknown language code: {lang_code}"))?;
        let descriptor = info.to_language();
        tg_lang::with_current_mut(|global| global.switch_to_id(&descriptor));
        self.active_language_code = info.lang_code.clone();
        if !descriptor.id.starts_with('#') {
            self.recent_languages.retain(|l| l.id != descriptor.id);
            self.recent_languages.insert(0, descriptor);
        }
        Ok(info.name)
    }

    fn open_set_language_link(
        &mut self,
        url: &str,
    ) -> Result<Option<super::backend::LanguagePrompt>, String> {
        let Some(parsed) = tg_lang::parse_set_language_link(url) else {
            return Err(format!("Invalid language link: {url}"));
        };
        match parsed {
            tg_lang::SetLanguageLink::OpenLanguageBox => Ok(None),
            tg_lang::SetLanguageLink::SwitchLanguage(code) => {
                if code == self.active_language_code {
                    let prompt = super::backend::LanguagePrompt::AlreadyUsing {
                        lang_code: code,
                        message: tg_lang::tr::lng_language_already(),
                    };
                    self.pending_language_prompt = Some(prompt.clone());
                    return Ok(Some(prompt));
                }
                let prompt = if let Some(info) = self.languages.iter().find(|l| l.lang_code == code)
                {
                    super::language::build_switch_confirmation_prompt(info)
                } else {
                    super::backend::LanguagePrompt::NotFound {
                        lang_code: code,
                        message: tg_lang::tr::lng_language_not_found(),
                    }
                };
                self.pending_language_prompt = Some(prompt.clone());
                Ok(Some(prompt))
            }
        }
    }

    fn confirm_language_prompt(&mut self) -> Result<Option<String>, String> {
        let target = match self.pending_language_prompt.take() {
            Some(super::backend::LanguagePrompt::ConfirmLinkSwitch { info, .. }) => {
                Some(info.lang_code)
            }
            Some(super::backend::LanguagePrompt::SuggestedLanguage { language, .. }) => {
                Some(language.id)
            }
            _ => None,
        };
        let Some(code) = target else {
            return Ok(None);
        };
        self.apply_language(&code).map(Some)
    }

    fn dismiss_language_prompt(&mut self) {
        self.pending_language_prompt = None;
    }

    fn pending_language_prompt(&self) -> Option<super::backend::LanguagePrompt> {
        self.pending_language_prompt.clone()
    }

    fn remove_recent_language(&mut self, lang_code: &str) {
        self.recent_languages.retain(|l| l.id != lang_code);
    }

    fn local_storage_stats(&self) -> LocalStorageStats {
        self.storage
    }

    fn clear_local_storage(&mut self) -> i64 {
        let freed = self.storage.total_bytes();
        self.storage.photos_bytes = 0;
        self.storage.videos_bytes = 0;
        self.storage.files_bytes = 0;
        self.storage.stickers_bytes = 0;
        freed
    }

    fn set_local_storage_limits(&mut self, size_limit_bytes: i64, time_limit_days: i32) {
        self.storage.size_limit_bytes = size_limit_bytes.max(0);
        self.storage.time_limit_days = time_limit_days.max(0);
    }

    fn request_export(&mut self, request: &ExportRequest) -> String {
        self.export_count += 1;
        format!(
            "Export #{} queued ({:?}) to {}",
            self.export_count, request.format, request.path
        )
    }

    fn proxy_settings(&self) -> Option<tg_core::SettingsProxy> {
        Some(self.proxy_settings.clone())
    }

    fn apply_proxy_settings(&mut self, settings: &tg_core::SettingsProxy) -> Result<(), String> {
        self.proxy_settings = settings.clone();
        Ok(())
    }

    fn check_proxy(&mut self, proxy: &tg_core::ProxyData) {
        let status = if proxy.is_valid() {
            ProxyCheckStatus::Available { ping_ms: 42 }
        } else {
            ProxyCheckStatus::Unavailable {
                reason: "invalid proxy configuration".to_owned(),
            }
        };
        if let Some((_, slot)) = self.proxy_checks.iter_mut().find(|(p, _)| p == proxy) {
            *slot = status;
        } else {
            self.proxy_checks.push((proxy.clone(), status));
        }
    }

    fn proxy_check_status(&self, proxy: &tg_core::ProxyData) -> ProxyCheckStatus {
        self.proxy_checks
            .iter()
            .find(|(p, _)| p == proxy)
            .map(|(_, s)| s.clone())
            .unwrap_or(ProxyCheckStatus::NotTested)
    }

    fn web_authorizations(&self) -> Vec<WebAuthorization> {
        let mut out = self.web_authorizations.clone();
        out.sort_by_key(|a| std::cmp::Reverse(a.date_active));
        out
    }

    fn disconnect_website(&mut self, hash: u64, block_bot: bool) -> Result<bool, String> {
        let Some(pos) = self.web_authorizations.iter().position(|w| w.hash == hash) else {
            return Ok(false);
        };
        let removed = self.web_authorizations.remove(pos);
        if block_bot {
            self.block_peer(removed.bot_id);
        }
        Ok(true)
    }

    fn disconnect_all_websites(&mut self) -> Result<usize, String> {
        let count = self.web_authorizations.len();
        self.web_authorizations.clear();
        Ok(count)
    }

    fn saved_ringtones(&self) -> Vec<SavedRingtone> {
        self.saved_ringtones.clone()
    }

    fn save_ringtone(&mut self, id: i64, unsave: bool) -> Result<(), String> {
        if unsave {
            self.saved_ringtones.retain(|r| r.id != id);
        }
        Ok(())
    }

    fn upload_ringtone(
        &mut self,
        file_name: &str,
        mime_type: &str,
        content: &[u8],
    ) -> Result<SavedRingtone, String> {
        if content.is_empty() {
            return Err("Selected audio file is empty.".to_owned());
        }
        if content.len() > DEFAULT_RINGTONE_MAX_SIZE {
            return Err(format!(
                "Ringtone exceeds the {} KB limit.",
                DEFAULT_RINGTONE_MAX_SIZE / 1024
            ));
        }
        let id = self.next_ringtone_id;
        self.next_ringtone_id += 1;
        let trimmed_name = if file_name.trim().is_empty() {
            format!("ringtone_{id}.mp3")
        } else {
            file_name.trim().to_owned()
        };
        let title = trimmed_name
            .rsplit_once('.')
            .map(|(stem, _)| stem.to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| trimmed_name.clone());
        let ringtone = SavedRingtone {
            id,
            access_hash: id * 13 + 7,
            file_reference: vec![0xAA, 0xBB],
            title,
            file_name: trimmed_name,
            mime_type: if mime_type.trim().is_empty() {
                "audio/mpeg".to_owned()
            } else {
                mime_type.trim().to_owned()
            },
            size: content.len() as i64,
            duration_secs: 2.0,
            date: 1_750_020_000,
        };
        self.saved_ringtones.insert(0, ringtone.clone());
        Ok(ringtone)
    }

    fn passkeys(&self) -> Vec<PasskeyEntry> {
        self.passkeys.clone()
    }

    fn is_passkey_supported(&self) -> bool {
        true
    }

    fn can_register_passkey(&self) -> bool {
        self.passkeys.len() < tg_app_core::core_types::misc::DEFAULT_MAX_PASSKEYS
    }

    fn register_passkey(&mut self) -> Result<PasskeyEntry, String> {
        if !self.can_register_passkey() {
            return Err("Maximum number of passkeys reached.".to_owned());
        }
        let seq = self.next_passkey_seq;
        self.next_passkey_seq += 1;
        let entry = PasskeyEntry {
            id: format!("demo-passkey-{seq}"),
            name: format!("Demo Passkey #{seq}"),
            date: 1_740_000_000 + (seq as i32) * 60,
            software_emoji_id: None,
            last_usage_date: None,
        };
        self.passkeys.push(entry.clone());
        Ok(entry)
    }

    fn start_passkey_hybrid_qr(&mut self) -> Result<String, String> {
        self.cable_scanner.0.stop();
        let session = tg_platform::webauthn::CableQrSession::generate(true, 1_740_000_000)
            .map_err(|e| e.to_string())?;
        let uri = session.qr_uri().to_owned();
        let eid_key = *session.eid_key();
        self.active_cable_eid_key = Some(eid_key);
        if let Ok(mut guard) = self.hybrid_qr_status.lock() {
            *guard = PasskeyHybridQrStatus::Scanning;
        }
        let status_avail = Arc::clone(&self.hybrid_qr_status);
        let status_match = Arc::clone(&self.hybrid_qr_status);
        let on_availability: tg_platform::cable_scanner::AvailabilityCallback =
            Arc::new(move |available| {
                if !available
                    && let Ok(mut guard) = status_avail.lock()
                    && matches!(*guard, PasskeyHybridQrStatus::Scanning)
                {
                    *guard = PasskeyHybridQrStatus::NoBluetooth;
                }
            });
        match start_cable_proximity_match(
            &self.cable_scanner.0,
            eid_key,
            true,
            move |matched| {
                if let Ok(mut guard) = status_match.lock() {
                    *guard = PasskeyHybridQrStatus::Connecting {
                        tunnel_domain: matched.tunnel_domain,
                    };
                }
            },
            on_availability,
        ) {
            Ok(true) => {}
            Ok(false) => {
                if let Ok(mut guard) = self.hybrid_qr_status.lock()
                    && matches!(*guard, PasskeyHybridQrStatus::Scanning)
                {
                    *guard = PasskeyHybridQrStatus::NoBluetooth;
                }
            }
            Err(err) => {
                if let Ok(mut guard) = self.hybrid_qr_status.lock() {
                    *guard = PasskeyHybridQrStatus::Error(err.to_string());
                }
            }
        }
        Ok(uri)
    }

    fn passkey_hybrid_qr_status(&self) -> PasskeyHybridQrStatus {
        self.hybrid_qr_status
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    fn cancel_passkey_hybrid_qr(&mut self) {
        self.cable_scanner.0.stop();
        self.active_cable_eid_key = None;
        if let Ok(mut guard) = self.hybrid_qr_status.lock() {
            *guard = PasskeyHybridQrStatus::Idle;
        }
    }

    fn delete_passkey(&mut self, id: &str) -> Result<(), String> {
        let before = self.passkeys.len();
        self.passkeys.retain(|p| p.id != id);
        if self.passkeys.len() == before {
            return Err("Passkey not found.".to_owned());
        }
        Ok(())
    }

    fn fetch_faq_suggestions(
        &self,
        _faq_url: &str,
    ) -> Result<Vec<super::backend::FaqEntry>, String> {
        Ok(super::faq_suggestions::default_offline_faq_entries())
    }
}
