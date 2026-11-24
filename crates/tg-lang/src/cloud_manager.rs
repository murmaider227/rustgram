//! Cloud language pack synchronization state machine (network abstracted via trait).
//!
//! Port of `Telegram/SourceFiles/lang/lang_cloud_manager.cpp` and `lang_cloud_manager.h`.

use std::collections::BTreeMap;

use tracing::{info, warn};

use crate::auto::{get_original_value, key_ids};
use crate::instance::{
    CLOUD_LANG_PACK_NAME, CUSTOM_LANGUAGE_ID, Instance, LangError, LangPackDifference,
    LangPackLanguageInfo, LangPackString, Language, Pack,
};
use crate::keys::{default_language_id, language_id_or_default};

/// Opaque identifier for an in-flight cloud request (0 = none, -1 = queued while disconnected).
pub type RequestId = i64;

const PENDING_OFFLINE_REQUEST: RequestId = -1;

/// Network transport abstraction for cloud language pack API calls (`langpack.*`).
///
/// Implemented by `tg-api` over `tg-mtproto` in production and mocked in tests.
pub trait LangCloudClient {
    /// Whether an active MTProto session is currently attached.
    fn is_connected(&self) -> bool;

    /// Invoke `langpack.getLanguages(lang_pack)`.
    fn request_languages(&mut self, lang_pack: &str) -> RequestId;

    /// Invoke `langpack.getLangPack(lang_pack, lang_code)`.
    fn request_lang_pack(&mut self, lang_pack: &str, lang_code: &str) -> RequestId;

    /// Invoke `langpack.getDifference(lang_pack, lang_code, from_version)`.
    fn request_difference(
        &mut self,
        lang_pack: &str,
        lang_code: &str,
        from_version: i32,
    ) -> RequestId;

    /// Invoke `langpack.getLanguage(lang_pack, lang_code)`.
    fn request_language(&mut self, lang_pack: &str, lang_code: &str) -> RequestId;

    /// Invoke `langpack.getStrings(lang_pack, lang_code, keys)`.
    fn request_strings(&mut self, lang_pack: &str, lang_code: &str, keys: Vec<String>)
    -> RequestId;

    /// Cancel an in-flight request if `request_id > 0`.
    fn cancel_request(&mut self, request_id: RequestId);

    /// Re-initialize the main MTProto connection after changing the active language ID.
    fn reinit_connection(&mut self);
}

/// Actions and UI/storage notifications produced by [`CloudManager`] transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudAction {
    /// Persist the updated [`Instance`] to local storage (`Local::writeLangPack`).
    WriteLangPack,
    /// Record `language` in recently used languages (`Local::pushRecentLanguage`).
    PushRecentLanguage(Language),
    /// Restart the application now that switched pack downloads completed (`Core::Restart`).
    RestartApp,
    /// Notify observers that the available language list changed.
    LanguageListChanged,
    /// First suggested language from the server arrived (`_firstLanguageSuggestion`).
    FirstLanguageSuggestion(String),
    /// Offer the user a one-time legacy switch to the suggested language.
    OfferSwitchLanguage(Language),
    /// Ask the user to confirm switching to a language (with translation coverage metadata).
    ConfirmSwitchWithWarning(LangPackLanguageInfo),
    /// Inform the user that the requested language has zero translated strings (`NotReadyBox`).
    LanguageNotReady(LangPackLanguageInfo),
    /// Inform the user that the requested language is already active (`lng_language_already`).
    AlreadyUsingLanguage,
    /// Inform the user that the requested language code is not supported (`lng_language_not_found`).
    LanguageNotFound,
    /// Prompt the user to choose a local `.strings` file (`performSwitchToCustom`).
    PromptCustomFile,
    /// Ask the user to confirm a restart-requiring switch, showing the bilingual prompt text.
    ConfirmRestartSwitch {
        /// Target language descriptor.
        language: Language,
        /// Bilingual `lng_sure_save_language` confirmation text.
        prompt_text: String,
    },
    /// Deliver the result of a [`CloudManager::get_value_for_lang`] lookup.
    ValueForLang {
        /// Requested language code.
        lang_id: String,
        /// Requested key name.
        key: String,
        /// Retrieved value (`""` if not found or failed).
        value: String,
    },
}

