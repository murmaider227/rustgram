//! Global `tdata` storage (`settings`, themes, backgrounds, langpack, recent languages,
//! update manifest, and legacy pre-TDF readers).
//!
//! Port of `Telegram/SourceFiles/storage/localstorage.h` and `localstorage.cpp`.

use std::fs;
use std::path::{Path, PathBuf};

use tg_crypto::{AuthKey, LOCAL_ENCRYPT_SALT_SIZE, create_legacy_local_key, random_bytes};

use crate::error::{Result, StorageError};
use crate::qdatastream::{QDataStreamReader, QDataStreamWriter, QtColor};
use crate::settings_scheme::{
    DbiBlockId, LegacyProxySettings, ReadSettingsContext, read_all_settings, read_setting,
};
use crate::tdf::{
    APP_VERSION, FileKey, FileWriteDescriptor, clear_key, decrypt_local, generate_key,
    read_encrypted_file_by_key, read_file, write_encrypted_file_by_key,
};

/// Tag marking modern relative theme paths in theme storage (`kThemeNewPathRelativeTag`).
pub const THEME_NEW_PATH_RELATIVE_TAG: &str = "special://new_tag";

/// Wallpaper serialization tag (`kWallPaperSerializeTagId = -112`).
pub const WALLPAPER_SERIALIZE_TAG_ID: i32 = -112;
/// Legacy wallpaper serialization tag (`kWallPaperLegacySerializeTagId = -111`).
pub const WALLPAPER_LEGACY_SERIALIZE_TAG_ID: i32 = -111;

/// Size of the detached Ed25519 signature prefixed to `tdata/update-manifest`.
pub const UPDATE_MANIFEST_SIGNATURE_SIZE: usize = 64;
/// Maximum allowed update manifest payload size (256 KiB).
pub const UPDATE_MANIFEST_MAX_SIZE: usize = 256 * 1024;

/// A language descriptor stored in `tdata`'s recent languages file (`Lang::Language`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoredLanguage {
    /// Language ID (e.g., `"en"`).
    pub id: String,
    /// Pluralization language ID.
    pub plural_id: String,
    /// Optional base language ID.
    pub base_id: String,
    /// English name.
    pub name: String,
    /// Native name.
    pub native_name: String,
}

/// Cloud theme per-variant settings stored inside a theme file (`Data::CloudTheme::Settings`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoredCloudThemeSettings {
    /// Accent color.
    pub accent_color: QtColor,
    /// Optional outgoing accent color.
    pub outgoing_accent_color: Option<QtColor>,
    /// Outgoing message gradient colors.
    pub outgoing_messages_colors: Vec<QtColor>,
    /// Serialized `Data::WallPaper` blob.
    pub paper_serialized: Vec<u8>,
}

/// Saved theme file contents (`Window::Theme::Saved`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoredTheme {
    /// Raw `.tdesktop-theme` / `.tdesktop-palette` file bytes.
    pub content: Vec<u8>,
    /// Relative path to theme file.
    pub path_relative: String,
    /// Absolute path to theme file.
    pub path_absolute: String,
    /// Cloud theme ID (`0` for local themes).
    pub cloud_id: u64,
    /// Cloud theme access hash.
    pub cloud_access_hash: u64,
    /// Cloud theme slug.
    pub cloud_slug: String,
    /// Cloud theme title.
    pub cloud_title: String,
    /// Cloud theme document ID.
    pub cloud_document_id: u64,
    /// Cloud theme creator bare user ID.
    pub cloud_created_by: u64,
    /// Cloud theme emoticon (for chat themes).
    pub cloud_emoticon: String,
    /// Cloud theme light/dark settings `(type_id, settings)`.
    pub cloud_settings: Vec<(i32, StoredCloudThemeSettings)>,
    /// Cached palette checksum.
    pub cache_palette_checksum: i32,
    /// Cached content checksum.
    pub cache_content_checksum: i32,
    /// Cached compiled colors bytes.
    pub cache_colors: Vec<u8>,
    /// Cached background bytes.
    pub cache_background: Vec<u8>,
    /// Whether cached background is tiled.
    pub cache_tiled: bool,
}

/// Saved global background file contents (`Local::writeBackground` / `readBackground`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoredBackground {
    /// Serialized `Data::WallPaper` payload.
    pub paper_serialized: Vec<u8>,
    /// Raw image data (width i32 LE + height i32 LE + ARGB32_Premultiplied pixels in modern format).
    pub image_data: Vec<u8>,
}

