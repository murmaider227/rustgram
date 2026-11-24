//! Live RPC glue for WP-40 features (reaction notifications, cloud Hunspell dictionaries,
//! and Windows offline translation fallback).
//!
//! Port origin:
//! - `Telegram/SourceFiles/api/api_reactions_notify_settings.cpp`
//! - `Telegram/SourceFiles/chat_helpers/spellchecker_common.cpp`
//! - `Telegram/SourceFiles/lang/translate_provider.cpp`

use std::path::{Path, PathBuf};

use tg_api::RpcClient;
use tg_platform::translate_win::{
    TranslateError, TranslateProvider, TranslateRequest, TranslateResponse, translate_with_fallback,
};
use tg_storage::{
    CloudBlobLoader, CloudBlobLocation, CloudBlobSource, STALE_ARCHIVE_TIMEOUT, UnpackOptions,
    UnpackReport, clear_stale_archive_files,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::cloud_blob_source::TelegramCloudBlobSource;
use crate::media::block_on_runtime;
use crate::session::LiveSession;
use tg_app_core::core_types::dto::dictionaries::{
    DICTIONARIES_MANIFEST_CHANNEL, DICTIONARIES_MANIFEST_POST_ID, DictionaryCloudSource,
    DictionaryEntry, dict_dir_for_lang, finalize_unpacked_dictionary_dir,
    is_good_dictionary_part_name, parse_dictionaries_manifest,
};
use tg_app_core::settings::backend::{NotificationEventsSettings, ReactionsNotifyFrom};

/// Parses a TL `Option<ReactionNotificationsFrom>` into [`ReactionsNotifyFrom`].
#[must_use]
pub fn reactions_notify_from_tl(
    from: Option<&tl_enums::ReactionNotificationsFrom>,
) -> ReactionsNotifyFrom {
    match from {
        None => ReactionsNotifyFrom::None,
        Some(tl_enums::ReactionNotificationsFrom::Contacts) => ReactionsNotifyFrom::Contacts,
        Some(tl_enums::ReactionNotificationsFrom::All) => ReactionsNotifyFrom::All,
    }
}

/// Serializes [`ReactionsNotifyFrom`] into an optional TL `ReactionNotificationsFrom`.
#[must_use]
pub const fn reactions_notify_to_tl(
    from: ReactionsNotifyFrom,
) -> Option<tl_enums::ReactionNotificationsFrom> {
    match from {
        ReactionsNotifyFrom::None => None,
        ReactionsNotifyFrom::Contacts => Some(tl_enums::ReactionNotificationsFrom::Contacts),
        ReactionsNotifyFrom::All => Some(tl_enums::ReactionNotificationsFrom::All),
    }
}

/// Applies a TL `ReactionsNotifySettings` response onto `events`.
pub fn apply_reactions_notify_settings_tl(
    events: &mut NotificationEventsSettings,
    tl: &tl_enums::ReactionsNotifySettings,
) {
    let tl_enums::ReactionsNotifySettings::ReactionsNotifySettings(data) = tl;
    events.message_reactions = reactions_notify_from_tl(data.messages_notify_from.as_ref());
    events.story_reactions = reactions_notify_from_tl(data.stories_notify_from.as_ref());
    events.poll_votes = reactions_notify_from_tl(data.poll_votes_notify_from.as_ref());
    events.show_reaction_previews = data.show_previews;
}

/// Builds a TL `ReactionsNotifySettings` payload matching `Api::ReactionsNotifySettings::save()`.
#[must_use]
pub fn build_reactions_notify_settings_tl(
    events: &NotificationEventsSettings,
) -> tl_enums::ReactionsNotifySettings {
    tl_enums::ReactionsNotifySettings::ReactionsNotifySettings(tl_types::ReactionsNotifySettings {
        messages_notify_from: reactions_notify_to_tl(events.message_reactions),
        stories_notify_from: reactions_notify_to_tl(events.story_reactions),
        poll_votes_notify_from: reactions_notify_to_tl(events.poll_votes),
        sound: tl_enums::NotificationSound::Default,
        show_previews: events.show_reaction_previews,
    })
}

/// Fetches contact sign-up and reaction notification settings from Telegram via live RPCs
/// (`account.getContactSignUpNotification` + `account.getReactionsNotifySettings`).
pub(crate) fn fetch_notification_events_live(session: &LiveSession) -> NotificationEventsSettings {
    let cached = session
        .cache
        .lock()
        .ok()
        .filter(|c| c.notify_events_loaded)
        .map(|c| c.notify_events.clone());
    if let Some(ev) = cached {
        return ev;
    }

    let contact_joined = session
        .invoke_rpc(tl_fns::account::GetContactSignUpNotification {})
        .map(|silent| !silent)
        .unwrap_or(true);

    let mut ev = NotificationEventsSettings {
        contact_joined,
        ..NotificationEventsSettings::default()
    };

    if let Ok(reactions) = session.invoke_rpc(tl_fns::account::GetReactionsNotifySettings {}) {
        apply_reactions_notify_settings_tl(&mut ev, &reactions);
    }

    if let Ok(mut c) = session.cache.lock() {
        c.notify_events_loaded = true;
        c.notify_events = ev.clone();
    }
    ev
}

/// Persists contact sign-up and reaction notification settings to Telegram via live RPCs
/// (`account.setContactSignUpNotification` + `account.setReactionsNotifySettings`).
pub(crate) fn save_notification_events_live(
    session: &LiveSession,
    mut events: NotificationEventsSettings,
) {
    let signup_ok = session
        .invoke_rpc(tl_fns::account::SetContactSignUpNotification {
            silent: !events.contact_joined,
        })
        .is_ok();

    let reactions_req = tl_fns::account::SetReactionsNotifySettings {
        settings: build_reactions_notify_settings_tl(&events),
    };
    let reactions_res = session.invoke_rpc(reactions_req);
    let reactions_ok = match reactions_res {
        Ok(updated) => {
            apply_reactions_notify_settings_tl(&mut events, &updated);
            true
        }
        Err(_) => false,
    };

    if (signup_ok || reactions_ok)
        && let Ok(mut c) = session.cache.lock()
    {
        c.notify_events_loaded = true;
        c.notify_events = events;
    }
}

// ============================================================================
// Cloud Hunspell Dictionaries (`tdhbcfiles` channel via MTProto RPCs)
// ============================================================================

/// Live [`DictionaryCloudSource`] backed by any [`RpcClient`] (including `FakeRpcClient` in tests).
///
/// Mirrors `DictManifestLoader` and `DictLoader` (`chat_helpers/spellchecker_common.cpp`):
/// 1. `contacts.resolveUsername(username)` to obtain `InputChannel`.
/// 2. `channels.getMessages(channel, [InputMessage::Id(post_id)])` to fetch either the JSON
///    manifest text (`tdhbcfiles#2949`) or the zipped `.dic`/`.aff` document attachment.
/// 3. `upload.getFile` via [`TelegramCloudBlobSource`] + [`CloudBlobLoader`] to stage, verify
///    SHA-256, reject zip-slip/symlinks, and atomically swap into `<root>/<locale>/`.
pub struct RpcDictionaryCloudSource<C: RpcClient + Clone + 'static> {
    blob_source: TelegramCloudBlobSource<C>,
}