/// Policy flags normally queried from `Core::Application` in tdesktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudManagerConfig {
    /// Whether language switches can be applied live without restarting the app.
    pub can_apply_without_restart: bool,
    /// Whether to offer the legacy first-run suggested language switch prompt.
    pub offer_legacy_lang_pack_switch: bool,
}

impl Default for CloudManagerConfig {
    fn default() -> Self {
        Self {
            can_apply_without_restart: true,
            offer_legacy_lang_pack_switch: false,
        }
    }
}

/// Manages cloud language pack discovery, version syncing, base-pack fallback, and switching.
///
/// Port of `Lang::CloudManager` (`lang_cloud_manager.cpp`).
#[derive(Debug)]
pub struct CloudManager {
    config: CloudManagerConfig,
    languages: Vec<Language>,
    language_infos: Vec<LangPackLanguageInfo>,
    lang_pack_request_id: RequestId,
    lang_pack_request_is_diff: bool,
    lang_pack_base_request_id: RequestId,
    lang_pack_base_request_is_diff: bool,
    languages_request_id: RequestId,

    offer_switch_to_id: String,
    restart_after_switch: bool,

    suggested_language: String,
    language_was_suggested: bool,

    switching_to_language_request: RequestId,
    switching_to_language_id: String,
    switching_to_language_warning: bool,

    get_keys_for_switch_request_id: RequestId,
    pending_switch_language: Option<Language>,

    /// Map from `"<lang_id>:<key>"` -> request ID (`-1` if queued while disconnected).
    get_value_requests: BTreeMap<String, RequestId>,
}

impl Default for CloudManager {
    fn default() -> Self {
        Self::new(CloudManagerConfig::default())
    }
}

impl CloudManager {
    /// Create a new [`CloudManager`] with the given configuration.
    pub fn new(config: CloudManagerConfig) -> Self {
        Self {
            config,
            languages: Vec::new(),
            language_infos: Vec::new(),
            lang_pack_request_id: 0,
            lang_pack_request_is_diff: false,
            lang_pack_base_request_id: 0,
            lang_pack_base_request_is_diff: false,
            languages_request_id: 0,
            offer_switch_to_id: String::new(),
            restart_after_switch: false,
            suggested_language: String::new(),
            language_was_suggested: false,
            switching_to_language_request: 0,
            switching_to_language_id: String::new(),
            switching_to_language_warning: false,
            get_keys_for_switch_request_id: 0,
            pending_switch_language: None,
            get_value_requests: BTreeMap::new(),
        }
    }

    /// Cached list of available cloud languages.
    pub fn language_list(&self) -> &[Language] {
        &self.languages
    }

    /// Cached list of full cloud language metadata (including official / beta / RTL flags
    /// and translation coverage counts).
    pub fn language_infos(&self) -> &[LangPackLanguageInfo] {
        &self.language_infos
    }

    /// Find full cloud metadata for `id` in the cached language list.
    pub fn find_language_info(&self, id: &str) -> Option<&LangPackLanguageInfo> {
        self.language_infos.iter().find(|l| l.lang_code == id)
    }

    /// Server-suggested language code (empty when English or unset).
    pub fn suggested_language(&self) -> &str {
        &self.suggested_language
    }

    /// Determine whether `id` corresponds to [`Pack::Current`], [`Pack::Base`], or [`Pack::None`].
    pub fn pack_type_from_id(&self, langpack: &Instance, id: &str) -> Pack {
        if id == language_id_or_default(langpack.id()) {
            Pack::Current
        } else if !langpack.base_id().is_empty() && id == langpack.base_id() {
            Pack::Base
        } else {
            Pack::None
        }
    }

    /// Whether switching to `id` can be applied without restarting (`canApplyWithoutRestart`).
    pub fn can_apply_without_restart(&self, id: &str) -> bool {
        if id == "#TEST_X" || id == "#TEST_0" {
            return true;
        }
        self.config.can_apply_without_restart
    }

    /// Handle server-suggested language code (`setSuggestedLanguage`).
    pub fn set_suggested_language(
        &mut self,
        langpack: &Instance,
        lang_code: &str,
        client: &mut dyn LangCloudClient,
    ) -> Vec<CloudAction> {
        let mut actions = Vec::new();
        if language_id_or_default(lang_code) != default_language_id() {
            self.suggested_language = lang_code.to_string();
        } else {
            self.suggested_language.clear();
        }

        if !self.language_was_suggested {
            self.language_was_suggested = true;
            actions.push(CloudAction::FirstLanguageSuggestion(
                self.suggested_language.clone(),
            ));

            if self.config.offer_legacy_lang_pack_switch
                && langpack.id().is_empty()
                && !self.suggested_language.is_empty()
            {
                self.offer_switch_to_id = self.suggested_language.clone();
                if let Some(offered) = self.find_offered_language() {
                    actions.push(CloudAction::OfferSwitchLanguage(offered));
                } else {
                    self.request_language_list(client);
                }
            }
        }
        actions
    }