/// Global settings values serialized into `tdata/settingss` (`Local::writeSettings`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalSettings {
    /// Auto-start with OS (`dbiAutoStart`).
    pub auto_start: bool,
    /// Start minimized (`dbiStartMinimized`).
    pub start_minimized: bool,
    /// Send-to context menu (`dbiSendToMenu`).
    pub send_to_menu: bool,
    /// Seen tray tooltip (`dbiSeenTrayTooltip`).
    pub seen_tray_tooltip: bool,
    /// Auto-update enabled (`dbiAutoUpdate`).
    pub auto_update: bool,
    /// Last update check timestamp (`dbiLastUpdateCheck`).
    pub last_update_check: i32,
    /// Configured scale percentage (`dbiScalePercent`).
    pub scale_percent: i32,
    /// Serialized fallback production `MTP::Config` (`dbiFallbackProductionConfig`).
    pub fallback_production_config: Vec<u8>,
    /// Serialized `Core::Settings` blob (`dbiApplicationSettings`).
    pub application_settings: Vec<u8>,
    /// Last file dialog directory (`dbiDialogLastPath`).
    pub dialog_last_path: String,
    /// Power saving bitflags (`dbiPowerSaving`).
    pub power_saving: i32,
    /// Day theme file key (`dbiThemeKey`).
    pub theme_key_day: FileKey,
    /// Night theme file key (`dbiThemeKey`).
    pub theme_key_night: FileKey,
    /// Night mode enabled (`dbiThemeKey`).
    pub night_mode: bool,
    /// Whether global background keys are active (`_useGlobalBackgroundKeys`).
    pub use_global_background_keys: bool,
    /// Day background file key (`dbiBackgroundKey`).
    pub background_key_day: FileKey,
    /// Night background file key (`dbiBackgroundKey`).
    pub background_key_night: FileKey,
    /// Tile day background (`dbiTileBackground`).
    pub tile_day: bool,
    /// Tile night background (`dbiTileBackground`).
    pub tile_night: bool,
    /// Langpack file key (`dbiLangPackKey`).
    pub lang_pack_key: FileKey,
    /// Recent languages file key (`dbiLanguagesKey`).
    pub languages_key: FileKey,
    /// Legacy proxy settings read from `dbiConnectionTypeOld*` / `dbiTryIPv6Old` if present.
    pub legacy_proxy: Option<LegacyProxySettings>,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            auto_start: false,
            start_minimized: false,
            send_to_menu: false,
            seen_tray_tooltip: false,
            auto_update: true,
            last_update_check: 0,
            scale_percent: 0,
            fallback_production_config: Vec::new(),
            application_settings: Vec::new(),
            dialog_last_path: String::new(),
            power_saving: 0,
            theme_key_day: 0,
            theme_key_night: 0,
            night_mode: false,
            use_global_background_keys: false,
            background_key_day: 0,
            background_key_night: 0,
            tile_day: false,
            tile_night: true,
            lang_pack_key: 0,
            languages_key: 0,
            legacy_proxy: None,
        }
    }
}

/// Manager for global (non-account-specific) `tdata` files (`Local::*`).
#[derive(Debug, Clone)]
pub struct LocalStorage {
    tdata_path: PathBuf,
    settings_salt: Vec<u8>,
    settings_key: AuthKey,
    settings: GlobalSettings,
    old_settings_version: i32,
}

