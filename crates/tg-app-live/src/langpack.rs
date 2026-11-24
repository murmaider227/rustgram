//! Live cloud language pack manager (`LiveLangPackHandle`) connecting `tg-lang`'s
//! [`CloudManager`] and [`tg_lang::Instance`] to live Telegram MTProto RPCs (`langpack.*`,
//! `help.getConfig`), live updates (`updateLangPack`, `updateLangPackTooLong`), encrypted
//! `tdata` persistence, and runtime UI re-rendering.
//!
//! Port origin: `Telegram/SourceFiles/lang/lang_cloud_manager.cpp`,
//! `Telegram/SourceFiles/lang/lang_instance.cpp`,
//! `Telegram/SourceFiles/mtproto/mtp_instance.cpp` (`configLoadDone`), and
//! `Telegram/SourceFiles/core/local_url_handlers.cpp` (`SetLanguage`).

use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;

use tg_api::lang::{LangConfigInfo, RpcLangCloudClient, fetch_lang_config, fetch_strings};
use tg_lang::{
    CLOUD_LANG_PACK_NAME, CloudAction, CloudManager, CloudManagerConfig, Instance as LangInstance,
    LangError, LangPackDifference, LangPackLanguageInfo, Language, Pack, SerializeLoadOutcome,
    SetLanguageLink, default_language_id, language_id_or_default, parse_set_language_link,
    push_recent_language_to_tdata, read_recent_languages_from_tdata,
    remove_recent_language_from_tdata, tr,
};
use tg_session::{SessionEvent, SessionRuntime};
use tokio::sync::{Mutex as AsyncMutex, broadcast};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::bridge::{LiveBridgeState, LiveRuntimeBridge};
use crate::media::{block_on_runtime, with_rpc_timeout};
use tg_app_core::settings::backend::LanguagePrompt;
use tg_app_core::settings::backend::build_switch_confirmation_prompt;

/// Default timeout for synchronous langpack RPC operations invoked from UI callbacks.
const LANGPACK_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Outcome of handling a `tg://setlanguage` or `t.me/setlanguage/...` link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetLanguageLinkOutcome {
    /// Bare `tg://setlanguage` with no `lang` code: open the Language picker screen.
    OpenLanguageBox,
    /// Link resolved to a confirmation, not-ready, already-using, or not-found prompt.
    Prompt(Box<LanguagePrompt>),
}

struct LiveLangPackCore {
    manager: CloudManager,
    instance: LangInstance,
    tdata_dir: Option<PathBuf>,
    languages_loaded: bool,
    recent_languages: Vec<Language>,
    pending_prompt: Option<LanguagePrompt>,
    config_checked: bool,
}

impl LiveLangPackCore {
    fn new(seed_from_global: bool) -> Self {
        let mut instance = LangInstance::new();
        let mut tdata_dir = None;
        let mut recent_languages = Vec::new();
        if seed_from_global {
            tg_lang::with_current(|global| {
                if !global.id().is_empty() || global.version(Pack::Current) > 0 {
                    instance.copy_from(global);
                }
            });
            if let Some(dir) = tg_lang::global_tdata_dir()
                && dir.exists()
            {
                if let Ok(recents) = read_recent_languages_from_tdata(&dir) {
                    recent_languages = recents;
                }
                tdata_dir = Some(dir);
            }
        }
        Self {
            manager: CloudManager::new(CloudManagerConfig {
                can_apply_without_restart: true,
                offer_legacy_lang_pack_switch: true,
            }),
            instance,
            tdata_dir,
            languages_loaded: false,
            recent_languages,
            pending_prompt: None,
            config_checked: false,
        }
    }
}

/// Lock-free / fast synchronous view of the live langpack state for `egui` 60fps rendering.
#[derive(Debug, Clone)]
struct LiveLangPackSnapshot {
    languages_loaded: bool,
    languages: Vec<LangPackLanguageInfo>,
    active_code: String,
    base_code: String,
    current_version: i32,
    base_version: i32,
    recent_languages: Vec<Language>,
    pending_prompt: Option<LanguagePrompt>,
    suggested_language: String,
    reinit_count: u64,
}

impl LiveLangPackSnapshot {
    fn from_core(core: &LiveLangPackCore, prev_reinit_count: u64) -> Self {
        let raw_id = core.instance.id();
        let active_code = if raw_id.is_empty() {
            default_language_id().to_owned()
        } else {
            raw_id.to_owned()
        };
        Self {
            languages_loaded: core.languages_loaded,
            languages: core.manager.language_infos().to_vec(),
            active_code,
            base_code: core.instance.base_id().to_owned(),
            current_version: core.instance.version(Pack::Current),
            base_version: core.instance.version(Pack::Base),
            recent_languages: core.recent_languages.clone(),
            pending_prompt: core.pending_prompt.clone(),
            suggested_language: core.manager.suggested_language().to_owned(),
            reinit_count: prev_reinit_count,
        }
    }
}