    /// Look up the offered language in the cached language list (`findOfferedLanguage`).
    pub fn find_offered_language(&self) -> Option<Language> {
        if self.offer_switch_to_id.is_empty() {
            return None;
        }
        self.languages
            .iter()
            .find(|l| l.id == self.offer_switch_to_id)
            .cloned()
    }

    /// Check server-reported `version` and `base_version` against local pack versions and
    /// request missing differences (`setCurrentVersions`).
    pub fn set_current_versions(
        &mut self,
        langpack: &Instance,
        version: i32,
        base_version: i32,
        client: &mut dyn LangCloudClient,
    ) {
        for (pack, target_version) in [(Pack::Current, version), (Pack::Base, base_version)] {
            if target_version > langpack.version(pack) && self.pack_request_id(pack) == 0 {
                self.request_lang_pack_difference(langpack, pack, client);
            }
        }
    }

    /// Request difference for `lang_id`, routing to [`Pack::Current`] or [`Pack::Base`].
    pub fn request_lang_pack_difference_by_id(
        &mut self,
        langpack: &Instance,
        lang_id: &str,
        client: &mut dyn LangCloudClient,
    ) {
        if lang_id.is_empty() {
            return;
        }
        let pack = self.pack_type_from_id(langpack, lang_id);
        if pack != Pack::None {
            self.request_lang_pack_difference(langpack, pack, client);
        }
    }

    /// Request a full pack (`version == 0`) or incremental difference (`version > 0`) for `pack`.
    pub fn request_lang_pack_difference(
        &mut self,
        langpack: &Instance,
        pack: Pack,
        client: &mut dyn LangCloudClient,
    ) {
        if !client.is_connected() {
            if !langpack.is_custom() && !langpack.cloud_lang_code(pack).is_empty() {
                *self.pack_request_id_mut(pack) = PENDING_OFFLINE_REQUEST;
                *self.pack_request_is_diff_mut(pack) = langpack.version(pack) > 0;
            }
            return;
        }
        let prev = std::mem::replace(self.pack_request_id_mut(pack), 0);
        if prev > 0 {
            client.cancel_request(prev);
        }
        if langpack.is_custom() {
            return;
        }
        let version = langpack.version(pack);
        let code = langpack.cloud_lang_code(pack);
        if code.is_empty() {
            return;
        }
        let (req_id, is_diff) = if version > 0 {
            (
                client.request_difference(CLOUD_LANG_PACK_NAME, code, version),
                true,
            )
        } else {
            (client.request_lang_pack(CLOUD_LANG_PACK_NAME, code), false)
        };
        *self.pack_request_id_mut(pack) = req_id;
        *self.pack_request_is_diff_mut(pack) = is_diff;
    }

    /// Unconditionally request a full `langpack.getLangPack` refetch for `pack` (e.g. after an
    /// unrecoverable version gap during incremental `langpack.getDifference`).
    pub fn request_full_lang_pack(
        &mut self,
        langpack: &Instance,
        pack: Pack,
        client: &mut dyn LangCloudClient,
    ) {
        if !client.is_connected() {
            if !langpack.is_custom() && !langpack.cloud_lang_code(pack).is_empty() {
                *self.pack_request_id_mut(pack) = PENDING_OFFLINE_REQUEST;
                *self.pack_request_is_diff_mut(pack) = false;
            }
            return;
        }
        let prev = std::mem::replace(self.pack_request_id_mut(pack), 0);
        if prev > 0 {
            client.cancel_request(prev);
        }
        if langpack.is_custom() {
            return;
        }
        let code = langpack.cloud_lang_code(pack);
        if code.is_empty() {
            return;
        }
        let req_id = client.request_lang_pack(CLOUD_LANG_PACK_NAME, code);
        *self.pack_request_id_mut(pack) = req_id;
        *self.pack_request_is_diff_mut(pack) = false;
    }

