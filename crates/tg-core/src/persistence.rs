//! End-to-end persistence adapters wiring `tg-core` (`CoreSettings`, `GlobalConfig`,
//! multi-account `Domain`) to `tg-storage` (`LocalStorage`, `DomainStorage`, `AccountStorage`).
//!
//! Ports persistence integration from:
//! - `Telegram/SourceFiles/storage/localstorage.cpp` (`dbiApplicationSettings` <-> `Core::Settings`)
//! - `Telegram/SourceFiles/storage/storage_domain.cpp` (`Storage::Domain` <-> `Main::Domain`)

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use tg_storage::domain::{AccountsInfo, DomainStorage as StorageDomain};
use tg_storage::serialize::peer::{SerializedPeerKind, deserialize_self_user};
use tg_storage::tdf::{clear_key, compose_data_string, compute_data_name_key, to_file_part};
use tg_storage::{
    AccountStorage, AuthKey, GlobalSettings, LocalStorage, MtpAuthorization, PasscodeKdf,
    StorageError,
};

use crate::domain::{
    Account, AccountSession, DomainStorage, DomainStorageError, Environment, LoadedDomainState,
};
use crate::globals::GlobalConfig;
use crate::settings::{CoreSettings, PowerSavingFlags, SettingsError};
use crate::settings_proxy::{ProxyData, ProxySettingsMode, ProxyType};

impl CoreSettings {
    /// Load `CoreSettings` from the `dbiApplicationSettings`, `dbiPowerSaving`, and legacy
    /// `dbiConnectionTypeOld*` / `dbiTryIPv6Old` blocks of `tdata/settingss`
    /// (`Local::readSettings` -> `Core::Settings::addFromSerialized`).
    pub fn load_from_local_storage(local: &LocalStorage) -> Result<Self, SettingsError> {
        let mut settings = Self::default();
        if let Some(legacy) = &local.settings().legacy_proxy {
            if let Some(try_ipv6) = legacy.try_ipv6 {
                settings.proxy.set_try_ipv6(try_ipv6);
            }
            settings
                .proxy
                .set_use_proxy_for_calls(legacy.use_proxy_for_calls);
            let list: Vec<ProxyData> = legacy
                .list
                .iter()
                .map(|p| ProxyData {
                    proxy_type: ProxyType::from_i32(p.proxy_type),
                    host: p.host.clone(),
                    port: p.port,
                    user: p.user.clone(),
                    password: p.password.clone(),
                })
                .filter(ProxyData::is_valid)
                .collect();
            settings.proxy.set_list(list);
            let selected = legacy
                .selected
                .as_ref()
                .map(|p| ProxyData {
                    proxy_type: ProxyType::from_i32(p.proxy_type),
                    host: p.host.clone(),
                    port: p.port,
                    user: p.user.clone(),
                    password: p.password.clone(),
                })
                .filter(ProxyData::is_valid)
                .unwrap_or_default();
            settings.proxy.set_selected(selected);
            let mode = match ProxySettingsMode::from_i32(legacy.mode) {
                Ok(ProxySettingsMode::Enabled) if settings.proxy.selected().is_valid() => {
                    ProxySettingsMode::Enabled
                }
                Ok(ProxySettingsMode::Disabled) => ProxySettingsMode::Disabled,
                _ => ProxySettingsMode::System,
            };
            settings.proxy.set_settings(mode);
        }
        let blob = &local.settings().application_settings;
        if !blob.is_empty() {
            settings.add_from_serialized(blob, local.old_settings_version())?;
        }
        settings.power_saving = PowerSavingFlags::from_i32(local.settings().power_saving);
        Ok(settings)
    }

    /// Serialize `CoreSettings` into `dbiApplicationSettings` and `dbiPowerSaving` and persist
    /// `tdata/settingss` (`Local::writeSettings`).
    pub fn save_to_local_storage(&self, local: &mut LocalStorage) -> Result<(), StorageError> {
        local.settings_mut().power_saving = self.power_saving.as_i32();
        local.settings_mut().application_settings = self.serialize();
        local.write_settings()
    }
}

