//! Language pack runtime instance, cloud diff application, local serialization, and events.
//!
//! Port of `Telegram/SourceFiles/lang/lang_instance.cpp` and `lang_instance.h`.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};

use thiserror::Error;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::auto::{
    COUNT_TAG_ID, KEYS_COUNT, get_key_index, get_original_value, get_tag_index, is_tag_replaced,
    key_ids, tags,
};
use crate::file_parser::{FileParser, FileParserError, read_lang_file};
use crate::keys::{default_language_id, language_id_or_default};
use crate::strings_parser::{
    PLURAL_PART_COUNT, PLURAL_PARTS, TEXT_COMMAND_CHAR, prepare_command_string,
};
use crate::tag::{PluralCount, PluralRule, plural_with_rule, replace_tag_str};
use tg_storage::qdatastream::{DataStreamError, QDataStreamReader, QDataStreamWriter};

/// Cloud lang pack identifier for Telegram Desktop (`"tdesktop"`).
pub const CLOUD_LANG_PACK_NAME: &str = "tdesktop";

/// Language ID used for user-loaded custom `.strings` files (`"#custom"`).
pub const CUSTOM_LANGUAGE_ID: &str = "#custom";

/// Special substring looked for in `lng_send_action_choose_sticker` for animated eyes.
pub const CHOOSING_STICKER_REPLACEMENT: &str = "oo";

const SERIALIZE_VERSION_TAG: &str = "#new";
const SERIALIZE_VERSION: i32 = 1;
const LANG_VALUES_LIMIT: i32 = 20_000;
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// Error returned by [`Instance`] operations.
#[derive(Debug, Error)]
pub enum LangError {
    /// Failed to decode a serialized language pack.
    #[error("corrupt serialized language pack: {0}")]
    DataStream(#[from] DataStreamError),
    /// Serialized format version is unsupported.
    #[error("unsupported langpack serialize version: {0}")]
    UnsupportedSerializeVersion(i32),
    /// Serialized non-default value count exceeded safety limit (`20000`).
    #[error("langpack values count limit exceeded: {0}")]
    ValuesLimitExceeded(i32),
    /// Received a diff whose `lang_code` does not match this pack.
    #[error("lang_code mismatch: expected '{expected}', got '{actual}'")]
    LangCodeMismatch {
        /// Language ID expected by this pack.
        expected: String,
        /// Language ID carried in the diff.
        actual: String,
    },
    /// Received a diff whose `from_version` is newer than the local pack version.
    #[error("diff from_version ({from_version}) exceeds current version ({current_version})")]
    VersionGap {
        /// Local pack version.
        current_version: i32,
        /// Diff starting version.
        from_version: i32,
    },
    /// Attempted to apply a diff to [`Pack::Base`] when no base pack is configured, or [`Pack::None`].
    #[error("invalid target pack for difference: {0:?}")]
    InvalidPack(Pack),
    /// Failed to read or decode a custom `.strings` file.
    #[error("custom lang file error: {0}")]
    CustomFile(#[from] FileParserError),
    /// Local storage I/O or decryption error.
    #[error("local storage error: {0}")]
    Storage(#[from] tg_storage::StorageError),
}

/// Identifies a language pack (matching `Lang::Language`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Language {
    /// Language code (e.g., `"en"`, `"ru"`, `"de"`, `"#custom"`).
    pub id: String,
    /// Plural rule language code (e.g., `"ru"`, `"en"`).
    pub plural_id: String,
    /// Optional base fallback language code (e.g., `"pt-br"` -> `"pt"`).
    pub base_id: String,
    /// English name of the language.
    pub name: String,
    /// Localized native name of the language.
    pub native_name: String,
}

impl Language {
    /// Default built-in English language descriptor (`Lang::DefaultLanguage`).
    pub fn default_english() -> Self {
        Self {
            id: default_language_id().to_string(),
            plural_id: String::new(),
            base_id: String::new(),
            name: "English".to_string(),
            native_name: "English".to_string(),
        }
    }

    /// Compare two language entries the way `Lang::operator==(Language, Language)` does
    /// (by `id` and `name`).
    pub fn same_listing_entry(&self, other: &Self) -> bool {
        self.id == other.id && self.name == other.name
    }
}

impl From<&Language> for tg_storage::local::StoredLanguage {
    fn from(lang: &Language) -> Self {
        Self {
            id: lang.id.clone(),
            plural_id: lang.plural_id.clone(),
            base_id: lang.base_id.clone(),
            name: lang.name.clone(),
            native_name: lang.native_name.clone(),
        }
    }
}

impl From<Language> for tg_storage::local::StoredLanguage {
    fn from(lang: Language) -> Self {
        Self {
            id: lang.id,
            plural_id: lang.plural_id,
            base_id: lang.base_id,
            name: lang.name,
            native_name: lang.native_name,
        }
    }
}

impl From<tg_storage::local::StoredLanguage> for Language {
    fn from(stored: tg_storage::local::StoredLanguage) -> Self {
        Self {
            id: stored.id,
            plural_id: stored.plural_id,
            base_id: stored.base_id,
            name: stored.name,
            native_name: stored.native_name,
        }
    }
}

/// Read recently used languages from `tdata` (`Local::readRecentLanguages`).
pub fn read_recent_languages(
    local: &tg_storage::LocalStorage,
) -> Result<Vec<Language>, tg_storage::StorageError> {
    Ok(local
        .read_recent_languages()?
        .into_iter()
        .map(Language::from)
        .collect())
}

/// Save recently used languages to `tdata` (`Local::saveRecentLanguages`).
pub fn save_recent_languages(
    local: &mut tg_storage::LocalStorage,
    list: &[Language],
) -> Result<(), tg_storage::StorageError> {
    let stored: Vec<tg_storage::local::StoredLanguage> = list.iter().map(Into::into).collect();
    local.write_recent_languages(&stored)
}

/// Push a language to the front of recent languages in `tdata` (`Local::pushRecentLanguage`).
pub fn push_recent_language(
    local: &mut tg_storage::LocalStorage,
    language: &Language,
) -> Result<(), tg_storage::StorageError> {
    local.push_recent_language(language.into())
}

/// Remove a language by `id` from recent languages in `tdata` (`Local::removeRecentLanguage`).
pub fn remove_recent_language(
    local: &mut tg_storage::LocalStorage,
    id: &str,
) -> Result<(), tg_storage::StorageError> {
    local.remove_recent_language(id)
}

/// Read recently used languages directly from a `tdata` directory path.
pub fn read_recent_languages_from_tdata(tdata_dir: &Path) -> Result<Vec<Language>, LangError> {
    let local = tg_storage::LocalStorage::open_or_init(tdata_dir)?;
    Ok(read_recent_languages(&local)?)
}

/// Save recently used languages directly to a `tdata` directory path.
pub fn save_recent_languages_to_tdata(
    tdata_dir: &Path,
    list: &[Language],
) -> Result<(), LangError> {
    let mut local = tg_storage::LocalStorage::open_or_init(tdata_dir)?;
    save_recent_languages(&mut local, list)?;
    Ok(())
}

/// Push a language to the front of recent languages in `tdata_dir`.
pub fn push_recent_language_to_tdata(
    tdata_dir: &Path,
    language: &Language,
) -> Result<(), LangError> {
    let mut local = tg_storage::LocalStorage::open_or_init(tdata_dir)?;
    push_recent_language(&mut local, language)?;
    Ok(())
}

/// Remove a language by `id` from recent languages in `tdata_dir`.
pub fn remove_recent_language_from_tdata(tdata_dir: &Path, id: &str) -> Result<(), LangError> {
    let mut local = tg_storage::LocalStorage::open_or_init(tdata_dir)?;
    remove_recent_language(&mut local, id)?;
    Ok(())
}

/// Full cloud language metadata (plain Rust equivalent of TL `langPackLanguage`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LangPackLanguageInfo {
    /// Whether this is an official Telegram translation.
    pub official: bool,
    /// Whether this language is right-to-left.
    pub rtl: bool,
    /// Whether this language is in beta.
    pub beta: bool,
    /// English language name.
    pub name: String,
    /// Native language name.
    pub native_name: String,
    /// Language code (`lang_code`).
    pub lang_code: String,
    /// Optional base language code (`base_lang_code`).
    pub base_lang_code: Option<String>,
    /// CLDR plural code (`plural_code`).
    pub plural_code: String,
    /// Total string count in the cloud pack.
    pub strings_count: i32,
    /// Translated string count in the cloud pack.
    pub translated_count: i32,
    /// URL to contribute translations.
    pub translations_url: String,
}

impl LangPackLanguageInfo {
    /// Convert cloud metadata into a [`Language`] descriptor (`Lang::ParseLanguage`).
    pub fn to_language(&self) -> Language {
        Language {
            id: self.lang_code.clone(),
            plural_id: self.plural_code.clone(),
            base_id: self.base_lang_code.clone().unwrap_or_default(),
            name: self.name.clone(),
            native_name: self.native_name.clone(),
        }
    }

