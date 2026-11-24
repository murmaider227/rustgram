//! Cloud language pack (`langpack.*`) and countries list (`help.getCountriesList`) integration
//! with `tg-lang`.
//!
//! Port origin: `lang/lang_cloud_manager.cpp`, `countries/countries_manager.cpp`.

use std::collections::{HashSet, VecDeque};

use tg_lang::{
    CloudAction, CloudCountriesList, CloudCountry, CloudCountryCode, CloudManager,
    CountriesInstance, Instance as LangInstance, LangCloudClient, LangPackDifference,
    LangPackLanguageInfo, LangPackString, Pack, RequestId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::error::ApiError;
use crate::invoker::Invoker;
use crate::rpc::InvocationError;

// ---------------------------------------------------------------------------
// TL -> tg-lang conversions
// ---------------------------------------------------------------------------

/// Converts a TL `LangPackLanguage` into `tg_lang::LangPackLanguageInfo`.
#[must_use]
pub fn lang_pack_language_from_tl(lang: &tl_enums::LangPackLanguage) -> LangPackLanguageInfo {
    let tl_enums::LangPackLanguage::LangPackLanguage(l) = lang;
    lang_pack_language_struct_from_tl(l)
}

/// Converts a TL `types::LangPackLanguage` struct into `tg_lang::LangPackLanguageInfo`.
#[must_use]
pub fn lang_pack_language_struct_from_tl(l: &tl_types::LangPackLanguage) -> LangPackLanguageInfo {
    LangPackLanguageInfo {
        official: l.official,
        rtl: l.rtl,
        beta: l.beta,
        name: l.name.clone(),
        native_name: l.native_name.clone(),
        lang_code: l.lang_code.clone(),
        base_lang_code: l.base_lang_code.clone(),
        plural_code: l.plural_code.clone(),
        strings_count: l.strings_count,
        translated_count: l.translated_count,
        translations_url: l.translations_url.clone(),
    }
}

/// Converts a TL `LangPackString` into `tg_lang::LangPackString`.
#[must_use]
pub fn lang_pack_string_from_tl(s: &tl_enums::LangPackString) -> LangPackString {
    match s {
        tl_enums::LangPackString::LangPackString(n) => LangPackString::Normal {
            key: n.key.clone(),
            value: n.value.clone(),
        },
        tl_enums::LangPackString::Pluralized(p) => LangPackString::Pluralized {
            key: p.key.clone(),
            zero_value: p.zero_value.clone(),
            one_value: p.one_value.clone(),
            two_value: p.two_value.clone(),
            few_value: p.few_value.clone(),
            many_value: p.many_value.clone(),
            other_value: p.other_value.clone(),
        },
        tl_enums::LangPackString::Deleted(d) => LangPackString::Deleted { key: d.key.clone() },
    }
}

/// Converts a TL `LangPackDifference` into `tg_lang::LangPackDifference`.
#[must_use]
pub fn lang_pack_difference_from_tl(diff: &tl_enums::LangPackDifference) -> LangPackDifference {
    let tl_enums::LangPackDifference::LangPackDifference(d) = diff;
    lang_pack_difference_struct_from_tl(d)
}

/// Converts a TL `types::LangPackDifference` struct into `tg_lang::LangPackDifference`.
#[must_use]
pub fn lang_pack_difference_struct_from_tl(d: &tl_types::LangPackDifference) -> LangPackDifference {
    LangPackDifference {
        lang_code: d.lang_code.clone(),
        from_version: d.from_version,
        version: d.version,
        strings: d.strings.iter().map(lang_pack_string_from_tl).collect(),
    }
}

/// Converts a TL `help.CountriesList` into `tg_lang::CloudCountriesList`.
#[must_use]
pub fn countries_list_from_tl(list: &tl_enums::help::CountriesList) -> CloudCountriesList {
    match list {
        tl_enums::help::CountriesList::NotModified => CloudCountriesList::NotModified,
        tl_enums::help::CountriesList::CountriesList(c) => CloudCountriesList::Modified {
            hash: c.hash,
            countries: c
                .countries
                .iter()
                .map(|country| {
                    let tl_enums::help::Country::Country(co) = country;
                    CloudCountry {
                        hidden: co.hidden,
                        iso2: co.iso2.clone(),
                        default_name: co.default_name.clone(),
                        name: co.name.clone(),
                        country_codes: co
                            .country_codes
                            .iter()
                            .map(|cc| {
                                let tl_enums::help::CountryCode::CountryCode(code) = cc;
                                CloudCountryCode {
                                    country_code: code.country_code.clone(),
                                    prefixes: code.prefixes.clone(),
                                    patterns: code.patterns.clone(),
                                }
                            })
                            .collect(),
                    }
                })
                .collect(),
        },
    }
}

// ---------------------------------------------------------------------------
// Direct async RPC helpers
// ---------------------------------------------------------------------------

/// Language pack fields extracted from `help.getConfig` (`MTP::Instance::Private::configLoadDone`
/// in `mtproto/mtp_instance.cpp:955-959`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LangConfigInfo {
    /// Server-suggested interface language code (`suggested_lang_code`, empty if omitted).
    pub suggested_lang_code: String,
    /// Server's current version of the active cloud language pack (`lang_pack_version`).
    pub lang_pack_version: i32,
    /// Server's current version of the base fallback language pack (`base_lang_pack_version`).
    pub base_lang_pack_version: i32,
}