impl GlobalConfig {
    /// Apply persisted `GlobalSettings` fields from `tdata/settingss` onto runtime `GlobalConfig`.
    pub fn apply_global_settings(&mut self, settings: &GlobalSettings) {
        self.auto_start = settings.auto_start;
        self.start_minimized = settings.start_minimized;
        self.send_to_menu = settings.send_to_menu;
        self.seen_tray_tooltip = settings.seen_tray_tooltip;
        self.auto_update = settings.auto_update;
        self.last_update_check = settings.last_update_check;
        self.config_scale = settings.scale_percent;
        self.validate_scale();
        if !settings.dialog_last_path.is_empty() {
            self.dialog_last_path = PathBuf::from(&settings.dialog_last_path);
        }
    }

    /// Copy runtime `GlobalConfig` settings into `GlobalSettings` prior to `LocalStorage::write_settings`.
    pub fn write_to_global_settings(&self, settings: &mut GlobalSettings) {
        settings.auto_start = self.auto_start;
        settings.start_minimized = self.start_minimized;
        settings.send_to_menu = self.send_to_menu;
        settings.seen_tray_tooltip = self.seen_tray_tooltip;
        settings.auto_update = self.auto_update;
        settings.last_update_check = self.last_update_check;
        settings.scale_percent = self.config_scale;
        settings.dialog_last_path = self.dialog_last_path.to_string_lossy().into_owned();
    }
}

/// Disk-backed [`DomainStorage`] implementation using `tg-storage`'s [`StorageDomain`] (`key_datas`)
/// and per-account [`AccountStorage`] (`maps`, MTP authorization, drafts, prefs, locations).
#[derive(Debug)]
pub struct TdataDomainStorage {
    tdata_path: PathBuf,
    data_name: String,
    default_environment: Environment,
    domain: Option<StorageDomain>,
    accounts: BTreeMap<i32, AccountStorage>,
}

impl TdataDomainStorage {
    /// Create an unopened `TdataDomainStorage` targeting `tdata_path` (e.g. `<workdir>/tdata`)
    /// and profile `data_name` (normally `"data"`).
    #[must_use]
    pub fn new(tdata_path: impl Into<PathBuf>, data_name: impl Into<String>) -> Self {
        Self {
            tdata_path: tdata_path.into(),
            data_name: data_name.into(),
            default_environment: Environment::Production,
            domain: None,
            accounts: BTreeMap::new(),
        }
    }

    /// Create an unopened `TdataDomainStorage` targeting `<working_dir>/tdata`.
    #[must_use]
    pub fn from_working_dir(working_dir: &Path, data_name: impl Into<String>) -> Self {
        Self::new(working_dir.join("tdata"), data_name)
    }

    /// Configures the default [`Environment`] used for newly created account slots when no
    /// persisted `MTP::Config` exists on disk yet (e.g. `-testmode`).
    #[must_use]
    pub const fn with_default_environment(mut self, environment: Environment) -> Self {
        self.default_environment = environment;
        self
    }

    /// Path to the `tdata` directory.
    #[must_use]
    pub fn tdata_path(&self) -> &Path {
        &self.tdata_path
    }

    /// Profile data name (normally `"data"`).
    #[must_use]
    pub fn data_name(&self) -> &str {
        &self.data_name
    }

    /// Check whether unlocking this `tdata` directory requires a non-empty local passcode
    /// before calling [`crate::Domain::start`].
    pub fn needs_passcode(&self) -> Result<bool, StorageError> {
        StorageDomain::needs_passcode(&self.tdata_path, &self.data_name)
    }

    /// Return the unlocked 256-byte local storage key, if started.
    #[must_use]
    pub fn local_key(&self) -> Option<&AuthKey> {
        self.domain.as_ref().map(StorageDomain::local_key)
    }

    /// Borrow the underlying [`tg_storage::DomainStorage`], if unlocked.
    #[must_use]
    pub fn domain_storage(&self) -> Option<&StorageDomain> {
        self.domain.as_ref()
    }