impl<C: RpcClient + Clone + 'static> RpcDictionaryCloudSource<C> {
    /// Creates a new cloud dictionary loader using `rpc` and `runtime`.
    #[must_use]
    pub fn new(rpc: C, runtime: tokio::runtime::Handle) -> Self {
        Self {
            blob_source: TelegramCloudBlobSource::new(rpc, runtime),
        }
    }

    /// Downloads, optionally SHA-256-verifies, and atomically unpacks a Hunspell dictionary
    /// archive from `channel#post_id` into `<root>/<locale>/` via `tg_storage::cloud_blob`,
    /// returning both the target directory and the full [`UnpackReport`].
    #[allow(clippy::too_many_arguments)]
    pub fn download_and_unpack_dictionary_verified(
        &mut self,
        channel: &str,
        post_id: i32,
        size_bytes: u64,
        root: &Path,
        lang_id: i32,
        locale_code: Option<&str>,
        expected_sha256: Option<[u8; 32]>,
    ) -> Result<(PathBuf, UnpackReport), String> {
        let _ = clear_stale_archive_files(root, STALE_ARCHIVE_TIMEOUT);
        let target_dir = dict_dir_for_lang(root, lang_id, locale_code);
        let loader = CloudBlobLoader::new(
            lang_id,
            CloudBlobLocation::new(channel, post_id),
            &target_dir,
            size_bytes,
            UnpackOptions {
                expected_sha256,
                ..UnpackOptions::default()
            },
        );
        let report = loader
            .download_and_unpack_sync(&self.blob_source, is_good_dictionary_part_name)
            .map_err(|e| format!("Dictionary cloud blob load failed: {e}"))?;
        finalize_unpacked_dictionary_dir(&target_dir, lang_id, &report.extracted_files);
        Ok((target_dir, report))
    }
}