impl LangConfigInfo {
    /// Extracts language pack fields from a TL `Config` object.
    #[must_use]
    pub fn from_tl_config(cfg: &tl_types::Config) -> Self {
        Self {
            suggested_lang_code: cfg.suggested_lang_code.clone().unwrap_or_default(),
            lang_pack_version: cfg.lang_pack_version.unwrap_or(0),
            base_lang_pack_version: cfg.base_lang_pack_version.unwrap_or(0),
        }
    }
}

/// Fetches server configuration via `help.getConfig` and extracts its language pack fields.
pub async fn fetch_lang_config<C: crate::rpc::RpcClient>(
    rpc: &C,
) -> Result<LangConfigInfo, ApiError> {
    let tl_enums::Config::Config(cfg) = rpc
        .invoke(&tl_fns::help::GetConfig)
        .await
        .map_err(|e| ApiError::Invocation(e.into()))?;
    Ok(LangConfigInfo::from_tl_config(&cfg))
}

/// Fetches the list of available cloud languages via `langpack.getLanguages`.
pub async fn fetch_languages<I: Invoker>(
    invoker: &I,
    lang_pack: &str,
) -> Result<Vec<LangPackLanguageInfo>, ApiError> {
    let list = invoker
        .langpack_get_languages(tl_fns::langpack::GetLanguages {
            lang_pack: lang_pack.to_owned(),
        })
        .await?;
    Ok(list.iter().map(lang_pack_language_from_tl).collect())
}

/// Fetches a full cloud language pack via `langpack.getLangPack`.
pub async fn fetch_lang_pack<I: Invoker>(
    invoker: &I,
    lang_pack: &str,
    lang_code: &str,
) -> Result<LangPackDifference, ApiError> {
    let diff = invoker
        .langpack_get_lang_pack(tl_fns::langpack::GetLangPack {
            lang_pack: lang_pack.to_owned(),
            lang_code: lang_code.to_owned(),
        })
        .await?;
    Ok(lang_pack_difference_struct_from_tl(&diff))
}

/// Fetches an incremental cloud language pack diff via `langpack.getDifference`.
pub async fn fetch_lang_pack_difference<I: Invoker>(
    invoker: &I,
    lang_pack: &str,
    lang_code: &str,
    from_version: i32,
) -> Result<LangPackDifference, ApiError> {
    let diff = invoker
        .langpack_get_difference(tl_fns::langpack::GetDifference {
            lang_pack: lang_pack.to_owned(),
            lang_code: lang_code.to_owned(),
            from_version,
        })
        .await?;
    Ok(lang_pack_difference_struct_from_tl(&diff))
}

/// Fetches metadata for a single language via `langpack.getLanguage`.
pub async fn fetch_language<I: Invoker>(
    invoker: &I,
    lang_pack: &str,
    lang_code: &str,
) -> Result<LangPackLanguageInfo, ApiError> {
    let lang = invoker
        .langpack_get_language(tl_fns::langpack::GetLanguage {
            lang_pack: lang_pack.to_owned(),
            lang_code: lang_code.to_owned(),
        })
        .await?;
    Ok(lang_pack_language_struct_from_tl(&lang))
}

/// Fetches specific language strings via `langpack.getStrings`.
pub async fn fetch_strings<I: Invoker>(
    invoker: &I,
    lang_pack: &str,
    lang_code: &str,
    keys: Vec<String>,
) -> Result<Vec<LangPackString>, ApiError> {
    let strings = invoker
        .langpack_get_strings(tl_fns::langpack::GetStrings {
            lang_pack: lang_pack.to_owned(),
            lang_code: lang_code.to_owned(),
            keys,
        })
        .await?;
    Ok(strings.iter().map(lang_pack_string_from_tl).collect())
}

/// Fetches the countries list via `help.getCountriesList`.
pub async fn fetch_countries_list<I: Invoker>(
    invoker: &I,
    lang_code: &str,
    hash: i32,
) -> Result<CloudCountriesList, ApiError> {
    let resp = invoker
        .help_get_countries_list(tl_fns::help::GetCountriesList {
            lang_code: lang_code.to_owned(),
            hash,
        })
        .await?;
    Ok(countries_list_from_tl(&resp))
}