    /// Mutably borrow the underlying [`tg_storage::DomainStorage`], if unlocked.
    pub fn domain_storage_mut(&mut self) -> Option<&mut StorageDomain> {
        self.domain.as_mut()
    }

    /// Whether app launch lock is currently armed (`Storage::Domain::appLockEnabled`).
    #[must_use]
    pub fn app_lock_enabled(&self) -> bool {
        self.domain
            .as_ref()
            .is_some_and(StorageDomain::app_lock_enabled)
    }

    /// Set, change, or clear the local passcode (`Storage::Domain::setPasscode`).
    pub fn set_passcode(
        &mut self,
        passcode: &[u8],
        kdf: PasscodeKdf,
        drop_open_wrap: bool,
    ) -> Result<(), StorageError> {
        let domain = self
            .domain
            .as_mut()
            .ok_or_else(|| StorageError::CorruptData("domain storage is not started".into()))?;
        domain.set_passcode(passcode, kdf, drop_open_wrap)
    }

    /// Set a legacy 3-blob passcode (or empty passcode for legacy no-passcode format).
    pub fn set_legacy_passcode(&mut self, passcode: &[u8]) -> Result<(), StorageError> {
        let domain = self
            .domain
            .as_mut()
            .ok_or_else(|| StorageError::CorruptData("domain storage is not started".into()))?;
        domain.set_legacy_passcode(passcode)
    }

    /// Enable or disable app launch lock without removing the configured passcode.
    pub fn set_app_lock_enabled(&mut self, enabled: bool) -> Result<(), StorageError> {
        let domain = self
            .domain
            .as_mut()
            .ok_or_else(|| StorageError::CorruptData("domain storage is not started".into()))?;
        domain.set_app_lock_enabled(enabled)
    }

    /// Borrow the loaded [`AccountStorage`] for slot `index`.
    #[must_use]
    pub fn account_storage(&self, index: i32) -> Option<&AccountStorage> {
        self.accounts.get(&index)
    }

    /// Mutably borrow the loaded [`AccountStorage`] for slot `index`.
    pub fn account_storage_mut(&mut self, index: i32) -> Option<&mut AccountStorage> {
        self.accounts.get_mut(&index)
    }

    /// Read the decrypted [`MtpAuthorization`] for account slot `index`.
    pub fn read_account_mtp_auth(
        &self,
        index: i32,
    ) -> Result<Option<MtpAuthorization>, StorageError> {
        let Some(acc) = self.accounts.get(&index) else {
            return Ok(None);
        };
        acc.read_mtp_data()
    }

    /// Write and encrypt [`MtpAuthorization`] for account slot `index`.
    pub fn write_account_mtp_auth(
        &self,
        index: i32,
        auth: &MtpAuthorization,
    ) -> Result<(), StorageError> {
        let acc = self
            .accounts
            .get(&index)
            .ok_or_else(|| StorageError::CorruptData(format!("account slot {index} not loaded")))?;
        acc.write_mtp_data(auth)
    }

    fn build_core_account(
        &self,
        index: i32,
        acc_storage: &AccountStorage,
    ) -> Result<Account, DomainStorageError> {
        let mut account = Account::new(index, &self.data_name, self.default_environment);
        let mtp_auth = acc_storage
            .read_mtp_data()
            .map_err(|e| DomainStorageError::Backend(e.to_string()))?;

        if let Some(auth) = mtp_auth {
            account.main_dc_id = auth.main_dc_id;
            if auth.user_id != 0 {
                let username = if !acc_storage.map().self_serialized.is_empty() {
                    deserialize_self_user(
                        acc_storage.old_map_version().max(tg_storage::APP_VERSION),
                        &acc_storage.map().self_serialized,
                    )
                    .ok()
                    .and_then(|(peer, _)| match peer.kind {
                        SerializedPeerKind::User(u) => Some(u.username),
                        _ => None,
                    })
                    .unwrap_or_default()
                } else {
                    String::new()
                };
                account.create_session(AccountSession {
                    user_id: auth.user_id as i64,
                    unique_id: auth.user_id,
                    premium: false,
                    is_test_mode: false,
                    username,
                    unread_badge: 0,
                    unread_badge_muted: false,
                });
            }
        }

        Ok(account)
    }