    /// Translation completion percentage (`0..=100`), returning `0` if `strings_count <= 0`.
    pub fn translated_percent(&self) -> i32 {
        if self.strings_count <= 0 {
            0
        } else {
            self.translated_count.saturating_mul(100) / self.strings_count
        }
    }
}

/// A single cloud language pack string update (plain Rust equivalent of TL `LangPackString`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LangPackString {
    /// Non-plural string (`langPackString`).
    Normal {
        /// Phrase key.
        key: String,
        /// UTF-8 phrase value (with `{tag}` placeholders).
        value: String,
    },
    /// Pluralized string (`langPackStringPluralized`).
    Pluralized {
        /// Base phrase key (without `#...` suffix).
        key: String,
        /// Optional `#zero` variant.
        zero_value: Option<String>,
        /// Optional `#one` variant.
        one_value: Option<String>,
        /// Optional `#two` variant.
        two_value: Option<String>,
        /// Optional `#few` variant.
        few_value: Option<String>,
        /// Optional `#many` variant.
        many_value: Option<String>,
        /// Required `#other` variant.
        other_value: String,
    },
    /// Deleted string (`langPackStringDeleted`), resets the key (and all 6 plural suffixes).
    Deleted {
        /// Key to reset back to base/English default.
        key: String,
    },
}

/// A cloud language pack difference (plain Rust equivalent of TL `langPackDifference`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LangPackDifference {
    /// Language code this difference applies to.
    pub lang_code: String,
    /// Minimum local version required to apply this difference cleanly.
    pub from_version: i32,
    /// New version after applying this difference.
    pub version: i32,
    /// List of string additions, updates, and deletions.
    pub strings: Vec<LangPackString>,
}

/// Which pack slot a cloud update or request targets (`Lang::Pack`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pack {
    /// Neither current nor base pack.
    None,
    /// The active language pack.
    Current,
    /// The fallback base language pack.
    Base,
}

/// Events emitted by [`Instance`] when its language or phrases change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LangEvent {
    /// One or more phrase values changed (e.g., cloud diff applied or custom file loaded).
    Updated,
    /// Active language ID switched to the given ID.
    IdChanged(String),
}

/// Outcome returned by [`Instance::fill_from_serialized`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerializeLoadOutcome {
    /// Restored directly from the serialized blob.
    LoadedFromCache,
    /// Detected that the referenced custom `.strings` file changed on disk and reloaded it;
    /// caller should persist the refreshed pack back to storage.
    ReloadedModifiedCustomFile,
}

#[derive(Debug, Clone, Copy, Default)]
struct ChoosingStickerReplacement {
    support: bool,
    right_index: usize,
    right_index_named: usize,
}

/// Runtime language pack holding translated strings, base pack fallback, and plural rules.
///
/// Port of `Lang::Instance`.
#[derive(Debug)]
pub struct Instance {
    id: String,
    plural_id: String,
    name: String,
    native_name: String,
    custom_file_path_absolute: String,
    custom_file_path_relative: String,
    custom_file_content: Vec<u8>,
    version: i32,
    system_language: String,
    values: Vec<String>,
    non_default_set: Vec<bool>,
    non_default_values: BTreeMap<String, Vec<u8>>,
    base: Option<Box<Instance>>,
    plural_rule: PluralRule,
    choosing_sticker_replacement: ChoosingStickerReplacement,
    generation: u64,
    events: broadcast::Sender<LangEvent>,
}

impl Default for Instance {
    fn default() -> Self {
        Self::new()
    }
}

impl Instance {
    /// Create a new language pack initialized with built-in English defaults.
    pub fn new() -> Self {
        let values: Vec<String> = (0..KEYS_COUNT)
            .map(|i| get_original_value(i as u16).to_string())
            .collect();
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let mut instance = Self {
            id: String::new(),
            plural_id: String::new(),
            name: String::new(),
            native_name: String::new(),
            custom_file_path_absolute: String::new(),
            custom_file_path_relative: String::new(),
            custom_file_content: Vec::new(),
            version: 0,
            system_language: String::new(),
            values,
            non_default_set: vec![false; KEYS_COUNT],
            non_default_values: BTreeMap::new(),
            base: None,
            plural_rule: PluralRule::english_default(),
            choosing_sticker_replacement: ChoosingStickerReplacement::default(),
            generation: 0,
            events,
        };
        instance.update_choosing_sticker_replacement();
        instance
    }