    /// Apply a received [`LangPackDifference`] (either from a push update or a response).
    pub fn apply_lang_pack_difference(
        &mut self,
        langpack: &mut Instance,
        difference: &LangPackDifference,
        client: &mut dyn LangCloudClient,
    ) -> Result<Vec<CloudAction>, LangError> {
        self.apply_lang_pack_difference_from_source(langpack, difference, false, client)
    }

    /// Apply a received [`LangPackDifference`], distinguishing whether it was returned by an
    /// incremental `langpack.getDifference` RPC (`from_difference_rpc = true`) or by a push
    /// update / full `langpack.getLangPack` (`from_difference_rpc = false`).
    ///
    /// On a version gap (`local_version < difference.from_version`):
    /// - If this arrived from a push update, first requests `langpack.getDifference` from the
    ///   local version (or full `langpack.getLangPack` when `local_version == 0`).
    /// - If the response to `langpack.getDifference` itself still has a version gap, falls back
    ///   to a full `langpack.getLangPack` refetch so the client never loops on an unbridgeable gap.
    pub fn apply_lang_pack_difference_from_source(
        &mut self,
        langpack: &mut Instance,
        difference: &LangPackDifference,
        from_difference_rpc: bool,
        client: &mut dyn LangCloudClient,
    ) -> Result<Vec<CloudAction>, LangError> {
        let mut actions = Vec::new();
        if langpack.is_custom() {
            return Ok(actions);
        }

        let pack = self.pack_type_from_id(langpack, &difference.lang_code);
        if pack == Pack::None {
            warn!(
                "Lang Warning: Ignoring update for '{}' because our language is '{}'",
                difference.lang_code,
                langpack.id()
            );
            return Ok(actions);
        }

        // Clear any in-flight request slot for this pack.
        let was_diff_request = std::mem::replace(self.pack_request_is_diff_mut(pack), false);
        *self.pack_request_id_mut(pack) = 0;

        if langpack.version(pack) < difference.from_version {
            if from_difference_rpc || was_diff_request {
                warn!(
                    lang_code = %difference.lang_code,
                    local_version = langpack.version(pack),
                    from_version = difference.from_version,
                    "Langpack version gap persists after getDifference; requesting full getLangPack"
                );
                self.request_full_lang_pack(langpack, pack, client);
            } else {
                self.request_lang_pack_difference(langpack, pack, client);
            }
        } else if !difference.strings.is_empty() {
            langpack.apply_difference(pack, difference)?;
            actions.push(CloudAction::WriteLangPack);
        } else if self.restart_after_switch {
            actions.push(CloudAction::WriteLangPack);
        } else {
            info!("Lang Info: Up to date.");
        }

        if self.restart_after_switch && self.should_restart_now() {
            self.restart_after_switch = false;
            actions.push(CloudAction::RestartApp);
        }

        Ok(actions)
    }

    /// Mark an in-flight pack request as failed so future syncs can retry.
    pub fn on_pack_request_failed(&mut self, pack: Pack) {
        *self.pack_request_id_mut(pack) = 0;
        *self.pack_request_is_diff_mut(pack) = false;
    }

    /// Handle failure of a pack request with an optional Telegram RPC error name.
    ///
    /// If an incremental `langpack.getDifference` fails because the local version is too old /
    /// invalid (e.g. `LANG_PACK_DIFFERENCE_TOO_OLD`, `VERSION_INVALID`), automatically falls back
    /// to a full `langpack.getLangPack` refetch.
    pub fn on_pack_request_failed_with_error(
        &mut self,
        langpack: &Instance,
        pack: Pack,
        error_type: &str,
        client: &mut dyn LangCloudClient,
    ) {
        let was_diff = std::mem::replace(self.pack_request_is_diff_mut(pack), false);
        *self.pack_request_id_mut(pack) = 0;
        if was_diff
            && langpack.version(pack) > 0
            && matches!(
                error_type,
                "LANG_PACK_DIFFERENCE_TOO_OLD" | "VERSION_INVALID" | "FROM_VERSION_INVALID"
            )
        {
            self.request_full_lang_pack(langpack, pack, client);
        }
    }

    /// Request the list of supported cloud languages (`requestLanguageList`).
    pub fn request_language_list(&mut self, client: &mut dyn LangCloudClient) {
        if !client.is_connected() {
            self.languages_request_id = PENDING_OFFLINE_REQUEST;
            return;
        }
        let prev = std::mem::replace(&mut self.languages_request_id, 0);
        if prev > 0 {
            client.cancel_request(prev);
        }
        self.languages_request_id = client.request_languages(CLOUD_LANG_PACK_NAME);
    }