    fn remove_account_files(&self, index: i32) {
        let acc_data_name = compose_data_string(&self.data_name, index);
        let key = compute_data_name_key(&acc_data_name);
        clear_key(key, &self.tdata_path);
        let base_dir = self.tdata_path.join(to_file_part(key));
        if base_dir.exists() {
            let _ = fs::remove_dir_all(&base_dir);
        }
    }
}

impl DomainStorage for TdataDomainStorage {
    fn load_accounts(&mut self, passcode: &[u8]) -> Result<LoadedDomainState, DomainStorageError> {
        let mut preloaded_legacy: Option<AccountStorage> = None;

        let domain = match StorageDomain::open(&self.tdata_path, &self.data_name, passcode) {
            Ok(ds) => ds,
            Err(StorageError::IncorrectPasscode) => {
                return Err(DomainStorageError::IncorrectPasscode);
            }
            Err(StorageError::NotFound(_)) => {
                // Match Storage::Domain::start: attempt legacy single-account `map` before
                // creating a fresh multi-account domain.
                match AccountStorage::open_legacy(&self.tdata_path, &self.data_name, passcode) {
                    Ok(legacy_acc) => {
                        let local_key = legacy_acc.local_key().clone();
                        let mut ds = StorageDomain::create_with_local_key(
                            &self.tdata_path,
                            &self.data_name,
                            local_key,
                            AccountsInfo {
                                indices: vec![0],
                                active: 0,
                            },
                        )
                        .map_err(|e| DomainStorageError::Backend(e.to_string()))?;
                        if !passcode.is_empty() {
                            ds.set_passcode(passcode, PasscodeKdf::default_argon2id(), true)
                                .map_err(|e| DomainStorageError::Backend(e.to_string()))?;
                        }
                        preloaded_legacy = Some(legacy_acc);
                        ds
                    }
                    Err(StorageError::IncorrectPasscode) => {
                        return Err(DomainStorageError::IncorrectPasscode);
                    }
                    Err(StorageError::NotFound(_)) => {
                        StorageDomain::create_fresh(&self.tdata_path, &self.data_name)
                            .map_err(|e| DomainStorageError::Backend(e.to_string()))?
                    }
                    Err(e) => return Err(DomainStorageError::Backend(e.to_string())),
                }
            }
            Err(e) => return Err(DomainStorageError::Backend(e.to_string())),
        };

        let indices = domain.accounts().indices.clone();
        let active_index = domain.accounts().active;
        let local_key = domain.local_key().clone();

        self.accounts.clear();
        let mut loaded_accounts = Vec::with_capacity(indices.len());
        for idx in indices {
            let acc_storage = if idx == 0
                && let Some(legacy_acc) = preloaded_legacy.take()
            {
                legacy_acc
            } else {
                AccountStorage::open(&self.tdata_path, &self.data_name, idx, local_key.clone())
                    .map_err(|e| DomainStorageError::Backend(e.to_string()))?
            };
            let core_account = self.build_core_account(idx, &acc_storage)?;
            self.accounts.insert(idx, acc_storage);
            loaded_accounts.push(core_account);
        }

        self.domain = Some(domain);

        Ok(LoadedDomainState {
            accounts: loaded_accounts,
            active_index,
        })
    }

    fn write_accounts(
        &mut self,
        active_index: i32,
        account_indices: &[i32],
    ) -> Result<(), DomainStorageError> {
        let domain = self
            .domain
            .as_mut()
            .ok_or_else(|| DomainStorageError::Backend("domain storage not started".into()))?;
        domain.accounts_mut().indices = account_indices.to_vec();
        domain.accounts_mut().active = active_index;
        domain
            .write_accounts()
            .map_err(|e| DomainStorageError::Backend(e.to_string()))
    }