/// Fetches `help.getCountriesList` using `countries.hash()` and applies the update to `countries`.
///
/// Returns `Ok(true)` if the server returned an updated countries list (caller should persist cache),
/// or `Ok(false)` if `help.countriesListNotModified`.
pub async fn sync_countries_list<I: Invoker>(
    invoker: &I,
    countries: &mut CountriesInstance,
    lang_code: &str,
) -> Result<bool, ApiError> {
    let list = fetch_countries_list(invoker, lang_code, countries.hash()).await?;
    Ok(countries.apply_cloud_list(list))
}

// ---------------------------------------------------------------------------
// RpcLangCloudClient: tg_lang::LangCloudClient bridge over Invoker/RpcClient
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum PendingLangRequest {
    Languages {
        lang_pack: String,
    },
    LangPack {
        lang_pack: String,
        lang_code: String,
    },
    Difference {
        lang_pack: String,
        lang_code: String,
        from_version: i32,
    },
    Language {
        lang_pack: String,
        lang_code: String,
    },
    Strings {
        lang_pack: String,
        lang_code: String,
        keys: Vec<String>,
    },
}

/// Bridges `tg_lang::LangCloudClient`'s synchronous request-ID contract to an async
/// [`Invoker`] / [`crate::RpcClient`].
///
/// Pass `&mut client` to [`tg_lang::CloudManager`] methods, then call [`Self::poll_pending`]
/// to execute all queued RPCs and deliver their responses back to [`CloudManager`] and
/// [`LangInstance`].
#[derive(Debug)]
pub struct RpcLangCloudClient<I> {
    invoker: I,
    connected: bool,
    next_request_id: RequestId,
    pending: VecDeque<(RequestId, PendingLangRequest)>,
    cancelled: HashSet<RequestId>,
    reinit_requested: bool,
    last_error: Option<String>,
}

impl<I> RpcLangCloudClient<I> {
    /// Creates a new `RpcLangCloudClient` wrapping `invoker` (initially connected).
    #[must_use]
    pub fn new(invoker: I) -> Self {
        Self {
            invoker,
            connected: true,
            next_request_id: 1,
            pending: VecDeque::new(),
            cancelled: HashSet::new(),
            reinit_requested: false,
            last_error: None,
        }
    }

    /// Returns and clears the last RPC error encountered during [`Self::poll_pending`], if any.
    pub fn take_last_error(&mut self) -> Option<String> {
        self.last_error.take()
    }

    /// Sets whether the underlying MTProto session is currently connected.
    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
    }

    /// Returns a reference to the wrapped invoker.
    #[must_use]
    pub fn invoker(&self) -> &I {
        &self.invoker
    }

    /// Returns the number of queued (unpolled) requests.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Returns and clears the flag set when `CloudManager` called `reinit_connection()`.
    pub fn take_reinit_requested(&mut self) -> bool {
        std::mem::take(&mut self.reinit_requested)
    }

    fn enqueue(&mut self, req: PendingLangRequest) -> RequestId {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1).max(1);
        self.pending.push_back((id, req));
        id
    }
}

impl<I> LangCloudClient for RpcLangCloudClient<I> {
    fn is_connected(&self) -> bool {
        self.connected
    }

    fn request_languages(&mut self, lang_pack: &str) -> RequestId {
        self.enqueue(PendingLangRequest::Languages {
            lang_pack: lang_pack.to_owned(),
        })
    }

    fn request_lang_pack(&mut self, lang_pack: &str, lang_code: &str) -> RequestId {
        self.enqueue(PendingLangRequest::LangPack {
            lang_pack: lang_pack.to_owned(),
            lang_code: lang_code.to_owned(),
        })
    }

    fn request_difference(
        &mut self,
        lang_pack: &str,
        lang_code: &str,
        from_version: i32,
    ) -> RequestId {
        self.enqueue(PendingLangRequest::Difference {
            lang_pack: lang_pack.to_owned(),
            lang_code: lang_code.to_owned(),
            from_version,
        })
    }

    fn request_language(&mut self, lang_pack: &str, lang_code: &str) -> RequestId {
        self.enqueue(PendingLangRequest::Language {
            lang_pack: lang_pack.to_owned(),
            lang_code: lang_code.to_owned(),
        })
    }

    fn request_strings(
        &mut self,
        lang_pack: &str,
        lang_code: &str,
        keys: Vec<String>,
    ) -> RequestId {
        self.enqueue(PendingLangRequest::Strings {
            lang_pack: lang_pack.to_owned(),
            lang_code: lang_code.to_owned(),
            keys,
        })
    }