    /// Handle the response to `langpack.getLanguages`.
    pub fn on_languages_received(&mut self, infos: &[LangPackLanguageInfo]) -> Vec<CloudAction> {
        self.languages_request_id = 0;
        let languages: Vec<Language> = infos
            .iter()
            .map(LangPackLanguageInfo::to_language)
            .collect();
        let changed = self.languages.len() != languages.len()
            || self
                .languages
                .iter()
                .zip(&languages)
                .any(|(a, b)| !a.same_listing_entry(b))
            || self.language_infos != infos;
        self.language_infos = infos.to_vec();
        let mut actions = Vec::new();
        if changed {
            self.languages = languages;
            actions.push(CloudAction::LanguageListChanged);
            if let Some(offered) = self.find_offered_language() {
                actions.push(CloudAction::OfferSwitchLanguage(offered));
            }
        }
        actions
    }

    /// Handle failure of `langpack.getLanguages`.
    pub fn on_languages_failed(&mut self) {
        self.languages_request_id = 0;
    }

    /// Reset to built-in English (`resetToDefault`).
    pub fn reset_to_default(&mut self, langpack: &mut Instance, client: &mut dyn LangCloudClient) {
        self.perform_switch(langpack, &Language::default_english(), client);
    }

    /// Toggle between `#TEST_X` and `#TEST_0` pseudo-languages (`switchToTestLanguage`).
    pub fn switch_to_test_language(
        &mut self,
        langpack: &mut Instance,
        client: &mut dyn LangCloudClient,
    ) {
        let test_id = if langpack.id() == "#TEST_X" {
            "#TEST_0"
        } else {
            "#TEST_X"
        };
        self.perform_switch(
            langpack,
            &Language {
                id: test_id.to_string(),
                ..Language::default()
            },
            client,
        );
    }

    /// Switch to language by ID without a confirmation warning (`switchToLanguage(id)`).
    pub fn switch_to_language_id(
        &mut self,
        langpack: &Instance,
        id: &str,
        client: &mut dyn LangCloudClient,
    ) -> Vec<CloudAction> {
        self.request_language_and_switch(langpack, id, false, client)
    }

    /// Switch to language by ID, showing coverage warning first (`switchWithWarning(id)`).
    pub fn switch_with_warning(
        &mut self,
        langpack: &Instance,
        id: &str,
        client: &mut dyn LangCloudClient,
    ) -> Vec<CloudAction> {
        self.request_language_and_switch(langpack, id, true, client)
    }

    fn request_language_and_switch(
        &mut self,
        langpack: &Instance,
        id: &str,
        warning: bool,
        client: &mut dyn LangCloudClient,
    ) -> Vec<CloudAction> {
        if id.is_empty() {
            return Vec::new();
        }
        if language_id_or_default(langpack.id()) == id {
            return vec![CloudAction::AlreadyUsingLanguage];
        }
        if id == CUSTOM_LANGUAGE_ID {
            return vec![CloudAction::PromptCustomFile];
        }
        self.switching_to_language_id = id.to_string();
        self.switching_to_language_warning = warning;
        self.send_switching_to_language_request(client);
        Vec::new()
    }

    fn send_switching_to_language_request(&mut self, client: &mut dyn LangCloudClient) {
        if !client.is_connected() {
            self.switching_to_language_request = PENDING_OFFLINE_REQUEST;
            return;
        }
        let prev = std::mem::replace(&mut self.switching_to_language_request, 0);
        if prev > 0 {
            client.cancel_request(prev);
        }
        self.switching_to_language_request =
            client.request_language(CLOUD_LANG_PACK_NAME, &self.switching_to_language_id);
    }

    /// Handle the response to `langpack.getLanguage`.
    pub fn on_language_received(
        &mut self,
        langpack: &mut Instance,
        info: &LangPackLanguageInfo,
        client: &mut dyn LangCloudClient,
    ) -> Vec<CloudAction> {
        self.switching_to_language_request = 0;
        let language = info.to_language();
        if !self.switching_to_language_warning {
            return self.finalize_switch(langpack, &language, client);
        }
        if info.strings_count > 0 {
            vec![CloudAction::ConfirmSwitchWithWarning(info.clone())]
        } else {
            vec![CloudAction::LanguageNotReady(info.clone())]
        }
    }