    fn start_added(
        &mut self,
        index: i32,
        _environment: Environment,
    ) -> Result<(), DomainStorageError> {
        let local_key = self
            .domain
            .as_ref()
            .ok_or_else(|| DomainStorageError::Backend("domain storage not started".into()))?
            .local_key()
            .clone();
        let acc = AccountStorage::open(&self.tdata_path, &self.data_name, index, local_key)
            .map_err(|e| DomainStorageError::Backend(e.to_string()))?;
        self.accounts.insert(index, acc);
        Ok(())
    }

    fn account_removed(&mut self, index: i32) -> Result<(), DomainStorageError> {
        self.accounts.remove(&index);
        self.remove_account_files(index);
        Ok(())
    }

    fn has_passcode(&self) -> bool {
        self.domain
            .as_ref()
            .is_some_and(StorageDomain::has_passcode)
    }

    fn check_passcode(&self, passcode: &[u8]) -> bool {
        match &self.domain {
            Some(ds) if ds.has_passcode() => ds.check_passcode(passcode),
            _ => true,
        }
    }

    fn clear_passcode_after_reset(&mut self) -> Result<(), DomainStorageError> {
        let Some(domain) = self.domain.as_mut() else {
            return Ok(());
        };
        domain
            .set_passcode(b"", PasscodeKdf::default_argon2id(), false)
            .map_err(|e| DomainStorageError::Backend(e.to_string()))
    }