impl<C: RpcClient + Clone + 'static> DictionaryCloudSource for RpcDictionaryCloudSource<C> {
    fn fetch_manifest(&mut self) -> Result<Vec<DictionaryEntry>, String> {
        let msg = self
            .blob_source
            .fetch_post_sync(DICTIONARIES_MANIFEST_CHANNEL, DICTIONARIES_MANIFEST_POST_ID)
            .map_err(|e| e.to_string())?;
        let parsed = parse_dictionaries_manifest(msg.message.as_bytes())?;
        if parsed.is_empty() {
            return Err("Dictionary manifest contained no valid entries.".to_owned());
        }
        Ok(parsed)
    }

    fn download_dictionary_archive(
        &mut self,
        channel: &str,
        post_id: i32,
    ) -> Result<Vec<u8>, String> {
        let location = CloudBlobLocation::new(channel, post_id);
        let mut bytes = Vec::new();
        self.blob_source
            .download_to(&location, &mut bytes, &mut |_| true)
            .map_err(|e| e.to_string())?;
        Ok(bytes)
    }

    fn download_and_unpack_dictionary(
        &mut self,
        channel: &str,
        post_id: i32,
        size_bytes: u64,
        root: &Path,
        lang_id: i32,
        locale_code: Option<&str>,
        expected_sha256: Option<[u8; 32]>,
    ) -> Result<PathBuf, String> {
        self.download_and_unpack_dictionary_verified(
            channel,
            post_id,
            size_bytes,
            root,
            lang_id,
            locale_code,
            expected_sha256,
        )
        .map(|(dir, _)| dir)
    }
}

/// Builds the live cloud dictionary source for `session`
/// (`SettingsBackend::dictionary_cloud_source` in live mode).
pub(crate) fn live_dictionary_source(
    session: &LiveSession,
) -> Option<Box<dyn DictionaryCloudSource + Send>> {
    Some(Box::new(RpcDictionaryCloudSource::new(
        session.runtime().rpc().clone(),
        session.runtime().tokio_handle().clone(),
    )))
}

// ============================================================================
// Windows Offline Translation Fallback (`lang/translate_provider.cpp`)
// ============================================================================

/// Translates `request` trying `platform` first when `use_platform_translation` is enabled,
/// falling back to Telegram's `messages.translateText` RPC through `rpc` when offline
/// translation is disabled, unavailable, or does not support the target language.
pub fn translate_text_with_platform_fallback<C: RpcClient + Clone + 'static>(
    runtime: &tokio::runtime::Handle,
    rpc: &C,
    request: &TranslateRequest,
    use_platform_translation: bool,
    platform: Option<&dyn TranslateProvider>,
) -> Result<TranslateResponse, TranslateError>
where
    C::Error: std::fmt::Debug,
{
    translate_with_fallback(request, use_platform_translation, platform, |req| {
        if req.text.trim().is_empty() {
            return Err(TranslateError::EmptyInput);
        }
        let tl_req = tl_fns::messages::TranslateText {
            peer: None,
            id: None,
            text: Some(vec![tl_enums::TextWithEntities::TextWithEntities(
                tl_types::TextWithEntities {
                    text: req.text.clone(),
                    entities: Vec::new(),
                },
            )]),
            to_lang: req.to_lang.clone(),
            tone: None,
        };
        let res = block_on_runtime(runtime, async { rpc.invoke(&tl_req).await })
            .map_err(|e| TranslateError::Backend(format!("{e:?}")))?;
        let tl_enums::messages::TranslatedText::TranslateResult(inner) = res;
        let translated_text = inner
            .result
            .into_iter()
            .next()
            .map(|tl_enums::TextWithEntities::TextWithEntities(t)| t.text)
            .ok_or_else(|| TranslateError::Backend("Empty translateText response".to_owned()))?;
        Ok(TranslateResponse {
            translated_text,
            detected_source_lang: req.from_lang.clone(),
            provider_name: "mtproto",
        })
    })
}

/// Holds the platform translation provider for one live session
/// (`Platform::CreateTranslateProvider()`; `None` on Windows, matching `translate_provider_win.h`).
#[derive(Clone, Default)]
pub struct PlatformTranslatorSlot(Option<std::sync::Arc<dyn TranslateProvider>>);

impl std::fmt::Debug for PlatformTranslatorSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PlatformTranslatorSlot")
            .field(&self.0.as_ref().map(|p| p.name()))
            .finish()
    }
}

impl PlatformTranslatorSlot {
    /// Wraps an optional provider.
    #[must_use]
    pub fn new(provider: Option<std::sync::Arc<dyn TranslateProvider>>) -> Self {
        Self(provider)
    }

    /// The host's native provider, if this platform has one.
    #[must_use]
    pub fn system() -> Self {
        Self(tg_platform::translate_win::create_translate_provider().map(std::sync::Arc::from))
    }

    /// Returns a shared handle to the provider.
    #[must_use]
    pub fn get(&self) -> Option<std::sync::Arc<dyn TranslateProvider>> {
        self.0.clone()
    }
}

/// Translates `text` into `lang` with `provider` only, returning `None` when the provider is
/// unavailable, does not support `lang`, or fails (the caller then asks Telegram's servers).
#[must_use]
pub fn translate_with_platform_only(
    provider: &dyn TranslateProvider,
    text: &str,
    lang: &str,
) -> Option<String> {
    if text.trim().is_empty() || !provider.is_available() || !provider.supports_language(lang) {
        return None;
    }
    provider
        .translate(&TranslateRequest::new(text, lang))
        .ok()
        .map(|r| r.translated_text)
}