    /// Handle failure of `langpack.getLanguage`.
    pub fn on_language_failed(&mut self, error_type: &str) -> Vec<CloudAction> {
        self.switching_to_language_request = 0;
        if error_type == "LANG_CODE_NOT_SUPPORTED" {
            vec![CloudAction::LanguageNotFound]
        } else {
            Vec::new()
        }
    }

    /// Switch to a known [`Language`] descriptor (`switchToLanguage(Language)`).
    pub fn switch_to_language(
        &mut self,
        langpack: &mut Instance,
        data: &Language,
        client: &mut dyn LangCloudClient,
    ) -> Vec<CloudAction> {
        if langpack.id() == data.id && data.id != CUSTOM_LANGUAGE_ID {
            return Vec::new();
        }
        if !client.is_connected() {
            return Vec::new();
        }
        let prev = std::mem::replace(&mut self.get_keys_for_switch_request_id, 0);
        if prev > 0 {
            client.cancel_request(prev);
        }

        if data.id == CUSTOM_LANGUAGE_ID {
            return vec![CloudAction::PromptCustomFile];
        }
        if self.can_apply_without_restart(&data.id) {
            return self.perform_switch_and_add_to_recent(langpack, data, client);
        }

        self.pending_switch_language = Some(data.clone());
        self.get_keys_for_switch_request_id = client.request_strings(
            CLOUD_LANG_PACK_NAME,
            &data.id,
            vec!["lng_sure_save_language".to_string()],
        );
        Vec::new()
    }

    /// Handle the response to the bilingual confirmation `langpack.getStrings` request.
    pub fn on_switch_keys_received(
        &mut self,
        langpack: &Instance,
        strings: &[LangPackString],
    ) -> Vec<CloudAction> {
        self.get_keys_for_switch_request_id = 0;
        let Some(language) = self.pending_switch_language.take() else {
            return Vec::new();
        };
        let values = Instance::parse_strings(strings);
        let key = key_ids::lng_sure_save_language;
        let target_text = values
            .get(&key)
            .map(String::as_str)
            .unwrap_or_else(|| get_original_value(key));
        let prompt_text = format!("{}\n\n{}", langpack.format_plain(key), target_text);
        vec![CloudAction::ConfirmRestartSwitch {
            language,
            prompt_text,
        }]
    }

    /// Apply a switch after user confirmation (either live or triggering restart after download).
    pub fn finalize_switch(
        &mut self,
        langpack: &mut Instance,
        language: &Language,
        client: &mut dyn LangCloudClient,
    ) -> Vec<CloudAction> {
        if self.can_apply_without_restart(&language.id) {
            self.perform_switch_and_add_to_recent(langpack, language, client)
        } else {
            self.perform_switch_and_restart(langpack, language, client)
        }
    }

    /// Switch language ID, re-init connection if changed, and request current + base packs.
    pub fn perform_switch(
        &mut self,
        langpack: &mut Instance,
        data: &Language,
        client: &mut dyn LangCloudClient,
    ) {
        self.restart_after_switch = false;
        self.switch_lang_pack_id(langpack, data, client);
        self.request_lang_pack_difference(langpack, Pack::Current, client);
        self.request_lang_pack_difference(langpack, Pack::Base, client);
    }

    /// Switch language and emit [`CloudAction::PushRecentLanguage`].
    pub fn perform_switch_and_add_to_recent(
        &mut self,
        langpack: &mut Instance,
        data: &Language,
        client: &mut dyn LangCloudClient,
    ) -> Vec<CloudAction> {
        self.perform_switch(langpack, data, client);
        vec![CloudAction::PushRecentLanguage(data.clone())]
    }

    /// Switch language, record as recent, and restart once downloads finish (or immediately).
    pub fn perform_switch_and_restart(
        &mut self,
        langpack: &mut Instance,
        data: &Language,
        client: &mut dyn LangCloudClient,
    ) -> Vec<CloudAction> {
        let mut actions = self.perform_switch_and_add_to_recent(langpack, data, client);
        if self.should_restart_now() {
            actions.push(CloudAction::RestartApp);
        } else {
            self.restart_after_switch = true;
        }
        actions
    }

    /// Cancel an offered switch and lock in English (`cancel` in `showOfferSwitchBox`).
    pub fn decline_offered_switch(
        &mut self,
        langpack: &mut Instance,
        client: &mut dyn LangCloudClient,
    ) -> Vec<CloudAction> {
        self.offer_switch_to_id.clear();
        self.change_id_and_reinit_connection(langpack, &Language::default_english(), client);
        vec![CloudAction::WriteLangPack]
    }