    fn start_from_scratch(&mut self) -> Result<LoadedDomainState, DomainStorageError> {
        let existing_indices: Vec<i32> = if let Some(ds) = &self.domain {
            ds.accounts().indices.clone()
        } else {
            (0..tg_storage::domain::PREMIUM_MAX_ACCOUNTS).collect()
        };
        for idx in existing_indices {
            self.remove_account_files(idx);
        }
        self.accounts.clear();
        self.domain = None;
        StorageDomain::clear_files(&self.tdata_path, &self.data_name);

        let fresh = StorageDomain::create_fresh(&self.tdata_path, &self.data_name)
            .map_err(|e| DomainStorageError::Backend(e.to_string()))?;
        let local_key = fresh.local_key().clone();
        let acc0 = AccountStorage::open(&self.tdata_path, &self.data_name, 0, local_key)
            .map_err(|e| DomainStorageError::Backend(e.to_string()))?;
        let core_acc0 = Account::new(0, &self.data_name, self.default_environment);
        self.accounts.insert(0, acc0);
        self.domain = Some(fresh);

        Ok(LoadedDomainState {
            accounts: vec![core_acc0],
            active_index: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use tg_storage::LocalStorage;

    use crate::settings::{AutoplaySettings, CoreSettings, PowerSavingFlags};

    #[test]
    fn dbi_power_saving_round_trips_with_synthetic_tdata() {
        let tmp = TempDir::new().expect("tempdir");
        let tdata = tmp.path().join("tdata");

        let flags = PowerSavingFlags::STICKERS_PANEL
            | PowerSavingFlags::EMOJI_REACTIONS
            | PowerSavingFlags::CHAT_SPOILER
            | PowerSavingFlags::CALLS
            | PowerSavingFlags::CHAT_EFFECTS;

        // 1. Save CoreSettings into synthetic tdata and confirm `dbiPowerSaving` mirrors it.
        {
            let mut local = LocalStorage::open_or_init(&tdata).expect("init local storage");
            let mut settings = CoreSettings::new();
            settings.power_saving = flags;
            settings.ignore_battery_saving = true;
            settings.autoplay = AutoplaySettings {
                gifs: false,
                videos: true,
                round_videos: false,
                video_stickers: false,
            };
            settings
                .save_to_local_storage(&mut local)
                .expect("save CoreSettings");
            assert_eq!(local.settings().power_saving, flags.as_i32());
        }

        // 2. Reopen from disk and confirm `dbiPowerSaving` and `CoreSettings::power_saving` match.
        {
            let local = LocalStorage::open_or_init(&tdata).expect("reopen local storage");
            assert_eq!(local.settings().power_saving, flags.as_i32());
            let loaded = CoreSettings::load_from_local_storage(&local).expect("load CoreSettings");
            assert_eq!(loaded.power_saving, flags);
            assert!(loaded.ignore_battery_saving);
            assert!(!loaded.autoplay.gifs);
            assert!(loaded.autoplay.videos);
            assert!(!loaded.autoplay.round_videos);
            assert!(!loaded.autoplay.video_stickers);
        }

        // 3. Clearing power_saving clears `dbiPowerSaving` back to 0 on disk.
        {
            let mut local = LocalStorage::open_or_init(&tdata).expect("reopen local storage");
            let mut loaded =
                CoreSettings::load_from_local_storage(&local).expect("load CoreSettings");
            loaded.power_saving = PowerSavingFlags::NONE;
            loaded
                .save_to_local_storage(&mut local)
                .expect("save cleared power_saving");
            assert_eq!(local.settings().power_saving, 0);
        }
        {
            let local = LocalStorage::open_or_init(&tdata).expect("reopen cleared local storage");
            assert_eq!(local.settings().power_saving, 0);
            let loaded = CoreSettings::load_from_local_storage(&local).expect("load cleared");
            assert_eq!(loaded.power_saving, PowerSavingFlags::NONE);
        }
    }

    #[test]
    fn dbi_power_saving_reads_from_tdesktop_only_tdata_and_truncates_unknown_bits() {
        let tmp = TempDir::new().expect("tempdir");
        let tdata = tmp.path().join("tdata");

        // Simulate a tdesktop `tdata/settingss` that has only `dbiPowerSaving` populated
        // (no `__tgfork_power_saving` pref inside `dbiApplicationSettings`), plus unknown high bits.
        let expected = PowerSavingFlags::ANIMATIONS
            | PowerSavingFlags::EMOJI_CHAT
            | PowerSavingFlags::CHAT_BACKGROUND;
        let raw_with_unknown_bits = expected.as_i32() | (1 << 20);
        {
            let mut local = LocalStorage::open_or_init(&tdata).expect("init local storage");
            local.settings_mut().power_saving = raw_with_unknown_bits;
            local.write_settings().expect("write tdesktop settingss");
        }

        let reopened = LocalStorage::open_or_init(&tdata).expect("reopen synthetic tdata");
        assert_eq!(reopened.settings().power_saving, raw_with_unknown_bits);
        let loaded = CoreSettings::load_from_local_storage(&reopened).expect("load CoreSettings");
        assert_eq!(loaded.power_saving, expected);
    }

    #[test]
    fn tdata_proxy_settings_full_round_trip() {
        use crate::settings_proxy::{ProxyData, ProxySettingsMode, ProxyType};

        let tmp = TempDir::new().expect("tempdir");
        let tdata = tmp.path().join("tdata");

        let socks = ProxyData {
            proxy_type: ProxyType::Socks5,
            host: "127.0.0.1".to_owned(),
            port: 1080,
            user: "proxy_user".to_owned(),
            password: "proxy_pass".to_owned(),
        };
        let http = ProxyData {
            proxy_type: ProxyType::Http,
            host: "127.0.0.1".to_owned(),
            port: 8080,
            user: String::new(),
            password: String::new(),
        };
        let mtproto = ProxyData {
            proxy_type: ProxyType::Mtproto,
            host: "127.0.0.1".to_owned(),
            port: 443,
            user: String::new(),
            password: "dd0123456789abcdef0123456789abcdef".to_owned(),
        };

        for mode in [
            ProxySettingsMode::Enabled,
            ProxySettingsMode::System,
            ProxySettingsMode::Disabled,
        ] {
            {
                let mut local = LocalStorage::open_or_init(&tdata).expect("open tdata");
                let mut settings = CoreSettings::new();
                settings.proxy.set_settings(mode);
                settings.proxy.set_try_ipv6(true);
                settings.proxy.set_use_proxy_for_calls(true);
                settings.proxy.set_proxy_rotation_enabled(true);
                settings.proxy.set_proxy_rotation_timeout(15);
                settings
                    .proxy
                    .set_list(vec![socks.clone(), http.clone(), mtproto.clone()]);
                settings.proxy.set_selected(mtproto.clone());
                settings
                    .proxy
                    .set_proxy_rotation_preferred_indices(vec![2, 0]);
                settings
                    .save_to_local_storage(&mut local)
                    .expect("save proxy settings to tdata");
            }

            {
                let local = LocalStorage::open_or_init(&tdata).expect("reopen tdata");
                let loaded =
                    CoreSettings::load_from_local_storage(&local).expect("load CoreSettings");
                assert_eq!(loaded.proxy.settings(), mode);
                assert!(loaded.proxy.try_ipv6());
                assert!(loaded.proxy.use_proxy_for_calls());
                assert!(loaded.proxy.proxy_rotation_enabled());
                assert_eq!(loaded.proxy.proxy_rotation_timeout(), 15);
                assert_eq!(
                    loaded.proxy.list(),
                    &[socks.clone(), http.clone(), mtproto.clone()]
                );
                assert_eq!(loaded.proxy.selected(), &mtproto);
                assert_eq!(loaded.proxy.proxy_rotation_preferred_indices(), &[2, 0]);
            }
        }
    }

    #[test]
    fn tdata_legacy_connection_type_migrates_into_core_settings() {
        use crate::settings_proxy::{ProxySettingsMode, ProxyType};
        use tg_crypto::{LOCAL_ENCRYPT_SALT_SIZE, create_legacy_local_key};
        use tg_storage::qdatastream::QDataStreamWriter;
        use tg_storage::settings_scheme::DbiBlockId;
        use tg_storage::tdf::FileWriteDescriptor;

        let tmp = TempDir::new().expect("tempdir");
        let tdata = tmp.path().join("tdata");

        // Write a synthetic legacy tdesktop `settingss` file containing only
        // `dbiConnectionTypeOld` and `dbiTryIPv6Old` (no `dbiApplicationSettings`).
        let mut w = QDataStreamWriter::new();
        w.write_u32(DbiBlockId::ConnectionTypeOld as u32);
        w.write_i32(5); // dbictProxiesList
        w.write_i32(1); // count
        w.write_i32(1); // 1-based selected index
        w.write_i32(1); // Enabled
        w.write_i32(1); // use_proxy_for_calls
        w.write_i32(1024 + 1); // Socks5
        w.write_qstring("127.0.0.1");
        w.write_i32(1080);
        w.write_qstring("u");
        w.write_qstring("p");
        w.write_u32(DbiBlockId::TryIPv6Old as u32);
        w.write_i32(1);

        let salt = vec![0x42u8; LOCAL_ENCRYPT_SALT_SIZE];
        let key = create_legacy_local_key(b"", &salt);
        let mut file = FileWriteDescriptor::new("settings", &tdata);
        file.write_data(&salt);
        file.write_encrypted(w.as_bytes(), &key)
            .expect("encrypt legacy settings");
        file.finish().expect("write legacy settings file");

        let local = LocalStorage::open_or_init(&tdata).expect("open legacy tdata");
        let loaded =
            CoreSettings::load_from_local_storage(&local).expect("load legacy CoreSettings");
        assert_eq!(loaded.proxy.settings(), ProxySettingsMode::Enabled);
        assert!(loaded.proxy.try_ipv6());
        assert!(loaded.proxy.use_proxy_for_calls());
        assert_eq!(loaded.proxy.list().len(), 1);
        assert_eq!(loaded.proxy.selected().proxy_type, ProxyType::Socks5);
        assert_eq!(loaded.proxy.selected().host, "127.0.0.1");
        assert_eq!(loaded.proxy.selected().port, 1080);
        assert_eq!(loaded.proxy.selected().user, "u");
        assert_eq!(loaded.proxy.selected().password, "p");
    }
}