    fn new_base() -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            id: String::new(),
            plural_id: String::new(),
            name: String::new(),
            native_name: String::new(),
            custom_file_path_absolute: String::new(),
            custom_file_path_relative: String::new(),
            custom_file_content: Vec::new(),
            version: 0,
            system_language: String::new(),
            values: Vec::new(),
            non_default_set: vec![false; KEYS_COUNT],
            non_default_values: BTreeMap::new(),
            base: None,
            plural_rule: PluralRule::english_default(),
            choosing_sticker_replacement: ChoosingStickerReplacement::default(),
            generation: 0,
            events,
        }
    }

    /// Monotonically increasing counter bumped whenever this pack's language or phrases change.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Copy the full language state (current + base pack, version, plural rule, values) from
    /// `other`, bumping this instance's generation and notifying subscribers.
    pub fn copy_from(&mut self, other: &Self) {
        let id_changed = self.id != other.id;
        self.id.clone_from(&other.id);
        self.plural_id.clone_from(&other.plural_id);
        self.name.clone_from(&other.name);
        self.native_name.clone_from(&other.native_name);
        self.custom_file_path_absolute
            .clone_from(&other.custom_file_path_absolute);
        self.custom_file_path_relative
            .clone_from(&other.custom_file_path_relative);
        self.custom_file_content
            .clone_from(&other.custom_file_content);
        self.version = other.version;
        self.system_language.clone_from(&other.system_language);
        self.values.clone_from(&other.values);
        self.non_default_set.clone_from(&other.non_default_set);
        self.non_default_values
            .clone_from(&other.non_default_values);
        self.base = other.base.as_deref().map(|b| {
            let mut copy = Box::new(Self::new_base());
            copy.id.clone_from(&b.id);
            copy.plural_id.clone_from(&b.plural_id);
            copy.name.clone_from(&b.name);
            copy.native_name.clone_from(&b.native_name);
            copy.version = b.version;
            copy.non_default_set.clone_from(&b.non_default_set);
            copy.non_default_values.clone_from(&b.non_default_values);
            copy.plural_rule = b.plural_rule;
            copy
        });
        self.plural_rule = other.plural_rule;
        self.choosing_sticker_replacement = other.choosing_sticker_replacement;
        self.generation = self.generation.wrapping_add(1);
        if id_changed {
            let _ = self.events.send(LangEvent::IdChanged(self.id.clone()));
        }
        let _ = self.events.send(LangEvent::Updated);
    }

    /// Reset the tracked cloud version for `pack` back to `0` (forcing a full `getLangPack`
    /// refetch on the next sync request).
    pub fn reset_version(&mut self, pack: Pack) {
        match pack {
            Pack::Current => {
                self.version = 0;
                self.generation = self.generation.wrapping_add(1);
            }
            Pack::Base => {
                if let Some(base) = self.base.as_deref_mut() {
                    base.version = 0;
                    self.generation = self.generation.wrapping_add(1);
                }
            }
            Pack::None => {}
        }
    }

    /// Subscribe to language switch and phrase update events.
    pub fn subscribe(&self) -> broadcast::Receiver<LangEvent> {
        self.events.subscribe()
    }

    /// Switch to the given language descriptor, resetting loaded strings to defaults.
    ///
    /// Special test languages `"#TEST_X"` and `"#TEST_0"` mask every non-whitespace character
    /// with `'X'` or `'0'` while preserving tag command markers (`PrepareTestValue`).
    pub fn switch_to_id(&mut self, data: &Language) {
        self.reset(data);
        if self.id == "#TEST_X" || self.id == "#TEST_0" {
            let filler = if self.id == "#TEST_X" { 'X' } else { '0' };
            for value in &mut self.values {
                *value = prepare_test_value(value, filler);
            }
            let _ = self.events.send(LangEvent::Updated);
        }
        self.update_plural_rules();
    }

    /// Load a custom `.strings` file from disk and emit [`LangEvent::Updated`].
    pub fn switch_to_custom_file(&mut self, file_path: &Path) -> Result<bool, LangError> {
        if self.load_from_custom_file(file_path)? {
            let _ = self.events.send(LangEvent::Updated);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Load a custom `.strings` pack directly from in-memory UTF-8 content.
    pub fn switch_to_custom_content(
        &mut self,
        absolute_path: &str,
        relative_path: &str,
        content: &[u8],
    ) {
        let plural_code = plural_code_for_custom(absolute_path, relative_path);
        self.reset(&Language {
            id: CUSTOM_LANGUAGE_ID.to_string(),
            plural_id: plural_code,
            base_id: String::new(),
            name: String::new(),
            native_name: String::new(),
        });
        self.load_from_custom_content(absolute_path, relative_path, content);
        self.update_plural_rules();
        let _ = self.events.send(LangEvent::Updated);
    }

    /// Set the cached system language code (usually provided by `tg-platform`).
    pub fn set_system_lang_code(&mut self, code: impl Into<String>) {
        self.system_language = code.into();
    }

    /// System language code, falling back to `"en"` if none was detected or set.
    pub fn system_lang_code(&self) -> &str {
        if self.system_language.is_empty() {
            default_language_id()
        } else {
            &self.system_language
        }
    }

    /// Cloud lang pack name (`""` for custom/test packs, `"tdesktop"` otherwise).
    pub fn lang_pack_name(&self) -> &'static str {
        if self.is_custom() {
            ""
        } else {
            CLOUD_LANG_PACK_NAME
        }
    }

    /// Language code to send to Telegram's cloud langpack API for `pack`.
    pub fn cloud_lang_code(&self, pack: Pack) -> &str {
        if self.is_custom() || self.id.is_empty() {
            default_language_id()
        } else {
            self.id_for_pack(pack)
        }
    }

    /// Current language ID (`""` before any explicit switch, or `"en"`, `"ru"`, etc.).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Language ID for the requested [`Pack`].
    pub fn id_for_pack(&self, pack: Pack) -> &str {
        match pack {
            Pack::Base => self.base.as_deref().map_or("", |b| b.id.as_str()),
            Pack::Current | Pack::None => &self.id,
        }
    }

    /// Base fallback language ID, or `""` if none.
    pub fn base_id(&self) -> &str {
        self.id_for_pack(Pack::Base)
    }

    /// Effective plural rule language ID.
    pub fn plural_id(&self) -> &str {
        &self.plural_id
    }

    /// Active [`PluralRule`] for this instance.
    pub fn plural_rule(&self) -> PluralRule {
        self.plural_rule
    }

    /// Display name (falls back to `lng_language_name` if unset).
    pub fn name(&self) -> &str {
        if self.name.is_empty() {
            self.get_value(key_ids::lng_language_name)
        } else {
            &self.name
        }
    }

    /// Native display name (falls back to `lng_language_name` if unset).
    pub fn native_name(&self) -> &str {
        if self.native_name.is_empty() {
            self.get_value(key_ids::lng_language_name)
        } else {
            &self.native_name
        }
    }

    /// Whether this instance is using a local custom file or test pseudo-language.
    pub fn is_custom(&self) -> bool {
        self.id == CUSTOM_LANGUAGE_ID || self.id == "#TEST_X" || self.id == "#TEST_0"
    }

    /// Current cloud version for `pack`.
    pub fn version(&self, pack: Pack) -> i32 {
        match pack {
            Pack::Base => self.base.as_deref().map_or(0, |b| b.version),
            Pack::Current | Pack::None => self.version,
        }
    }

    /// Whether the active pack's sticker-choosing phrases contain `"oo"` for animated eyes.
    pub fn support_choosing_sticker_replacement(&self) -> bool {
        self.choosing_sticker_replacement.support
    }

    /// Right-hand char offset for animated eyes in sticker-choosing action text.
    pub fn right_index_choosing_sticker_replacement(&self, named: bool) -> usize {
        if named {
            self.choosing_sticker_replacement.right_index_named
        } else {
            self.choosing_sticker_replacement.right_index
        }
    }

    /// Raw phrase template (with encoded tag markers) for `key`.
    ///
    /// Falls back to built-in English if `key` is out of range (never panics).
    pub fn get_value(&self, key: u16) -> &str {
        self.values
            .get(usize::from(key))
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Look up a raw non-default value (including `cloud_*` keys not in `lang.strings`),
    /// cascading to the base pack (`Instance::getNonDefaultValue`).
    pub fn get_non_default_value(&self, key: &str) -> Option<String> {
        if let Some(bytes) = self.non_default_values.get(key) {
            return Some(String::from_utf8_lossy(bytes).into_owned());
        }
        self.base
            .as_deref()
            .and_then(|b| b.get_non_default_value(key))
    }

    /// Whether any of the 6 plural variants starting at `key_base` has a non-default translation
    /// in this pack or its base pack (`Instance::isNonDefaultPlural`).
    pub fn is_non_default_plural(&self, key_base: u16) -> bool {
        let base_idx = usize::from(key_base);
        if base_idx + PLURAL_PART_COUNT > self.non_default_set.len() {
            return false;
        }
        if self.non_default_set[base_idx..base_idx + PLURAL_PART_COUNT]
            .iter()
            .any(|&set| set)
        {
            return true;
        }
        self.base
            .as_deref()
            .is_some_and(|b| b.is_non_default_plural(key_base))
    }

    /// Format a plain phrase with no placeholders.
    pub fn format_plain(&self, key: u16) -> String {
        self.get_value(key).to_string()
    }

    /// Format a non-plural phrase with `(tag_id, replacement)` substitutions.
    pub fn format_tagged(&self, key: u16, replacements: &[(u16, &str)]) -> String {
        let mut out = self.get_value(key).to_string();
        for &(tag_id, value) in replacements {
            out = replace_tag_str(out, tag_id, value);
        }
        out
    }

    /// Format a plural phrase with `count` and any additional `(tag_id, replacement)` pairs.
    pub fn format_plural(
        &self,
        key_base: u16,
        count: PluralCount,
        extra_tags: &[(u16, &str)],
    ) -> String {
        let use_non_default = self.plural_rule != PluralRule::english_default()
            && self.is_non_default_plural(key_base);
        let plural = plural_with_rule(self.plural_rule, use_non_default, count.value, count.format);
        let selected_key = key_base.saturating_add(plural.key_shift as u16);
        let mut out = replace_tag_str(
            self.get_value(selected_key).to_string(),
            COUNT_TAG_ID,
            &plural.replacement,
        );
        for &(tag_id, value) in extra_tags {
            out = replace_tag_str(out, tag_id, value);
        }
        out
    }

    /// Apply a cloud [`LangPackDifference`] to either [`Pack::Current`] or [`Pack::Base`].
    ///
    /// When `difference.from_version == 0` and the target pack already had a non-zero version
    /// (i.e. a full `langpack.getLangPack` refetch replacing stale state), existing overrides for
    /// that pack are cleared cleanly before applying the new strings.
    pub fn apply_difference(
        &mut self,
        pack: Pack,
        difference: &LangPackDifference,
    ) -> Result<(), LangError> {
        match pack {
            Pack::Current => {
                self.validate_diff(difference)?;
                if difference.from_version == 0 && self.version > 0 {
                    self.clear_current_overrides_to_base();
                }
                self.version = difference.version;
                for item in &difference.strings {
                    handle_lang_pack_string(item, |k, v| match v {
                        Some(bytes) => self.apply_value_current(k, bytes),
                        None => self.reset_value_current(k),
                    });
                }
                self.generation = self.generation.wrapping_add(1);
                let _ = self.events.send(LangEvent::Updated);
                Ok(())
            }
            Pack::Base => {
                let mut base = self.base.take().ok_or(LangError::InvalidPack(Pack::Base))?;
                let res = base.validate_diff(difference);
                if let Err(e) = res {
                    self.base = Some(base);
                    return Err(e);
                }
                if difference.from_version == 0 && base.version > 0 {
                    base.non_default_values.clear();
                    base.non_default_set.fill(false);
                    for (idx, slot) in self.values.iter_mut().enumerate() {
                        if !self.non_default_set[idx] {
                            *slot = get_original_value(idx as u16).to_string();
                        }
                    }
                    self.update_choosing_sticker_replacement();
                }
                base.version = difference.version;
                for item in &difference.strings {
                    handle_lang_pack_string(item, |k, v| match v {
                        Some(bytes) => self.apply_value_into_base(&mut base, k, bytes),
                        None => self.reset_value_in_base(&mut base, k),
                    });
                }
                self.base = Some(base);
                self.generation = self.generation.wrapping_add(1);
                let _ = self.events.send(LangEvent::Updated);
                Ok(())
            }
            Pack::None => Err(LangError::InvalidPack(Pack::None)),
        }
    }

    fn clear_current_overrides_to_base(&mut self) {
        self.non_default_values.clear();
        self.non_default_set.fill(false);
        for (idx, slot) in self.values.iter_mut().enumerate() {
            *slot = get_original_value(idx as u16).to_string();
        }
        if let Some(base) = self.base.as_deref() {
            let base_pairs: Vec<(String, Vec<u8>)> = base
                .non_default_values
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (k, v) in base_pairs {
                if let Some((idx, parsed)) = parse_key_value(&k, &v) {
                    self.values[usize::from(idx)] = parsed;
                }
            }
        }
        self.update_choosing_sticker_replacement();
    }

    /// Parse a list of [`LangPackString`] entries into a map of `key_index -> parsed_value`
    /// (`Instance::ParseStrings`).
    pub fn parse_strings(strings: &[LangPackString]) -> BTreeMap<u16, String> {
        let mut result = BTreeMap::new();
        for item in strings {
            handle_lang_pack_string(item, |key, value| match value {
                Some(bytes) => {
                    if let Some((idx, parsed)) = parse_key_value(key, bytes) {
                        result.insert(idx, parsed);
                    }
                }
                None => {
                    if let Some(idx) = get_key_index(key) {
                        result.remove(&idx);
                    }
                }
            });
        }
        result
    }

    /// Serialize this language pack to Qt 5 `QDataStream` binary format, wire-compatible
    /// with `Instance::serialize` in `lang_instance.cpp`.
    pub fn serialize(&self) -> Vec<u8> {
        let base_bytes = self.base.as_deref().map_or_else(Vec::new, Self::serialize);
        let mut writer = QDataStreamWriter::with_capacity(256 + self.non_default_values.len() * 64);
        writer.write_qstring(SERIALIZE_VERSION_TAG);
        writer.write_i32(SERIALIZE_VERSION);
        writer.write_qstring(&self.id);
        writer.write_qstring(&self.plural_id);
        writer.write_qstring(&self.name);
        writer.write_qstring(&self.native_name);
        writer.write_i32(self.version);
        writer.write_qstring(&self.custom_file_path_absolute);
        writer.write_qstring(&self.custom_file_path_relative);
        writer.write_byte_array(&self.custom_file_content);
        writer.write_i32(self.non_default_values.len() as i32);
        for (key, value) in &self.non_default_values {
            writer.write_byte_array(key.as_bytes());
            writer.write_byte_array(value);
        }
        writer.write_byte_array(&base_bytes);
        writer.into_bytes()
    }

    /// Persist this language pack into `tdata` (`Local::writeLangPack` in `storage/localstorage.cpp`),
    /// allocating `dbiLangPackKey` in `tdata/settingss` if needed and encrypting with `SettingsKey`.
    pub fn save_to_local_storage(
        &self,
        local: &mut tg_storage::LocalStorage,
    ) -> Result<(), LangError> {
        local.write_lang_pack(&self.serialize())?;
        Ok(())
    }

    /// Load this language pack from `tdata` (`Local::readLangPack` in `storage/localstorage.cpp`),
    /// returning `Ok(None)` if no langpack is stored yet.
    pub fn load_from_local_storage(
        &mut self,
        local: &tg_storage::LocalStorage,
    ) -> Result<Option<SerializeLoadOutcome>, LangError> {
        let Some((data, version)) = local.read_lang_pack_with_version()? else {
            return Ok(None);
        };
        let outcome = self.fill_from_serialized(&data, version)?;
        Ok(Some(outcome))
    }

    /// Persist this language pack directly into a `tdata` directory path.
    pub fn save_to_tdata_dir(&self, tdata_dir: &Path) -> Result<(), LangError> {
        let mut local = tg_storage::LocalStorage::open_or_init(tdata_dir)?;
        self.save_to_local_storage(&mut local)
    }

    /// Load this language pack directly from a `tdata` directory path.
    ///
    /// If a referenced custom `.strings` file changed on disk, persists the refreshed pack back
    /// to `tdata` (matching `Instance::fillFromSerialized` in `lang_instance.cpp:486`).
    pub fn load_from_tdata_dir(
        &mut self,
        tdata_dir: &Path,
    ) -> Result<Option<SerializeLoadOutcome>, LangError> {
        let mut local = tg_storage::LocalStorage::open_or_init(tdata_dir)?;
        let outcome = self.load_from_local_storage(&local)?;
        if outcome == Some(SerializeLoadOutcome::ReloadedModifiedCustomFile) {
            self.save_to_local_storage(&mut local)?;
        }
        Ok(outcome)
    }

    /// Restore this language pack from bytes produced by [`Self::serialize`] (or legacy tdesktop
    /// format without the `"#new"` header).
    ///
    /// Port of `Instance::fillFromSerialized`.
    pub fn fill_from_serialized(
        &mut self,
        data: &[u8],
        data_app_version: i32,
    ) -> Result<SerializeLoadOutcome, LangError> {
        self.fill_from_serialized_inner(data, data_app_version, false)
    }

    fn fill_from_serialized_inner(
        &mut self,
        data: &[u8],
        _data_app_version: i32,
        is_base: bool,
    ) -> Result<SerializeLoadOutcome, LangError> {
        let mut reader = QDataStreamReader::new(data);
        let serialize_version_tag = reader.read_qstring()?;
        let legacy_format = serialize_version_tag != SERIALIZE_VERSION_TAG;

        let mut id;
        let mut plural_id = String::new();
        let mut name = String::new();
        let mut native_name = String::new();
        let version;
        let custom_abs;
        let custom_rel;
        let custom_content;
        let non_default_count;

        if legacy_format {
            id = serialize_version_tag;
            version = reader.read_i32()?;
            custom_abs = reader.read_qstring()?;
            custom_rel = reader.read_qstring()?;
            custom_content = reader.read_byte_array()?;
            non_default_count = reader.read_i32()?;
        } else {
            let serialize_version = reader.read_i32()?;
            if serialize_version != SERIALIZE_VERSION {
                return Err(LangError::UnsupportedSerializeVersion(serialize_version));
            }
            id = reader.read_qstring()?;
            plural_id = reader.read_qstring()?;
            name = reader.read_qstring()?;
            native_name = reader.read_qstring()?;
            version = reader.read_i32()?;
            custom_abs = reader.read_qstring()?;
            custom_rel = reader.read_qstring()?;
            custom_content = reader.read_byte_array()?;
            non_default_count = reader.read_i32()?;
        }

        if !(0..=LANG_VALUES_LIMIT).contains(&non_default_count) {
            return Err(LangError::ValuesLimitExceeded(non_default_count));
        }

        if !is_base && !custom_abs.is_empty() {
            id = CUSTOM_LANGUAGE_ID.to_string();
            let rel_opt = (!custom_rel.is_empty()).then(|| Path::new(custom_rel.as_str()));
            if let Ok(current_content) = read_lang_file(Path::new(&custom_abs), rel_opt)
                && !current_content.is_empty()
                && current_content != custom_content
            {
                self.fill_from_custom_content(&custom_abs, &custom_rel, &current_content);
                return Ok(SerializeLoadOutcome::ReloadedModifiedCustomFile);
            }
        }

        let count_usize = non_default_count as usize;
        let mut non_default_pairs: Vec<(String, Vec<u8>)> = Vec::with_capacity(count_usize);
        for _ in 0..count_usize {
            let key_bytes = reader.read_byte_array()?;
            let val_bytes = reader.read_byte_array()?;
            let key = String::from_utf8_lossy(&key_bytes).into_owned();
            non_default_pairs.push((key, val_bytes));
        }

        let base_bytes;
        if legacy_format {
            if !reader.at_end() {
                plural_id = reader.read_qstring()?;
            } else {
                plural_id = id.clone();
            }
            if !reader.at_end() {
                base_bytes = reader.read_byte_array()?;
                if base_bytes.is_empty() {
                    return Err(LangError::DataStream(DataStreamError::CorruptString));
                }
            } else {
                base_bytes = Vec::new();
            }
        } else {
            base_bytes = reader.read_byte_array()?;
        }

        // Reset current buffers cleanly before applying base + own values.
        self.base = None;
        self.non_default_values.clear();
        self.non_default_set.fill(false);
        if !is_base {
            for (idx, slot) in self.values.iter_mut().enumerate() {
                *slot = get_original_value(idx as u16).to_string();
            }
        }

        if !base_bytes.is_empty() {
            let mut base = Box::new(Self::new_base());
            base.fill_from_serialized_inner(&base_bytes, _data_app_version, true)?;
            if !is_base {
                // Cascade loaded base values into derived before applying own overrides.
                let base_pairs: Vec<(String, Vec<u8>)> = base
                    .non_default_values
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                for (k, v) in base_pairs {
                    if let Some((idx, parsed)) = parse_key_value(&k, &v) {
                        self.values[usize::from(idx)] = parsed;
                    }
                }
            }
            self.base = Some(base);
        }

        self.id = id.clone();
        self.plural_id = if id == CUSTOM_LANGUAGE_ID {
            plural_code_for_custom(&custom_abs, &custom_rel)
        } else {
            plural_id
        };
        self.name = name;
        self.native_name = native_name;
        self.version = version;
        self.custom_file_path_absolute = custom_abs;
        self.custom_file_path_relative = custom_rel;
        self.custom_file_content = custom_content;

        info!(keys = non_default_count, "Loaded cached langpack");
        for (k, v) in non_default_pairs {
            if is_base {
                self.non_default_values.insert(k.clone(), v.clone());
                if let Some((idx, _)) = parse_key_value(&k, &v) {
                    self.non_default_set[usize::from(idx)] = true;
                }
            } else {
                self.apply_value_current(&k, &v);
            }
        }

        if !is_base {
            self.update_plural_rules();
            self.update_choosing_sticker_replacement();
            self.generation = self.generation.wrapping_add(1);
            let _ = self.events.send(LangEvent::IdChanged(self.id.clone()));
        }

        Ok(SerializeLoadOutcome::LoadedFromCache)
    }

    fn validate_diff(&self, diff: &LangPackDifference) -> Result<(), LangError> {
        let expected = language_id_or_default(&self.id);
        if expected != diff.lang_code {
            return Err(LangError::LangCodeMismatch {
                expected: expected.to_string(),
                actual: diff.lang_code.clone(),
            });
        }
        if diff.from_version > self.version {
            return Err(LangError::VersionGap {
                current_version: self.version,
                from_version: diff.from_version,
            });
        }
        Ok(())
    }

    fn set_base_id(&mut self, base_id: &str, plural_id: &str) {
        if base_id.is_empty() {
            self.base = None;
        } else {
            let base = self.base.get_or_insert_with(|| Box::new(Self::new_base()));
            base.reset_base(&Language {
                id: base_id.to_string(),
                plural_id: plural_id.to_string(),
                base_id: String::new(),
                name: String::new(),
                native_name: String::new(),
            });
        }
    }

    fn reset_base(&mut self, data: &Language) {
        self.base = None;
        self.id = language_id_or_default(&data.id).to_string();
        self.plural_id = if !data.plural_id.is_empty() {
            data.plural_id.clone()
        } else {
            data.id.clone()
        };
        self.name = data.name.clone();
        self.native_name = data.native_name.clone();
        self.custom_file_path_absolute.clear();
        self.custom_file_path_relative.clear();
        self.custom_file_content.clear();
        self.version = 0;
        self.non_default_values.clear();
        self.non_default_set.fill(false);
        self.plural_rule = PluralRule::for_language(&self.plural_id);
    }

    fn reset(&mut self, data: &Language) {
        let computed_plural_id = if !data.plural_id.is_empty() {
            data.plural_id.clone()
        } else if !data.base_id.is_empty() {
            data.base_id.clone()
        } else {
            data.id.clone()
        };
        self.set_base_id(&data.base_id, &computed_plural_id);
        self.id = language_id_or_default(&data.id).to_string();
        self.plural_id = computed_plural_id;
        self.name = data.name.clone();
        self.native_name = data.native_name.clone();

        self.custom_file_path_absolute.clear();
        self.custom_file_path_relative.clear();
        self.custom_file_content.clear();
        self.version = 0;
        self.non_default_values.clear();
        for (i, slot) in self.values.iter_mut().enumerate() {
            *slot = get_original_value(i as u16).to_string();
        }
        self.non_default_set.fill(false);
        self.update_choosing_sticker_replacement();
        self.generation = self.generation.wrapping_add(1);
        let _ = self.events.send(LangEvent::IdChanged(self.id.clone()));
    }

    fn fill_from_custom_content(
        &mut self,
        absolute_path: &str,
        relative_path: &str,
        content: &[u8],
    ) {
        self.set_base_id("", "");
        self.id = CUSTOM_LANGUAGE_ID.to_string();
        self.plural_id = plural_code_for_custom(absolute_path, relative_path);
        self.name.clear();
        self.native_name.clear();
        self.non_default_values.clear();
        self.non_default_set.fill(false);
        for (i, slot) in self.values.iter_mut().enumerate() {
            *slot = get_original_value(i as u16).to_string();
        }
        self.load_from_custom_content(absolute_path, relative_path, content);
        self.update_plural_rules();
        self.update_choosing_sticker_replacement();
        self.generation = self.generation.wrapping_add(1);
        let _ = self.events.send(LangEvent::IdChanged(self.id.clone()));
    }

    fn load_from_custom_content(
        &mut self,
        absolute_path: &str,
        relative_path: &str,
        content: &[u8],
    ) {
        self.version = 0;
        self.custom_file_path_absolute = absolute_path.to_string();
        self.custom_file_path_relative = relative_path.to_string();
        self.custom_file_content = content.to_vec();
        self.load_from_content(content);
    }

    fn load_from_custom_file(&mut self, file_path: &Path) -> Result<bool, LangError> {
        let abs = file_path
            .canonicalize()
            .unwrap_or_else(|_| file_path.to_path_buf());
        let abs_str = abs.to_string_lossy().into_owned();
        let rel_str = file_path.to_string_lossy().into_owned();
        let content = read_lang_file(&abs, Some(file_path))?;
        if content.is_empty() {
            return Ok(false);
        }
        let plural_code = plural_code_for_custom(&abs_str, &rel_str);
        self.reset(&Language {
            id: CUSTOM_LANGUAGE_ID.to_string(),
            plural_id: plural_code,
            base_id: String::new(),
            name: String::new(),
            native_name: String::new(),
        });
        self.load_from_custom_content(&abs_str, &rel_str, &content);
        self.update_plural_rules();
        Ok(true)
    }

    fn load_from_content(&mut self, content: &[u8]) {
        let mut pairs: Vec<(String, Vec<u8>)> = Vec::new();
        let loader = FileParser::parse_content(content, |key, val| {
            pairs.push((key.to_string(), val.to_vec()));
        });
        if !loader.errors_list().is_empty() {
            warn!("Lang load errors: {}", loader.errors());
        } else if !loader.warnings().is_empty() {
            warn!("Lang load warnings: {}", loader.warnings());
        }
        for (k, v) in pairs {
            self.apply_value_current(&k, &v);
        }
    }

    fn apply_value_current(&mut self, key: &str, value: &[u8]) {
        self.non_default_values
            .insert(key.to_string(), value.to_vec());
        if let Some((idx, parsed)) = parse_key_value(key, value) {
            let i = usize::from(idx);
            self.non_default_set[i] = true;
            self.values[i] = parsed;
            if idx == key_ids::lng_send_action_choose_sticker
                || idx == key_ids::lng_user_action_choose_sticker
            {
                self.update_choosing_sticker_replacement();
            }
        }
    }

    fn apply_value_into_base(&mut self, base: &mut Instance, key: &str, value: &[u8]) {
        base.non_default_values
            .insert(key.to_string(), value.to_vec());
        if let Some((idx, parsed)) = parse_key_value(key, value) {
            let i = usize::from(idx);
            base.non_default_set[i] = true;
            if !self.non_default_set[i] {
                self.values[i] = parsed;
                if idx == key_ids::lng_send_action_choose_sticker
                    || idx == key_ids::lng_user_action_choose_sticker
                {
                    self.update_choosing_sticker_replacement();
                }
            }
        }
    }

    fn reset_value_current(&mut self, key: &str) {
        self.non_default_values.remove(key);
        if let Some(idx) = get_key_index(key) {
            let i = usize::from(idx);
            self.non_default_set[i] = false;
            let fallback = self
                .base
                .as_deref()
                .and_then(|b| b.non_default_values.get(key))
                .and_then(|raw| parse_key_value(key, raw))
                .map(|(_, parsed)| parsed)
                .filter(|parsed| !parsed.is_empty())
                .unwrap_or_else(|| get_original_value(idx).to_string());
            self.values[i] = fallback;
            if idx == key_ids::lng_send_action_choose_sticker
                || idx == key_ids::lng_user_action_choose_sticker
            {
                self.update_choosing_sticker_replacement();
            }
        }
    }

    fn reset_value_in_base(&mut self, base: &mut Instance, key: &str) {
        base.non_default_values.remove(key);
        if let Some(idx) = get_key_index(key) {
            let i = usize::from(idx);
            base.non_default_set[i] = false;
            if !self.non_default_set[i] {
                self.values[i] = get_original_value(idx).to_string();
                if idx == key_ids::lng_send_action_choose_sticker
                    || idx == key_ids::lng_user_action_choose_sticker
                {
                    self.update_choosing_sticker_replacement();
                }
            }
        }
    }

    fn update_plural_rules(&mut self) {
        if self.plural_id.is_empty() {
            self.plural_id = if self.is_custom() {
                plural_code_for_custom(
                    &self.custom_file_path_absolute,
                    &self.custom_file_path_relative,
                )
            } else {
                language_id_or_default(&self.id).to_string()
            };
        }
        self.plural_rule = PluralRule::for_language(&self.plural_id);
    }

    fn update_choosing_sticker_replacement(&mut self) {
        let phrase = self.format_plain(key_ids::lng_send_action_choose_sticker);
        let first = phrase.find(CHOOSING_STICKER_REPLACEMENT);
        let phrase_named =
            self.format_tagged(key_ids::lng_user_action_choose_sticker, &[(tags::USER, "")]);
        let first_named = phrase_named.find(CHOOSING_STICKER_REPLACEMENT);

        match (first, first_named) {
            (Some(f), Some(fnamed)) => {
                let phrase_chars = phrase.chars().count();
                let f_chars = phrase[..f].chars().count();
                let named_chars = phrase_named.chars().count();
                let fnamed_chars = phrase_named[..fnamed].chars().count();
                self.choosing_sticker_replacement = ChoosingStickerReplacement {
                    support: true,
                    right_index: phrase_chars - f_chars,
                    right_index_named: named_chars - fnamed_chars,
                };
            }
            _ => {
                self.choosing_sticker_replacement = ChoosingStickerReplacement::default();
            }
        }
    }
}

fn handle_lang_pack_string<F>(item: &LangPackString, mut on_entry: F)
where
    F: FnMut(&str, Option<&[u8]>),
{
    match item {
        LangPackString::Normal { key, value } => {
            on_entry(key, Some(value.as_bytes()));
        }
        LangPackString::Pluralized {
            key,
            zero_value,
            one_value,
            two_value,
            few_value,
            many_value,
            other_value,
        } => {
            let variants: [(&str, &str); PLURAL_PART_COUNT] = [
                ("zero", zero_value.as_deref().unwrap_or("")),
                ("one", one_value.as_deref().unwrap_or("")),
                ("two", two_value.as_deref().unwrap_or("")),
                ("few", few_value.as_deref().unwrap_or("")),
                ("many", many_value.as_deref().unwrap_or("")),
                ("other", other_value.as_str()),
            ];
            for (suffix, val) in variants {
                let full_key = format!("{key}#{suffix}");
                on_entry(&full_key, Some(val.as_bytes()));
            }
        }
        LangPackString::Deleted { key } => {
            on_entry(key, None);
            for suffix in PLURAL_PARTS {
                let full_key = format!("{key}#{suffix}");
                on_entry(&full_key, None);
            }
        }
    }
}

/// Validate and convert `{tag}` placeholders inside a cloud/custom value into encoded command markers.
///
/// Port of `ValueParser` and `ParseKeyValue` in `lang_instance.cpp`.
pub fn parse_key_value(key: &str, value: &[u8]) -> Option<(u16, String)> {
    let Some(key_index) = get_key_index(key) else {
        if !key.starts_with("cloud_") {
            debug!("Lang Warning: Unknown key '{key}'");
        }
        return None;
    };
    parse_value_for_key(key, key_index, value).map(|parsed| (key_index, parsed))
}

fn parse_value_for_key(key: &str, key_index: u16, value: &[u8]) -> Option<String> {
    let mut result = String::with_capacity(value.len());
    let mut used_tags: HashSet<u16> = HashSet::new();
    let mut begin = 0usize;
    let mut pos = 0usize;

    while pos < value.len() {
        if value[pos] == b'{' {
            if pos > begin {
                result.push_str(&String::from_utf8_lossy(&value[begin..pos]));
            }
            pos += 1;
            let tag_start = pos;
            while pos < value.len() && is_tag_byte(value[pos]) {
                pos += 1;
            }
            if pos == tag_start {
                warn!("Lang Error: Expected tag name (key '{key}')");
                return None;
            }
            let tag = std::str::from_utf8(&value[tag_start..pos]).ok()?;
            if pos >= value.len() || value[pos] != b'}' {
                warn!("Lang Error: Expected '}}' after tag name (key '{key}:{tag}')");
                return None;
            }
            let Some(tag_idx) = get_tag_index(tag) else {
                warn!("Lang Error: Unknown tag (key '{key}:{tag}')");
                return None;
            };
            if !is_tag_replaced(key_index, tag_idx) {
                warn!("Lang Error: Unexpected tag (key '{key}:{tag}')");
                return None;
            }
            if !used_tags.insert(tag_idx) {
                warn!("Lang Error: Repeated tag (key '{key}:{tag}')");
                return None;
            }
            result.push_str(&prepare_command_string(tag_idx));
            begin = pos + 1;
        }
        pos += 1;
    }

    if begin < value.len() {
        result.push_str(&String::from_utf8_lossy(&value[begin..]));
    }
    Some(result)
}

fn is_tag_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Mask non-whitespace characters with `filler` while preserving encoded tag commands
/// (`PrepareTestValue` in `lang_instance.cpp`).
pub fn prepare_test_value(current: &str, filler: char) -> String {
    let mut result = String::with_capacity(current.len() + filler.len_utf8());
    result.push(filler);
    let mut in_command = false;
    for ch in current.chars() {
        let new_in_command = if ch == TEXT_COMMAND_CHAR {
            !in_command
        } else {
            in_command
        };
        if in_command || new_in_command || ch.is_whitespace() {
            result.push(ch);
        } else {
            result.push(filler);
        }
        in_command = new_in_command;
    }
    result
}

/// Extract plural rule language code from a custom `.strings` filename.
///
/// Exact port of `PluralCodeForCustom` regex `_([a-z]{2,3}_[A-Z]{2,3}|-[a-z]{2,3})?\.`.
pub fn plural_code_for_custom(absolute_path: &str, relative_path: &str) -> String {
    let path_str = if !absolute_path.is_empty() {
        absolute_path
    } else {
        relative_path
    };
    let name = Path::new(path_str)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path_str);
    let bytes = name.as_bytes();

    for i in 0..bytes.len() {
        if bytes[i] != b'_' {
            continue;
        }
        let rest = &bytes[i + 1..];
        // Branch 1: `[a-z]{2,3}_[A-Z]{2,3}\.`
        for low_len in [2usize, 3] {
            for up_len in [2usize, 3] {
                let total = low_len + 1 + up_len;
                if rest.len() > total
                    && rest[..low_len].iter().all(u8::is_ascii_lowercase)
                    && rest[low_len] == b'_'
                    && rest[low_len + 1..total].iter().all(u8::is_ascii_uppercase)
                    && rest[total] == b'.'
                {
                    return name[i + 1..i + 1 + total].to_string();
                }
            }
        }
        // Branch 2: `-[a-z]{2,3}\.`
        for low_len in [2usize, 3] {
            let total = 1 + low_len;
            if rest.len() > total
                && rest[0] == b'-'
                && rest[1..total].iter().all(u8::is_ascii_lowercase)
                && rest[total] == b'.'
            {
                return name[i + 1..i + 1 + total].to_string();
            }
        }
        // Branch 3: empty capture before `.` (from `(...)?\.`)
        if !rest.is_empty() && rest[0] == b'.' {
            return String::new();
        }
    }

    default_language_id().to_string()
}

