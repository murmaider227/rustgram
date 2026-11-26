//! Server-suggested language for the intro's language link.
//!
//! Port origin: `Telegram/SourceFiles/intro/intro_widget.cpp` (`Widget::createLanguageLink`) and
//! `Telegram/SourceFiles/lang/lang_cloud_manager.cpp` (`CloudManager::setSuggestedLanguage`,
//! `CloudManager::switchToLanguage`).
//!
//! The link is offered only when `help.getConfig` suggests a language other than the default
//! English and `langpack.getStrings` returns that language's `lng_switch_to_this`; the link text
//! is that server string. Without a suggestion there is no link.

use tg_lang::{
    CLOUD_LANG_PACK_NAME, Instance, LangPackDifference, Language, Pack, key_ids, with_current_mut,
};
use tg_tl::{enums as tl_enums, functions as tl_fns};

use std::sync::Arc;

use crate::intro::{DynIntroRpc, IntroWidget};

/// A language the server suggests, with the link text in that language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageSuggestion {
    /// Suggested language code (`suggested_lang_code`).
    pub lang_id: String,
    /// `lng_switch_to_this` in the suggested language (e.g. "Continue in German" translated).
    pub label: String,
}

/// A downloaded cloud language pack ready to become the active language.
#[derive(Debug, Clone)]
pub struct DownloadedLanguage {
    /// Language descriptor from `langpack.getLanguage`.
    pub language: Language,
    /// Full pack from `langpack.getLangPack`.
    pub pack: LangPackDifference,
    /// Base pack (`base_lang_code`), when the language has one.
    pub base_pack: Option<LangPackDifference>,
}

/// Returns the suggested language code when it differs from the default English
/// (`CloudManager::setSuggestedLanguage`).
#[must_use]
pub fn effective_suggestion(suggested_lang_code: &str) -> Option<&str> {
    if suggested_lang_code.is_empty()
        || tg_lang::language_id_or_default(suggested_lang_code) == tg_lang::default_language_id()
    {
        None
    } else {
        Some(suggested_lang_code)
    }
}

/// Asks the server for a suggested language and its `lng_switch_to_this` text.
///
/// Returns `None` when the server suggests nothing (or English), when a request fails, or when
/// the strings for the suggested language do not include `lng_switch_to_this`.
pub async fn load_language_suggestion(rpc: &dyn DynIntroRpc) -> Option<LanguageSuggestion> {
    let tl_enums::Config::Config(cfg) = rpc.help_get_config().await.ok()?;
    let code = effective_suggestion(cfg.suggested_lang_code.as_deref().unwrap_or_default())?;
    let strings = rpc
        .langpack_get_strings(tl_fns::langpack::GetStrings {
            lang_pack: CLOUD_LANG_PACK_NAME.to_owned(),
            lang_code: code.to_owned(),
            keys: vec!["lng_switch_to_this".to_owned()],
        })
        .await
        .ok()?;
    let parsed = Instance::parse_strings(
        &strings
            .iter()
            .map(tg_api::lang::lang_pack_string_from_tl)
            .collect::<Vec<_>>(),
    );
    let label = parsed.get(&key_ids::lng_switch_to_this)?.clone();
    if label.trim().is_empty() {
        return None;
    }
    Some(LanguageSuggestion {
        lang_id: code.to_owned(),
        label,
    })
}

/// Downloads the cloud pack for `lang_id` (`langpack.getLanguage` + `langpack.getLangPack`, and
/// the base pack when the language has one).
pub async fn download_language(
    rpc: &dyn DynIntroRpc,
    lang_id: &str,
) -> Result<DownloadedLanguage, String> {
    let info = rpc
        .langpack_get_language(tl_fns::langpack::GetLanguage {
            lang_pack: CLOUD_LANG_PACK_NAME.to_owned(),
            lang_code: lang_id.to_owned(),
        })
        .await
        .map_err(|e| e.to_string())?;
    let language = tg_api::lang::lang_pack_language_struct_from_tl(&info).to_language();
    let pack = rpc
        .langpack_get_lang_pack(tl_fns::langpack::GetLangPack {
            lang_pack: CLOUD_LANG_PACK_NAME.to_owned(),
            lang_code: lang_id.to_owned(),
        })
        .await
        .map_err(|e| e.to_string())?;
    let base_pack = if language.base_id.is_empty() {
        None
    } else {
        let base = rpc
            .langpack_get_lang_pack(tl_fns::langpack::GetLangPack {
                lang_pack: CLOUD_LANG_PACK_NAME.to_owned(),
                lang_code: language.base_id.clone(),
            })
            .await
            .map_err(|e| e.to_string())?;
        Some(tg_api::lang::lang_pack_difference_struct_from_tl(&base))
    };
    Ok(DownloadedLanguage {
        language,
        pack: tg_api::lang::lang_pack_difference_struct_from_tl(&pack),
        base_pack,
    })
}

/// Makes `downloaded` the active global language and saves it to `tdata` when a `tdata`
/// directory is configured (`Lang::Instance::switchToId` + `applyDifference`).
pub fn apply_downloaded_language(downloaded: &DownloadedLanguage) -> Result<(), String> {
    with_current_mut(|inst| {
        apply_downloaded_language_to(inst, downloaded)?;
        if let Some(dir) = tg_lang::global_tdata_dir() {
            inst.save_to_tdata_dir(&dir).map_err(|e| e.to_string())?;
        }
        Ok(())
    })
}