struct LiveLangPackInner {
    runtime: Arc<SessionRuntime<ErasedRpcClient>>,
    bridge: Arc<LiveBridgeState>,
    core: AsyncMutex<LiveLangPackCore>,
    snapshot: StdMutex<LiveLangPackSnapshot>,
    sync_global: AtomicBool,
    events_task: StdMutex<Option<JoinHandle<()>>>,
}

impl Drop for LiveLangPackInner {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.events_task.lock()
            && let Some(task) = guard.take()
        {
            task.abort();
        }
    }
}

/// Shared live cloud language pack controller attached to a [`crate::LiveSession`].
///
/// Cheaply cloneable (`Arc`-backed). Automatically consumes `SessionEvent::LangPackDifference`
/// and `SessionEvent::LangPackTooLong` from `SessionRuntime`, applies diffs and full refetches,
/// persists the active pack to `tdata`, synchronizes `tg_lang::global_instance()` (updating
/// `TgLangStrings` and `tg_lang::tr::*` live without restart), and wakes the `egui` UI.
pub struct LiveLangPackHandle {
    inner: Arc<LiveLangPackInner>,
}

impl Clone for LiveLangPackHandle {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl LiveLangPackHandle {
    /// Creates a new [`LiveLangPackHandle`] wired to `runtime`'s event stream and `bridge`.
    ///
    /// Syncs mutations into the process-wide `tg_lang::global_instance()` so `TgLangStrings` and
    /// `tg_lang::tr::*` re-render immediately without an app restart.
    #[must_use]
    pub fn new(runtime: Arc<SessionRuntime<ErasedRpcClient>>, bridge: &LiveRuntimeBridge) -> Self {
        Self::create(runtime, Arc::clone(bridge.state()), true)
    }

    /// Creates an isolated [`LiveLangPackHandle`] that does NOT read from or mutate the
    /// process-wide `tg_lang::global_instance()` (useful for parallel unit tests).
    #[must_use]
    pub fn new_isolated(
        runtime: Arc<SessionRuntime<ErasedRpcClient>>,
        bridge: &LiveRuntimeBridge,
    ) -> Self {
        Self::create(runtime, Arc::clone(bridge.state()), false)
    }