// ============================================================================
// Global active Instance (backing `tr::*` runtime lookups)
// ============================================================================

static GLOBAL_INSTANCE: OnceLock<RwLock<Instance>> = OnceLock::new();
static GLOBAL_GENERATION: AtomicU64 = AtomicU64::new(1);
static GLOBAL_TDATA_DIR: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

/// Records the active `tdata` directory path used by process-wide langpack persistence
/// (`Local::writeLangPack` / `Local::readRecentLanguages`).
pub fn set_global_tdata_dir(dir: Option<PathBuf>) {
    let lock = GLOBAL_TDATA_DIR.get_or_init(|| RwLock::new(None));
    let mut guard = lock.write().unwrap_or_else(|p| p.into_inner());
    *guard = dir;
}

/// Returns the active `tdata` directory path configured at startup, if any.
#[must_use]
pub fn global_tdata_dir() -> Option<PathBuf> {
    let lock = GLOBAL_TDATA_DIR.get_or_init(|| RwLock::new(None));
    lock.read().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Access the process-wide active [`Instance`] (`Lang::GetInstance`).
pub fn global_instance() -> &'static RwLock<Instance> {
    GLOBAL_INSTANCE.get_or_init(|| RwLock::new(Instance::new()))
}

/// Monotonically increasing counter bumped whenever the global [`Instance`] is modified.
///
/// UI string caches (such as `TgLangStrings`) compare against this counter to refresh
/// transparently on live language switches without requiring an app restart.
#[must_use]
pub fn global_generation() -> u64 {
    GLOBAL_GENERATION.load(Ordering::Acquire)
}