/// Makes `downloaded` the active language of `inst`.
pub fn apply_downloaded_language_to(
    inst: &mut Instance,
    downloaded: &DownloadedLanguage,
) -> Result<(), String> {
    inst.switch_to_id(&downloaded.language);
    if let Some(base) = &downloaded.base_pack {
        inst.apply_difference(Pack::Base, base)
            .map_err(|e| e.to_string())?;
    }
    inst.apply_difference(Pack::Current, &downloaded.pack)
        .map_err(|e| e.to_string())
}

/// Switches the global language back to the built-in English pack and saves it when a `tdata`
/// directory is configured.
pub fn switch_to_default_language() {
    with_current_mut(|inst| {
        inst.switch_to_id(&Language::default_english());
        if let Some(dir) = tg_lang::global_tdata_dir()
            && let Err(err) = inst.save_to_tdata_dir(&dir)
        {
            tracing::warn!(%err, "Could not save the intro language switch");
        }
    });
}

type TaskRx<T> = std::sync::mpsc::Receiver<T>;

/// Background requests behind the intro's language link.
#[derive(Debug, Default)]
pub(crate) struct LanguageLinkTasks {
    requested: bool,
    suggestion_rx: Option<TaskRx<Option<LanguageSuggestion>>>,
    switch_rx: Option<TaskRx<Result<DownloadedLanguage, String>>>,
}

/// Runs `task` on its own thread with a private runtime and sends the result back, waking the
/// UI. Returns `None` when the thread could not start.
fn spawn_rpc_task<T, F, Fut>(
    name: &str,
    rpc: Arc<dyn DynIntroRpc>,
    ctx: Option<egui::Context>,
    task: F,
) -> Option<TaskRx<T>>
where
    T: Send + 'static,
    F: FnOnce(Arc<dyn DynIntroRpc>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = T>,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            let _ = tx.send(rt.block_on(task(rpc)));
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        })
        .ok()
        .map(|_| rx)
}

impl IntroWidget {
    /// Hides the language link for good (tdesktop shows none when an account is already
    /// logged in, e.g. in the add-account intro) and skips the suggestion request.
    pub fn disable_language_link(&mut self) {
        self.start.language_link_enabled = false;
        self.language_link.requested = true;
        self.language_link.suggestion_rx = None;
    }

    /// Label of the language link, or `None` when the intro shows no link.
    #[must_use]
    pub fn language_link_label(&self) -> Option<String> {
        self.start.language_link_label()
    }

    /// Requests the server's language suggestion once (`help.getConfig`, then
    /// `langpack.getStrings` for `lng_switch_to_this`). The offline fake client answers
    /// synchronously so tests stay deterministic; a real client answers on a background thread.
    pub fn request_language_suggestion(&mut self, ctx: Option<&egui::Context>) {
        if self.language_link.requested {
            return;
        }
        self.language_link.requested = true;
        if self.fake_rpc.is_some() {
            let rpc = Arc::clone(&self.invoker.0);
            self.start.suggestion = self
                .runtime
                .block_on(load_language_suggestion(rpc.as_ref()));
            return;
        }
        self.language_link.suggestion_rx = spawn_rpc_task(
            "intro-language-suggestion",
            Arc::clone(&self.invoker.0),
            ctx.cloned(),
            |rpc| async move { load_language_suggestion(rpc.as_ref()).await },
        );
    }

    /// Applies finished background language requests.
    pub fn poll_language_link(&mut self) {
        if let Some(rx) = &self.language_link.suggestion_rx {
            match rx.try_recv() {
                Ok(suggestion) => {
                    self.start.suggestion = suggestion;
                    self.language_link.suggestion_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.language_link.suggestion_rx = None;
                }
            }
        }
        if let Some(rx) = &self.language_link.switch_rx {
            match rx.try_recv() {
                Ok(result) => {
                    self.language_link.switch_rx = None;
                    finish_language_switch(result);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.language_link.switch_rx = None;
                }
            }
        }
    }

    /// Handles a click on the language link (`CloudManager::switchToLanguage`): back to English
    /// when another language is active, otherwise downloads and applies the suggested language.
    pub fn click_language_link(&mut self, ctx: Option<&egui::Context>) {
        let current_id =
            tg_lang::with_current(|inst| tg_lang::language_id_or_default(inst.id()).to_owned());
        if current_id != tg_lang::default_language_id() {
            switch_to_default_language();
            return;
        }
        let Some(lang_id) = self.start.suggestion.as_ref().map(|s| s.lang_id.clone()) else {
            return;
        };
        if self.language_link.switch_rx.is_some() {
            return;
        }
        if self.fake_rpc.is_some() {
            let rpc = Arc::clone(&self.invoker.0);
            let result = self
                .runtime
                .block_on(download_language(rpc.as_ref(), &lang_id));
            finish_language_switch(result);
            return;
        }
        self.language_link.switch_rx = spawn_rpc_task(
            "intro-language-switch",
            Arc::clone(&self.invoker.0),
            ctx.cloned(),
            move |rpc| async move { download_language(rpc.as_ref(), &lang_id).await },
        );
    }
}

fn finish_language_switch(result: Result<DownloadedLanguage, String>) {
    if let Err(err) = result.and_then(|downloaded| apply_downloaded_language(&downloaded)) {
        tracing::warn!(%err, "Could not switch the intro language");
    }
}