    /// Request a single key's translation in `lang_id` (`getValueForLang`).
    pub fn get_value_for_lang(
        &mut self,
        key: &str,
        lang_id: &str,
        client: &mut dyn LangCloudClient,
    ) {
        let req_key = format!("{lang_id}:{key}");
        if !client.is_connected() {
            self.get_value_requests
                .insert(req_key, PENDING_OFFLINE_REQUEST);
            return;
        }
        if let Some(prev) = self.get_value_requests.get(&req_key).copied()
            && prev > 0
        {
            client.cancel_request(prev);
        }
        let req_id = client.request_strings(CLOUD_LANG_PACK_NAME, lang_id, vec![key.to_string()]);
        self.get_value_requests.insert(req_key, req_id);
    }

    /// Handle response for a [`Self::get_value_for_lang`] request.
    pub fn on_value_for_lang_received(
        &mut self,
        lang_id: &str,
        key: &str,
        strings: &[LangPackString],
    ) -> Option<CloudAction> {
        let req_key = format!("{lang_id}:{key}");
        self.get_value_requests.remove(&req_key)?;
        let values = Instance::parse_strings(strings);
        let value = values.into_values().next().unwrap_or_default();
        Some(CloudAction::ValueForLang {
            lang_id: lang_id.to_string(),
            key: key.to_string(),
            value,
        })
    }

    /// Resend any pending requests when an MTProto session attaches (`resendRequests`).
    pub fn resend_requests(&mut self, langpack: &Instance, client: &mut dyn LangCloudClient) {
        if !client.is_connected() {
            return;
        }
        if self.lang_pack_base_request_id != 0 {
            self.request_lang_pack_difference(langpack, Pack::Base, client);
        }
        if self.lang_pack_request_id != 0 {
            self.request_lang_pack_difference(langpack, Pack::Current, client);
        }
        if self.languages_request_id != 0 {
            self.request_language_list(client);
        }
        if self.switching_to_language_request != 0 {
            self.send_switching_to_language_request(client);
        }
        let pending_values: Vec<(String, String)> = self
            .get_value_requests
            .iter()
            .filter_map(|(k, &id)| {
                if id == PENDING_OFFLINE_REQUEST {
                    k.split_once(':')
                        .map(|(lang, key)| (lang.to_string(), key.to_string()))
                } else {
                    None
                }
            })
            .collect();
        for (lang_id, key) in pending_values {
            self.get_value_for_lang(&key, &lang_id, client);
        }
    }

    fn switch_lang_pack_id(
        &mut self,
        langpack: &mut Instance,
        data: &Language,
        client: &mut dyn LangCloudClient,
    ) {
        let current_id = langpack.id();
        let current_base_id = langpack.base_id();
        let not_changed = (current_id == data.id && current_base_id == data.base_id)
            || (current_id.is_empty()
                && current_base_id.is_empty()
                && data.id == default_language_id());
        if !not_changed {
            self.change_id_and_reinit_connection(langpack, data, client);
        }
    }

    fn change_id_and_reinit_connection(
        &mut self,
        langpack: &mut Instance,
        data: &Language,
        client: &mut dyn LangCloudClient,
    ) {
        langpack.switch_to_id(data);
        if client.is_connected() {
            client.reinit_connection();
        }
    }

    fn should_restart_now(&self) -> bool {
        self.lang_pack_request_id == 0 && self.lang_pack_base_request_id == 0
    }

    fn pack_request_id(&self, pack: Pack) -> RequestId {
        match pack {
            Pack::Base => self.lang_pack_base_request_id,
            Pack::Current | Pack::None => self.lang_pack_request_id,
        }
    }

    fn pack_request_id_mut(&mut self, pack: Pack) -> &mut RequestId {
        match pack {
            Pack::Base => &mut self.lang_pack_base_request_id,
            Pack::Current | Pack::None => &mut self.lang_pack_request_id,
        }
    }

    fn pack_request_is_diff_mut(&mut self, pack: Pack) -> &mut bool {
        match pack {
            Pack::Base => &mut self.lang_pack_base_request_is_diff,
            Pack::Current | Pack::None => &mut self.lang_pack_request_is_diff,
        }
    }
}