/// Read-lock the global [`Instance`] and run `f`.
pub fn with_current<R>(f: impl FnOnce(&Instance) -> R) -> R {
    let guard = global_instance()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&guard)
}

/// Write-lock the global [`Instance`] and run `f`.
///
/// Automatically bumps [`global_generation`] if `f` mutated the global instance.
pub fn with_current_mut<R>(f: impl FnOnce(&mut Instance) -> R) -> R {
    let mut guard = global_instance()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = guard.generation();
    let result = f(&mut guard);
    if guard.generation() != before {
        GLOBAL_GENERATION.fetch_add(1, Ordering::AcqRel);
    }
    result
}

/// Load the persisted language pack from `tdata_dir` directly into [`global_instance`]
/// before any UI is constructed (`Local::start` -> `Local::readLangPack`).
pub fn load_global_from_tdata(tdata_dir: &Path) -> Result<Option<SerializeLoadOutcome>, LangError> {
    with_current_mut(|global| global.load_from_tdata_dir(tdata_dir))
}

/// Runtime lookup used by generated plain `tr::*` functions.
#[inline]
pub fn lookup_plain(key: u16) -> String {
    with_current(|inst| inst.format_plain(key))
}

/// Runtime lookup used by generated tagged `tr::*` functions.
#[inline]
pub fn lookup_tagged(key: u16, replacements: &[(u16, &str)]) -> String {
    with_current(|inst| inst.format_tagged(key, replacements))
}

/// Runtime lookup used by generated plural `tr::*` functions.
#[inline]
pub fn lookup_plural(key_base: u16, count: PluralCount, extra_tags: &[(u16, &str)]) -> String {
    with_current(|inst| inst.format_plural(key_base, count, extra_tags))
}