    fn cancel_request(&mut self, request_id: RequestId) {
        if request_id > 0 {
            self.cancelled.insert(request_id);
        }
    }

    fn reinit_connection(&mut self) {
        self.reinit_requested = true;
    }
}

impl<I: Invoker> RpcLangCloudClient<I> {
    /// Executes all currently queued `langpack.*` requests against the underlying [`Invoker`]
    /// and delivers their results into `manager` and `langpack`.
    ///
    /// Any follow-up requests queued by `manager` while applying responses (for instance,
    /// switching to a language after `getLanguage` returns) are executed in subsequent
    /// iterations of the drain loop until the queue is empty.
    pub async fn poll_pending(
        &mut self,
        manager: &mut CloudManager,
        langpack: &mut LangInstance,
    ) -> Result<Vec<CloudAction>, ApiError> {
        let mut all_actions = Vec::new();
        while let Some((req_id, req)) = self.pending.pop_front() {
            if self.cancelled.remove(&req_id) {
                continue;
            }
            match req {
                PendingLangRequest::Languages { lang_pack } => {
                    match fetch_languages(&self.invoker, &lang_pack).await {
                        Ok(infos) => {
                            all_actions.extend(manager.on_languages_received(&infos));
                        }
                        Err(err) => {
                            self.last_error = Some(err.to_string());
                            manager.on_languages_failed();
                        }
                    }
                }
                PendingLangRequest::LangPack {
                    lang_pack,
                    lang_code,
                } => {
                    let pack = manager.pack_type_from_id(langpack, &lang_code);
                    match fetch_lang_pack(&self.invoker, &lang_pack, &lang_code).await {
                        Ok(diff) => {
                            let actions = manager
                                .apply_lang_pack_difference_from_source(
                                    langpack, &diff, false, self,
                                )
                                .map_err(|e| ApiError::ServerError(e.to_string()))?;
                            all_actions.extend(actions);
                        }
                        Err(err) => {
                            self.last_error = Some(err.to_string());
                            if pack != Pack::None {
                                manager.on_pack_request_failed(pack);
                            }
                        }
                    }
                }
                PendingLangRequest::Difference {
                    lang_pack,
                    lang_code,
                    from_version,
                } => {
                    let pack = manager.pack_type_from_id(langpack, &lang_code);
                    match fetch_lang_pack_difference(
                        &self.invoker,
                        &lang_pack,
                        &lang_code,
                        from_version,
                    )
                    .await
                    {
                        Ok(diff) => {
                            let actions = manager
                                .apply_lang_pack_difference_from_source(langpack, &diff, true, self)
                                .map_err(|e| ApiError::ServerError(e.to_string()))?;
                            all_actions.extend(actions);
                        }
                        Err(ApiError::Invocation(InvocationError::Rpc(rpc))) => {
                            if pack != Pack::None {
                                manager.on_pack_request_failed_with_error(
                                    langpack, pack, &rpc.name, self,
                                );
                            }
                        }
                        Err(err) => {
                            self.last_error = Some(err.to_string());
                            if pack != Pack::None {
                                manager.on_pack_request_failed(pack);
                            }
                        }
                    }
                }
                PendingLangRequest::Language {
                    lang_pack,
                    lang_code,
                } => match fetch_language(&self.invoker, &lang_pack, &lang_code).await {
                    Ok(info) => {
                        all_actions.extend(manager.on_language_received(langpack, &info, self));
                    }
                    Err(ApiError::Invocation(InvocationError::Rpc(rpc))) => {
                        let acts = manager.on_language_failed(&rpc.name);
                        if acts.is_empty() {
                            self.last_error = Some(format!("RPC {}: {}", rpc.code, rpc.name));
                        }
                        all_actions.extend(acts);
                    }
                    Err(err) => {
                        self.last_error = Some(err.to_string());
                        all_actions.extend(manager.on_language_failed(""));
                    }
                },
                PendingLangRequest::Strings {
                    lang_pack,
                    lang_code,
                    keys,
                } => {
                    let strings =
                        fetch_strings(&self.invoker, &lang_pack, &lang_code, keys.clone())
                            .await
                            .unwrap_or_default();
                    if keys.len() == 1 && keys[0] == "lng_sure_save_language" {
                        let switch_actions = manager.on_switch_keys_received(langpack, &strings);
                        if !switch_actions.is_empty() {
                            all_actions.extend(switch_actions);
                            continue;
                        }
                    }
                    if let Some(first_key) = keys.first()
                        && let Some(action) =
                            manager.on_value_for_lang_received(&lang_code, first_key, &strings)
                    {
                        all_actions.push(action);
                    }
                }
            }
        }
        Ok(all_actions)
    }
}