impl LocalStorage {
    /// Open `tdata/settingss` (or initialize a fresh salt/key if absent) (`Local::start`).
    pub fn open_or_init(tdata_path: impl Into<PathBuf>) -> Result<Self> {
        let tdata_path = tdata_path.into();
        match read_file("settings", &tdata_path) {
            Ok(file) => {
                let mut r = file.stream();
                let salt = r.read_byte_array()?;
                let encrypted = r.read_byte_array()?;
                if salt.len() != LOCAL_ENCRYPT_SALT_SIZE {
                    return Err(StorageError::CorruptData(format!(
                        "bad salt in settings file, size: {}",
                        salt.len()
                    )));
                }
                // Important: `settings` uses CreateLegacyLocalKey("", salt) (PBKDF2-HMAC-SHA1, 4 iters).
                let settings_key = create_legacy_local_key(b"", &salt);
                let inner = decrypt_local(&encrypted, &settings_key)?;
                let mut ctx = ReadSettingsContext::default();
                read_all_settings(&inner, file.version, &mut ctx)?;

                let settings = GlobalSettings {
                    auto_start: ctx.auto_start.unwrap_or(false),
                    start_minimized: ctx.start_minimized.unwrap_or(false),
                    send_to_menu: ctx.send_to_menu.unwrap_or(false),
                    seen_tray_tooltip: ctx.seen_tray_tooltip.unwrap_or(false),
                    auto_update: ctx.auto_update.unwrap_or(true),
                    last_update_check: ctx.last_update_check.unwrap_or(0),
                    scale_percent: ctx.scale_percent.unwrap_or(0),
                    fallback_production_config: ctx.fallback_config,
                    application_settings: ctx.application_settings,
                    dialog_last_path: ctx.dialog_last_path.unwrap_or_default(),
                    power_saving: ctx.power_saving.unwrap_or(0),
                    theme_key_day: ctx.theme_key_day,
                    theme_key_night: ctx.theme_key_night,
                    night_mode: ctx.night_mode,
                    use_global_background_keys: ctx.background_keys_read,
                    background_key_day: ctx.background_key_day,
                    background_key_night: ctx.background_key_night,
                    tile_day: ctx.tile_day,
                    tile_night: ctx.tile_night,
                    lang_pack_key: ctx.lang_pack_key,
                    languages_key: ctx.languages_key,
                    legacy_proxy: ctx.legacy_proxy,
                };

                Ok(Self {
                    tdata_path,
                    settings_salt: salt,
                    settings_key,
                    settings,
                    old_settings_version: file.version,
                })
            }
            Err(StorageError::NotFound(_)) => {
                let mut salt = vec![0u8; LOCAL_ENCRYPT_SALT_SIZE];
                random_bytes(&mut salt);
                let settings_key = create_legacy_local_key(b"", &salt);
                Ok(Self {
                    tdata_path,
                    settings_salt: salt,
                    settings_key,
                    settings: GlobalSettings::default(),
                    old_settings_version: 0,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// Path to `tdata`.
    #[must_use]
    pub fn tdata_path(&self) -> &Path {
        &self.tdata_path
    }

    /// Current global settings.
    #[must_use]
    pub fn settings(&self) -> &GlobalSettings {
        &self.settings
    }

    /// Mutable global settings (call [`Self::write_settings`] to persist).
    pub fn settings_mut(&mut self) -> &mut GlobalSettings {
        &mut self.settings
    }

    /// Version read from `tdata/settingss`.
    #[must_use]
    pub const fn old_settings_version(&self) -> i32 {
        self.old_settings_version
    }

    /// Write `tdata/settingss` (`Local::writeSettings`).
    pub fn write_settings(&self) -> Result<()> {
        let s = &self.settings;
        let mut w = QDataStreamWriter::new();
        w.write_u32(DbiBlockId::AutoStart as u32);
        w.write_i32(i32::from(s.auto_start));
        w.write_u32(DbiBlockId::StartMinimized as u32);
        w.write_i32(i32::from(s.start_minimized));
        w.write_u32(DbiBlockId::SendToMenu as u32);
        w.write_i32(i32::from(s.send_to_menu));
        w.write_u32(DbiBlockId::SeenTrayTooltip as u32);
        w.write_i32(i32::from(s.seen_tray_tooltip));
        w.write_u32(DbiBlockId::AutoUpdate as u32);
        w.write_i32(i32::from(s.auto_update));
        w.write_u32(DbiBlockId::LastUpdateCheck as u32);
        w.write_i32(s.last_update_check);
        w.write_u32(DbiBlockId::ScalePercent as u32);
        w.write_i32(s.scale_percent);
        w.write_u32(DbiBlockId::FallbackProductionConfig as u32);
        w.write_byte_array(&s.fallback_production_config);
        w.write_u32(DbiBlockId::ApplicationSettings as u32);
        w.write_byte_array(&s.application_settings);
        w.write_u32(DbiBlockId::DialogLastPath as u32);
        w.write_qstring(&s.dialog_last_path);
        w.write_u32(DbiBlockId::PowerSaving as u32);
        w.write_i32(s.power_saving);

        w.write_u32(DbiBlockId::ThemeKey as u32);
        w.write_u64(s.theme_key_day);
        w.write_u64(s.theme_key_night);
        w.write_u32(u32::from(s.night_mode));

        if s.use_global_background_keys {
            w.write_u32(DbiBlockId::BackgroundKey as u32);
            w.write_u64(s.background_key_day);
            w.write_u64(s.background_key_night);
            w.write_u32(DbiBlockId::TileBackground as u32);
            w.write_i32(i32::from(s.tile_day));
            w.write_i32(i32::from(s.tile_night));
        }
        if s.lang_pack_key != 0 {
            w.write_u32(DbiBlockId::LangPackKey as u32);
            w.write_u64(s.lang_pack_key);
        }
        if s.languages_key != 0 {
            w.write_u32(DbiBlockId::LanguagesKey as u32);
            w.write_u64(s.languages_key);
        }

        let mut file = FileWriteDescriptor::new("settings", &self.tdata_path);
        file.write_data(&self.settings_salt);
        file.write_encrypted(w.as_bytes(), &self.settings_key)?;
        file.finish()
    }

    /// Read serialized langpack blob (`Local::readLangPack`).
    pub fn read_lang_pack(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.read_lang_pack_with_version()?.map(|(data, _)| data))
    }

    /// Read serialized langpack blob together with the TDF header app version
    /// (`Local::readLangPack` -> `Lang::Instance::fillFromSerialized(data, langpack.version)`).
    pub fn read_lang_pack_with_version(&self) -> Result<Option<(Vec<u8>, i32)>> {
        if self.settings.lang_pack_key == 0 {
            return Ok(None);
        }
        let file = match read_encrypted_file_by_key(
            self.settings.lang_pack_key,
            &self.tdata_path,
            &self.settings_key,
        ) {
            Ok(f) => f,
            Err(StorageError::NotFound(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut r = file.stream();
        Ok(Some((r.read_byte_array()?, file.version)))
    }

    /// Write serialized langpack blob (`Local::writeLangPack`).
    pub fn write_lang_pack(&mut self, data: &[u8]) -> Result<()> {
        if self.settings.lang_pack_key == 0 {
            self.settings.lang_pack_key = generate_key(&self.tdata_path);
            self.write_settings()?;
        }
        let mut w = QDataStreamWriter::with_capacity(4 + data.len());
        w.write_byte_array(data);
        write_encrypted_file_by_key(
            self.settings.lang_pack_key,
            &self.tdata_path,
            w.as_bytes(),
            &self.settings_key,
        )
    }

    /// Push a language to the front of recent languages, deduplicating by `id` and ignoring
    /// custom/test IDs starting with `'#'` (`Local::pushRecentLanguage`).
    pub fn push_recent_language(&mut self, language: StoredLanguage) -> Result<()> {
        if language.id.starts_with('#') {
            return Ok(());
        }
        let mut list = self.read_recent_languages()?;
        list.retain(|v| v.id != language.id);
        list.insert(0, language);
        self.write_recent_languages(&list)
    }

    /// Remove a language by `id` from recent languages (`Local::removeRecentLanguage`).
    pub fn remove_recent_language(&mut self, id: &str) -> Result<()> {
        let mut list = self.read_recent_languages()?;
        list.retain(|v| v.id != id);
        self.write_recent_languages(&list)
    }

    /// Path to the unencrypted `tdata/countries` cache file (`Countries::Manager::_path`).
    #[must_use]
    pub fn countries_cache_path(&self) -> PathBuf {
        self.tdata_path.join("countries")
    }

    /// Read raw `tdata/countries` bytes (`Countries::Manager::read`), returning `Ok(None)` if absent.
    pub fn read_countries_cache(&self) -> Result<Option<Vec<u8>>> {
        let path = self.countries_cache_path();
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StorageError::Io(e)),
        }
    }

    /// Write raw `tdata/countries` bytes (`Countries::Manager::write`).
    pub fn write_countries_cache(&self, data: &[u8]) -> Result<()> {
        fs::create_dir_all(&self.tdata_path)?;
        fs::write(self.countries_cache_path(), data)?;
        Ok(())
    }

    /// Read recent languages list (`Local::readRecentLanguages`).
    pub fn read_recent_languages(&self) -> Result<Vec<StoredLanguage>> {
        if self.settings.languages_key == 0 {
            return Ok(Vec::new());
        }
        let file = read_encrypted_file_by_key(
            self.settings.languages_key,
            &self.tdata_path,
            &self.settings_key,
        )?;
        let mut r = file.stream();
        let count = r.read_i32()?;
        if count <= 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(r.capacity_for(count as usize, 20));
        for _ in 0..count {
            out.push(StoredLanguage {
                id: r.read_qstring()?,
                plural_id: r.read_qstring()?,
                base_id: r.read_qstring()?,
                name: r.read_qstring()?,
                native_name: r.read_qstring()?,
            });
        }
        Ok(out)
    }

    /// Write recent languages list (`Local::saveRecentLanguages`).
    pub fn write_recent_languages(&mut self, list: &[StoredLanguage]) -> Result<()> {
        if list.is_empty() {
            if self.settings.languages_key != 0 {
                clear_key(self.settings.languages_key, &self.tdata_path);
                self.settings.languages_key = 0;
                self.write_settings()?;
            }
            return Ok(());
        }
        if self.settings.languages_key == 0 {
            self.settings.languages_key = generate_key(&self.tdata_path);
            self.write_settings()?;
        }
        let mut w = QDataStreamWriter::new();
        w.write_i32(list.len() as i32);
        for lang in list {
            w.write_qstring(&lang.id);
            w.write_qstring(&lang.plural_id);
            w.write_qstring(&lang.base_id);
            w.write_qstring(&lang.name);
            w.write_qstring(&lang.native_name);
        }
        write_encrypted_file_by_key(
            self.settings.languages_key,
            &self.tdata_path,
            w.as_bytes(),
            &self.settings_key,
        )
    }

    /// Read theme for day or night mode (`Local::readThemeUsingKey`).
    pub fn read_theme(&self, night: bool) -> Result<Option<StoredTheme>> {
        let key = if night {
            self.settings.theme_key_night
        } else {
            self.settings.theme_key_day
        };
        if key == 0 {
            return Ok(None);
        }
        let file = read_encrypted_file_by_key(key, &self.tdata_path, &self.settings_key)?;
        let mut r = file.stream();

        let mut theme = StoredTheme {
            content: r.read_byte_array()?,
            ..Default::default()
        };
        let tag = r.read_qstring()?;
        theme.path_absolute = r.read_qstring()?;

        let mut field1 = 0i32;
        if tag == THEME_NEW_PATH_RELATIVE_TAG {
            theme.path_relative = r.read_qstring()?;
            theme.cloud_id = r.read_u64()?;
            theme.cloud_access_hash = r.read_u64()?;
            theme.cloud_slug = r.read_qstring()?;
            theme.cloud_title = r.read_qstring()?;
            theme.cloud_document_id = r.read_u64()?;
            field1 = r.read_i32()?;
        } else {
            theme.path_relative = tag.clone();
        }

        theme.cache_palette_checksum = r.read_i32()?;
        theme.cache_content_checksum = r.read_i32()?;
        theme.cache_colors = r.read_byte_array()?;
        theme.cache_background = r.read_byte_array()?;
        let field2 = r.read_u32()?;
        theme.cache_tiled = (field2 & 0xFF) == 1;

        if tag == THEME_NEW_PATH_RELATIVE_TAG {
            theme.cloud_created_by = ((u64::from(field2) >> 8) << 32) | u64::from(field1 as u32);
        }

        if !r.at_end() {
            let chat_theme_blob = r.read_byte_array()?;
            if !chat_theme_blob.is_empty() {
                let mut inner = QDataStreamReader::new(&chat_theme_blob);
                let emoticon = inner.read_qstring()?;
                let count = inner.read_i32()?;
                if (0..=4).contains(&count) {
                    let mut list = Vec::with_capacity(count as usize);
                    for _ in 0..count {
                        let type_id = inner.read_i32()?;
                        let accent_color = inner.read_color()?;
                        let has_out = inner.read_i32()?;
                        let outgoing_accent_color = if has_out != 0 {
                            Some(inner.read_color()?)
                        } else {
                            None
                        };
                        let colors_count = inner.read_i32()?;
                        if !(0..=8).contains(&colors_count) {
                            break;
                        }
                        let mut outgoing_messages_colors =
                            Vec::with_capacity(colors_count as usize);
                        for _ in 0..colors_count {
                            outgoing_messages_colors.push(inner.read_color()?);
                        }
                        let paper_serialized = inner.read_byte_array()?;
                        list.push((
                            type_id,
                            StoredCloudThemeSettings {
                                accent_color,
                                outgoing_accent_color,
                                outgoing_messages_colors,
                                paper_serialized,
                            },
                        ));
                    }
                    theme.cloud_emoticon = emoticon;
                    theme.cloud_settings = list;
                }
            }
        }

        Ok(Some(theme))
    }

    /// Write theme for day or night mode (`Local::writeTheme`).
    pub fn write_theme(&mut self, night: bool, theme: &StoredTheme) -> Result<()> {
        let key_slot = if night {
            &mut self.settings.theme_key_night
        } else {
            &mut self.settings.theme_key_day
        };
        if theme.content.is_empty() {
            if *key_slot != 0 {
                let old = *key_slot;
                *key_slot = 0;
                clear_key(old, &self.tdata_path);
                self.write_settings()?;
            }
            return Ok(());
        }
        if *key_slot == 0 {
            *key_slot = generate_key(&self.tdata_path);
            self.write_settings()?;
        }
        let key = if night {
            self.settings.theme_key_night
        } else {
            self.settings.theme_key_day
        };

        let mut chat_theme = Vec::new();
        if !theme.cloud_settings.is_empty() {
            let mut cw = QDataStreamWriter::new();
            cw.write_qstring(&theme.cloud_emoticon);
            cw.write_i32(theme.cloud_settings.len() as i32);
            for (type_id, s) in &theme.cloud_settings {
                cw.write_i32(*type_id);
                cw.write_color(s.accent_color);
                cw.write_i32(i32::from(s.outgoing_accent_color.is_some()));
                if let Some(c) = s.outgoing_accent_color {
                    cw.write_color(c);
                }
                cw.write_i32(s.outgoing_messages_colors.len() as i32);
                for &c in &s.outgoing_messages_colors {
                    cw.write_color(c);
                }
                cw.write_byte_array(&s.paper_serialized);
            }
            chat_theme = cw.into_bytes();
        }

        let field1 = (theme.cloud_created_by & 0xFFFF_FFFF) as u32 as i32;
        let field2 = u32::from(theme.cache_tiled) | (((theme.cloud_created_by >> 32) as u32) << 8);

        let mut w = QDataStreamWriter::new();
        w.write_byte_array(&theme.content);
        w.write_qstring(THEME_NEW_PATH_RELATIVE_TAG);
        w.write_qstring(&theme.path_absolute);
        w.write_qstring(&theme.path_relative);
        w.write_u64(theme.cloud_id);
        w.write_u64(theme.cloud_access_hash);
        w.write_qstring(&theme.cloud_slug);
        w.write_qstring(&theme.cloud_title);
        w.write_u64(theme.cloud_document_id);
        w.write_i32(field1);
        w.write_i32(theme.cache_palette_checksum);
        w.write_i32(theme.cache_content_checksum);
        w.write_byte_array(&theme.cache_colors);
        w.write_byte_array(&theme.cache_background);
        w.write_u32(field2);
        w.write_byte_array(&chat_theme);

        write_encrypted_file_by_key(key, &self.tdata_path, w.as_bytes(), &self.settings_key)
    }

    /// Read global background for day or night mode (`Local::readBackground`).
    pub fn read_background(&self, night: bool) -> Result<Option<StoredBackground>> {
        let key = if night {
            self.settings.background_key_night
        } else {
            self.settings.background_key_day
        };
        if key == 0 {
            return Ok(None);
        }
        let file = read_encrypted_file_by_key(key, &self.tdata_path, &self.settings_key)?;
        let mut r = file.stream();
        let tag = r.read_i32()?;
        let paper_serialized = if tag == WALLPAPER_SERIALIZE_TAG_ID {
            r.read_byte_array()?
        } else if tag == WALLPAPER_LEGACY_SERIALIZE_TAG_ID {
            // Preserve legacy header fields as empty modern blob
            let _id = r.read_u64()?;
            let _access_hash = r.read_u64()?;
            let _flags = r.read_u32()?;
            let _slug = r.read_qstring()?;
            Vec::new()
        } else {
            Vec::new()
        };
        let image_data = if !r.at_end() {
            r.read_byte_array()?
        } else {
            Vec::new()
        };
        Ok(Some(StoredBackground {
            paper_serialized,
            image_data,
        }))
    }

    /// Write global background for day or night mode (`Local::writeBackground`).
    pub fn write_background(&mut self, night: bool, bg: &StoredBackground) -> Result<()> {
        self.settings.use_global_background_keys = true;
        let key_slot = if night {
            &mut self.settings.background_key_night
        } else {
            &mut self.settings.background_key_day
        };
        if *key_slot == 0 {
            *key_slot = generate_key(&self.tdata_path);
            self.write_settings()?;
        }
        let key = if night {
            self.settings.background_key_night
        } else {
            self.settings.background_key_day
        };

        let mut w = QDataStreamWriter::new();
        w.write_i32(WALLPAPER_SERIALIZE_TAG_ID);
        w.write_byte_array(&bg.paper_serialized);
        w.write_byte_array(&bg.image_data);
        write_encrypted_file_by_key(key, &self.tdata_path, w.as_bytes(), &self.settings_key)
    }

    /// Read `tdata/prefix` autoupdate URL prefix (`Local::readAutoupdatePrefix`).
    #[must_use]
    pub fn read_autoupdate_prefix(&self) -> String {
        let raw = fs::read_to_string(self.tdata_path.join("prefix"))
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "https://td.telegram.org".to_string());
        raw.trim_end_matches('/').to_string()
    }

    /// Write `tdata/prefix` autoupdate URL prefix (`Local::writeAutoupdatePrefix`).
    pub fn write_autoupdate_prefix(&self, prefix: &str) -> Result<()> {
        fs::create_dir_all(&self.tdata_path)?;
        fs::write(self.tdata_path.join("prefix"), prefix.as_bytes())?;
        Ok(())
    }

    /// Write `tdata/update-manifest` (`64-byte signature || manifest JSON`) (`Local::writeUpdateManifest`).
    pub fn write_update_manifest(&self, manifest: &[u8], signature: &[u8; 64]) -> Result<()> {
        if manifest.is_empty() || manifest.len() > UPDATE_MANIFEST_MAX_SIZE {
            return Err(StorageError::CorruptData(
                "invalid update manifest size".into(),
            ));
        }
        fs::create_dir_all(&self.tdata_path)?;
        let mut combined = Vec::with_capacity(UPDATE_MANIFEST_SIGNATURE_SIZE + manifest.len());
        combined.extend_from_slice(signature);
        combined.extend_from_slice(manifest);
        fs::write(self.tdata_path.join("update-manifest"), combined)?;
        Ok(())
    }

    /// Read `tdata/update-manifest` returning `(manifest, signature)` (`Local::readUpdateManifest`).
    pub fn read_update_manifest(&self) -> Result<Option<(Vec<u8>, [u8; 64])>> {
        let path = self.tdata_path.join("update-manifest");
        if !path.is_file() {
            return Ok(None);
        }
        let bytes = fs::read(&path)?;
        if bytes.len() <= UPDATE_MANIFEST_SIGNATURE_SIZE
            || bytes.len() > UPDATE_MANIFEST_SIGNATURE_SIZE + UPDATE_MANIFEST_MAX_SIZE
        {
            return Ok(None);
        }
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&bytes[..64]);
        Ok(Some((bytes[64..].to_vec(), sig)))
    }
}

/// Read a legacy pre-TDF unencrypted/inline-encrypted `tdata/config` or `data_config` stream
/// (`_readOldSettings` / `_readOldUserSettingsFields` / `_readOldMtpDataFields`).
pub fn read_legacy_settings_stream(
    bytes: &[u8],
    existing_old_key: &mut Option<AuthKey>,
    context: &mut ReadSettingsContext,
) -> Result<()> {
    let mut r = QDataStreamReader::new(bytes);
    let mut version = 0i32;

    while !r.at_end() {
        let block_id = r.read_u32()?;
        if block_id == DbiBlockId::Version as u32 {
            version = r.read_i32()?;
            if version > APP_VERSION {
                return Err(StorageError::UnsupportedVersion(version));
            }
        } else if block_id == DbiBlockId::EncryptedWithSalt as u32 {
            let salt = r.read_byte_array()?;
            let encrypted = r.read_byte_array()?;
            if salt.len() != LOCAL_ENCRYPT_SALT_SIZE {
                continue;
            }
            let key = create_legacy_local_key(b"", &salt);
            if let Ok(inner) = decrypt_local(&encrypted, &key) {
                *existing_old_key = Some(key);
                read_legacy_settings_stream(&inner, existing_old_key, context)?;
            }
        } else if block_id == DbiBlockId::Encrypted as u32 {
            let encrypted = r.read_byte_array()?;
            if let Some(key) = existing_old_key.as_ref()
                && let Ok(inner) = decrypt_local(&encrypted, key)
            {
                read_legacy_settings_stream(&inner, existing_old_key, context)?;
            }
        } else {
            read_setting(block_id, &mut r, version, context)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn local_storage_settings_theme_lang_and_manifest_roundtrip() {
        let dir = TempDir::new().expect("tempdir");
        let mut local = LocalStorage::open_or_init(dir.path()).expect("init");

        local.settings_mut().auto_start = true;
        local.settings_mut().scale_percent = 150;
        local.settings_mut().dialog_last_path = "C:/Users/Test/Downloads".into();
        local.settings_mut().application_settings = b"app_settings_v1".to_vec();
        local.settings_mut().night_mode = true;
        local.write_settings().expect("write settings");
        assert!(dir.path().join("settingss").exists());

        // Langpack & recent languages
        local
            .write_lang_pack(b"langpack_bytes")
            .expect("write langpack");
        let langs = vec![StoredLanguage {
            id: "en".into(),
            plural_id: "en".into(),
            base_id: String::new(),
            name: "English".into(),
            native_name: "English".into(),
        }];
        local.write_recent_languages(&langs).expect("write langs");

        // Theme & background
        let theme = StoredTheme {
            content: b"windowBg: #ffffff;".to_vec(),
            path_relative: "themes/custom.tdesktop-palette".into(),
            path_absolute: "C:/themes/custom.tdesktop-palette".into(),
            cloud_id: 42,
            cloud_slug: "my-theme".into(),
            cloud_title: "My Theme".into(),
            cloud_created_by: 0x0001_0000_0005,
            cache_tiled: true,
            ..Default::default()
        };
        local.write_theme(true, &theme).expect("write theme");

        let bg = StoredBackground {
            paper_serialized: b"wallpaper_meta".to_vec(),
            image_data: vec![1, 0, 0, 0, 1, 0, 0, 0, 255, 0, 0, 255],
        };
        local.write_background(true, &bg).expect("write bg");

        // Prefix & update manifest
        local
            .write_autoupdate_prefix("https://updates.example.org/")
            .expect("prefix");
        assert_eq!(
            local.read_autoupdate_prefix(),
            "https://updates.example.org"
        );

        let sig = [0xEEu8; 64];
        local
            .write_update_manifest(br#"{"version":"1.0"}"#, &sig)
            .expect("manifest");
        assert_eq!(
            local.read_update_manifest().expect("read manifest"),
            Some((br#"{"version":"1.0"}"#.to_vec(), sig))
        );

        // Reopen from disk and verify everything
        let reopened = LocalStorage::open_or_init(dir.path()).expect("reopen");
        assert!(reopened.settings().auto_start);
        assert_eq!(reopened.settings().scale_percent, 150);
        assert_eq!(
            reopened.settings().dialog_last_path,
            "C:/Users/Test/Downloads"
        );
        assert_eq!(reopened.settings().application_settings, b"app_settings_v1");
        assert!(reopened.settings().night_mode);
        assert_eq!(
            reopened.read_lang_pack().expect("read langpack"),
            Some(b"langpack_bytes".to_vec())
        );
        assert_eq!(reopened.read_recent_languages().expect("read langs"), langs);
        assert_eq!(reopened.read_theme(true).expect("read theme"), Some(theme));
        assert_eq!(reopened.read_background(true).expect("read bg"), Some(bg));
    }

    /// A corrupt recent-languages count used to size a `Vec` directly and abort the process.
    #[test]
    fn huge_recent_languages_count_is_an_error_not_an_abort() {
        let dir = TempDir::new().expect("tempdir");
        let mut local = LocalStorage::open_or_init(dir.path()).expect("init");
        let langs = vec![StoredLanguage {
            id: "en".into(),
            plural_id: "en".into(),
            base_id: String::new(),
            name: "English".into(),
            native_name: "English".into(),
        }];
        local.write_recent_languages(&langs).expect("write langs");

        let key = local.settings.languages_key;
        let file = read_encrypted_file_by_key(key, &local.tdata_path, &local.settings_key)
            .expect("read raw");
        let mut payload = file.data.clone();
        assert_eq!(payload[..4], 1i32.to_be_bytes());
        payload[..4].copy_from_slice(&i32::MAX.to_be_bytes());
        write_encrypted_file_by_key(key, &local.tdata_path, &payload, &local.settings_key)
            .expect("write patched");

        assert!(local.read_recent_languages().is_err());
    }
}