    fn create(
        runtime: Arc<SessionRuntime<ErasedRpcClient>>,
        bridge: Arc<LiveBridgeState>,
        sync_global: bool,
    ) -> Self {
        let core = LiveLangPackCore::new(sync_global);
        let snapshot = LiveLangPackSnapshot::from_core(&core, 0);
        let inner = Arc::new(LiveLangPackInner {
            runtime: Arc::clone(&runtime),
            bridge,
            core: AsyncMutex::new(core),
            snapshot: StdMutex::new(snapshot),
            sync_global: AtomicBool::new(sync_global),
            events_task: StdMutex::new(None),
        });

        let mut events_rx = runtime.subscribe_events();
        let weak_inner = Arc::downgrade(&inner);
        let task = runtime.tokio_handle().spawn(async move {
            loop {
                match events_rx.recv().await {
                    Ok(SessionEvent::LangPackDifference(diff)) => {
                        let Some(strong) = weak_inner.upgrade() else {
                            break;
                        };
                        if let Err(err) = apply_incoming_difference_async(&strong, diff).await {
                            warn!(error = %err, "failed to apply incoming updateLangPack");
                        }
                    }
                    Ok(SessionEvent::LangPackTooLong { lang_code }) => {
                        let Some(strong) = weak_inner.upgrade() else {
                            break;
                        };
                        if let Err(err) = handle_lang_pack_too_long_async(&strong, &lang_code).await
                        {
                            warn!(
                                lang_code = %lang_code,
                                error = %err,
                                "failed to handle updateLangPackTooLong"
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        debug!(skipped, "LiveLangPackHandle event receiver lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        if let Ok(mut guard) = inner.events_task.lock() {
            *guard = Some(task);
        }

        Self { inner }
    }

    /// Controls whether mutations are mirrored into the process-wide `tg_lang::global_instance()`.
    pub fn set_sync_global(&self, enabled: bool) {
        self.inner.sync_global.store(enabled, Ordering::Release);
    }

    /// Attaches a `tdata` directory for encrypted langpack and recent-language persistence,
    /// immediately loading any previously persisted pack before UI renders.
    pub fn set_tdata_dir(
        &self,
        tdata_dir: PathBuf,
    ) -> Result<Option<SerializeLoadOutcome>, LangError> {
        let inner = Arc::clone(&self.inner);
        block_on_runtime(self.inner.runtime.tokio_handle(), async move {
            set_tdata_dir_async(&inner, tdata_dir).await
        })
    }

    /// Alias for [`Self::set_tdata_dir`] attaching a `tdata` directory to this live langpack handle.
    pub fn attach_tdata_dir(
        &self,
        tdata_dir: PathBuf,
    ) -> Result<Option<SerializeLoadOutcome>, LangError> {
        self.set_tdata_dir(tdata_dir)
    }

    /// Async version of [`Self::set_tdata_dir`].
    pub async fn set_tdata_dir_async(
        &self,
        tdata_dir: PathBuf,
    ) -> Result<Option<SerializeLoadOutcome>, LangError> {
        set_tdata_dir_async(&self.inner, tdata_dir).await
    }

    /// Returns the cached cloud language list, fetching via `langpack.getLanguages("tdesktop")`
    /// on the first call (and falling back to built-in English if the RPC fails).
    #[must_use]
    pub fn languages(&self) -> Vec<LangPackLanguageInfo> {
        if let Ok(snap) = self.inner.snapshot.lock()
            && snap.languages_loaded
        {
            return snap.languages.clone();
        }
        match self.fetch_languages() {
            Ok(list) if !list.is_empty() => list,
            _ => vec![fallback_english_info()],
        }
    }

    /// Fetches the cloud language list via `langpack.getLanguages("tdesktop")`.
    pub fn fetch_languages(&self) -> Result<Vec<LangPackLanguageInfo>, String> {
        let inner = Arc::clone(&self.inner);
        block_on_runtime(
            self.inner.runtime.tokio_handle(),
            with_rpc_timeout(LANGPACK_RPC_TIMEOUT, async move {
                fetch_languages_async(&inner).await
            }),
        )
    }

    /// Async version of [`Self::fetch_languages`].
    pub async fn fetch_languages_async(&self) -> Result<Vec<LangPackLanguageInfo>, String> {
        fetch_languages_async(&self.inner).await
    }

    /// Switches to `lang_code`, downloading the full pack (`langpack.getLangPack`) and its
    /// `base_lang_code` pack if present, persisting to `tdata`, updating `tg_lang::global_instance()`
    /// + `TgLangStrings` at runtime, and waking `egui` to re-render with the new strings.
    pub fn apply_language(&self, lang_code: &str) -> Result<Language, String> {
        let inner = Arc::clone(&self.inner);
        let code = lang_code.to_owned();
        block_on_runtime(
            self.inner.runtime.tokio_handle(),
            with_rpc_timeout(LANGPACK_RPC_TIMEOUT, async move {
                apply_language_async(&inner, &code).await
            }),
        )
    }

    /// Async version of [`Self::apply_language`].
    pub async fn apply_language_async(&self, lang_code: &str) -> Result<Language, String> {
        apply_language_async(&self.inner, lang_code).await
    }

    /// Calls `help.getConfig`, evaluates `suggested_lang_code` (producing a
    /// [`LanguagePrompt::SuggestedLanguage`] when tdesktop would offer a first-run switch), and
    /// checks `lang_pack_version` / `base_lang_pack_version` to fetch any pending differences.
    pub fn sync_from_config(&self) -> Result<Option<LanguagePrompt>, String> {
        let inner = Arc::clone(&self.inner);
        block_on_runtime(
            self.inner.runtime.tokio_handle(),
            with_rpc_timeout(LANGPACK_RPC_TIMEOUT, async move {
                sync_from_config_async(&inner).await
            }),
        )
    }

    /// Async version of [`Self::sync_from_config`].
    pub async fn sync_from_config_async(&self) -> Result<Option<LanguagePrompt>, String> {
        sync_from_config_async(&self.inner).await
    }

    /// Applies an already-fetched [`LangConfigInfo`] (`suggested_lang_code`, `lang_pack_version`,
    /// `base_lang_pack_version`) without issuing another `help.getConfig` RPC.
    pub async fn apply_config_info_async(
        &self,
        info: &LangConfigInfo,
    ) -> Result<Option<LanguagePrompt>, String> {
        apply_config_info_async(&self.inner, info).await
    }

    /// Parses and handles a `tg://setlanguage?lang=...` or `t.me/setlanguage/...` link, calling
    /// `langpack.getLanguage` when a code is present and returning the confirmation prompt.
    pub fn handle_set_language_link(&self, url: &str) -> Result<SetLanguageLinkOutcome, String> {
        let inner = Arc::clone(&self.inner);
        let url_owned = url.to_owned();
        block_on_runtime(
            self.inner.runtime.tokio_handle(),
            with_rpc_timeout(LANGPACK_RPC_TIMEOUT, async move {
                handle_set_language_link_async(&inner, &url_owned).await
            }),
        )
    }

    /// Async version of [`Self::handle_set_language_link`].
    pub async fn handle_set_language_link_async(
        &self,
        url: &str,
    ) -> Result<SetLanguageLinkOutcome, String> {
        handle_set_language_link_async(&self.inner, url).await
    }

    /// Confirms and applies the currently pending [`LanguagePrompt`] (either a `setlanguage`
    /// link switch or a suggested-language offer).
    pub fn confirm_pending_prompt(&self) -> Result<Option<Language>, String> {
        let inner = Arc::clone(&self.inner);
        block_on_runtime(
            self.inner.runtime.tokio_handle(),
            with_rpc_timeout(LANGPACK_RPC_TIMEOUT, async move {
                confirm_pending_prompt_async(&inner).await
            }),
        )
    }

    /// Async version of [`Self::confirm_pending_prompt`].
    pub async fn confirm_pending_prompt_async(&self) -> Result<Option<Language>, String> {
        confirm_pending_prompt_async(&self.inner).await
    }

    /// Dismisses the currently pending prompt; if it was a first-run [`LanguagePrompt::SuggestedLanguage`],
    /// locks in English and persists to `tdata` so it is not offered again (`showOfferSwitchBox`
    /// cancel branch in `lang_cloud_manager.cpp:365-369`).
    pub fn dismiss_pending_prompt(&self) {
        let inner = Arc::clone(&self.inner);
        block_on_runtime(self.inner.runtime.tokio_handle(), async move {
            dismiss_pending_prompt_async(&inner).await;
        });
    }

    /// Async version of [`Self::dismiss_pending_prompt`].
    pub async fn dismiss_pending_prompt_async(&self) {
        dismiss_pending_prompt_async(&self.inner).await;
    }

    /// Applies an incoming `updateLangPack` difference directly (also invoked automatically by
    /// the background `SessionEvent` listener).
    pub async fn apply_incoming_difference_async(
        &self,
        diff: LangPackDifference,
    ) -> Result<(), String> {
        apply_incoming_difference_async(&self.inner, diff).await
    }

    /// Handles an incoming `updateLangPackTooLong` notification directly (also invoked
    /// automatically by the background `SessionEvent` listener).
    pub async fn handle_lang_pack_too_long_async(&self, lang_code: &str) -> Result<(), String> {
        handle_lang_pack_too_long_async(&self.inner, lang_code).await
    }

    /// Removes a language by ID from recent languages (both in memory and in `tdata` if configured).
    pub fn remove_recent_language(&self, lang_code: &str) {
        let inner = Arc::clone(&self.inner);
        let code = lang_code.to_owned();
        block_on_runtime(self.inner.runtime.tokio_handle(), async move {
            let mut core = inner.core.lock().await;
            core.recent_languages.retain(|l| l.id != code);
            if let Some(ref tdata) = core.tdata_dir
                && let Err(err) = remove_recent_language_from_tdata(tdata, &code)
            {
                warn!(error = %err, "failed to remove recent language from tdata");
            }
            refresh_snapshot(&inner, &core, false);
        });
    }

    /// Returns the currently active language code (e.g. `"en"`, `"de"`, `"pt-br"`).
    #[must_use]
    pub fn active_language_code(&self) -> String {
        self.inner
            .snapshot
            .lock()
            .map(|s| s.active_code.clone())
            .unwrap_or_else(|_| default_language_id().to_owned())
    }

    /// Returns the currently active base fallback language code (empty if none).
    #[must_use]
    pub fn base_language_code(&self) -> String {
        self.inner
            .snapshot
            .lock()
            .map(|s| s.base_code.clone())
            .unwrap_or_default()
    }

    /// Returns the current `(current_version, base_version)` tuple.
    #[must_use]
    pub fn versions(&self) -> (i32, i32) {
        self.inner
            .snapshot
            .lock()
            .map(|s| (s.current_version, s.base_version))
            .unwrap_or((0, 0))
    }

    /// Returns the cached recent languages list.
    #[must_use]
    pub fn recent_languages(&self) -> Vec<Language> {
        self.inner
            .snapshot
            .lock()
            .map(|s| s.recent_languages.clone())
            .unwrap_or_default()
    }

    /// Returns the currently pending [`LanguagePrompt`], if any.
    #[must_use]
    pub fn pending_prompt(&self) -> Option<LanguagePrompt> {
        self.inner
            .snapshot
            .lock()
            .ok()
            .and_then(|s| s.pending_prompt.clone())
    }

    /// Returns the server-suggested language code from `help.getConfig`, if any.
    #[must_use]
    pub fn suggested_language(&self) -> String {
        self.inner
            .snapshot
            .lock()
            .map(|s| s.suggested_language.clone())
            .unwrap_or_default()
    }

    /// Returns how many times `CloudManager` requested an MTProto connection re-initialization
    /// due to a language switch (`reinit_connection`).
    #[must_use]
    pub fn reinit_count(&self) -> u64 {
        self.inner
            .snapshot
            .lock()
            .map(|s| s.reinit_count)
            .unwrap_or(0)
    }

    /// Inspects the owned [`tg_lang::Instance`] synchronously.
    pub fn with_instance<R: Send>(&self, f: impl FnOnce(&LangInstance) -> R + Send) -> R {
        let inner = Arc::clone(&self.inner);
        block_on_runtime(self.inner.runtime.tokio_handle(), async move {
            let core = inner.core.lock().await;
            f(&core.instance)
        })
    }

    /// Async inspection of the owned [`tg_lang::Instance`].
    pub async fn with_instance_async<R>(&self, f: impl FnOnce(&LangInstance) -> R) -> R {
        let core = self.inner.core.lock().await;
        f(&core.instance)
    }
}

// -----------------------------------------------------------------------------
// Async core routines
// -----------------------------------------------------------------------------

async fn set_tdata_dir_async(
    inner: &LiveLangPackInner,
    tdata_dir: PathBuf,
) -> Result<Option<SerializeLoadOutcome>, LangError> {
    let mut core = inner.core.lock().await;
    let outcome = core.instance.load_from_tdata_dir(&tdata_dir)?;
    if let Ok(recents) = read_recent_languages_from_tdata(&tdata_dir) {
        core.recent_languages = recents;
    }
    core.tdata_dir = Some(tdata_dir);
    if outcome.is_some() {
        sync_to_global_if_enabled(inner, &core.instance);
    }
    refresh_snapshot(inner, &core, false);
    inner.bridge.notify_ui();
    Ok(outcome)
}

async fn fetch_languages_async(
    inner: &LiveLangPackInner,
) -> Result<Vec<LangPackLanguageInfo>, String> {
    let rpc = inner.runtime.rpc().clone();
    let mut client = RpcLangCloudClient::new(rpc);
    let mut core = inner.core.lock().await;
    core.manager.request_language_list(&mut client);
    let actions = {
        let LiveLangPackCore {
            manager, instance, ..
        } = &mut *core;
        client
            .poll_pending(manager, instance)
            .await
            .map_err(|e| e.to_string())?
    };
    let reinit = client.take_reinit_requested();
    let infos = core.manager.language_infos().to_vec();
    if !infos.is_empty() {
        core.languages_loaded = true;
    }
    process_cloud_actions(inner, &mut core, actions, None).await;
    refresh_snapshot(inner, &core, reinit);
    Ok(infos)
}

async fn apply_language_async(
    inner: &LiveLangPackInner,
    lang_code: &str,
) -> Result<Language, String> {
    let code = lang_code.trim();
    if code.is_empty() {
        return Err("Language code cannot be empty.".to_owned());
    }

    let rpc = inner.runtime.rpc().clone();
    let mut client = RpcLangCloudClient::new(rpc);
    let mut core = inner.core.lock().await;

    // Handle test pseudo-languages (`#TEST_X` / `#TEST_0`) locally with zero RPCs.
    if code == "#TEST_X" || code == "#TEST_0" {
        let test_lang = Language {
            id: code.to_owned(),
            plural_id: "en".to_owned(),
            base_id: String::new(),
            name: code.to_owned(),
            native_name: code.to_owned(),
        };
        core.instance.switch_to_id(&test_lang);
        sync_to_global_if_enabled(inner, &core.instance);
        refresh_snapshot(inner, &core, false);
        inner.bridge.notify_ui();
        return Ok(test_lang);
    }

    // Look up the language descriptor in cached cloud languages, or fetch it via
    // `langpack.getLanguage` if not in the official list (e.g. unofficial/custom cloud pack).
    let mut initial_actions = if let Some(info) = core.manager.find_language_info(code).cloned() {
        let descriptor = info.to_language();
        let LiveLangPackCore {
            manager, instance, ..
        } = &mut *core;
        manager.switch_to_language(instance, &descriptor, &mut client)
    } else if code == default_language_id() {
        let default_lang = Language::default_english();
        let LiveLangPackCore {
            manager, instance, ..
        } = &mut *core;
        manager.switch_to_language(instance, &default_lang, &mut client)
    } else {
        let LiveLangPackCore {
            manager, instance, ..
        } = &mut *core;
        manager.switch_to_language_id(instance, code, &mut client)
    };

    let polled_actions = {
        let LiveLangPackCore {
            manager, instance, ..
        } = &mut *core;
        client
            .poll_pending(manager, instance)
            .await
            .map_err(|e| e.to_string())?
    };
    initial_actions.extend(polled_actions);

    let reinit = client.take_reinit_requested();
    let rpc_err = client.take_last_error();
    let not_found = initial_actions
        .iter()
        .any(|a| matches!(a, CloudAction::LanguageNotFound));
    process_cloud_actions(inner, &mut core, initial_actions, Some(code)).await;
    sync_to_global_if_enabled(inner, &core.instance);
    refresh_snapshot(inner, &core, reinit);
    inner.bridge.notify_ui();

    if not_found {
        return Err(tr::lng_language_not_found());
    }
    if let Some(err) = rpc_err {
        return Err(err);
    }

    Ok(Language {
        id: language_id_or_default(core.instance.id()).to_owned(),
        plural_id: core.instance.plural_id().to_owned(),
        base_id: core.instance.base_id().to_owned(),
        name: core.instance.name().to_owned(),
        native_name: core.instance.native_name().to_owned(),
    })
}

async fn sync_from_config_async(
    inner: &LiveLangPackInner,
) -> Result<Option<LanguagePrompt>, String> {
    let rpc = inner.runtime.rpc().clone();
    let cfg = fetch_lang_config(&rpc).await.map_err(|e| e.to_string())?;
    apply_config_info_async(inner, &cfg).await
}

async fn apply_config_info_async(
    inner: &LiveLangPackInner,
    cfg: &LangConfigInfo,
) -> Result<Option<LanguagePrompt>, String> {
    let rpc = inner.runtime.rpc().clone();
    let mut client = RpcLangCloudClient::new(rpc.clone());
    let mut core = inner.core.lock().await;
    core.config_checked = true;

    let mut actions = {
        let LiveLangPackCore {
            manager, instance, ..
        } = &mut *core;
        let mut acts =
            manager.set_suggested_language(instance, &cfg.suggested_lang_code, &mut client);
        manager.set_current_versions(
            instance,
            cfg.lang_pack_version,
            cfg.base_lang_pack_version,
            &mut client,
        );
        let polled = client
            .poll_pending(manager, instance)
            .await
            .map_err(|e| e.to_string())?;
        acts.extend(polled);
        acts
    };

    let reinit = client.take_reinit_requested();
    if !core.manager.language_infos().is_empty() {
        core.languages_loaded = true;
    }

    // If `OfferSwitchLanguage` fired, optionally fetch `lng_switch_to_this` in the suggested
    // language (`intro_widget.cpp:297-307`) to localize the CTA label.
    let offered_lang = actions.iter().find_map(|a| match a {
        CloudAction::OfferSwitchLanguage(lang) => Some(lang.clone()),
        _ => None,
    });
    let mut switch_cta = String::new();
    if let Some(ref lang) = offered_lang
        && let Ok(strings) = fetch_strings(
            &rpc,
            CLOUD_LANG_PACK_NAME,
            &lang.id,
            vec!["lng_switch_to_this".to_owned()],
        )
        .await
    {
        let parsed = LangInstance::parse_strings(&strings);
        if let Some(cta) = parsed.get(&tg_lang::key_ids::lng_switch_to_this) {
            switch_cta = cta.clone();
        }
    }

    // Retain all non-offer actions for standard processing; build the rich SuggestedLanguage prompt.
    actions.retain(|a| !matches!(a, CloudAction::OfferSwitchLanguage(_)));
    process_cloud_actions(inner, &mut core, actions, None).await;

    if let Some(language) = offered_lang {
        let prompt_text = format!(
            "Do you want to switch your language to {}? You can always change your language in Settings.",
            language.native_name
        );
        core.pending_prompt = Some(LanguagePrompt::SuggestedLanguage {
            language,
            switch_cta,
            prompt_text,
        });
    }

    sync_to_global_if_enabled(inner, &core.instance);
    let prompt_out = core.pending_prompt.clone();
    refresh_snapshot(inner, &core, reinit);
    inner.bridge.notify_ui();
    Ok(prompt_out)
}

async fn handle_set_language_link_async(
    inner: &LiveLangPackInner,
    url: &str,
) -> Result<SetLanguageLinkOutcome, String> {
    let Some(parsed) = parse_set_language_link(url) else {
        return Err(format!("Invalid language link: {url}"));
    };
    match parsed {
        SetLanguageLink::OpenLanguageBox => Ok(SetLanguageLinkOutcome::OpenLanguageBox),
        SetLanguageLink::SwitchLanguage(lang_code) => {
            let rpc = inner.runtime.rpc().clone();
            let mut client = RpcLangCloudClient::new(rpc);
            let mut core = inner.core.lock().await;

            let mut actions = {
                let LiveLangPackCore {
                    manager, instance, ..
                } = &mut *core;
                let mut acts = manager.switch_with_warning(instance, &lang_code, &mut client);
                let polled = client
                    .poll_pending(manager, instance)
                    .await
                    .map_err(|e| e.to_string())?;
                acts.extend(polled);
                acts
            };

            let reinit = client.take_reinit_requested();
            let rpc_err = client.take_last_error();
            let mut prompt_opt = None;
            for act in actions.drain(..) {
                match act {
                    CloudAction::ConfirmSwitchWithWarning(info)
                    | CloudAction::LanguageNotReady(info) => {
                        prompt_opt = Some(build_switch_confirmation_prompt(&info));
                    }
                    CloudAction::AlreadyUsingLanguage => {
                        prompt_opt = Some(LanguagePrompt::AlreadyUsing {
                            lang_code: lang_code.clone(),
                            message: tr::lng_language_already(),
                        });
                    }
                    CloudAction::LanguageNotFound => {
                        prompt_opt = Some(LanguagePrompt::NotFound {
                            lang_code: lang_code.clone(),
                            message: tr::lng_language_not_found(),
                        });
                    }
                    other => {
                        process_cloud_actions(inner, &mut core, vec![other], Some(&lang_code))
                            .await;
                    }
                }
            }

            if prompt_opt.is_none()
                && let Some(err) = rpc_err
            {
                refresh_snapshot(inner, &core, reinit);
                return Err(err);
            }

            let prompt = prompt_opt.unwrap_or_else(|| LanguagePrompt::NotFound {
                lang_code: lang_code.clone(),
                message: tr::lng_language_not_found(),
            });
            core.pending_prompt = Some(prompt.clone());
            refresh_snapshot(inner, &core, reinit);
            inner.bridge.notify_ui();
            Ok(SetLanguageLinkOutcome::Prompt(Box::new(prompt)))
        }
    }
}

async fn confirm_pending_prompt_async(
    inner: &LiveLangPackInner,
) -> Result<Option<Language>, String> {
    let rpc = inner.runtime.rpc().clone();
    let mut client = RpcLangCloudClient::new(rpc);
    let mut core = inner.core.lock().await;

    let target_lang = match core.pending_prompt.take() {
        Some(LanguagePrompt::ConfirmLinkSwitch { info, .. }) => Some(info.to_language()),
        Some(LanguagePrompt::SuggestedLanguage { language, .. }) => Some(language),
        _ => None,
    };

    let Some(language) = target_lang else {
        refresh_snapshot(inner, &core, false);
        return Ok(None);
    };

    let actions = {
        let LiveLangPackCore {
            manager, instance, ..
        } = &mut *core;
        let mut acts = manager.finalize_switch(instance, &language, &mut client);
        let polled = client
            .poll_pending(manager, instance)
            .await
            .map_err(|e| e.to_string())?;
        acts.extend(polled);
        acts
    };

    let reinit = client.take_reinit_requested();
    process_cloud_actions(inner, &mut core, actions, Some(&language.id)).await;
    sync_to_global_if_enabled(inner, &core.instance);
    refresh_snapshot(inner, &core, reinit);
    inner.bridge.notify_ui();
    Ok(Some(language))
}

async fn dismiss_pending_prompt_async(inner: &LiveLangPackInner) {
    let rpc = inner.runtime.rpc().clone();
    let mut client = RpcLangCloudClient::new(rpc);
    let mut core = inner.core.lock().await;
    let was_suggested = matches!(
        core.pending_prompt.take(),
        Some(LanguagePrompt::SuggestedLanguage { .. })
    );
    let mut reinit = false;
    if was_suggested {
        let actions = {
            let LiveLangPackCore {
                manager, instance, ..
            } = &mut *core;
            manager.decline_offered_switch(instance, &mut client)
        };
        reinit = client.take_reinit_requested();
        process_cloud_actions(inner, &mut core, actions, None).await;
        sync_to_global_if_enabled(inner, &core.instance);
    }
    refresh_snapshot(inner, &core, reinit);
    inner.bridge.notify_ui();
}

async fn apply_incoming_difference_async(
    inner: &LiveLangPackInner,
    diff: LangPackDifference,
) -> Result<(), String> {
    let rpc = inner.runtime.rpc().clone();
    let mut client = RpcLangCloudClient::new(rpc);
    let mut core = inner.core.lock().await;

    let actions = {
        let LiveLangPackCore {
            manager, instance, ..
        } = &mut *core;
        let mut acts = manager
            .apply_lang_pack_difference(instance, &diff, &mut client)
            .map_err(|e| e.to_string())?;
        // Drain any gap-recovery RPCs (`getDifference` -> fallback `getLangPack`) queued by manager.
        let polled = client
            .poll_pending(manager, instance)
            .await
            .map_err(|e| e.to_string())?;
        acts.extend(polled);
        acts
    };

    let reinit = client.take_reinit_requested();
    let changed = !actions.is_empty();
    process_cloud_actions(inner, &mut core, actions, Some(&diff.lang_code)).await;
    if changed {
        sync_to_global_if_enabled(inner, &core.instance);
    }
    refresh_snapshot(inner, &core, reinit);
    if changed {
        inner.bridge.notify_ui();
    }
    Ok(())
}

async fn handle_lang_pack_too_long_async(
    inner: &LiveLangPackInner,
    lang_code: &str,
) -> Result<(), String> {
    let rpc = inner.runtime.rpc().clone();
    let mut client = RpcLangCloudClient::new(rpc);
    let mut core = inner.core.lock().await;

    let actions = {
        let LiveLangPackCore {
            manager, instance, ..
        } = &mut *core;
        manager.request_lang_pack_difference_by_id(instance, lang_code, &mut client);
        client
            .poll_pending(manager, instance)
            .await
            .map_err(|e| e.to_string())?
    };

    let reinit = client.take_reinit_requested();
    let changed = !actions.is_empty();
    process_cloud_actions(inner, &mut core, actions, Some(lang_code)).await;
    if changed {
        sync_to_global_if_enabled(inner, &core.instance);
    }
    refresh_snapshot(inner, &core, reinit);
    if changed {
        inner.bridge.notify_ui();
    }
    Ok(())
}

async fn process_cloud_actions(
    _inner: &LiveLangPackInner,
    core: &mut LiveLangPackCore,
    actions: Vec<CloudAction>,
    context_lang_code: Option<&str>,
) {
    let mut should_write = false;
    for action in actions {
        match action {
            CloudAction::WriteLangPack => {
                should_write = true;
            }
            CloudAction::PushRecentLanguage(lang) => {
                if !lang.id.starts_with('#') {
                    core.recent_languages.retain(|v| v.id != lang.id);
                    core.recent_languages.insert(0, lang.clone());
                    if let Some(ref tdata) = core.tdata_dir
                        && let Err(err) = push_recent_language_to_tdata(tdata, &lang)
                    {
                        warn!(error = %err, "failed to persist recent language to tdata");
                    }
                }
            }
            CloudAction::ConfirmSwitchWithWarning(info) | CloudAction::LanguageNotReady(info) => {
                core.pending_prompt = Some(build_switch_confirmation_prompt(&info));
            }
            CloudAction::AlreadyUsingLanguage => {
                core.pending_prompt = Some(LanguagePrompt::AlreadyUsing {
                    lang_code: context_lang_code.unwrap_or_default().to_owned(),
                    message: tr::lng_language_already(),
                });
            }
            CloudAction::LanguageNotFound => {
                core.pending_prompt = Some(LanguagePrompt::NotFound {
                    lang_code: context_lang_code.unwrap_or_default().to_owned(),
                    message: tr::lng_language_not_found(),
                });
            }
            CloudAction::OfferSwitchLanguage(language) => {
                let prompt_text = format!(
                    "Do you want to switch your language to {}? You can always change your language in Settings.",
                    language.native_name
                );
                core.pending_prompt = Some(LanguagePrompt::SuggestedLanguage {
                    language,
                    switch_cta: String::new(),
                    prompt_text,
                });
            }
            CloudAction::LanguageListChanged
            | CloudAction::FirstLanguageSuggestion(_)
            | CloudAction::RestartApp
            | CloudAction::PromptCustomFile
            | CloudAction::ConfirmRestartSwitch { .. }
            | CloudAction::ValueForLang { .. } => {}
        }
    }

    if should_write
        && let Some(ref tdata) = core.tdata_dir
        && let Err(err) = persist_pack(tdata, &core.instance)
    {
        warn!(error = %err, "failed to persist cloud langpack to tdata");
    }
}

fn persist_pack(tdata_dir: &Path, instance: &LangInstance) -> Result<(), LangError> {
    instance.save_to_tdata_dir(tdata_dir)
}

fn sync_to_global_if_enabled(inner: &LiveLangPackInner, instance: &LangInstance) {
    if inner.sync_global.load(Ordering::Acquire) {
        tg_lang::with_current_mut(|global| {
            global.copy_from(instance);
        });
    }
}

fn refresh_snapshot(inner: &LiveLangPackInner, core: &LiveLangPackCore, reinit_requested: bool) {
    if let Ok(mut snap) = inner.snapshot.lock() {
        let next_reinit = if reinit_requested {
            snap.reinit_count.saturating_add(1)
        } else {
            snap.reinit_count
        };
        *snap = LiveLangPackSnapshot::from_core(core, next_reinit);
    }
}

fn fallback_english_info() -> LangPackLanguageInfo {
    LangPackLanguageInfo {
        lang_code: "en".to_owned(),
        plural_code: "en".to_owned(),
        base_lang_code: None,
        name: "English".to_owned(),
        native_name: "English".to_owned(),
        official: true,
        rtl: false,
        beta: false,
        strings_count: 100,
        translated_count: 100,
        translations_url: String::new(),
    }
}
